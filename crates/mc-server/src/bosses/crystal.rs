//! End crystals (minecraft.wiki, "End Crystal"):
//! - placed on obsidian or bedrock with nothing in the block above and no
//!   entity in the two blocks above (not in adventure mode); a placed one
//!   hides its base;
//! - in the End, the block it stands in catches fire while it is air;
//! - almost any hit destroys it with a power 6 explosion (no fire); an
//!   explosion only makes it disappear, so they do not chain, and fireworks
//!   and the dragon cannot hurt it; invulnerable ones (the pillars' during a
//!   respawn) are not hurt at all;
//! - the dragon fight hears about every crystal destroyed.

use mc_entity::boss::Crystal;
use mc_entity::{Entity, Kind};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::MetadataValue;

use crate::context::{Event, ServerContext};
use crate::interact::{Actor, Outcome};
use crate::level::WorldLevel;
use crate::tick::lock;

/// The explosion's power.
pub const POWER: f32 = 6.0;

/// The fields clients know: the beam's target and the base.
pub fn metadata(c: &Crystal) -> Vec<(u8, MetadataValue)> {
    vec![
        (
            8,
            MetadataValue::OptBlockPos(c.beam.map(|b| Position::new(b[0], b[1], b[2]))),
        ),
        (9, MetadataValue::Boolean(c.show_bottom)),
    ]
}

/// A crystal standing at `pos` (its feet).
pub fn new(
    ctx: &ServerContext,
    pos: [f64; 3],
    show_bottom: bool,
    beam: Option<[i32; 3]>,
    invulnerable: bool,
) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Crystal(Crystal {
            beam,
            show_bottom,
            invulnerable,
            struck: None,
        }),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Something hurt the crystal: it goes in the next crystal tick. `kind` is
/// the damage type.
pub fn strike(c: &mut Crystal, by: Option<u128>, kind: &str) {
    if c.invulnerable || c.struck.is_some() || kind == "minecraft:fireworks" {
        return;
    }
    let explosion = matches!(
        kind,
        "minecraft:explosion" | "minecraft:player_explosion" | "minecraft:bad_respawn_point"
    );
    c.struck = Some((by, explosion));
}

/// One tick of every crystal: new ones from world generation, fire, and
/// those destroyed.
pub fn tick_all(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    fresh: &mut Vec<Entity>,
) {
    for nbt in ctx.world.take_generated_entities() {
        if let Some(e) = from_nbt(ctx, &nbt) {
            fresh.push(e);
        }
    }
    let end = ctx.world.dimension == "minecraft:the_end";
    let fire = ctx.data.blocks.default_state("minecraft:fire");
    let mut destroyed = Vec::new();
    let mut blasts = Vec::new();
    for e in entities.iter_mut().filter(|e| !e.removed) {
        let Kind::Crystal(c) = &mut e.kind else {
            continue;
        };
        e.age += 1;
        if let Some((by, explosion)) = c.struck.take() {
            e.removed = true;
            if !explosion {
                blasts.push(crate::explosion::Blast {
                    center: e.pos,
                    power: POWER,
                    source: e.id,
                    owner: by,
                    mob: false,
                    dangerous: false,
                    by: None,
                });
            }
            destroyed.push((e.uuid, e.id, e.pos, by));
            continue;
        }
        if end && let Some(fire) = fire {
            let (x, y, z) = e.block_pos();
            let p = Position::new(x, y, z);
            if ctx
                .world
                .block(p, &ctx.data.blocks)
                .is_some_and(|s| ctx.data.blocks.is_air(s))
            {
                level.set_block(p, fire);
            }
        }
    }
    let gone = crate::explosion::explode_all(ctx, level, entities, &blasts);
    if !gone.is_empty() {
        let _ = ctx
            .events
            .send(Event::EntitiesRemoved(gone.iter().map(|g| g.0).collect()));
    }
    for (uuid, id, pos, by) in destroyed {
        super::fight::crystal_destroyed(ctx, entities, uuid, id, pos, by);
    }
}

