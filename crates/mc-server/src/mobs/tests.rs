use super::*;
use crate::combat::Blow;

#[test]
fn darkness_follows_the_day() {
    assert_eq!(sky_darken(1000), 0);
    assert_eq!(sky_darken(18000), 11);
    assert!(sky_darken(13000) > 0 && sky_darken(13000) < 11);
    assert_eq!(sky_darken(24000 + 100), 0);
}

/// A superflat server context, `None` without the game data.
fn flat() -> Option<std::sync::Arc<ServerContext>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    ServerContext::new(crate::Config::new(dir), data).ok()
}

const STEVE: u128 = 0xfeed;

/// Puts a survival player at `pos` and loads the chunks around.
fn join(ctx: &ServerContext, pos: [f64; 3]) -> u128 {
    for cx in -2..=2 {
        for cz in -2..=2 {
            let _ = ctx.world.block(
                mc_protocol::Position::new(cx * 16, 0, cz * 16),
                &ctx.data.blocks,
            );
        }
    }
    lock(&ctx.players).insert(
        STEVE,
        crate::context::OnlinePlayer {
            profile: mc_protocol::packets::GameProfile {
                uuid: STEVE,
                name: "Steve".into(),
                properties: Vec::new(),
            },
            game_mode: 0,
            entity_id: 7000,
            skin_parts: 0,
            pose: Some(crate::context::Pose {
                x: pos[0],
                y: pos[1],
                z: pos[2],
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            }),
            dimension: "minecraft:overworld".into(),
            main_hand: None,
            flying: false,
            address: None,
            extra: Default::default(),
        },
    );
    lock(&ctx.vitals).insert(STEVE, crate::vitals::Vitals::default());
    STEVE
}

fn steve(pos: [f64; 3]) -> Actor {
    Actor {
        uuid: STEVE,
        entity_id: 7000,
        name: "Steve".into(),
        pos,
        game_mode: 0,
        sneaking: false,
    }
}

fn blow(damage: f32, from: [f64; 3]) -> Blow {
    Blow {
        damage,
        strength: 1.0,
        from,
        ..Blow::default()
    }
}

/// Puts `count` of an item in the player's main hand.
fn hold(ctx: &ServerContext, name: &str, count: i32) {
    let item = ctx.items.id(name).unwrap();
    let mut inv = lock(&ctx.inventories);
    let hands = inv.entry(STEVE).or_default();
    let slot = hands.hand_slot(0);
    hands.slots[slot] = Some(mc_protocol::packets::play::ItemStack::new(item, count));
}

fn run(ctx: &ServerContext, ticks: usize) {
    for _ in 0..ticks {
        lock(&ctx.time).age += 1;
        let mut level = WorldLevel::new(ctx);
        tick(ctx, &mut level);
    }
}

fn mob_of(ctx: &ServerContext, id: i32) -> Option<Entity> {
    lock(&ctx.entities).iter().find(|e| e.id == id).cloned()
}

fn with_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&Mob) -> R) -> R {
    let e = mob_of(ctx, id).expect("mob still there");
    let Kind::Mob(m) = &e.kind else { panic!() };
    f(m)
}

fn put(ctx: &ServerContext, species: Species, pos: [f64; 3]) -> i32 {
    let e = new_mob(ctx, species, pos);
    let id = e.id;
    lock(&ctx.entities).push(e);
    id
}

fn items_of(ctx: &ServerContext, name: &str) -> i32 {
    let item = ctx.items.id(name).unwrap();
    lock(&ctx.entities)
        .iter()
        .filter_map(|e| match e.kind {
            Kind::Item { item: i, count, .. } if i == item => Some(count),
            _ => None,
        })
        .sum()
}

#[test]
fn a_zombie_comes_for_the_player_and_hurts_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Zombie, [12.5, -60.0, 0.5]);
    run(&ctx, 200);
    let health = lock(&ctx.vitals)[&uuid].health;
    assert!(health < 20.0, "health {health}");
    let z = mob_of(&ctx, id).expect("zombie still there");
    assert!(z.pos[0] < 6.0, "{:?}", z.pos);
}

#[test]
fn hitting_a_cow_kills_it_and_it_drops_loot() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    run(&ctx, 5);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(4.0, at));
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 6.0);
    // The animal runs.
    let before = mob_of(&ctx, id).unwrap().pos;
    run(&ctx, 40);
    let after = mob_of(&ctx, id).unwrap().pos;
    assert!(
        (after[0] - before[0]).hypot(after[2] - before[2]) > 1.5,
        "{before:?} {after:?}"
    );
    ctx.mobs.hit(ctx.dim, id, steve(after), blow(100.0, after));
    run(&ctx, 1);
    assert!(items_of(&ctx, "minecraft:beef") > 0, "no beef");
    // Killed by a player: experience too, once.
    let orbs = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::ExperienceOrb { .. }))
        .count();
    assert!(orbs >= 1, "no experience");
    // The body goes after a second.
    run(&ctx, 25);
    assert!(mob_of(&ctx, id).is_none());
}

