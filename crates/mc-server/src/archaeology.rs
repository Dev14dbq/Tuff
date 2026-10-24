//! Brushing suspicious sand and gravel (minecraft.wiki, "Brush", "Suspicious
//! Sand").
//!
//! Holding use on the block is one packet a tick. Dust is cleared in four
//! stages of 6, 20, 30 and 40 ticks (96 ticks in all). Stopping lets the dust
//! settle back one stage every two seconds. The loot table is rolled when
//! brushing starts; at the end the block becomes sand or gravel and the brush
//! takes one point of damage. Suspicious blocks fall only while `dusted` is 0,
//! and a fall destroys them instead of placing them again.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_world::BlockEntity;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::interact::Actor;
use crate::level::WorldLevel;
use crate::tick::lock;

const BRUSH: &str = "minecraft:brush";
const ENTITY: &str = "minecraft:brushable_block";
/// Progress at which `dusted` becomes 1, 2, 3, then the block is cleared.
const STAGE: [u32; 4] = [6, 26, 56, 96];
/// Ticks a stage stays after brushing stops (two seconds).
const DECAY: u32 = 40;

#[derive(Clone, Copy, Debug)]
struct Session {
    player: u128,
    entity_id: i32,
    hand: i32,
    pos: Position,
    progress: u32,
    /// Game age of the latest use packet.
    pressed: i64,
    /// Game age already counted as a brush tick.
    counted: i64,
    face: u8,
}

/// Brush sessions and blocks waiting to settle, for one dimension.
#[derive(Debug, Default)]
pub struct State {
    sessions: Vec<Session>,
    dusty: HashMap<Position, u32>,
}

/// A click with a brush. `Start` is a new block; `Keep` continues one.
#[derive(Debug)]
pub enum Press {
    No,
    Start,
    Keep,
}

/// Right click with a brush. Repeated clicks while held keep the session.
pub fn brush(ctx: &ServerContext, actor: Actor, hand: i32, at: Position, face: usize) -> Press {
    let Some(held) = held::held(ctx, actor.uuid, hand) else {
        return Press::No;
    };
    if held.name != BRUSH {
        return Press::No;
    }
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(at, blocks) else {
        return Press::No;
    };
    if !blocks.block_name(state).is_some_and(suspicious) {
        return Press::No;
    }
    let now = lock(&ctx.time).age;
    let face = u8::try_from(face).unwrap_or(1);
    let progress = stage_start(dusted_of(ctx, at));
    let mut arch = ctx.archaeology.lock().expect("archaeology");
    if let Some(session) = arch.sessions.iter_mut().find(|s| s.player == actor.uuid) {
        let same = session.pos == at;
        let face_changed = session.face != face;
        session.pos = at;
        session.hand = hand;
        session.entity_id = actor.entity_id;
        session.pressed = now;
        session.face = face;
        if same {
            drop(arch);
            if face_changed {
                set_face(ctx, at, Some(face));
            }
            return Press::Keep;
        }
        session.progress = progress;
        session.counted = -1;
        drop(arch);
        begin(ctx, at, face);
        show_using(ctx, actor.uuid, actor.entity_id, Some(hand));
        return Press::Start;
    }
    arch.sessions.push(Session {
        player: actor.uuid,
        entity_id: actor.entity_id,
        hand,
        pos: at,
        progress,
        pressed: now,
        counted: -1,
        face,
    });
    drop(arch);
    begin(ctx, at, face);
    show_using(ctx, actor.uuid, actor.entity_id, Some(hand));
    Press::Start
}

/// The player let go, or changed what they hold.
pub fn stop(ctx: &ServerContext, uuid: u128) {
    let removed: Vec<Session> = {
        let mut arch = ctx.archaeology.lock().expect("archaeology");
        let mut kept = Vec::new();
        let mut removed = Vec::new();
        for session in arch.sessions.drain(..) {
            if session.player == uuid {
                removed.push(session);
            } else {
                kept.push(session);
            }
        }
        arch.sessions = kept;
        removed
    };
    for session in removed {
        show_using(ctx, session.player, session.entity_id, None);
        if dusted_of(ctx, session.pos) == 0 {
            set_face(ctx, session.pos, None);
        }
    }
}

/// A loaded brushable block that is already dusty keeps settling.
pub fn loaded(ctx: &ServerContext, at: Position) {
    if dusted_of(ctx, at) > 0 {
        ctx.archaeology
            .lock()
            .expect("archaeology")
            .dusty
            .entry(at)
            .or_insert(0);
    }
}

