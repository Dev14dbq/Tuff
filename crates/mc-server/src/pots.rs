//! Decorated pots (minecraft.wiki, "Decorated Pot").
//!
//! The block entity keeps `sherds` (back, left, right, front; a brick where a
//! side was left out) and one `item`. A loot table, as trial chambers save it,
//! is rolled the first time the slot is read. Mining with a tool in
//! `#breaks_decorated_pots` and without Silk Touch, or a projectile, shatters
//! the pot into those four pieces; anything else drops the pot item with its
//! `pot_decorations`. A pot that is already `cracked` always shatters.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_world::BlockEntity;

use crate::containers::Kind;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held::{self, Held};
use crate::interact::Actor;
use crate::level::WorldLevel;
use crate::tick::lock;

const SIDES: [&str; 4] = ["back", "left", "right", "front"];
const POT: &str = "minecraft:decorated_pot";

/// Hopper access only. Not [`crate::containers::kind_of`]: that opens a window.
pub const KIND: Kind = Kind {
    entity: POT,
    slots: 1,
    menu: "minecraft:generic_9x1",
    title: "Decorated Pot",
};

/// Who is breaking the pot. Decides sherds versus the pot item.
#[derive(Debug)]
pub enum Breaker {
    Player {
        creative: bool,
        silk: bool,
        tool: bool,
    },
    Projectile,
    /// A piston or an explosion: the pot item, unless it is already cracked.
    Other,
}

impl Breaker {
    pub fn player(ctx: &ServerContext, uuid: u128, creative: bool) -> Self {
        let held = held::held(ctx, uuid, 0);
        let silk = held
            .as_ref()
            .is_some_and(|h| held::enchant(h.comps.as_ref(), "silk_touch") > 0);
        let tool = held.as_ref().is_some_and(|h| {
            ctx.recipes
                .get()
                .item_tags
                .get("minecraft:breaks_decorated_pots")
                .is_some_and(|tag| tag.contains(&h.stack.item))
        });
        Self::Player {
            creative,
            silk,
            tool,
        }
    }
}

/// What breaking the pot yields, and the state clients should show breaking.
#[derive(Debug)]
pub struct Harvest {
    pub drops: Vec<crate::loot::Drop>,
    /// Cracked, when it shatters, so the break particles use that model.
    pub shown: u32,
}

/// A right click: one item goes in, or the pot wobbles and refuses.
pub fn use_on(ctx: &ServerContext, actor: Actor, hand: i32, at: Position) -> bool {
    let blocks = &ctx.data.blocks;
    if ctx
        .world
        .block(at, blocks)
        .and_then(|s| blocks.block_name(s))
        != Some(POT)
    {
        return false;
    }
    let held = held::held(ctx, actor.uuid, hand);
    let ok = held
        .as_ref()
        .is_some_and(|h| insert(ctx, at, actor.uuid, h, actor.game_mode != 1));
    wobble(ctx, at, !ok);
    true
}

/// The placed pot copies `pot_decorations` off the item in `hand`.
pub fn placed(ctx: &ServerContext, at: Position, state: u32, uuid: u128, hand: i32) {
    let blocks = &ctx.data.blocks;
    if blocks.block_name(state) != Some(POT) {
        return;
    }
    let sides = held::held(ctx, uuid, hand)
        .map(|h| decorations(ctx, &h.stack))
        .unwrap_or_else(|| std::array::from_fn(|_| brick(ctx)));
    let mut data = Compound::new();
    data.insert("sherds", Tag::Compound(save_sides(ctx, &sides)));
    save(ctx, at, data);
}