/// A crystal from saved NBT (`/summon` and world generation).
pub fn from_nbt(ctx: &ServerContext, nbt: &Compound) -> Option<Entity> {
    let pos = match nbt.get("Pos") {
        Some(Tag::List(l)) if l.len() == 3 => {
            let d = |i: usize| match l[i] {
                Tag::Double(v) => v,
                Tag::Float(v) => f64::from(v),
                _ => 0.0,
            };
            [d(0), d(1), d(2)]
        }
        _ => return None,
    };
    let mut e = new(ctx, pos, true, None, false);
    if let Kind::Crystal(c) = &mut e.kind {
        apply_nbt(c, nbt);
    }
    if let Some(Tag::IntArray(u)) = nbt.get("UUID")
        && u.len() == 4
    {
        e.uuid = u
            .iter()
            .fold(0u128, |a, &p| (a << 32) | u128::from(p as u32));
    }
    Some(e)
}

/// `beam_target`, `ShowBottom` and `Invulnerable` from NBT.
pub fn apply_nbt(c: &mut Crystal, nbt: &Compound) {
    match nbt.get("beam_target") {
        Some(Tag::IntArray(b)) if b.len() == 3 => c.beam = Some([b[0], b[1], b[2]]),
        _ => {}
    }
    if let Some(Tag::Byte(b)) = nbt.get("ShowBottom") {
        c.show_bottom = *b != 0;
    }
    if let Some(Tag::Byte(b)) = nbt.get("Invulnerable") {
        c.invulnerable = *b != 0;
    }
}

/// The crystal's own fields, as `/data` shows them.
pub fn to_nbt(c: &Crystal, out: &mut Compound) {
    if let Some(b) = c.beam {
        out.insert("beam_target", Tag::IntArray(b.to_vec()));
    }
    out.insert("ShowBottom", Tag::Byte(i8::from(c.show_bottom)));
    out.insert("Invulnerable", Tag::Byte(i8::from(c.invulnerable)));
}

/// A player uses an end crystal on a block: it stands on obsidian or
/// bedrock. Returns whether the item was an end crystal.
pub fn place(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    clicked: Position,
) -> bool {
    if name != "minecraft:end_crystal" {
        return false;
    }
    let blocks = &ctx.data.blocks;
    let base = ctx
        .world
        .block(clicked, blocks)
        .and_then(|s| blocks.block_name(s));
    if !matches!(base, Some("minecraft:obsidian" | "minecraft:bedrock")) || actor.game_mode == 2 {
        return true;
    }
    let above = Position::new(clicked.x, clicked.y + 1, clicked.z);
    if !ctx
        .world
        .block(above, blocks)
        .is_some_and(|s| blocks.is_air(s))
    {
        return true;
    }
    let (x, y, z) = (f64::from(above.x), f64::from(above.y), f64::from(above.z));
    let room = mc_entity::Aabb {
        min: [x, y, z],
        max: [x + 1.0, y + 2.0, z + 1.0],
    };
    let overlaps =
        |b: &mc_entity::Aabb| (0..3).all(|i| b.min[i] < room.max[i] && b.max[i] > room.min[i]);
    let taken = lock(&ctx.entities)
        .iter()
        .any(|e| !e.removed && overlaps(&e.aabb()))
        || lock(&ctx.players)
            .values()
            .filter(|p| p.dimension == ctx.world.dimension)
            .filter_map(|p| p.pose)
            .any(|q| {
                overlaps(&mc_entity::Aabb {
                    min: [q.x - 0.3, q.y, q.z - 0.3],
                    max: [q.x + 0.3, q.y + 1.8, q.z + 0.3],
                })
            });
    if taken {
        return true;
    }
    let crystal = new(ctx, [x + 0.5, y, z + 0.5], false, None, false);
    crate::tick::spawn_entity(ctx, crystal);
    if ctx.world.dimension == "minecraft:the_end" {
        super::fight::try_respawn_later(ctx);
    }
    if actor.game_mode != 1 {
        let mut inventories = lock(&ctx.inventories);
        if let Some(hands) = inventories.get_mut(&actor.uuid) {
            let slot = hands.hand_slot(hand);
            if let Some(stack) = &mut hands.slots[slot] {
                stack.count -= 1;
                if stack.count <= 0 {
                    hands.slots[slot] = None;
                }
            }
            outcome.events.push(Event::InventorySlot {
                uuid: actor.uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    true
}
