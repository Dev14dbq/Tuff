//! What illagers, vexes and ravagers do in the world (minecraft.wiki, the
//! page of each mob): evoker fangs and vexes, the wololo, an illusioner's
//! blindness, a ravager's roar and what it tramples.

use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use super::Target;
use crate::context::ServerContext;
use crate::fx;
use crate::level::WorldLevel;

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// Before the mobs think: evokers learn of a blue sheep near them, vexes
/// take up their evoker's target.
pub fn tend(entities: &mut [Entity]) {
    let sheep: Vec<(i32, [f64; 3])> = entities
        .iter()
        .filter_map(|e| match &e.kind {
            // Dye 11 is blue.
            Kind::Mob(m) if m.species == Species::Sheep && m.color == 11 && !m.is_dead() => {
                Some((e.id, e.pos))
            }
            _ => None,
        })
        .collect();
    let owners: Vec<(i32, Option<i32>)> = entities
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if m.species == Species::Evoker && !m.is_dead() => Some((e.id, m.target)),
            _ => None,
        })
        .collect();
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        match m.species {
            Species::Evoker => {
                m.quirks.illager.blue_sheep = sheep
                    .iter()
                    .filter(|(_, p)| dist_sq(*p, pos) <= 16.0 * 16.0)
                    .min_by(|a, b| dist_sq(a.1, pos).total_cmp(&dist_sq(b.1, pos)))
                    .map(|(id, _)| *id);
            }
            Species::Vex => {
                if let Some(owner) = m.quirks.illager.owner
                    && let Some((_, target)) = owners.iter().find(|(id, _)| *id == owner)
                    && target.is_some()
                    && m.target.is_none()
                {
                    m.target = *target;
                }
            }
            _ => {}
        }
    }
}

/// The evoker's fangs come up.
pub fn fangs(
    ctx: &ServerContext,
    e: &Entity,
    spots: Vec<([f64; 3], f32, i32)>,
    fresh: &mut Vec<Entity>,
) {
    let me = (e.id, Species::Evoker.display_name());
    for (pos, yaw, delay) in spots {
        fresh.push(crate::projectiles::fangs::new(ctx, pos, yaw, delay, me));
    }
    fx::sound(ctx, "entity.evoker.cast_spell", 5, e.pos, 1.0, 1.0);
}

/// Three vexes come out round the evoker, after its target, wasting away
/// after 30 to 119 seconds.
pub fn summon_vexes(ctx: &ServerContext, e: &Entity, fresh: &mut Vec<Entity>) {
    let Kind::Mob(evoker) = &e.kind else {
        return;
    };
    let target = evoker.target;
    for _ in 0..3 {
        let r = |n: u64| (crate::util::random_u64() % n) as f64;
        let pos = [
            e.pos[0] + r(5) - 2.0 + 0.5,
            e.pos[1] + 1.0,
            e.pos[2] + r(5) - 2.0 + 0.5,
        ];
        let at = Position::new(
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        );
        if crate::hits::block_at(ctx, [at.x, at.y, at.z]).is_none_or(|s| ctx.rules.blocks_motion(s))
        {
            continue;
        }
        let mut vex = super::new_mob(ctx, Species::Vex, pos);
        if let Kind::Mob(m) = &mut vex.kind {
            m.quirks.illager.owner = Some(e.id);
            m.quirks.illager.bound = Some(pos);
            m.quirks.illager.life = 20 * (30 + (crate::util::random_u64() % 90) as i32);
            m.target = target;
        }
        fresh.push(vex);
    }
    fx::sound(ctx, "entity.evoker.cast_spell", 5, e.pos, 1.0, 1.0);
}

/// "Wololo": the blue sheep turns red.
pub fn wololo(ctx: &ServerContext, entities: &mut [Entity], sheep: i32) {
    let _ = ctx;
    if let Some(e) = entities.iter_mut().find(|e| e.id == sheep)
        && let Kind::Mob(m) = &mut e.kind
        && m.species == Species::Sheep
        && m.color == 11
    {
        m.color = 14;
        m.dirty = true;
    }
}

/// An illusioner blinds its target for twenty seconds.
pub fn blind(ctx: &ServerContext, entities: &mut [Entity], targets: &[Target], target: i32) {
    let Some(kind) = crate::effects::kind_of("blindness") else {
        return;
    };
    let instance = crate::effects::Instance::new(0, 400, true);
    if let Some(t) = targets.iter().find(|t| t.id == target) {
        crate::effects::give(ctx, t.uuid, kind, instance);
        return;
    }
    if let Some(e) = entities.iter_mut().find(|e| e.id == target)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.effects.active.insert("blindness", instance);
        m.dirty = true;
    }
}

