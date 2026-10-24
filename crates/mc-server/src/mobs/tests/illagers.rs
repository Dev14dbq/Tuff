//! Pillagers, vindicators, evokers, illusioners, vexes and ravagers.

use super::*;
use mc_entity::extra::Shot;
use mc_protocol::packets::play::MetadataValue;

fn set_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut Mob) -> R) -> R {
    let mut all = lock(&ctx.entities);
    let Kind::Mob(m) = &mut all.iter_mut().find(|e| e.id == id).unwrap().kind else {
        panic!()
    };
    f(m)
}

fn count(ctx: &ServerContext, pick: impl Fn(&Entity) -> bool) -> usize {
    lock(&ctx.entities).iter().filter(|e| pick(e)).count()
}

fn held(ctx: &ServerContext, m: &Mob) -> String {
    m.gear[mc_entity::mob::slot::MAINHAND]
        .as_ref()
        .and_then(|g| ctx.items.get(g.item))
        .map(|i| i.name.clone())
        .unwrap_or_default()
}

#[test]
fn illagers_have_their_wiki_stats_and_weapons() {
    for (s, hp, size) in [
        (Species::Pillager, 24.0, (0.6, 1.95)),
        (Species::Vindicator, 24.0, (0.6, 1.95)),
        (Species::Evoker, 24.0, (0.6, 1.95)),
        (Species::Illusioner, 32.0, (0.6, 1.95)),
        (Species::Vex, 14.0, (0.4, 0.8)),
        (Species::Ravager, 100.0, (1.95, 2.2)),
    ] {
        assert_eq!(s.max_health(), hp, "{s:?}");
        assert_eq!(s.size(), size, "{s:?}");
        assert_eq!(Species::from_id(s.id()), Some(s));
        assert!(s.raider() || s == Species::Vex);
    }
    assert!(Species::Vex.flies() && !Species::Vex.takes_fall_damage());
    assert_eq!(Species::Evoker.xp_reward(), (10, 10));
    assert_eq!(Species::Ravager.xp_reward(), (20, 20));
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    for (s, weapon) in [
        (Species::Pillager, "minecraft:crossbow"),
        (Species::Vindicator, "minecraft:iron_axe"),
        (Species::Vex, "minecraft:iron_sword"),
        (Species::Illusioner, "minecraft:bow"),
    ] {
        let e = new_mob(&ctx, s, [0.5, -60.0, 0.5]);
        let Kind::Mob(m) = &e.kind else { panic!() };
        assert_eq!(held(&ctx, m), weapon, "{s:?}");
    }
}

#[test]
fn casters_and_pillagers_show_what_they_are_doing() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut evoker = Mob::new(Species::Evoker, 5);
    evoker.quirks.illager.spell = Some(mc_entity::mob::illager::Spell::Fangs);
    let v = metadata_of(&ctx, &evoker, 0);
    assert!(v.contains(&(17, MetadataValue::Byte(2))));
    assert!(v.contains(&(16, MetadataValue::Boolean(false))));
    let mut pillager = Mob::new(Species::Pillager, 5);
    pillager.quirks.illager.charging = true;
    assert!(metadata_of(&ctx, &pillager, 0).contains(&(17, MetadataValue::Boolean(true))));
    let mut vex = Mob::new(Species::Vex, 5);
    vex.quirks.illager.lunging = true;
    assert!(metadata_of(&ctx, &vex, 0).contains(&(16, MetadataValue::Byte(1))));
}

#[test]
fn a_pillager_loads_its_crossbow_and_shoots_the_player() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    join(&ctx, [0.5, -60.0, 0.5]);
    put(&ctx, Species::Pillager, [6.5, -60.0, 0.5]);
    let mut shot = false;
    for _ in 0..120 {
        run(&ctx, 1);
        shot |= count(
            &ctx,
            |e| matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, Shot::Arrow(_))),
        ) > 0;
        if shot {
            break;
        }
    }
    assert!(shot, "no bolt");
}

