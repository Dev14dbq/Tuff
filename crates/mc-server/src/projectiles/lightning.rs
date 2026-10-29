//! Lightning bolts (minecraft.wiki, "Lightning"), here those Channeling
//! calls: thunder and the impact sound, fire where it strikes (on normal
//! and hard), 5 damage and fire to everything within 3 blocks while it
//! flashes, one to three flashes. A pig becomes a zombified piglin, a
//! villager a witch, a creeper charges and a mooshroom changes colour
//! ([`crate::mobs::hurt::thunder`]).

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use crate::hits::{self, Hit, Tick};

/// The weather sound category.
const WEATHER: i32 = 3;

/// A new bolt at `pos`.
pub fn bolt(ctx: &crate::context::ServerContext, pos: [f64; 3], visual: bool) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(mc_entity::extra::Projectile {
            shot: Shot::Lightning {
                life: 2,
                flashes: 1 + (crate::util::random_u64() % 3) as i32,
                struck: Vec::new(),
                visual,
            },
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        }),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Calls a bolt down at `pos` (weather, outside any entity's tick).
pub fn call_down(ctx: &crate::context::ServerContext, pos: [f64; 3]) {
    let b = bolt(ctx, pos, false);
    let _ = ctx
        .events
        .send(crate::context::Event::EntitySpawned(b.clone()));
    crate::tick::lock(&ctx.entities).push(b);
}

/// Calls a bolt down at `pos` from an entity's tick.
pub fn strike(t: &mut Tick, pos: [f64; 3]) {
    let b = bolt(t.ctx, pos, false);
    t.out.spawn.push(b);
}

/// The block under a bolt at `pos` is struck: a lightning rod gives its
/// signal, copper there is cleaned.
fn hit_block(ctx: &crate::context::ServerContext, pos: [f64; 3]) {
    let at = mc_tick::BlockPos::new(
        pos[0].floor() as i32,
        (pos[1] - 1.0e-6).floor() as i32,
        pos[2].floor() as i32,
    );
    let mut level = crate::level::WorldLevel::new(ctx);
    let logic = level.logic();
    let mut rng = mc_tick::rng::Rng::new(crate::util::random_u64());
    mc_tick::lightning::strike(&mut level, logic, at, &mut rng);
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx
            .events
            .send(crate::context::Event::BlocksChanged { by: 0, changes });
    }
}

/// Fire where the bolt strikes and `extra` more around it (normal and
/// hard difficulty only).
fn fire(t: &mut Tick, pos: [f64; 3], extra: usize) {
    if t.ctx.difficulty() < 2 {
        return;
    }
    let c = pos.map(|v| v.floor() as i32);
    t.out.fires.push(c);
    for _ in 0..extra {
        let r = || (crate::util::random_u64() % 3) as i32 - 1;
        t.out.fires.push([c[0] + r(), c[1] + r(), c[2] + r()]);
    }
}