/// One game tick: advance brushes that are still held, then settle the rest.
pub fn tick(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    let sessions: Vec<Session> = ctx
        .archaeology
        .lock()
        .expect("archaeology")
        .sessions
        .clone();
    let mut brushed = Vec::new();
    let mut finished = Vec::new();
    for session in sessions {
        if !holding(ctx, session.player, session.hand) {
            finished.push(session.player);
            continue;
        }
        if !aiming(ctx, session.player, session.pos) {
            reset(ctx, &session);
            continue;
        }
        let snap = {
            let mut arch = ctx.archaeology.lock().expect("archaeology");
            let Some(live) = arch
                .sessions
                .iter_mut()
                .find(|s| s.player == session.player && s.pos == session.pos)
            else {
                continue;
            };
            if now > live.pressed.saturating_add(1) {
                live.progress = stage_start(dusted_of(ctx, live.pos));
                continue;
            }
            if live.counted >= now {
                continue;
            }
            live.progress += 1;
            live.counted = now;
            *live
        };
        if apply(ctx, &snap) {
            finished.push(snap.player);
        } else {
            brushed.push(snap.pos);
        }
    }
    for player in finished {
        stop(ctx, player);
    }
    decay(ctx, &brushed);
}

fn apply(ctx: &ServerContext, session: &Session) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(session.pos, blocks) else {
        return true;
    };
    let Some(name) = blocks.block_name(state) else {
        return true;
    };
    if !suspicious(name) {
        return true;
    }
    if session.progress >= STAGE[3] {
        finish(ctx, session, name);
        return true;
    }
    let want = dust_level(session.progress);
    if want != dusted_of(ctx, session.pos) {
        set_state(
            ctx,
            session.pos,
            ctx.shapes.with(state, "dusted", &want.to_string()),
        );
        play(ctx, name, session.pos, false);
        mark(ctx, session.pos, want > 0);
    } else if session.progress.is_multiple_of(5) {
        play(ctx, name, session.pos, false);
    }
    false
}

fn finish(ctx: &ServerContext, session: &Session, name: &str) {
    let found = ctx
        .world
        .block_entity(session.pos, &ctx.data.blocks)
        .and_then(|be| item_of(ctx, be.data.get("item")));
    let block = if name.ends_with("gravel") {
        "minecraft:gravel"
    } else {
        "minecraft:sand"
    };
    let state = ctx.data.blocks.default_state(block).unwrap_or(0);
    set_state(ctx, session.pos, state);
    mark(ctx, session.pos, false);
    play(ctx, name, session.pos, true);
    if ctx.game_rule_bool("block_drops")
        && let Some(stack) = found
    {
        crate::tick::drop_item(ctx, session.pos, crate::loot::Drop::of(&stack));
    }
    if lock(&ctx.players)
        .get(&session.player)
        .is_none_or(|p| p.game_mode != 1)
    {
        let slot = lock(&ctx.inventories)
            .get(&session.player)
            .map(|h| h.hand_slot(session.hand));
        if let Some(slot) = slot {
            held::wear(ctx, session.player, slot, 1);
        }
    }
}

fn decay(ctx: &ServerContext, brushed: &[Position]) {
    let mut arch = ctx.archaeology.lock().expect("archaeology");
    let positions: Vec<Position> = arch.dusty.keys().copied().collect();
    let mut due = Vec::new();
    for pos in positions {
        if brushed.contains(&pos) {
            if let Some(idle) = arch.dusty.get_mut(&pos) {
                *idle = 0;
            }
            continue;
        }
        let Some(idle) = arch.dusty.get_mut(&pos) else {
            continue;
        };
        *idle += 1;
        if *idle >= DECAY {
            due.push(pos);
        }
    }
    for pos in &due {
        arch.dusty.remove(pos);
    }
    drop(arch);
    for pos in due {
        lower(ctx, pos);
    }
}

fn lower(ctx: &ServerContext, at: Position) {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(at, blocks) else {
        return;
    };
    if !blocks.block_name(state).is_some_and(suspicious) {
        return;
    }
    let have = dusted_of(ctx, at);
    if have == 0 {
        set_face(ctx, at, None);
        return;
    }
    let want = have - 1;
    set_state(ctx, at, ctx.shapes.with(state, "dusted", &want.to_string()));
    let start = stage_start(want);
    if want == 0 {
        set_face(ctx, at, None);
        wake(ctx, at);
    } else {
        ctx.archaeology
            .lock()
            .expect("archaeology")
            .dusty
            .insert(at, 0);
    }
    for session in &mut ctx.archaeology.lock().expect("archaeology").sessions {
        if session.pos == at && session.progress > start {
            session.progress = start;
        }
    }
}