/// Drops for a pot that is about to be removed. Does not change the world.
pub fn harvest(ctx: &ServerContext, at: Position, state: u32, breaker: Breaker) -> Harvest {
    let blocks = &ctx.data.blocks;
    let data = ctx
        .world
        .block_entity(at, blocks)
        .map(|be| be.data)
        .unwrap_or_default();
    let sides = match data.get("sherds") {
        Some(tag) => read_sides(ctx, tag),
        None => std::array::from_fn(|_| brick(ctx)),
    };
    let stored = contents(ctx, at, &data);
    let cracked = blocks.property(state, "cracked") == Some("true");
    let shatter = match &breaker {
        // Already cracked: even Silk Touch yields the pieces.
        _ if cracked => true,
        Breaker::Projectile => true,
        Breaker::Player {
            creative,
            silk,
            tool,
        } => !creative && !silk && *tool,
        Breaker::Other => false,
    };
    let mut drops = Vec::new();
    if shatter {
        drops.extend(sides.iter().map(crate::loot::Drop::of));
        fx::sound(
            ctx,
            "block.decorated_pot.shatter",
            fx::category::BLOCKS,
            centre(at),
            1.0,
            0.8,
        );
    } else {
        drops.push(pot_item(ctx, &sides));
    }
    if let Some(stack) = stored {
        drops.push(crate::loot::Drop::of(&stack));
    }
    let shown = if shatter {
        ctx.shapes.with(state, "cracked", "true")
    } else {
        state
    };
    Harvest { drops, shown }
}

/// Who sent an impact projectile (arrows, tridents, snowballs, eggs,
/// fireballs, wither skulls, wind charges, fireworks), as far as breaking
/// blocks goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launcher {
    Player(u128),
    Mob,
    /// A dispenser, or a projectile that lost its owner.
    Nobody,
}

impl Launcher {
    pub fn of(owner: Option<u128>, mob: bool) -> Self {
        match owner {
            Some(uuid) => Launcher::Player(uuid),
            None if mob => Launcher::Mob,
            None => Launcher::Nobody,
        }
    }
}

/// What a projectile that struck a block was.
#[derive(Clone, Copy, Debug)]
pub struct Impact {
    pub by: Launcher,
    /// A thrown trident going faster than 0.6 a tick: the only thing that
    /// snaps pointed dripstone and sulfur spikes.
    pub fast_trident: bool,
}

impl Impact {
    pub fn by(by: Launcher) -> Self {
        Impact {
            by,
            fast_trident: false,
        }
    }
}

/// Vanilla lets an impact projectile break a block when the game rule
/// `projectiles_can_break_blocks` is on and its adventure-mode check passes:
/// a player's only outside adventure mode, a mob's only with a `can_break`
/// list (mob-shot ones never carry one), an ownerless one unless the world's
/// default game mode is adventure.
fn may_break(ctx: &ServerContext, by: Launcher) -> bool {
    if !ctx.game_rule_bool("projectiles_can_break_blocks") {
        return false;
    }
    match by {
        Launcher::Player(uuid) => lock(&ctx.players)
            .get(&uuid)
            .is_none_or(|p| p.game_mode != ADVENTURE),
        Launcher::Mob => false,
        Launcher::Nobody => {
            ctx.default_game_mode
                .load(std::sync::atomic::Ordering::Relaxed)
                != ADVENTURE
        }
    }
}

const ADVENTURE: i32 = 2;

fn speleothem(name: &str) -> bool {
    name == "minecraft:pointed_dripstone" || name == "minecraft:sulfur_spike"
}