#[test]
fn an_evoker_calls_up_vexes_and_fangs() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Evoker, [6.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.target = Some(7000));
    let (mut vexes, mut fangs) = (0, 0);
    for _ in 0..300 {
        run(&ctx, 1);
        vexes = vexes.max(count(
            &ctx,
            |e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Vex),
        ));
        fangs = fangs.max(count(
            &ctx,
            |e| matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, Shot::Fangs { .. })),
        ));
        if vexes > 0 && fangs > 0 {
            break;
        }
    }
    assert!((1..=3).contains(&vexes), "vexes {vexes}");
    assert!(fangs > 0, "no fangs");
    // The vexes know their evoker and go after its target.
    let vex = lock(&ctx.entities)
        .iter()
        .find_map(|e| match &e.kind {
            Kind::Mob(m) if m.species == Species::Vex => Some((m.quirks.illager.owner, m.target)),
            _ => None,
        })
        .unwrap();
    assert_eq!(vex.0, Some(id));
    assert_eq!(vex.1, Some(7000));
}

#[test]
fn an_evoker_turns_a_blue_sheep_red() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [40.5, -60.0, 40.5]);
    put(&ctx, Species::Evoker, [0.5, -60.0, 0.5]);
    let sheep = put(&ctx, Species::Sheep, [4.5, -60.0, 0.5]);
    set_mob(&ctx, sheep, |m| {
        m.set_color(11);
        m.no_ai = true;
    });
    run(&ctx, 80);
    assert_eq!(with_mob(&ctx, sheep, |m| m.color), 14);
}

#[test]
fn a_vex_wastes_away_once_its_time_is_up() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [40.5, -60.0, 40.5]);
    let id = put(&ctx, Species::Vex, [0.5, -58.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.illager.life = 1);
    run(&ctx, 3);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 13.0);
}

#[test]
fn a_stunned_ravager_roars_and_hurts_what_is_near() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [40.5, -60.0, 40.5]);
    let id = put(&ctx, Species::Ravager, [0.5, -60.0, 0.5]);
    let cow = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    let pillager = put(&ctx, Species::Pillager, [0.5, -60.0, 2.5]);
    set_mob(&ctx, cow, |m| m.no_ai = true);
    set_mob(&ctx, id, |m| m.quirks.illager.stun_tick = 2);
    run(&ctx, 15);
    assert!(with_mob(&ctx, cow, |m| m.health) < 10.0, "cow unhurt");
    assert_eq!(with_mob(&ctx, pillager, |m| m.health), 24.0);
}

#[test]
fn a_ravager_tramples_leaves_and_crops_but_not_stone() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [40.5, -60.0, 40.5]);
    set(&ctx, [1, -60, 0], "minecraft:oak_leaves");
    set(&ctx, [0, -60, 1], "minecraft:wheat");
    set(&ctx, [-1, -60, 0], "minecraft:stone");
    let e = new_mob(&ctx, Species::Ravager, [0.5, -60.0, 0.5]);
    let mut level = WorldLevel::new(&ctx);
    super::super::illager::crush(&ctx, &mut level, &e);
    let changes = level.into_changes();
    let _ = ctx
        .events
        .send(crate::context::Event::BlocksChanged { by: 0, changes });
    assert_eq!(block(&ctx, [1, -60, 0]), "minecraft:air");
    assert_eq!(block(&ctx, [0, -60, 1]), "minecraft:air");
    assert_eq!(block(&ctx, [-1, -60, 0]), "minecraft:stone");
}

#[test]
fn raiders_are_saved_with_their_patrol_and_a_vex_with_its_life() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut p = Mob::new(Species::Pillager, 1);
    p.quirks.illager.patrol_leader = true;
    p.quirks.illager.patrolling = true;
    p.quirks.illager.patrol_target = Some([10.5, 64.0, -3.5]);
    let mut c = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &p, &mut c);
    let mut back = Mob::new(Species::Pillager, 2);
    crate::mobs::nbt::apply(&ctx, &mut back, &c);
    assert!(back.quirks.illager.patrol_leader && back.quirks.illager.patrolling);
    assert_eq!(back.quirks.illager.patrol_target, Some([10.5, 64.0, -3.5]));
    let mut v = Mob::new(Species::Vex, 1);
    v.quirks.illager.life = 600;
    let mut c = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &v, &mut c);
    let mut back = Mob::new(Species::Vex, 2);
    crate::mobs::nbt::apply(&ctx, &mut back, &c);
    assert_eq!(back.quirks.illager.life, 600);
}
