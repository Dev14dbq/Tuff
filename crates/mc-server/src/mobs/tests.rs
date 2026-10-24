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

fn actor_at(pos: [f64; 3]) -> crate::interact::Actor {
    crate::interact::Actor {
        uuid: STEVE,
        entity_id: 7000,
        game_mode: 0,
        pose: crate::context::Pose {
            x: pos[0],
            y: pos[1],
            z: pos[2],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        },
        sneaking: false,
    }
}

fn mobs_of(ctx: &ServerContext, species: Species) -> Vec<Entity> {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == species))
        .cloned()
        .collect()
}

#[test]
fn spawn_eggs_work_on_the_ground_and_on_grown_mobs() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    assert_eq!(eggs::species_of("minecraft:cow_spawn_egg"), Some(Species::Cow));
    hold(&ctx, "minecraft:cow_spawn_egg", 3);
    // Clicking the top of the ground.
    let ground = mc_protocol::Position::new(2, -61, 0);
    crate::interact::use_item_on(&ctx, actor_at([0.5, -60.0, 0.5]), 0, ground, 1, [0.5, 1.0, 0.5], 1);
    let cows = mobs_of(&ctx, Species::Cow);
    assert_eq!(cows.len(), 1);
    assert_eq!(cows[0].pos, [2.5, -60.0, 0.5]);
    // On the grown cow: a calf.
    ctx.mobs.use_on(ctx.dim, cows[0].id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    let calves = mobs_of(&ctx, Species::Cow)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.is_baby()))
        .count();
    assert_eq!(calves, 1);
    let left = lock(&ctx.inventories)[&STEVE].slots[36].as_ref().map(|s| s.count);
    assert_eq!(left, Some(1));
    // Picking the cow gives its egg.
    let egg = ctx.items.id("minecraft:cow_spawn_egg");
    assert_eq!(eggs::pick(&ctx, &cows[0]), egg);
}

#[test]
fn a_crowd_pushes_apart_and_crushes() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    for _ in 0..30 {
        put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    }
    run(&ctx, 40);
    let pigs = mobs_of(&ctx, Species::Pig);
    let hurt = pigs
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.health < m.max_health))
        .count();
    assert!(hurt > 0 || pigs.len() < 30, "nobody was crushed");
    let spread = pigs
        .iter()
        .map(|e| (e.pos[0] - 0.5).hypot(e.pos[2] - 0.5))
        .fold(0.0, f64::max);
    assert!(spread > 0.5, "{spread}");
}

#[test]
fn mobs_suffocate_inside_blocks() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 10.5]);
    let stone = ctx.data.blocks.default_state("minecraft:stone").unwrap();
    for y in -60..=-58 {
        ctx.world
            .set_block(mc_protocol::Position::new(0, y, 0), stone, &ctx.data.blocks, &ctx.rules)
            .unwrap();
    }
    let id = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
    run(&ctx, 30);
    assert!(with_mob(&ctx, id, |m| m.health) < 10.0);
}

fn move_steve(ctx: &ServerContext, x: f64, sneaking: bool) -> Actor {
    if let Some(p) = lock(&ctx.players).get_mut(&STEVE) {
        p.pose.as_mut().unwrap().x = x;
    }
    Actor {
        sneaking,
        ..steve([x, -60.0, 0.5])
    }
}

#[test]
fn a_lead_ties_an_animal_pulls_it_and_lets_go() {
    use crate::decor::leash::Holder;
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let cow = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    let zombie = put(&ctx, Species::Zombie, [0.5, -60.0, 2.5]);
    let leashes = |ctx: &ServerContext| lock(&ctx.leashes).links.clone();
    // Monsters take no lead.
    hold(&ctx, "minecraft:lead", 2);
    ctx.mobs.use_on(ctx.dim, zombie, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(leashes(&ctx).is_empty());
    ctx.mobs.use_on(ctx.dim, cow, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&cow), Some(&Holder::Player(STEVE)));
    let left = lock(&ctx.inventories)[&STEVE].slots[36]
        .as_ref()
        .map(|s| s.count);
    assert_eq!(left, Some(1), "the lead was used");
    // Walking off: past six blocks the cow is drawn along and faces the
    // player; it never runs away.
    let _ = move_steve(&ctx, 11.0, false);
    let before = mob_of(&ctx, cow).unwrap().pos[0];
    for _ in 0..20 {
        crate::decor::leash::tick(&ctx);
        run(&ctx, 1);
        let e = mob_of(&ctx, cow).unwrap();
        assert!(e.vel[0] >= 0.0, "pulled, not pushed: {:?}", e.vel);
    }
    let e = mob_of(&ctx, cow).unwrap();
    assert!(e.pos[0] > before + 0.5, "{} -> {}", before, e.pos[0]);
    assert!(matches!(&e.kind, Kind::Mob(m) if (m.yaw + 90.0).abs() < 30.0), "faces east");
    assert_eq!(leashes(&ctx).len(), 1, "still on at under twelve blocks");
    // Using it again lets go, and the lead drops.
    let at = mob_of(&ctx, cow).unwrap().pos[0];
    ctx.mobs.use_on(ctx.dim, cow, move_steve(&ctx, at, false), 0);
    run(&ctx, 1);
    assert!(leashes(&ctx).is_empty());
    assert_eq!(items_of(&ctx, "minecraft:lead"), 1);
}

#[test]
fn leads_snap_at_twelve_blocks_and_move_between_mobs() {
    use crate::decor::leash::Holder;
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let cow = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    let pig = put(&ctx, Species::Pig, [1.5, -60.0, 3.5]);
    let leashes = |ctx: &ServerContext| lock(&ctx.leashes).links.clone();
    // Eleven blocks hold; past twelve it snaps (centres of the boxes).
    lock(&ctx.leashes).links.insert(cow, Holder::Player(STEVE));
    let _ = move_steve(&ctx, 13.5, false);
    crate::decor::leash::tick(&ctx);
    assert_eq!(leashes(&ctx).len(), 1);
    let _ = move_steve(&ctx, 15.0, false);
    crate::decor::leash::tick(&ctx);
    assert!(leashes(&ctx).is_empty(), "snapped");
    assert_eq!(items_of(&ctx, "minecraft:lead"), 1);
    // Sneaking at the pig ties the cow to it.
    lock(&ctx.leashes).links.insert(cow, Holder::Player(STEVE));
    ctx.mobs.use_on(ctx.dim, pig, move_steve(&ctx, 0.5, true), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&cow), Some(&Holder::Knot(pig)));
    // A lead from the pig to the cow is the cow's to keep: taking it with a
    // lead in hand moves it to the player.
    hold(&ctx, "minecraft:lead", 1);
    ctx.mobs.use_on(ctx.dim, cow, move_steve(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&cow), Some(&Holder::Player(STEVE)));
    // Nobody takes another player's: another holder leaves it be.
    lock(&ctx.leashes).links.insert(pig, Holder::Player(0xbeef));
    hold(&ctx, "minecraft:lead", 1);
    ctx.mobs.use_on(ctx.dim, pig, move_steve(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&pig), Some(&Holder::Player(0xbeef)));
    // Shears snip every lead of the pig.
    lock(&ctx.leashes).links.insert(cow, Holder::Knot(pig));
    hold(&ctx, "minecraft:shears", 1);
    ctx.mobs.use_on(ctx.dim, pig, move_steve(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert!(leashes(&ctx).is_empty(), "{:?}", leashes(&ctx));
}
