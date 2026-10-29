//! Spawn eggs: used on a block the mob appears there (a spawner takes the
//! kind instead), looked at water it appears in the water, used on a grown
//! mob of its kind it makes a baby. The pick-block key on a mob in creative
//! gives its egg.

use mc_entity::Collide as _;

use mc_entity::nav::{Liquid, Terrain as _};
use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use super::{Ground, new_mob};
use crate::context::{Event, ServerContext};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// The kind an egg makes.
pub fn species_of(egg: &str) -> Option<Species> {
    let kind = egg.strip_suffix("_spawn_egg")?;
    Species::from_id(kind)
}

/// The egg of a kind.
pub fn egg_of(species: Species) -> String {
    format!("{}_spawn_egg", species.id())
}

fn used_up(ctx: &ServerContext, actor: &Actor, slot: usize) {
    if actor.game_mode != 1 {
        crate::held::consume(ctx, actor.uuid, slot, 1);
    }
}

fn swing(outcome: &mut Outcome, actor: &Actor) {
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
}

/// Puts a new mob of the kind into the world at `pos`, facing away from
/// the player.
fn place(ctx: &ServerContext, species: Species, pos: [f64; 3], actor: &Actor) -> Entity {
    let mut e = new_mob(ctx, species, pos);
    if let Kind::Mob(m) = &mut e.kind {
        m.yaw = (actor.pose.yaw + 180.0).rem_euclid(360.0);
        m.head_yaw = m.yaw;
        // Placed by hand: it stays.
        m.persistent = true;
    }
    let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    lock(&ctx.entities).push(e.clone());
    e
}

/// Any spawn egg used on a spawner or trial spawner while the game rule
/// `spawner_blocks_work` is off: vanilla refuses it and tells the player
/// why; the egg stays in the hand. Returns whether it was refused.
pub fn refused_by_spawner(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    clicked: Position,
) -> bool {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(clicked, blocks).unwrap_or(0);
    if !matches!(
        blocks.block_name(state),
        Some("minecraft:spawner" | "minecraft:trial_spawner")
    ) || ctx.game_rule_bool("spawner_blocks_work")
    {
        return false;
    }
    outcome.events.push(Event::Message {
        uuid: actor.uuid,
        line: mc_protocol::text::translate("advMode.notEnabled.spawner", Vec::new()),
        overlay: false,
    });
    true
}

/// A spawn egg used on a block. Returns whether the egg did something.
pub fn use_on_block(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    clicked: Position,
    face: usize,
) -> bool {
    let Some(species) = species_of(name) else {
        return false;
    };
    let Some(held) = crate::held::held(ctx, actor.uuid, hand) else {
        return false;
    };
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(clicked, blocks).unwrap_or(0);
    if blocks.block_name(state) == Some("minecraft:spawner") {
        set_spawner(ctx, outcome, clicked, species);
        used_up(ctx, &actor, held.slot);
        swing(outcome, &actor);
        return true;
    }
    // Into a block one can walk through (grass, snow layers aside), else
    // next to it; on top of a fence or wall it stands on its top.
    let ground = Ground::new(ctx);
    let open = !ground.solid(clicked.x, clicked.y, clicked.z);
    let at = if open {
        clicked
    } else {
        crate::blocks::offset(clicked, face)
    };
    // On a fence it stands on the fence's top.
    let y = match ground.span(at.x, at.y - 1, at.z) {
        Some((_, top)) if face == 1 => f64::from(at.y - 1) + top,
        _ => f64::from(at.y),
    };
    place(
        ctx,
        species,
        [f64::from(at.x) + 0.5, y, f64::from(at.z) + 0.5],
        &actor,
    );
    used_up(ctx, &actor, held.slot);
    swing(outcome, &actor);
    true
}

/// A spawn egg used in the air: in water looked at, the mob appears there.
pub fn use_in_air(
    ctx: &ServerContext,
    actor: Actor,
    hand: i32,
    yaw: f32,
    pitch: f32,
) -> Option<Outcome> {
    let held = crate::held::held(ctx, actor.uuid, hand)?;
    let species = species_of(&held.name)?;
    let mut outcome = Outcome::default();
    let (hit, _) = crate::interact::look_at(ctx, actor, yaw, pitch, true)?;
    let ground = Ground::new(ctx);
    if ground.liquid(hit.x, hit.y, hit.z) != Liquid::Water {
        return Some(outcome);
    }
    place(
        ctx,
        species,
        [
            f64::from(hit.x) + 0.5,
            f64::from(hit.y),
            f64::from(hit.z) + 0.5,
        ],
        &actor,
    );
    used_up(ctx, &actor, held.slot);
    swing(&mut outcome, &actor);
    Some(outcome)
}

/// A spawner now makes this kind (`SpawnData`, which the client shows
/// turning inside).
fn set_spawner(ctx: &ServerContext, outcome: &mut Outcome, at: Position, species: Species) {
    use mc_nbt::{Compound, Tag};
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(at, blocks)
        .unwrap_or_else(|| mc_world::BlockEntity {
            x: at.x,
            y: at.y,
            z: at.z,
            id: "minecraft:mob_spawner".into(),
            data: Compound::new(),
        });
    let mut entity = Compound::new();
    entity.insert("id", Tag::String(species.id().into()));
    let mut spawn = Compound::new();
    spawn.insert("entity", Tag::Compound(entity));
    be.data.insert("SpawnData", Tag::Compound(spawn));
    be.data.remove("SpawnPotentials");
    let data = be.data.clone();
    ctx.world.set_block_entity(be, blocks);
    if let Some(kind) = ctx.world.block_entity_kind("minecraft:mob_spawner") {
        outcome.events.push(Event::BlockEntity {
            position: at,
            kind,
            data,
        });
    }
}

/// A spawn egg used on a grown mob of its kind: a baby of it.
pub fn breed_with_egg(
    ctx: &ServerContext,
    e: &Entity,
    by: &super::Actor,
    name: &str,
    slot: usize,
) -> bool {
    let Kind::Mob(m) = &e.kind else {
        return false;
    };
    if species_of(name) != Some(m.species) || !m.species.ageable() || m.is_baby() {
        return false;
    }
    let mut baby = new_mob(ctx, m.species, e.pos);
    if let Kind::Mob(b) = &mut baby.kind {
        b.set_baby(true);
        b.variant = m.variant;
        b.persistent = true;
    }
    let _ = ctx.events.send(Event::EntitySpawned(baby.clone()));
    lock(&ctx.entities).push(baby);
    if by.game_mode != 1 {
        crate::held::consume(ctx, by.uuid, slot, 1);
    }
    true
}

/// The egg a creative player picks from a mob (pick-block on it).
pub fn pick(ctx: &ServerContext, e: &Entity) -> Option<i32> {
    let name = match &e.kind {
        Kind::Mob(m) => egg_of(m.species),
        Kind::Merchant(m) if m.wandering => "minecraft:wandering_trader_spawn_egg".into(),
        Kind::Merchant(_) => "minecraft:villager_spawn_egg".into(),
        _ => return None,
    };
    ctx.items.id(&name)
}