/// A bolt is over: `lightning_strike` for the players about, with what it
/// struck and the fires burning where it fell.
fn struck_near(t: &Tick, e: &Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::Lightning { struck, .. } = &p.shot else {
        return;
    };
    let ctx = t.ctx;
    let at = e.pos.map(|c| c.floor() as i32);
    let fires = (-1..=1)
        .flat_map(|dx| {
            (-1..=1)
                .flat_map(move |dy| (-1..=1).map(move |dz| [at[0] + dx, at[1] + dy, at[2] + dz]))
        })
        .filter(|c| {
            hits::block_at(ctx, *c).and_then(|s| ctx.data.blocks.block_name(s))
                == Some("minecraft:fire")
        })
        .count() as i32;
    let bolt = crate::criteria::Subject {
        kind: "minecraft:lightning_bolt".into(),
        id: e.id,
        pos: e.pos,
        dimension: ctx.world.dimension.clone(),
        fires_lit: Some(fires),
        ..Default::default()
    };
    let bystanders: Vec<crate::criteria::Subject> = t
        .targets
        .iter()
        .filter(|g| struck.contains(&g.id))
        .filter_map(|g| {
            Some(crate::criteria::Subject {
                kind: g.kind?.to_owned(),
                id: g.id,
                ..Default::default()
            })
        })
        .collect();
    for uuid in crate::criteria::players_near(ctx, e.pos, 64.0) {
        crate::criteria::lightning(ctx, uuid, &bolt, &bystanders);
    }
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Lightning {
        life,
        flashes,
        visual,
        ..
    } = &mut p.shot
    else {
        return;
    };
    let ctx = t.ctx;
    if e.age == 0 {
        crate::fx::sound(
            ctx,
            "entity.lightning_bolt.thunder",
            WEATHER,
            e.pos,
            10000.0,
            0.8 + hits::rnd() as f32 * 0.2,
        );
        crate::fx::sound(
            ctx,
            "entity.lightning_bolt.impact",
            WEATHER,
            e.pos,
            2.0,
            0.5 + hits::rnd() as f32 * 0.2,
        );
    }
    let visual = *visual;
    if e.age == 0 {
        hit_block(ctx, e.pos);
    }
    if e.age == 0 && !visual {
        fire(t, e.pos, 4);
    }
    e.age += 1;
    *life -= 1;
    if *life < 0 {
        if *flashes == 0 {
            e.removed = true;
            if !visual {
                struck_near(t, e);
            }
            return;
        }
        if *life < -((crate::util::random_u64() % 10) as i32) {
            *flashes -= 1;
            *life = 1;
            if !visual {
                fire(t, e.pos, 0);
            }
        }
    }
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Lightning { life, struck, .. } = &mut p.shot else {
        return;
    };
    if *life < 0 || visual {
        return;
    }
    let area = mc_entity::Aabb {
        min: [e.pos[0] - 3.0, e.pos[1] - 3.0, e.pos[2] - 3.0],
        max: [e.pos[0] + 3.0, e.pos[1] + 9.0, e.pos[2] + 3.0],
    };
    let inside =
        |b: &mc_entity::Aabb| (0..3).all(|i| b.min[i] < area.max[i] && b.max[i] > area.min[i]);
    for w in t.players {
        if !w.solid() || struck.contains(&w.entity_id) || !inside(&w.aabb()) {
            continue;
        }
        struck.push(w.entity_id);
        crate::vitals::try_hurt_by_entity(
            ctx,
            w.uuid,
            5.0,
            "minecraft:lightning_bolt",
            "death.attack.lightningBolt",
            None,
            Some(e.id),
        );
        crate::worn::in_fire(ctx, w.uuid);
    }
    for g in t.targets {
        if struck.contains(&g.id) || !inside(&g.aabb) {
            continue;
        }
        struck.push(g.id);
        t.out.hits.push(Hit {
            target: g.id,
            amount: 5.0,
            by: None,
            with: Some(e.id),
            kind: "minecraft:lightning_bolt",
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick::lock;
    use mc_protocol::Position;

    fn put(ctx: &crate::context::ServerContext, p: [i32; 3], name: &str, props: &[(&str, &str)]) {
        let state = ctx.data.blocks.state_id(name, props).unwrap();
        let mut level = crate::level::WorldLevel::new(ctx);
        level.set_block(Position::new(p[0], p[1], p[2]), state);
        let _ = level.into_changes();
    }

    fn describe(ctx: &crate::context::ServerContext, p: [i32; 3]) -> String {
        let s = ctx
            .world
            .block(Position::new(p[0], p[1], p[2]), &ctx.data.blocks)
            .unwrap();
        ctx.data
            .blocks
            .describe(s)
            .unwrap()
            .trim_end_matches("[]")
            .to_owned()
    }

    fn pass(ctx: &crate::context::ServerContext, n: usize) {
        for _ in 0..n {
            lock(&ctx.time).age += 1;
            crate::tick::run_scheduled_ticks(ctx);
        }
    }

    #[test]
    fn a_struck_rod_gives_a_signal_for_eight_ticks_and_copper_is_cleaned() {
        let Some(ctx) = super::super::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // A rod on an oxidized copper block, wire beside it, and copper near.
        put(&ctx, [3, -60, 3], "minecraft:oxidized_copper", &[]);
        put(
            &ctx,
            [3, -59, 3],
            "minecraft:lightning_rod",
            &[("facing", "up"), ("powered", "false")],
        );
        put(&ctx, [4, -60, 3], "minecraft:stone", &[]);
        put(&ctx, [4, -59, 3], "minecraft:redstone_wire", &[]);
        put(&ctx, [3, -60, 4], "minecraft:weathered_cut_copper", &[]);
        put(&ctx, [3, -60, 2], "minecraft:waxed_oxidized_copper", &[]);
        hit_block(&ctx, [3.5, -58.0, 3.5]);
        assert!(describe(&ctx, [3, -59, 3]).contains("powered=true"));
        assert!(describe(&ctx, [4, -59, 3]).contains("power=15"));
        // The block it stands on is new again; the wax stays.
        assert_eq!(describe(&ctx, [3, -60, 3]), "minecraft:copper_block");
        assert_eq!(
            describe(&ctx, [3, -60, 2]),
            "minecraft:waxed_oxidized_copper"
        );
        pass(&ctx, 7);
        assert!(describe(&ctx, [3, -59, 3]).contains("powered=true"));
        pass(&ctx, 1);
        assert!(describe(&ctx, [3, -59, 3]).contains("powered=false"));
        assert!(describe(&ctx, [4, -59, 3]).contains("power=0"));
        // The block under a rod is strongly powered by it.
        put(&ctx, [8, -60, 8], "minecraft:redstone_lamp", &[]);
        put(
            &ctx,
            [8, -59, 8],
            "minecraft:lightning_rod",
            &[("facing", "up"), ("powered", "false")],
        );
        assert!(describe(&ctx, [8, -60, 8]).contains("lit=false"));
        hit_block(&ctx, [8.5, -58.0, 8.5]);
        assert!(describe(&ctx, [8, -60, 8]).contains("lit=true"));
        pass(&ctx, 8);
        pass(&ctx, 4);
        assert!(describe(&ctx, [8, -60, 8]).contains("lit=false"));
        // Copper struck directly is cleaned too.
        put(&ctx, [9, -60, 9], "minecraft:exposed_cut_copper", &[]);
        hit_block(&ctx, [9.5, -59.0, 9.5]);
        assert_eq!(describe(&ctx, [9, -60, 9]), "minecraft:cut_copper");
    }

    #[test]
    fn a_storm_strikes_the_rod_within_reach_not_the_ground() {
        let Some(ctx) = super::super::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        {
            let mut w = lock(&ctx.weather);
            w.raining = true;
            w.thundering = true;
        }
        // Chunk 0, 0 must be there to strike in.
        put(&ctx, [1, -60, 1], "minecraft:stone", &[]);
        put(
            &ctx,
            [60, -60, 5],
            "minecraft:lightning_rod",
            &[("facing", "up"), ("powered", "false")],
        );
        // A closer one with a block over it does not count.
        put(
            &ctx,
            [20, -60, 5],
            "minecraft:lightning_rod",
            &[("facing", "up")],
        );
        put(&ctx, [20, -59, 5], "minecraft:stone", &[]);
        let mut rng = mc_tick::rng::Rng::new(5);
        crate::precipitation::lightning(&ctx, 0, 0, &mut rng);
        let bolts: Vec<[f64; 3]> = lock(&ctx.entities)
            .iter()
            .filter(|e| {
                matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, Shot::Lightning { .. }))
            })
            .map(|e| e.pos)
            .collect();
        assert_eq!(bolts, vec![[60.5, -59.0, 5.5]]);
    }

    #[test]
    fn a_bolt_turns_a_pig_into_a_zombified_piglin_and_charges_a_creeper() {
        let Some(ctx) = super::super::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let pig = crate::mobs::new_mob(&ctx, mc_entity::Species::Pig, [0.5, -60.0, 0.5]);
        let pig_id = pig.id;
        let creeper = crate::mobs::new_mob(&ctx, mc_entity::Species::Creeper, [3.0, -60.0, 0.5]);
        let creeper_id = creeper.id;
        let moosh = crate::mobs::new_mob(&ctx, mc_entity::Species::Mooshroom, [5.0, -60.0, 0.5]);
        let moosh_id = moosh.id;
        let villager =
            crate::trading::new_merchant(&ctx, "minecraft:villager", [7.0, -60.0, 0.5]).unwrap();
        let villager_id = villager.id;
        let mut entities = vec![pig, creeper, moosh, villager];
        let mut removed = Vec::new();
        let mut gone = Vec::new();
        let strike = |id| crate::hits::Hit {
            target: id,
            amount: 5.0,
            by: None,
            with: None,
            kind: "minecraft:lightning_bolt",
        };
        for id in [pig_id, creeper_id, moosh_id, villager_id] {
            crate::hits::apply(&ctx, &mut entities, &mut removed, &mut gone, strike(id));
        }
        assert!(removed.contains(&pig_id));
        assert!(removed.contains(&villager_id));
        let stored = lock(&ctx.entities);
        assert!(stored.iter().any(|e| {
            matches!(&e.kind, Kind::Mob(m) if m.species == mc_entity::Species::ZombifiedPiglin && m.persistent)
        }));
        assert!(stored.iter().any(|e| {
            matches!(&e.kind, Kind::Mob(m) if m.species == mc_entity::Species::Witch && m.persistent)
        }));
        drop(stored);
        let Kind::Mob(m) = &entities.iter().find(|e| e.id == creeper_id).unwrap().kind else {
            panic!("creeper");
        };
        assert!(m.powered);
        let Kind::Mob(m) = &entities.iter().find(|e| e.id == moosh_id).unwrap().kind else {
            panic!("mooshroom");
        };
        assert_eq!(m.variant, 1);
    }
}
