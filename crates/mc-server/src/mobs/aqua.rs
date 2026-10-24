//! Aquatic mobs (Claude): what a squid's ink, a pufferfish's sting, a
//! guardian's beam and curse, a frog's tongue and the eggs and frogspawn do
//! in the world.

use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use super::Target;
use super::hurt::{self, Harm};
use crate::context::ServerContext;
use crate::fx;
use crate::level::WorldLevel;

/// The biome at a place.
pub fn biome_at(ctx: &ServerContext, pos: [f64; 3]) -> Option<String> {
    let at = Position::new(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    );
    let id = ctx.world.biome(at, &ctx.data.blocks)?;
    let name = ctx.world.biome_name(id)?;
    Some(if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    })
}

/// A squid squirts ink: the cloud and the sound.
pub fn ink(ctx: &ServerContext, e: &Entity) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let glow = m.species == Species::GlowSquid;
    let (sound, particle) = if glow {
        ("entity.glow_squid.squirt", "glow_squid_ink")
    } else {
        ("entity.squid.squirt", "squid_ink")
    };
    fx::sound(ctx, sound, fx::category::NEUTRAL, e.pos, 0.4, 1.0);
    fx::particles(ctx, particle, [e.pos[0], e.pos[1] + 0.4, e.pos[2]], 0.3, 0.05, 20);
}

/// The player or mob with this id, hurt by `hit` (already scaled by the
/// difficulty for players). Returns whether it took the blow.
fn hurt_target(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    amount: f32,
    kind: &'static str,
    death_key: &'static str,
    targets: &[Target],
    difficulty: i32,
) -> Option<Option<u128>> {
    let (id, pos) = (entities[i].id, entities[i].pos);
    let Kind::Mob(m) = &entities[i].kind else {
        return None;
    };
    let species = m.species;
    if let Some(t) = targets.iter().find(|t| t.id == target) {
        if difficulty == 0 {
            return None;
        }
        let hit = crate::combat::Hit {
            amount: crate::damage::scale_for_difficulty(amount, difficulty),
            kind,
            death_key,
            attacker: Some(species.display_name()),
            by: Some(id),
            from: Some(pos),
            knockback: 0.0,
            disables_shield: 0.0,
        };
        return crate::combat::hurt_player(ctx, t.uuid, &hit).map(|_| Some(t.uuid));
    }
    let j = entities.iter().position(|e| e.id == target)?;
    if !matches!(entities[j].kind, Kind::Mob(_)) {
        return None;
    }
    let harm = Harm {
        by: Some(id),
        from: Some(pos),
        killer: Some(species.id().to_owned()),
        ..Harm::new(amount, kind)
    };
    hurt::harm(ctx, &mut entities[j], &harm).then_some(None)
}

/// A pufferfish stings whatever touched it: damage, then poison.
pub fn sting(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    poison: i32,
    targets: &[Target],
    difficulty: i32,
) {
    let hurt = hurt_target(
        ctx,
        entities,
        i,
        target,
        damage,
        "minecraft:mob_attack",
        "death.attack.mob",
        targets,
        difficulty,
    );
    if hurt.is_none() {
        return;
    }
    let Some(kind) = crate::effects::kind_of("poison") else {
        return;
    };
    let effect = crate::effects::Instance::new(0, poison, true);
    match hurt {
        Some(Some(uuid)) => {
            crate::effects::give(ctx, uuid, kind, effect);
        }
        _ => {
            if let Some(j) = entities.iter().position(|e| e.id == target) {
                crate::mob_effects::give(ctx, &mut entities[j], kind, effect, None);
            }
        }
    }
}

/// The magic part of a guardian's beam.
pub fn magic(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    targets: &[Target],
    difficulty: i32,
) {
    hurt_target(
        ctx,
        entities,
        i,
        target,
        damage,
        "minecraft:indirect_magic",
        "death.attack.indirectMagic",
        targets,
        difficulty,
    );
}

/// An elder guardian curses every player within fifty blocks who is not
/// cursed already: Mining Fatigue III for five minutes.
pub fn curse(ctx: &ServerContext, e: &Entity, targets: &[Target]) {
    let Some(kind) = crate::effects::kind_of("mining_fatigue") else {
        return;
    };
    for t in targets.iter().filter(|t| t.targetable) {
        if super::dist_sq(t.pos, e.pos) > 50.0 * 50.0 {
            continue;
        }
        let cursed = crate::effects::change(ctx, t.uuid, |v| {
            v.effects
                .active
                .get("mining_fatigue")
                .is_some_and(|i| i.amplifier >= 2 && i.duration >= 1200)
        })
        .unwrap_or(false);
        if cursed {
            continue;
        }
        crate::effects::give(
            ctx,
            t.uuid,
            kind,
            crate::effects::Instance::new(2, 6000, true),
        );
        fx::sound(
            ctx,
            "entity.elder_guardian.curse",
            5,
            t.pos,
            1.0,
            1.0,
        );
    }
}

/// A frog swallows a small slime or magma cube: a slimeball, or a froglight
/// of the colour of the frog.
pub fn eat(ctx: &ServerContext, entities: &mut [Entity], i: usize, target: i32) {
    let Kind::Mob(frog) = &entities[i].kind else {
        return;
    };
    let variant = frog.variant;
    let Some(j) = entities.iter().position(|e| e.id == target) else {
        return;
    };
    let pos = entities[j].pos;
    let Kind::Mob(prey) = &mut entities[j].kind else {
        return;
    };
    if prey.is_dead() || prey.dying.is_some() || prey.size > 1 {
        return;
    }
    let drop = match prey.species {
        Species::Slime => "minecraft:slime_ball",
        Species::MagmaCube => {
            match super::meta::variant_name(ctx, Species::Frog, variant) {
                Some("minecraft:warm") => "minecraft:pearlescent_froglight",
                Some("minecraft:cold") => "minecraft:verdant_froglight",
                _ => "minecraft:ochre_froglight",
            }
        }
        _ => return,
    };
    // It goes without a death animation.
    prey.health = 0.0;
    prey.dying = None;
    entities[j].removed = true;
    if let Some(item) = ctx.items.id(drop) {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], item, 1);
    }
}

/// A turtle lays a clutch in the sand where it stands.
pub fn lay_egg(ctx: &ServerContext, level: &mut WorldLevel, cell: [i32; 3], count: i32) {
    let p = Position::new(cell[0], cell[1], cell[2]);
    let eggs = count.clamp(1, 4).to_string();
    if let Some(state) = ctx
        .data
        .blocks
        .state_id("minecraft:turtle_egg", &[("eggs", eggs.as_str()), ("hatch", "0")])
    {
        level.set_block(p, state);
    }
}

/// A frog lays frogspawn on the water.
pub fn lay_spawn(ctx: &ServerContext, level: &mut WorldLevel, cell: [i32; 3]) {
    let p = Position::new(cell[0], cell[1], cell[2]);
    if let Some(state) = ctx.data.blocks.default_state("minecraft:frogspawn") {
        level.set_block(p, state);
    }
}

/// A dolphin's grace on a swimmer: five seconds.
pub fn grace(ctx: &ServerContext, player: i32, targets: &[Target]) {
    let Some(t) = targets.iter().find(|t| t.id == player) else {
        return;
    };
    if let Some(kind) = crate::effects::kind_of("dolphins_grace") {
        crate::effects::give(ctx, t.uuid, kind, crate::effects::Instance::new(0, 100, true));
    }
}