/// A projectile's path ends on a block: a decorated pot shatters, a chorus
/// flower breaks, and pointed dripstone or a sulfur spike snaps under a
/// fast trident, when [`may_break`] allows.
pub fn projectile(ctx: &ServerContext, from: [f64; 3], to: [f64; 3], hit: Impact) {
    if !may_break(ctx, hit.by) {
        return;
    }
    let Some((_, cell, _)) = crate::hits::first_solid(ctx, from, to) else {
        return;
    };
    let at = Position::new(cell[0], cell[1], cell[2]);
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(at, blocks) else {
        return;
    };
    let Some(name) = blocks.block_name(state) else {
        return;
    };
    let shown = if name == POT {
        harvest(ctx, at, state, Breaker::Projectile)
    } else if name == "minecraft:chorus_flower" || speleothem(name) && hit.fast_trident {
        let props = |k: &str| blocks.property(state, k).map(str::to_owned);
        Harvest {
            drops: ctx.loot.get().drops(name, &props),
            shown: state,
        }
    } else {
        return;
    };
    let left = crate::blocks::break_block(&ctx.data, &ctx.rules, at, state, &|p| {
        ctx.world.block(p, blocks)
    })[0]
        .1;
    let mut level = WorldLevel::new(ctx);
    if !level.set_block(at, left) {
        return;
    }
    let changes = level.into_changes();
    if ctx.game_rule_bool("block_drops") {
        for drop in shown.drops {
            crate::tick::drop_item(ctx, at, drop);
        }
    }
    let _ = ctx.events.send(Event::BlockBroken {
        by: 0,
        position: at,
        state: shown.shown,
    });
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// The one slot, rolling `LootTable` the first time it is read.
pub fn stored(ctx: &ServerContext, at: Position) -> Option<ItemStack> {
    let blocks = &ctx.data.blocks;
    let be = ctx.world.block_entity(at, blocks)?;
    if let Some(stack) = item_tag(ctx, be.data.get("item")) {
        if be.data.get("LootTable").is_some() {
            let mut data = be.data;
            data.remove("LootTable");
            data.remove("LootTableSeed");
            save(ctx, at, data);
        }
        return Some(stack);
    }
    let Tag::String(table) = be.data.get("LootTable")?.clone() else {
        return None;
    };
    let mut data = be.data;
    data.remove("LootTable");
    data.remove("LootTableSeed");
    let stack = roll_one(ctx, at, &table);
    if let Some(stack) = &stack {
        data.insert("item", save_stack(ctx, stack));
    }
    save(ctx, at, data);
    stack
}

/// Writes the hopper slot back. Sherds stay; the loot table has been used.
pub fn write_stored(ctx: &ServerContext, at: Position, stack: Option<ItemStack>) {
    let blocks = &ctx.data.blocks;
    let mut data = ctx
        .world
        .block_entity(at, blocks)
        .map(|be| be.data)
        .unwrap_or_default();
    data.remove("LootTable");
    data.remove("LootTableSeed");
    match &stack {
        Some(stack) => {
            data.insert("item", save_stack(ctx, stack));
        }
        None => {
            data.remove("item");
        }
    }
    if data.get("sherds").is_none() {
        let sides = std::array::from_fn(|_| brick(ctx));
        data.insert("sherds", Tag::Compound(save_sides(ctx, &sides)));
    }
    save(ctx, at, data);
}

/// Comparator reading of the one slot (0 empty, 15 full).
pub fn signal(ctx: &ServerContext, at: Position) -> u8 {
    crate::containers::fullness(ctx, &[stored(ctx, at)])
}

/// Puts one of `held` into the pot. `false` when the slot refuses it.
fn insert(ctx: &ServerContext, at: Position, uuid: u128, held: &Held, consume: bool) -> bool {
    let max = ctx.items.max_stack(held.stack.item).max(1);
    let next = match stored(ctx, at) {
        Some(have)
            if have.item == held.stack.item
                && have.damage == held.stack.damage
                && have.components == held.stack.components
                && have.count < max =>
        {
            let mut next = have;
            next.count += 1;
            next
        }
        None => {
            let mut next = held.stack.clone();
            next.count = 1;
            next
        }
        Some(_) => return false,
    };
    write_stored(ctx, at, Some(next));
    if consume {
        held::consume(ctx, uuid, held.slot, 1);
    }
    lock(&ctx.container_updates).push(at);
    true
}

fn wobble(ctx: &ServerContext, at: Position, fail: bool) {
    let block = ctx.data.builtin.id("minecraft:block", POT).unwrap_or(0);
    let _ = ctx.events.send(Event::BlockAction {
        position: at,
        action: 1,
        param: u8::from(fail),
        block,
    });
    let unit = (crate::util::random_u64() >> 11) as f32 / (1u64 << 53) as f32;
    let (name, pitch) = if fail {
        ("block.decorated_pot.insert_fail", 1.0)
    } else {
        ("block.decorated_pot.insert", 0.7 + unit * 0.5)
    };
    fx::sound(ctx, name, fx::category::BLOCKS, centre(at), 0.9, pitch);
}

fn contents(ctx: &ServerContext, at: Position, data: &Compound) -> Option<ItemStack> {
    if let Some(stack) = item_tag(ctx, data.get("item")) {
        return Some(stack);
    }
    let Tag::String(table) = data.get("LootTable")? else {
        return None;
    };
    roll_one(ctx, at, table)
}

fn roll_one(ctx: &ServerContext, at: Position, table: &str) -> Option<ItemStack> {
    let table = ctx.loot.get().table(table)?.clone();
    let cx = crate::loot::LootCtx {
        origin: Some(centre(at)),
        ..crate::loot::LootCtx::default()
    };
    let env = crate::commands::loot::Env(ctx);
    ctx.loot
        .get()
        .roll(&env, &table, &cx)
        .first()
        .map(|d| d.stack())
}

fn pot_item(ctx: &ServerContext, sides: &[ItemStack; 4]) -> crate::loot::Drop {
    let id = ctx.items.id(POT).unwrap_or(0);
    let mut stack = ItemStack::new(id, 1);
    if !sides.iter().all(|s| is_plain_brick(ctx, s)) {
        stack
            .components
            .set("pot_decorations", Tag::Compound(save_sides(ctx, sides)));
    }
    crate::loot::Drop::of(&stack)
}

fn decorations(ctx: &ServerContext, stack: &ItemStack) -> [ItemStack; 4] {
    match stack.components.get("pot_decorations") {
        Some(tag) => read_sides(ctx, tag),
        None => std::array::from_fn(|_| brick(ctx)),
    }
}

fn read_sides(ctx: &ServerContext, tag: &Tag) -> [ItemStack; 4] {
    let mut sides = std::array::from_fn(|_| brick(ctx));
    match tag {
        Tag::Compound(c) => {
            for (i, key) in SIDES.iter().enumerate() {
                if let Some(stack) = c.get(key).and_then(|v| item_tag(ctx, Some(v))) {
                    sides[i] = stack;
                }
            }
        }
        Tag::List(list) => {
            for (i, value) in list.iter().take(4).enumerate() {
                if let Some(stack) = item_tag(ctx, Some(value)) {
                    sides[i] = stack;
                }
            }
        }
        _ => {}
    }
    sides
}

fn save_sides(ctx: &ServerContext, sides: &[ItemStack; 4]) -> Compound {
    let mut c = Compound::new();
    for (key, stack) in SIDES.iter().zip(sides) {
        c.insert(*key, save_stack(ctx, stack));
    }
    c
}

fn item_tag(ctx: &ServerContext, tag: Option<&Tag>) -> Option<ItemStack> {
    match tag? {
        Tag::String(id) => ctx.items.id(id).map(|item| ItemStack::new(item, 1)),
        Tag::Compound(c) => {
            let Tag::String(id) = c.get("id")? else {
                return None;
            };
            ctx.items
                .id(id)
                .map(|item| ItemStack::from_nbt_with(c, item))
        }
        _ => None,
    }
}

fn save_stack(ctx: &ServerContext, stack: &ItemStack) -> Tag {
    let name = ctx.items.name(stack.item).unwrap_or("minecraft:brick");
    Tag::Compound(stack.to_nbt_with(name))
}

fn is_plain_brick(ctx: &ServerContext, stack: &ItemStack) -> bool {
    ctx.items.name(stack.item) == Some("minecraft:brick")
        && stack.damage == 0
        && stack.components.is_empty()
}

fn brick(ctx: &ServerContext) -> ItemStack {
    ItemStack::new(ctx.items.id("minecraft:brick").unwrap_or(0), 1)
}

fn save(ctx: &ServerContext, at: Position, data: Compound) {
    let blocks = &ctx.data.blocks;
    ctx.world.set_block_entity(
        BlockEntity {
            x: at.x,
            y: at.y,
            z: at.z,
            id: POT.to_owned(),
            data: data.clone(),
        },
        blocks,
    );
    let Some(kind) = ctx.world.block_entity_kind(POT) else {
        return;
    };
    let Some(shown) = crate::world::client_data(POT, &data) else {
        return;
    };
    let _ = ctx.events.send(Event::BlockEntity {
        position: at,
        kind,
        data: shown,
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
    use crate::interact::{Hands, dig, use_item_on};
    use crate::tick::lock;

    const AT: Position = Position { x: 0, y: -60, z: 2 };

    fn setup() -> Option<(std::sync::Arc<ServerContext>, Actor)> {
        let ctx = crate::testkit::flat()?;
        let actor = Actor {
            uuid: 7,
            entity_id: 7,
            game_mode: 0,
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
        lock(&ctx.inventories).insert(7, Hands::default());
        Some((ctx, actor))
    }

    fn hold(ctx: &ServerContext, stack: ItemStack) {
        lock(&ctx.inventories).get_mut(&7).unwrap().slots[36] = Some(stack);
    }

    fn item(ctx: &ServerContext, name: &str, count: i32) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), count)
    }

    fn put_pot(ctx: &ServerContext, at: Position, sherds: Compound, item: Option<ItemStack>) {
        let state = ctx.data.blocks.default_state(POT).unwrap();
        WorldLevel::new(ctx).set_block(at, state);
        let mut data = Compound::new();
        data.insert("sherds", Tag::Compound(sherds));
        if let Some(stack) = item {
            data.insert("item", save_stack(ctx, &stack));
        }
        save(ctx, at, data);
    }

    fn side(ctx: &ServerContext, name: &str) -> Tag {
        save_stack(ctx, &item(ctx, name, 1))
    }

    fn brick_sides(ctx: &ServerContext) -> Compound {
        let mut c = Compound::new();
        for key in SIDES {
            c.insert(key, side(ctx, "minecraft:brick"));
        }
        c
    }

    #[test]
    fn placing_keeps_the_patterns() {
        let Some((ctx, actor)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        let ground = ctx.data.blocks.default_state("minecraft:stone").unwrap();
        WorldLevel::new(&ctx).set_block(AT, ground);
        let mut dec = Compound::new();
        dec.insert("back", side(&ctx, "minecraft:angler_pottery_sherd"));
        dec.insert("left", side(&ctx, "minecraft:brick"));
        let mut pot = item(&ctx, POT, 1);
        pot.components.set("pot_decorations", Tag::Compound(dec));
        hold(&ctx, pot);
        use_item_on(&ctx, actor, 0, AT, 1, [0.5, 1.0, 0.5], 1);
        let above = Position::new(AT.x, AT.y + 1, AT.z);
        let be = ctx.world.block_entity(above, &ctx.data.blocks).unwrap();
        let Tag::Compound(sherds) = be.data.get("sherds").unwrap() else {
            panic!("sherds");
        };
        let Tag::Compound(back) = sherds.get("back").unwrap() else {
            panic!("back");
        };
        assert_eq!(
            back.get("id"),
            Some(&Tag::String("minecraft:angler_pottery_sherd".into()))
        );
    }

    #[test]
    fn insert_stacks_and_comparator_follows() {
        let Some((ctx, actor)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        put_pot(&ctx, AT, brick_sides(&ctx), None);
        hold(&ctx, item(&ctx, "minecraft:cobblestone", 16));
        use_item_on(&ctx, actor, 0, AT, 1, [0.5, 0.5, 0.5], 1);
        use_item_on(&ctx, actor, 0, AT, 1, [0.5, 0.5, 0.5], 2);
        let slot = stored(&ctx, AT).unwrap();
        assert_eq!(
            (slot.item, slot.count),
            (item(&ctx, "minecraft:cobblestone", 1).item, 2)
        );
        assert_eq!(
            lock(&ctx.inventories).get(&7).unwrap().slots[36]
                .as_ref()
                .unwrap()
                .count,
            14
        );
        assert_eq!(signal(&ctx, AT), 1);
        hold(&ctx, item(&ctx, "minecraft:dirt", 1));
        use_item_on(&ctx, actor, 0, AT, 1, [0.5, 0.5, 0.5], 3);
        assert_eq!(stored(&ctx, AT).unwrap().count, 2);
        assert_eq!(
            lock(&ctx.inventories).get(&7).unwrap().slots[36]
                .as_ref()
                .unwrap()
                .count,
            1
        );
    }

    #[test]
    fn tools_shatter_silk_and_hand_keep_the_pot() {
        let Some((ctx, mut actor)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut sherds = brick_sides(&ctx);
        sherds.insert("back", side(&ctx, "minecraft:angler_pottery_sherd"));
        let diamond = item(&ctx, "minecraft:diamond", 1);
        put_pot(&ctx, AT, sherds, Some(diamond.clone()));
        hold(&ctx, crate::testkit::stack(&ctx, "diamond_pickaxe", &[]));
        let outcome = dig(&ctx, actor, 0, AT, 1, 1);
        let angler = ctx.items.id("minecraft:angler_pottery_sherd").unwrap();
        let brick_id = ctx.items.id("minecraft:brick").unwrap();
        assert_eq!(
            outcome
                .drops
                .iter()
                .filter(|(_, d)| d.item == angler)
                .count(),
            1
        );
        assert_eq!(
            outcome
                .drops
                .iter()
                .filter(|(_, d)| d.item == brick_id)
                .count(),
            3
        );
        assert!(outcome.drops.iter().any(|(_, d)| d.item == diamond.item));
        assert!(
            outcome
                .drops
                .iter()
                .all(|(_, d)| d.item != ctx.items.id(POT).unwrap())
        );
        let shown = outcome.events.iter().find_map(|e| match e {
            Event::BlockBroken { state, .. } => Some(*state),
            _ => None,
        });
        assert_eq!(
            ctx.data.blocks.property(shown.unwrap(), "cracked"),
            Some("true")
        );

        put_pot(&ctx, AT, brick_sides(&ctx), None);
        hold(
            &ctx,
            crate::testkit::stack(&ctx, "diamond_pickaxe", &[("silk_touch", 1)]),
        );
        let outcome = dig(&ctx, actor, 0, AT, 1, 1);
        assert_eq!(outcome.drops.len(), 1);
        assert_eq!(outcome.drops[0].1.item, ctx.items.id(POT).unwrap());

        put_pot(&ctx, AT, brick_sides(&ctx), None);
        hold(&ctx, item(&ctx, "minecraft:shears", 1));
        let outcome = dig(&ctx, actor, 0, AT, 1, 1);
        assert_eq!(outcome.drops[0].1.item, ctx.items.id(POT).unwrap());

        let cracked = ctx
            .data
            .blocks
            .state_id(POT, &[("cracked", "true"), ("facing", "north")])
            .unwrap();
        WorldLevel::new(&ctx).set_block(AT, cracked);
        save(&ctx, AT, {
            let mut data = Compound::new();
            data.insert("sherds", Tag::Compound(brick_sides(&ctx)));
            data
        });
        let outcome = dig(&ctx, actor, 0, AT, 1, 1);
        assert!(outcome.drops.iter().all(|(_, d)| d.item == brick_id));

        actor.game_mode = 1;
        put_pot(&ctx, AT, brick_sides(&ctx), Some(diamond));
        let outcome = dig(&ctx, actor, 0, AT, 1, 1);
        assert!(outcome.drops.is_empty());
    }

    #[test]
    fn a_projectile_shatters_it() {
        let Some((ctx, _)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -58, 2);
        put_pot(&ctx, at, brick_sides(&ctx), None);
        projectile(
            &ctx,
            [0.5, -57.5, 0.5],
            [0.5, -57.5, 4.5],
            Impact::by(Launcher::Nobody),
        );
        assert!(
            ctx.world
                .block(at, &ctx.data.blocks)
                .is_some_and(|s| { ctx.data.blocks.block_name(s) != Some(POT) })
        );
        let bricks = lock(&ctx.entities)
            .iter()
            .filter(|e| {
                e.kind
                    .as_stack()
                    .is_some_and(|s| s.item == ctx.items.id("minecraft:brick").unwrap())
            })
            .count();
        assert_eq!(bricks, 4);
    }

    #[test]
    fn what_a_projectile_may_break() {
        let Some((ctx, _)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -58, 2);
        let name = |ctx: &ServerContext| {
            let s = ctx.world.block(at, &ctx.data.blocks).unwrap();
            ctx.data.blocks.block_name(s).unwrap().to_owned()
        };
        let place = |ctx: &ServerContext, block: &str| {
            let state = ctx.data.blocks.default_state(block).unwrap();
            WorldLevel::new(ctx).set_block(at, state);
        };
        let shoot = |ctx: &ServerContext, hit: Impact| {
            projectile(ctx, [0.5, -57.5, 0.5], [0.5, -57.5, 4.5], hit);
        };
        crate::testkit::join(&ctx, 1, 70, [0.5, -60.0, 0.5], 0);
        crate::testkit::join(&ctx, 2, 71, [0.5, -60.0, 0.5], ADVENTURE);
        let player = Impact::by(Launcher::Player(1));

        // A mob's shot carries no `can_break`, so it breaks nothing.
        place(&ctx, "minecraft:chorus_flower");
        shoot(&ctx, Impact::by(Launcher::Mob));
        assert_eq!(name(&ctx), "minecraft:chorus_flower");
        // Nor does a player's in adventure mode.
        shoot(&ctx, Impact::by(Launcher::Player(2)));
        assert_eq!(name(&ctx), "minecraft:chorus_flower");
        shoot(&ctx, player);
        assert_eq!(name(&ctx), "minecraft:air");

        // Pointed dripstone snaps only under a fast trident.
        place(&ctx, "minecraft:pointed_dripstone");
        shoot(&ctx, player);
        assert_eq!(name(&ctx), "minecraft:pointed_dripstone");
        shoot(
            &ctx,
            Impact {
                fast_trident: true,
                ..player
            },
        );
        assert_ne!(name(&ctx), "minecraft:pointed_dripstone");

        lock(&ctx.game_rules).set(
            "projectiles_can_break_blocks",
            crate::rules::Value::Bool(false),
        );
        put_pot(&ctx, at, brick_sides(&ctx), None);
        shoot(&ctx, Impact::by(Launcher::Nobody));
        assert_eq!(name(&ctx), POT);
    }

    #[test]
    fn hoppers_move_the_slot_and_loot_is_rolled_once() {
        let Some((ctx, _)) = setup() else {
            eprintln!("skipped: no game data");
            return;
        };
        put_pot(&ctx, AT, brick_sides(&ctx), None);
        let mut data = ctx.world.block_entity(AT, &ctx.data.blocks).unwrap().data;
        data.insert(
            "LootTable",
            Tag::String("minecraft:archaeology/desert_pyramid".into()),
        );
        save(&ctx, AT, data);
        let rolled = stored(&ctx, AT).unwrap();
        assert!(ctx.items.name(rolled.item).is_some());
        let again = ctx.world.block_entity(AT, &ctx.data.blocks).unwrap();
        assert!(again.data.get("LootTable").is_none());
        assert!(again.data.get("item").is_some());

        write_stored(&ctx, AT, None);
        let hopper = Position::new(0, -59, 2);
        let state = ctx.data.blocks.default_state("minecraft:hopper").unwrap();
        WorldLevel::new(&ctx).set_block(hopper, state);
        let stone = item(&ctx, "minecraft:stone", 1);
        crate::containers::put(
            &ctx,
            hopper,
            crate::containers::kind_of("minecraft:hopper").unwrap(),
            &[(0, Some(stone.clone()))],
        );
        assert!(crate::hoppers::transfer(
            &ctx,
            hopper,
            mc_tick::Dir::Down,
            AT,
            mc_tick::Dir::Up
        ));
        assert_eq!(stored(&ctx, AT).unwrap().item, stone.item);
        assert!(crate::hoppers::transfer(
            &ctx,
            AT,
            mc_tick::Dir::Down,
            hopper,
            mc_tick::Dir::Up
        ));
        assert!(stored(&ctx, AT).is_none());
    }
}