fn begin(ctx: &ServerContext, at: Position, face: u8) {
    edit(ctx, at, |data| {
        let has_item = data.get("item").is_some();
        if let Some(Tag::String(table)) = data.get("LootTable").cloned() {
            data.remove("LootTable");
            data.remove("LootTableSeed");
            if !has_item
                && let Some(stack) = roll(ctx, at, &table)
                && let Some(name) = ctx.items.name(stack.item)
            {
                data.insert("item", Tag::Compound(stack.to_nbt_with(name)));
            }
        }
        data.insert("hit_direction", Tag::Byte(i8::try_from(face).unwrap_or(1)));
    });
}

fn roll(ctx: &ServerContext, at: Position, table: &str) -> Option<ItemStack> {
    let table = ctx.loot.table(table)?.clone();
    let cx = crate::loot::LootCtx {
        origin: Some(centre(at)),
        ..crate::loot::LootCtx::default()
    };
    let env = crate::commands::loot::Env(ctx);
    ctx.loot.roll(&env, &table, &cx).first().map(|d| d.stack())
}

fn edit(ctx: &ServerContext, at: Position, change: impl FnOnce(&mut Compound)) {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(at, blocks) else {
        return;
    };
    if !blocks.block_name(state).is_some_and(suspicious) {
        return;
    }
    let mut be = ctx.world.block_entity(at, blocks).unwrap_or(BlockEntity {
        x: at.x,
        y: at.y,
        z: at.z,
        id: ENTITY.to_owned(),
        data: Compound::new(),
    });
    change(&mut be.data);
    let data = be.data.clone();
    ctx.world.set_block_entity(be, blocks);
    let Some(kind) = ctx.world.block_entity_kind(ENTITY) else {
        return;
    };
    let Some(shown) = crate::world::client_data(ENTITY, &data) else {
        return;
    };
    let _ = ctx.events.send(Event::BlockEntity {
        position: at,
        kind,
        data: shown,
    });
}

fn set_face(ctx: &ServerContext, at: Position, face: Option<u8>) {
    edit(ctx, at, |data| match face {
        Some(face) => {
            data.insert("hit_direction", Tag::Byte(i8::try_from(face).unwrap_or(1)));
        }
        None => {
            data.remove("hit_direction");
        }
    });
}

fn set_state(ctx: &ServerContext, at: Position, state: u32) {
    let mut level = WorldLevel::new(ctx);
    level.set_block(at, state);
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

fn wake(ctx: &ServerContext, at: Position) {
    let Some(state) = ctx.world.block(at, &ctx.data.blocks) else {
        return;
    };
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    mc_tick::gravity::wake(&mut level, logic, crate::level::to_pos(at), state);
}

fn mark(ctx: &ServerContext, at: Position, on: bool) {
    let mut arch = ctx.archaeology.lock().expect("archaeology");
    if on {
        arch.dusty.entry(at).or_insert(0);
    } else {
        arch.dusty.remove(&at);
    }
}

fn reset(ctx: &ServerContext, session: &Session) {
    let start = stage_start(dusted_of(ctx, session.pos));
    if let Some(live) = ctx
        .archaeology
        .lock()
        .expect("archaeology")
        .sessions
        .iter_mut()
        .find(|s| s.player == session.player && s.pos == session.pos)
    {
        live.progress = start;
    }
}

fn holding(ctx: &ServerContext, uuid: u128, hand: i32) -> bool {
    held::held(ctx, uuid, hand).is_some_and(|h| h.name == BRUSH)
}

fn aiming(ctx: &ServerContext, uuid: u128, at: Position) -> bool {
    let Some(pose) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose) else {
        return false;
    };
    let eye = [pose.x, pose.y + 1.62, pose.z];
    let center = centre(at);
    let delta = [center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]];
    let dist = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    if !(1e-6..6.0).contains(&dist) {
        return false;
    }
    let (yaw, pitch) = (
        f64::from(pose.yaw).to_radians(),
        f64::from(pose.pitch).to_radians(),
    );
    let look = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    (look[0] * delta[0] + look[1] * delta[1] + look[2] * delta[2]) / dist > 0.3
}

fn suspicious(name: &str) -> bool {
    matches!(
        name,
        "minecraft:suspicious_sand" | "minecraft:suspicious_gravel"
    )
}