#[test]
fn animals_and_monsters_spawn_naturally() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    run(&ctx, 400);
    let all: Vec<_> = lock(&ctx.entities).clone();
    let animals = all
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species.category() == Category::Creature))
        .count();
    // A superflat plains world: animals on the grass.
    assert!(animals > 0, "no animals among {}", all.len());
    for e in &all {
        if let Kind::Mob(m) = &e.kind
            && m.species.category() == Category::Monster
        {
            let d = ((e.pos[0] - 0.5).powi(2) + (e.pos[2] - 0.5).powi(2)).sqrt();
            assert!(d > 20.0 || e.age > 0, "spawned too close: {:?}", e.pos);
        }
    }
}

#[test]
fn shears_take_the_wool() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let mut sheep = new_mob(&ctx, Species::Sheep, [1.5, -60.0, 0.5]);
    if let Kind::Mob(m) = &mut sheep.kind {
        m.set_color(0);
    }
    let id = sheep.id;
    lock(&ctx.entities).push(sheep);
    hold(&ctx, "minecraft:shears", 1);
    let by = steve([0.5, -60.0, 0.5]);
    ctx.mobs.use_on(ctx.dim, id, by.clone(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.sheared));
    let dropped = items_of(&ctx, "minecraft:white_wool");
    assert!((1..=3).contains(&dropped), "{dropped}");
    let worn = lock(&ctx.inventories)[&uuid].slots[36]
        .as_ref()
        .map(|s| s.damage);
    assert_eq!(worn, Some(1));
    // Shearing again does nothing.
    ctx.mobs.use_on(ctx.dim, id, by, 0);
    run(&ctx, 1);
    assert_eq!(items_of(&ctx, "minecraft:white_wool"), dropped);
}

#[test]
fn a_dye_colours_a_sheep() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Sheep, [1.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:red_dye", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.color), 14);
}

#[test]
fn a_creeper_blows_up_near_the_player() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Creeper, [5.5, -60.0, 0.5]);
    run(&ctx, 160);
    assert!(
        mob_of(&ctx, id).is_none(),
        "the creeper should have blown up"
    );
    assert!(lock(&ctx.vitals)[&uuid].health < 20.0);
}

#[test]
fn two_cows_fed_wheat_make_a_calf() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let a = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    let b = put(&ctx, Species::Cow, [3.5, -60.0, 1.5]);
    run(&ctx, 2);
    hold(&ctx, "minecraft:wheat", 4);
    let by = steve([0.5, -60.0, 0.5]);
    ctx.mobs.use_on(ctx.dim, a, by.clone(), 0);
    ctx.mobs.use_on(ctx.dim, b, by, 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, a, |m| m.in_love()));
    run(&ctx, 200);
    let calves = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Cow && m.is_baby()))
        .count();
    assert_eq!(calves, 1);
    // Both parents rest for five minutes.
    assert!(with_mob(&ctx, a, |m| m.baby_age) > 0);
}

#[test]
fn bones_tame_a_wolf() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Wolf, [1.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:bone", 64);
    for _ in 0..40 {
        if with_mob(&ctx, id, |m| m.is_tame()) {
            break;
        }
        ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
        run(&ctx, 1);
    }
    assert!(with_mob(&ctx, id, |m| m.is_tame() && m.owner == Some(STEVE)));
}

#[test]
fn a_saddled_pig_carries_the_player() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Pig, [1.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:saddle", 1);
    let by = steve([0.5, -60.0, 0.5]);
    ctx.mobs.use_on(ctx.dim, id, by.clone(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.saddled()));
    ctx.mobs.use_on(ctx.dim, id, by, 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.riders == vec![7000]));
}

#[test]
fn skeletons_carry_bows_and_metadata_matches_vanilla() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let e = new_mob(&ctx, Species::Skeleton, [0.5, -60.0, 0.5]);
    let Kind::Mob(m) = &e.kind else { panic!() };
    let bow = ctx.items.id("minecraft:bow").unwrap();
    assert_eq!(
        m.gear[mc_entity::mob::slot::MAINHAND]
            .as_ref()
            .map(|g| g.item),
        Some(bow)
    );
    let mut sheep = Mob::new(Species::Sheep, 3);
    sheep.set_color(14);
    sheep.shear();
    let v = metadata_of(&ctx, &sheep, 0);
    assert!(v.contains(&(18, mc_protocol::packets::play::MetadataValue::Byte(14 | 16))));
}
