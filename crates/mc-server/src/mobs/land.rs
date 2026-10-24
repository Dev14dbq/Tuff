//! Land animals (Cursor): what a goat's ram and its horns do in the world.

use mc_entity::{Entity, Kind, Species};
use mc_nbt::Tag;
use mc_protocol::components::ItemStack;

use super::Target;
use crate::context::ServerContext;
use crate::fx;

/// The ram connects. Most mobs do not fight back; piglins and the wither do
/// (minecraft.wiki, "Goat" → "Ramming"). Piglin brutes, hoglins and zoglins
/// join that list with those kinds.
pub fn ram(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    knockback: f64,
    targets: &[Target],
) {
    let (id, pos) = (entities[i].id, entities[i].pos);
    let Kind::Mob(m) = &entities[i].kind else {
        return;
    };
    let species = m.species;
    if let Some(t) = targets.iter().find(|t| t.id == target) {
        let hit = crate::combat::Hit {
            amount: damage,
            kind: "minecraft:mob_attack",
            death_key: "death.attack.mob",
            attacker: Some(species.display_name()),
            by: Some(id),
            from: Some(pos),
            knockback,
            disables_shield: 0.0,
        };
        crate::combat::hurt_player(ctx, t.uuid, &hit);
        return;
    }
    let Some(j) = entities.iter().position(|e| e.id == target) else {
        return;
    };
    if !matches!(entities[j].kind, Kind::Mob(_)) {
        return;
    }
    let harm = super::hurt::Harm {
        by: Some(id),
        from: Some(pos),
        knockback,
        killer: Some(species.id().to_owned()),
        ..super::hurt::Harm::new(damage, "minecraft:mob_attack")
    };
    if super::hurt::harm(ctx, &mut entities[j], &harm)
        && let Kind::Mob(t) = &mut entities[j].kind
        && !retaliates(t.species)
    {
        t.revenge = None;
        t.anger = 0;
    }
}

fn retaliates(species: Species) -> bool {
    matches!(species, Species::Piglin | Species::Wither)
}

/// An adult goat loses one horn on a `#snaps_goat_horn` block. The two horns
/// of one goat are the same instrument, chosen from the biome the first time.
pub fn horn(ctx: &ServerContext, e: &mut Entity) {
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.species != Species::Goat || m.is_baby() {
        return;
    }
    if m.quirks.land.horn_group > 3 {
        let group = horn_group(&biome_at(ctx, e.pos));
        m.quirks.land.horn_group = group;
    }
    let group = m.quirks.land.horn_group;
    let dropped = if m.quirks.land.left_horn {
        m.quirks.land.left_horn = false;
        true
    } else if m.quirks.land.right_horn {
        m.quirks.land.right_horn = false;
        true
    } else {
        false
    };
    if !dropped {
        return;
    }
    m.dirty = true;
    let screaming = m.quirks.land.screaming;
    let pos = [e.pos[0], e.pos[1] + 0.3, e.pos[2]];
    fx::sound(
        ctx,
        "entity.goat.horn_break",
        fx::category::NEUTRAL,
        e.pos,
        0.9,
        1.0,
    );
    let Some(item) = ctx.items.id("minecraft:goat_horn") else {
        return;
    };
    let mut stack = ItemStack::new(item, 1);
    stack.components.set(
        "minecraft:instrument",
        Tag::String(instrument(group, screaming).to_owned()),
    );
    crate::tick::spawn_stack_at(ctx, pos, &stack);
}

fn biome_at(ctx: &ServerContext, pos: [f64; 3]) -> String {
    let at = mc_protocol::Position::new(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    );
    ctx.world
        .biome(at, &ctx.data.blocks)
        .and_then(|id| ctx.world.biome_name(id))
        .unwrap_or("minecraft:plains")
        .to_owned()
}

/// 0 ponder, 1 sing, 2 seek, 3 feel (and the screaming twin of each).
pub fn horn_group(biome: &str) -> u8 {
    let b = biome.trim_start_matches("minecraft:");
    if b == "swamp" || b == "mangrove_swamp" {
        return 1;
    }
    const SEEK: &[&str] = &[
        "snowy_plains",
        "ice_spikes",
        "snowy_taiga",
        "snowy_beach",
        "frozen_river",
        "frozen_ocean",
        "deep_frozen_ocean",
        "grove",
        "snowy_slopes",
        "jagged_peaks",
        "frozen_peaks",
    ];
    if SEEK.contains(&b) {
        return 2;
    }
    const FEEL: &[&str] = &[
        "desert",
        "badlands",
        "eroded_badlands",
        "wooded_badlands",
        "savanna",
        "savanna_plateau",
        "windswept_savanna",
    ];
    if FEEL.contains(&b) { 3 } else { 0 }
}

fn instrument(group: u8, screaming: bool) -> &'static str {
    const REGULAR: [&str; 4] = [
        "minecraft:ponder_goat_horn",
        "minecraft:sing_goat_horn",
        "minecraft:seek_goat_horn",
        "minecraft:feel_goat_horn",
    ];
    const SCREAMING: [&str; 4] = [
        "minecraft:admire_goat_horn",
        "minecraft:call_goat_horn",
        "minecraft:yearn_goat_horn",
        "minecraft:dream_goat_horn",
    ];
    let group = usize::from(group.min(3));
    if screaming {
        SCREAMING[group]
    } else {
        REGULAR[group]
    }
}