/// Raiders spare each other.
fn spared(s: Species) -> bool {
    s.raider() || s == Species::Vex
}

/// The ravager roars after its stun: everything but raiders within four
/// blocks takes 6 and is thrown back.
pub fn roar(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    targets: &[Target],
    difficulty: i32,
) {
    let (id, pos) = (entities[i].id, entities[i].pos);
    let near = |p: [f64; 3]| {
        (p[0] - pos[0]).abs() <= 4.0 + 1.0
            && (p[1] - pos[1]).abs() <= 4.0 + 1.1
            && (p[2] - pos[2]).abs() <= 4.0 + 1.0
    };
    let push = |p: [f64; 3]| {
        let (dx, dz) = (p[0] - pos[0], p[2] - pos[2]);
        let len = (dx * dx + dz * dz).max(0.001);
        [dx / len * 4.0, 0.2, dz / len * 4.0]
    };
    for t in targets.iter().filter(|t| t.targetable && near(t.pos)) {
        let hit = crate::combat::Hit {
            amount: crate::damage::scale_for_difficulty(6.0, difficulty),
            kind: "minecraft:mob_attack",
            death_key: "death.attack.mob",
            attacker: Some(Species::Ravager.display_name()),
            by: Some(id),
            from: Some(pos),
            knockback: 0.0,
            disables_shield: 0.0,
        };
        crate::combat::hurt_player(ctx, t.uuid, &hit);
        crate::combat::push_player(ctx, t.uuid, t.id, push(t.pos));
    }
    for (j, e) in entities.iter_mut().enumerate() {
        if j == i || !near(e.pos) {
            continue;
        }
        let is_target = match &e.kind {
            Kind::Mob(m) => !m.is_dead() && !spared(m.species),
            Kind::Merchant(_) => true,
            _ => false,
        };
        if !is_target {
            continue;
        }
        let harm = super::hurt::Harm {
            by: Some(id),
            from: Some(pos),
            killer: Some(Species::Ravager.id().to_owned()),
            ..super::hurt::Harm::new(6.0, "minecraft:mob_attack")
        };
        super::hurt::harm(ctx, e, &harm);
        let v = push(e.pos);
        e.vel[0] += v[0] * 0.25;
        e.vel[1] += v[1];
        e.vel[2] += v[2] * 0.25;
    }
    fx::status(ctx, id, 39);
}

/// Crops a ravager tramples (minecraft.wiki, "Ravager").
const CROPS: &[&str] = &[
    "minecraft:beetroots",
    "minecraft:carrots",
    "minecraft:potatoes",
    "minecraft:wheat",
    "minecraft:pitcher_crop",
    "minecraft:torchflower_crop",
];

/// A ravager walked into something: the leaves and crops in its way (its
/// box grown by 0.2) break, dropping what they drop.
pub fn crush(ctx: &ServerContext, level: &mut WorldLevel, e: &Entity) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let (w, h) = m.size();
    let half = w / 2.0 + 0.2;
    let blocks = &ctx.data.blocks;
    let lo = [
        (e.pos[0] - half).floor() as i32,
        (e.pos[1] - 0.2).floor() as i32,
        (e.pos[2] - half).floor() as i32,
    ];
    let hi = [
        (e.pos[0] + half).floor() as i32,
        (e.pos[1] + h + 0.2).floor() as i32,
        (e.pos[2] + half).floor() as i32,
    ];
    let mut broke = false;
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let p = Position::new(x, y, z);
                let Some(s) = ctx.world.block(p, blocks) else {
                    continue;
                };
                let name = blocks.block_name(s).unwrap_or("");
                if !name.ends_with("_leaves") && !CROPS.contains(&name) {
                    continue;
                }
                if ctx.game_rule_bool("minecraft:block_drops") {
                    let props = |k: &str| blocks.property(s, k).map(str::to_owned);
                    for d in ctx.loot.drops(name, &props) {
                        crate::tick::spawn_stack_at(
                            ctx,
                            [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5],
                            &d.stack(),
                        );
                    }
                }
                fx::level_event(
                    ctx,
                    2001,
                    [f64::from(x), f64::from(y), f64::from(z)],
                    s as i32,
                );
                broke |= level.set_block(p, ctx.shapes.air());
            }
        }
    }
    let _ = broke;
}