fn dusted_of(ctx: &ServerContext, at: Position) -> u32 {
    ctx.world
        .block(at, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.property(s, "dusted"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn dust_level(progress: u32) -> u32 {
    if progress >= STAGE[2] {
        3
    } else if progress >= STAGE[1] {
        2
    } else if progress >= STAGE[0] {
        1
    } else {
        0
    }
}

fn stage_start(dusted: u32) -> u32 {
    match dusted {
        0 => 0,
        1 => STAGE[0],
        2 => STAGE[1],
        _ => STAGE[2],
    }
}

fn item_of(ctx: &ServerContext, tag: Option<&Tag>) -> Option<ItemStack> {
    let Tag::Compound(c) = tag? else {
        return None;
    };
    let Tag::String(id) = c.get("id")? else {
        return None;
    };
    ctx.items
        .id(id)
        .map(|item| ItemStack::from_nbt_with(c, item))
}

fn play(ctx: &ServerContext, name: &str, at: Position, done: bool) {
    let gravel = name.ends_with("gravel");
    let sound = match (gravel, done) {
        (true, true) => "item.brush.brushing.gravel.complete",
        (false, true) => "item.brush.brushing.sand.complete",
        (true, false) => "item.brush.brushing.gravel",
        (false, false) => "item.brush.brushing.sand",
    };
    let (volume, pitch) = if done { (1.0, 1.0) } else { (0.6, 0.6) };
    fx::sound(ctx, sound, fx::category::BLOCKS, centre(at), volume, pitch);
}

fn show_using(ctx: &ServerContext, uuid: u128, entity_id: i32, hand: Option<i32>) {
    let _ = ctx.events.send(Event::UsingItem {
        uuid,
        entity_id,
        hand,
    });
}

fn centre(at: Position) -> [f64; 3] {
    [
        f64::from(at.x) + 0.5,
        f64::from(at.y) + 0.5,
        f64::from(at.z) + 0.5,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Pose;
    use crate::interact::{Actor, use_item_on};
    use crate::level::WorldLevel;
    use crate::tick::lock;
    use mc_nbt::{Compound, Tag};

    const AT: Position = Position { x: 0, y: -60, z: 2 };

    fn setup(mode: i32) -> Option<(std::sync::Arc<ServerContext>, Actor)> {
        let ctx = crate::testkit::flat()?;
        crate::testkit::join(&ctx, 7, 7, [0.5, -60.0, 0.5], mode);
        let actor = Actor {
            uuid: 7,
            entity_id: 7,
            game_mode: mode,
            sneaking: false,
            pose: Pose {
                x: 0.5,
                y: -60.0,
                z: 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
        };
        Some((ctx, actor))
    }

    fn hold_brush(ctx: &ServerContext) {
        let id = ctx.items.id(BRUSH).unwrap();
        lock(&ctx.inventories).get_mut(&7).unwrap().slots[36] = Some(ItemStack::new(id, 1));
    }

    fn put_sand(ctx: &ServerContext, at: Position, dusted: &str) {
        let state = ctx
            .data
            .blocks
            .state_id("minecraft:suspicious_sand", &[("dusted", dusted)])
            .unwrap();
        WorldLevel::new(ctx).set_block(at, state);
    }

    fn brush_for(ctx: &ServerContext, actor: Actor, n: u32) {
        for _ in 0..n {
            lock(&ctx.time).age += 1;
            use_item_on(ctx, actor, 0, AT, 1, [0.5, 0.5, 0.5], 1);
            tick(ctx);
        }
    }

    fn dusted(ctx: &ServerContext, at: Position) -> Option<&str> {
        let state = ctx.world.block(at, &ctx.data.blocks)?;
        ctx.data.blocks.property(state, "dusted")
    }

    #[test]
    fn brushing_rolls_loot_then_becomes_sand() {
        let Some((ctx, actor)) = setup(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        put_sand(&ctx, AT, "0");
        let mut data = Compound::new();
        data.insert(
            "LootTable",
            Tag::String("minecraft:archaeology/desert_pyramid".into()),
        );
        data.insert("LootTableSeed", Tag::Long(1));
        ctx.world.set_block_entity(
            BlockEntity {
                x: AT.x,
                y: AT.y,
                z: AT.z,
                id: ENTITY.to_owned(),
                data,
            },
            &ctx.data.blocks,
        );
        hold_brush(&ctx);
        brush_for(&ctx, actor, 1);
        let be = ctx.world.block_entity(AT, &ctx.data.blocks).unwrap();
        assert!(be.data.get("LootTable").is_none());
        assert!(be.data.get("item").is_some());
        assert_eq!(dusted(&ctx, AT), Some("0"));
        brush_for(&ctx, actor, 5);
        assert_eq!(dusted(&ctx, AT), Some("1"));
        brush_for(&ctx, actor, 20);
        assert_eq!(dusted(&ctx, AT), Some("2"));
        brush_for(&ctx, actor, 70);
        let state = ctx.world.block(AT, &ctx.data.blocks).unwrap();
        assert_eq!(ctx.data.blocks.block_name(state), Some("minecraft:sand"));
        let brush = lock(&ctx.inventories).get(&7).unwrap().slots[36]
            .clone()
            .unwrap();
        assert_eq!(brush.damage, 1);
        assert!(
            lock(&ctx.entities)
                .iter()
                .any(|e| e.kind.as_stack().is_some())
        );
    }

    #[test]
    fn dust_settles_when_brushing_stops() {
        let Some((ctx, actor)) = setup(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        put_sand(&ctx, AT, "0");
        hold_brush(&ctx);
        brush_for(&ctx, actor, 6);
        assert_eq!(dusted(&ctx, AT), Some("1"));
        stop(&ctx, 7);
        for _ in 0..DECAY {
            lock(&ctx.time).age += 1;
            tick(&ctx);
        }
        assert_eq!(dusted(&ctx, AT), Some("0"));
        let be = ctx.world.block_entity(AT, &ctx.data.blocks);
        assert!(be.is_none_or(|be| be.data.get("hit_direction").is_none()));
    }

    #[test]
    fn only_undusted_suspicious_sand_falls_and_a_fall_removes_it() {
        let Some((ctx, _)) = setup(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let below = Position::new(4, -60, 4);
        let at = Position::new(4, -59, 4);
        put_sand(&ctx, at, "1");
        WorldLevel::new(&ctx).set_block(below, ctx.rules.air);
        lock(&ctx.time).age += 2;
        crate::tick::run_scheduled_ticks(&ctx);
        assert_eq!(
            ctx.data
                .blocks
                .block_name(ctx.world.block(at, &ctx.data.blocks).unwrap()),
            Some("minecraft:suspicious_sand")
        );

        put_sand(&ctx, at, "0");
        WorldLevel::new(&ctx).set_block(below, ctx.rules.air);
        lock(&ctx.time).age += 2;
        crate::tick::run_scheduled_ticks(&ctx);
        assert_ne!(
            ctx.data
                .blocks
                .block_name(ctx.world.block(at, &ctx.data.blocks).unwrap()),
            Some("minecraft:suspicious_sand")
        );
        assert!(
            lock(&ctx.entities)
                .iter()
                .any(|e| { matches!(e.kind, mc_entity::Kind::FallingBlock { .. }) })
        );

        let state = ctx
            .data
            .blocks
            .default_state("minecraft:suspicious_sand")
            .unwrap();
        WorldLevel::new(&ctx).set_block(
            below,
            ctx.data.blocks.default_state("minecraft:stone").unwrap(),
        );
        lock(&ctx.entities).push(mc_entity::Entity {
            id: ctx.next_entity_id(),
            uuid: 9,
            kind: mc_entity::Kind::FallingBlock { state },
            pos: [4.5, -59.0, 4.5],
            vel: [0.0, -0.4, 0.0],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        });
        crate::tick::run_scheduled_ticks(&ctx);
        assert_ne!(
            ctx.data
                .blocks
                .block_name(ctx.world.block(at, &ctx.data.blocks).unwrap()),
            Some("minecraft:suspicious_sand")
        );

        let high = Position::new(4, 40, 4);
        let _ = ctx.world.block(high, &ctx.data.blocks);
        lock(&ctx.entities).push(mc_entity::Entity {
            id: ctx.next_entity_id(),
            uuid: 10,
            kind: mc_entity::Kind::FallingBlock { state },
            pos: [4.5, 40.0, 4.5],
            vel: [0.0, 0.0, 0.0],
            on_ground: false,
            age: 600,
            removed: false,
            tags: Vec::new(),
        });
        crate::tick::run_scheduled_ticks(&ctx);
        let item = ctx.items.id("minecraft:suspicious_sand").unwrap();
        assert!(
            lock(&ctx.entities)
                .iter()
                .any(|e| { e.kind.as_stack().is_some_and(|s| s.item == item) })
        );
        assert_ne!(
            ctx.data
                .blocks
                .block_name(ctx.world.block(high, &ctx.data.blocks).unwrap()),
            Some("minecraft:suspicious_sand")
        );
    }
}
