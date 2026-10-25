use super::*;
use crate::combat::Blow;

mod air;
mod critters;
// Aquatic (Claude)
mod aquatic;
// Land animals (Cursor)
mod illagers;
mod land;
mod raids;
mod villages;
mod warden;

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
    assert_eq!(
        eggs::species_of("minecraft:cow_spawn_egg"),
        Some(Species::Cow)
    );
    hold(&ctx, "minecraft:cow_spawn_egg", 3);
    // Clicking the top of the ground.
    let ground = mc_protocol::Position::new(2, -61, 0);
    crate::interact::use_item_on(
        &ctx,
        actor_at([0.5, -60.0, 0.5]),
        0,
        ground,
        1,
        [0.5, 1.0, 0.5],
        1,
    );
    let cows = mobs_of(&ctx, Species::Cow);
    assert_eq!(cows.len(), 1);
    assert_eq!(cows[0].pos, [2.5, -60.0, 0.5]);
    // On the grown cow: a calf.
    ctx.mobs
        .use_on(ctx.dim, cows[0].id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    let calves = mobs_of(&ctx, Species::Cow)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.is_baby()))
        .count();
    assert_eq!(calves, 1);
    let left = lock(&ctx.inventories)[&STEVE].slots[36]
        .as_ref()
        .map(|s| s.count);
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
            .set_block(
                mc_protocol::Position::new(0, y, 0),
                stone,
                &ctx.data.blocks,
                &ctx.rules,
            )
            .unwrap();
    }
    let id = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
    run(&ctx, 30);
    assert!(with_mob(&ctx, id, |m| m.health) < 10.0);
}

/// Sets blocks by name.
fn set(ctx: &ServerContext, at: [i32; 3], name: &str) {
    let state = ctx.data.blocks.default_state(name).unwrap();
    set_state(ctx, at, state);
}

fn set_state(ctx: &ServerContext, at: [i32; 3], state: u32) {
    ctx.world
        .set_block(
            mc_protocol::Position::new(at[0], at[1], at[2]),
            state,
            &ctx.data.blocks,
            &ctx.rules,
        )
        .unwrap();
}

fn block(ctx: &ServerContext, at: [i32; 3]) -> String {
    let s = ctx
        .world
        .block(
            mc_protocol::Position::new(at[0], at[1], at[2]),
            &ctx.data.blocks,
        )
        .unwrap();
    ctx.data.blocks.block_name(s).unwrap().to_owned()
}

/// How many of `n` pigs penned in one block get hurt.
fn crushed(n: usize) -> usize {
    let ctx = flat().unwrap();
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        for y in -60..=-59 {
            set(&ctx, [dx, y, dz], "minecraft:stone");
        }
    }
    for _ in 0..n {
        put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    }
    run(&ctx, 60);
    mobs_of(&ctx, Species::Pig)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.health < m.max_health))
        .count()
}

#[test]
fn cramming_starts_past_the_game_rule() {
    if flat().is_none() {
        eprintln!("skipped: no game data");
        return;
    }
    // max_entity_cramming 24: 23 others in the space are fine, 24 crush.
    assert_eq!(crushed(24), 0);
    assert!(crushed(26) > 0);
}

#[test]
fn glass_and_leaves_do_not_smother() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 10.5]);
    for y in -60..=-58 {
        set(&ctx, [0, y, 0], "minecraft:glass");
        set(&ctx, [3, y, 0], "minecraft:oak_leaves");
    }
    let a = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
    let b = put(&ctx, Species::Cow, [3.5, -60.0, 0.5]);
    run(&ctx, 30);
    assert_eq!(with_mob(&ctx, a, |m| m.health), 10.0);
    assert_eq!(with_mob(&ctx, b, |m| m.health), 10.0);
}

#[test]
fn a_zombie_breaks_a_door_in_the_world() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(3, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [8.5, -60.0, 0.5]);
    for z in -8..=8 {
        for y in -60..=-58 {
            if z != 0 || y == -58 {
                set(&ctx, [3, y, z], "minecraft:stone");
            }
        }
    }
    let lower = ctx.data.blocks.default_state("minecraft:oak_door").unwrap();
    let upper = ctx
        .data
        .blocks
        .with_property(lower, "half", "upper")
        .unwrap();
    set_state(&ctx, [3, -60, 0], lower);
    set_state(&ctx, [3, -59, 0], upper);
    let id = put(&ctx, Species::Zombie, [0.5, -60.0, 0.5]);
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.breaks_doors = true;
        m.target = Some(7000);
    }
    run(&ctx, 400);
    assert_eq!(block(&ctx, [3, -59, 0]), "minecraft:air");
    assert_eq!(block(&ctx, [3, -60, 0]), "minecraft:air");
}

#[test]
fn an_enderman_puts_its_block_on_firm_ground_only() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 10.5]);
    let id = put(&ctx, Species::Enderman, [0.5, -60.0, 0.5]);
    let dirt = ctx.data.blocks.default_state("minecraft:dirt").unwrap();
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    let i = entities.iter().position(|e| e.id == id).unwrap();
    if let Kind::Mob(m) = &mut entities[i].kind {
        m.carried = Some(dirt);
    }
    let mut level = WorldLevel::new(&ctx);
    // Not in the air, and not where it stands.
    blocks::place(&ctx, &mut level, &mut entities, i, [5, -58, 0], &[]);
    blocks::place(&ctx, &mut level, &mut entities, i, [0, -60, 0], &[]);
    assert_eq!(block(&ctx, [5, -58, 0]), "minecraft:air");
    assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:air");
    blocks::place(&ctx, &mut level, &mut entities, i, [5, -60, 0], &[]);
    assert_eq!(block(&ctx, [5, -60, 0]), "minecraft:dirt");
    let Kind::Mob(m) = &mut entities[i].kind else {
        panic!()
    };
    assert_eq!(m.carried, None);
    // And it takes a block back up.
    blocks::take(&ctx, &mut level, &mut entities[i], [5, -60, 0]);
    assert_eq!(block(&ctx, [5, -60, 0]), "minecraft:air");
    let Kind::Mob(m) = &entities[i].kind else {
        panic!()
    };
    assert_eq!(m.carried, Some(dirt));
}

#[test]
fn a_carrot_on_a_stick_boosts_the_pig_and_wears_to_a_rod() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    let saddle = ctx.items.id("minecraft:saddle").unwrap();
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.gear[mc_entity::mob::slot::SADDLE] = Some(mc_entity::mob::Gear {
            item: saddle,
            count: 1,
            damage: 0,
            components: Default::default(),
        });
        m.riders.push(7000);
    }
    hold(&ctx, "minecraft:carrot_on_a_stick", 1);
    let actor = actor_at([0.5, -60.0, 0.5]);
    let held = |ctx: &ServerContext| lock(&ctx.inventories)[&STEVE].slots[36].clone().unwrap();
    for n in 1..=3 {
        crate::interact::use_item(&ctx, actor, 0, 0.0, 0.0);
        let boost = with_mob(&ctx, id, |m| (m.boost, m.boost_total));
        assert!(
            (140..=980).contains(&boost.0) && boost.0 == boost.1,
            "{boost:?}"
        );
        assert_eq!(held(&ctx).damage, 7 * n);
        // Still fast: another use does nothing.
        crate::interact::use_item(&ctx, actor, 0, 0.0, 0.0);
        assert_eq!(held(&ctx).damage, 7 * n);
        if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
            && let Kind::Mob(m) = &mut e.kind
        {
            m.boost = 0;
        }
    }
    crate::interact::use_item(&ctx, actor, 0, 0.0, 0.0);
    assert_eq!(
        ctx.items.name(held(&ctx).item),
        Some("minecraft:fishing_rod")
    );
}

fn move_steve(ctx: &ServerContext, pos: [f64; 3]) {
    if let Some(p) = lock(&ctx.players).get_mut(&STEVE)
        && let Some(pose) = &mut p.pose
    {
        (pose.x, pose.y, pose.z) = (pos[0], pos[1], pos[2]);
    }
}

#[test]
fn a_lead_ties_leads_and_snaps() {
    use crate::decor::leash::Holder;
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let cow = put(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:lead", 2);
    ctx.mobs.use_on(ctx.dim, cow, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert_eq!(
        lock(&ctx.leashes).links.get(&cow),
        Some(&Holder::Player(STEVE))
    );
    assert_eq!(
        lock(&ctx.inventories)[&STEVE].slots[36]
            .as_ref()
            .map(|s| s.count),
        Some(1)
    );
    // A zombie takes no lead.
    let z = put(&ctx, Species::Zombie, [0.5, -60.0, 2.5]);
    ctx.mobs.use_on(ctx.dim, z, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(!lock(&ctx.leashes).links.contains_key(&z));
    // Walking off, the player leads the cow along.
    move_steve(&ctx, [9.5, -60.0, 0.5]);
    run(&ctx, 200);
    let x = mob_of(&ctx, cow).unwrap().pos[0];
    assert!(x > 5.0, "cow at {x}");
    assert!(lock(&ctx.leashes).links.contains_key(&cow));
    // Far off at once, the lead snaps and falls.
    move_steve(&ctx, [40.5, -60.0, 0.5]);
    run(&ctx, 1);
    crate::decor::leash::tick(&ctx);
    assert!(!lock(&ctx.leashes).links.contains_key(&cow));
    assert_eq!(items_of(&ctx, "minecraft:lead"), 1);
}

/// Steve at `x` on the row the lead tests use, sneaking or not.
fn steve_at(ctx: &ServerContext, x: f64, sneaking: bool) -> Actor {
    if let Some(p) = lock(&ctx.players).get_mut(&STEVE) {
        p.pose.as_mut().unwrap().x = x;
    }
    Actor {
        sneaking,
        ..steve([x, -60.0, 0.5])
    }
}

#[test]
fn the_holder_takes_a_lead_off() {
    use crate::decor::leash::Holder;
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let pig = put(&ctx, Species::Pig, [2.5, -60.0, 0.5]);
    crate::decor::leash::attach(&ctx, pig, Holder::Player(STEVE));
    ctx.mobs.use_on(ctx.dim, pig, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(!lock(&ctx.leashes).links.contains_key(&pig));
    assert_eq!(items_of(&ctx, "minecraft:lead"), 1);
}

fn tame_donkey(ctx: &ServerContext) -> i32 {
    let id = put(ctx, Species::Donkey, [2.5, -60.0, 0.5]);
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.owner = Some(STEVE);
        m.tame = true;
    }
    id
}

#[test]
fn a_chest_goes_on_a_tame_donkey_and_its_window_mirrors_the_saddle() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = tame_donkey(&ctx);
    let wild = put(&ctx, Species::Donkey, [4.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:chest", 2);
    let mut actor = actor_at([0.5, -60.0, 0.5]);
    // A wild one takes no chest.
    let out = crate::hits::use_entity(&ctx, actor, wild, 0, false, None);
    assert!(out.open_mount.is_none());
    assert!(!with_mob(&ctx, wild, |m| m.chested));
    crate::hits::use_entity(&ctx, actor, id, 0, false, None);
    assert!(with_mob(&ctx, id, |m| m.chested));
    assert_eq!(
        lock(&ctx.inventories)[&STEVE].slots[36]
            .as_ref()
            .map(|s| s.count),
        Some(1)
    );
    // Sneaking opens the window: 2 + 15 slots.
    actor.sneaking = true;
    let out = crate::hits::use_entity(&ctx, actor, id, 0, false, None);
    assert_eq!(out.open_mount, Some((id, 5)));
    let key = crate::containers::entity_key(id);
    let kind = crate::containers::Kind {
        entity: "",
        slots: 17,
        menu: "",
        title: "",
    };
    let slots = crate::containers::open(&ctx, STEVE, key, kind);
    assert_eq!(slots.len(), 17);
    // A saddle put in the first slot saddles it; a stack in the chest is
    // dropped when it dies.
    let saddle = ctx.items.id("minecraft:saddle").unwrap();
    let dirt = ctx.items.id("minecraft:dirt").unwrap();
    {
        let mut all = lock(&ctx.containers);
        let open = all.get_mut(&(key.x, key.y, key.z)).unwrap();
        open.slots[0] = Some(mc_protocol::packets::play::ItemStack::new(saddle, 1));
        open.slots[5] = Some(mc_protocol::packets::play::ItemStack::new(dirt, 3));
    }
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.saddled()));
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([2.5, -60.0, 0.5]),
        blow(1000.0, [2.5, -60.0, 0.5]),
    );
    run(&ctx, 1);
    assert_eq!(items_of(&ctx, "minecraft:dirt"), 3);
    assert_eq!(items_of(&ctx, "minecraft:chest"), 1);
}

fn orb_points(ctx: &ServerContext) -> i32 {
    lock(&ctx.entities)
        .iter()
        .map(|e| match e.kind {
            Kind::ExperienceOrb { value, count } => value * count,
            _ => 0,
        })
        .sum()
}

fn clear_orbs(ctx: &ServerContext) {
    lock(&ctx.entities).retain(|e| !matches!(e.kind, Kind::ExperienceOrb { .. }));
}

/// The experience a mob's death gives when a player did it.
fn kill_xp(ctx: &ServerContext, species: Species, baby: bool) -> i32 {
    clear_orbs(ctx);
    let id = put(ctx, species, [0.5, -60.0, 0.5]);
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.set_baby(baby);
    }
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([0.5, -60.0, 0.5]),
        blow(1000.0, [0.5, -60.0, 0.5]),
    );
    run(ctx, 1);
    orb_points(ctx)
}

#[test]
fn kills_pay_the_vanilla_experience() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    for _ in 0..5 {
        assert!((1..=3).contains(&kill_xp(&ctx, Species::Cow, false)));
        assert_eq!(kill_xp(&ctx, Species::Cow, true), 0, "calves pay nothing");
        assert_eq!(kill_xp(&ctx, Species::Creeper, false), 5);
        assert_eq!(kill_xp(&ctx, Species::Blaze, false), 10);
        // A baby zombie pays 12 (2.5 times 5, rounded down).
        assert_eq!(kill_xp(&ctx, Species::Zombie, true), 12);
    }
    // A slime pays its size.
    clear_orbs(&ctx);
    let id = put(&ctx, Species::Slime, [0.5, -60.0, 0.5]);
    let size = with_mob(&ctx, id, |m| m.size);
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([0.5, -60.0, 0.5]),
        blow(1000.0, [0.5, -60.0, 0.5]),
    );
    run(&ctx, 1);
    assert_eq!(orb_points(&ctx), size);
}

#[test]
fn breeding_pays_one_to_seven() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    crate::stats::load(&ctx, STEVE);
    for bred in 1..=8 {
        clear_orbs(&ctx);
        let a = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
        let b = put(&ctx, Species::Cow, [1.0, -60.0, 0.5]);
        for id in [a, b] {
            if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
                && let Kind::Mob(m) = &mut e.kind
            {
                m.fall_in_love(Some(STEVE));
            }
        }
        run(&ctx, 200);
        let xp = orb_points(&ctx);
        assert!((1..=7).contains(&xp), "bred for {xp}");
        assert_eq!(
            crate::stats::get(&ctx, STEVE, crate::stats::CUSTOM, "animals_bred"),
            bred
        );
        lock(&ctx.entities).retain(|e| !matches!(e.kind, Kind::Mob(_)));
    }
}

#[test]
fn villagers_open_and_close_wooden_doors_but_not_iron_ones() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let blocks = &ctx.data.blocks;
    for (name, opens) in [("minecraft:oak_door", true), ("minecraft:iron_door", false)] {
        let lower = blocks.default_state(name).unwrap();
        let upper = blocks.with_property(lower, "half", "upper").unwrap();
        set_state(&ctx, [5, -60, 5], lower);
        set_state(&ctx, [5, -59, 5], upper);
        let mut level = WorldLevel::new(&ctx);
        self::blocks::open_door(&ctx, &mut level, [5, -60, 5], true);
        drop(level);
        let state = |y| {
            let s = ctx
                .world
                .block(mc_protocol::Position::new(5, y, 5), &ctx.data.blocks)
                .unwrap();
            ctx.data.blocks.property(s, "open") == Some("true")
        };
        assert_eq!((state(-60), state(-59)), (opens, opens), "{name}");
        if opens {
            let mut level = WorldLevel::new(&ctx);
            self::blocks::open_door(&ctx, &mut level, [5, -59, 5], false);
            drop(level);
            assert_eq!((state(-60), state(-59)), (false, false));
        }
    }
}

#[test]
fn llamas_are_tamed_with_hay_and_carry_a_chest_and_a_carpet() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Llama, [2.5, -60.0, 0.5]);
    let (strength, health) = with_mob(&ctx, id, |m| (m.strength, m.max_health));
    assert!((1..=5).contains(&strength) && (15.0..=30.0).contains(&health));
    let mut actor = actor_at([0.5, -60.0, 0.5]);
    // A chest does not go on a wild one.
    hold(&ctx, "minecraft:chest", 1);
    crate::hits::use_entity(&ctx, actor, id, 0, false, None);
    assert!(!with_mob(&ctx, id, |m| m.chested));
    // Hay bales tame it in the end (temper reaches 30 after five).
    hold(&ctx, "minecraft:hay_block", 8);
    for _ in 0..8 {
        ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
        run(&ctx, 1);
        if with_mob(&ctx, id, |m| m.is_tame()) {
            break;
        }
    }
    assert_eq!(with_mob(&ctx, id, |m| m.owner), Some(STEVE));
    hold(&ctx, "minecraft:chest", 1);
    crate::hits::use_entity(&ctx, actor, id, 0, false, None);
    assert!(with_mob(&ctx, id, |m| m.chested));
    hold(&ctx, "minecraft:red_carpet", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.gear[mc_entity::mob::slot::BODY].is_some()));
    // Its window has a column of three chest slots for each point of
    // strength.
    actor.sneaking = true;
    let out = crate::hits::use_entity(&ctx, actor, id, 0, false, None);
    assert_eq!(out.open_mount, Some((id, strength)));
}

#[test]
fn a_chest_and_its_items_round_trip_through_nbt() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let mut nbt = mc_nbt::Compound::new();
    nbt.insert("ChestedHorse", mc_nbt::Tag::Byte(1));
    let mut item = mc_nbt::Compound::new();
    item.insert("Slot", mc_nbt::Tag::Byte(3));
    item.insert("id", mc_nbt::Tag::String("minecraft:dirt".into()));
    item.insert("count", mc_nbt::Tag::Int(5));
    nbt.insert(
        "Items",
        mc_nbt::Tag::List(vec![mc_nbt::Tag::Compound(item)]),
    );
    nbt.insert("CanBreakDoors", mc_nbt::Tag::Byte(1));
    let mut e = new_mob(&ctx, Species::Donkey, [2.5, -60.0, 0.5]);
    let Kind::Mob(m) = &mut e.kind else { panic!() };
    nbt::apply(&ctx, m, &nbt);
    assert!(m.chested && !m.breaks_doors, "doors are for zombies");
    let id = e.id;
    lock(&ctx.entities).push(e);
    run(&ctx, 2);
    // The saved slot 3 is window slot 5.
    let key = crate::containers::entity_key(id);
    let kind = crate::containers::Kind {
        entity: "",
        slots: 17,
        menu: "",
        title: "",
    };
    let slots = crate::containers::open(&ctx, STEVE, key, kind);
    let dirt = ctx.items.id("minecraft:dirt").unwrap();
    assert_eq!(
        slots[5].as_ref().map(|s| (s.item, s.count)),
        Some((dirt, 5))
    );
    // And it is written out the same way.
    run(&ctx, 1);
    let mut c = mc_nbt::Compound::new();
    let m = with_mob(&ctx, id, |m| m.clone());
    nbt::write(&ctx, &m, &mut c);
    let Some(mc_nbt::Tag::List(list)) = c.get("Items") else {
        panic!("no Items")
    };
    assert_eq!(list.len(), 1);
    let mc_nbt::Tag::Compound(first) = &list[0] else {
        panic!()
    };
    assert_eq!(first.get("Slot"), Some(&mc_nbt::Tag::Byte(3)));
    assert_eq!(c.get("ChestedHorse"), Some(&mc_nbt::Tag::Byte(1)));
    // A zombie keeps its door breaking.
    let mut z = new_mob(&ctx, Species::Zombie, [0.0; 3]);
    let Kind::Mob(zm) = &mut z.kind else { panic!() };
    nbt::apply(&ctx, zm, &nbt);
    assert!(zm.breaks_doors);
    let mut c = mc_nbt::Compound::new();
    nbt::write(&ctx, zm, &mut c);
    assert_eq!(c.get("CanBreakDoors"), Some(&mc_nbt::Tag::Byte(1)));
}

#[test]
fn a_door_a_villager_left_open_shuts_once_nobody_is_near() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let blocks = &ctx.data.blocks;
    let lower = blocks.default_state("minecraft:oak_door").unwrap();
    let upper = blocks.with_property(lower, "half", "upper").unwrap();
    set_state(&ctx, [5, -60, 5], lower);
    set_state(&ctx, [5, -59, 5], upper);
    let mut level = WorldLevel::new(&ctx);
    self::blocks::open_door(&ctx, &mut level, [5, -60, 5], true);
    drop(level);
    let open = |ctx: &ServerContext| {
        let s = ctx
            .world
            .block(mc_protocol::Position::new(5, -60, 5), &ctx.data.blocks)
            .unwrap();
        ctx.data.blocks.property(s, "open") == Some("true")
    };
    // Someone standing in the doorway keeps it open.
    let pig = put(&ctx, Species::Pig, [5.5, -60.0, 5.5]);
    if let Some(Kind::Mob(m)) = lock(&ctx.entities)
        .iter_mut()
        .find(|e| e.id == pig)
        .map(|e| &mut e.kind)
    {
        m.no_ai = true;
    }
    run(&ctx, 60);
    assert!(open(&ctx));
    lock(&ctx.entities).retain(|e| e.id != pig);
    run(&ctx, 40);
    assert!(!open(&ctx));
    assert!(lock(&ctx.door_watch).is_empty());
}

#[test]
fn a_hurt_llama_spits_at_the_player_and_it_stings() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let uuid = join(&ctx, [0.5, -60.0, 3.5]);
    let id = put(&ctx, Species::Llama, [0.5, -60.0, 0.5]);
    // A blow too light to kill: the llama has a grudge now.
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([0.5, -60.0, 3.5]),
        blow(0.5, [0.5, -60.0, 3.5]),
    );
    run(&ctx, 1);
    move_steve(&ctx, [0.5, -60.0, 8.5]);
    let mut spat = false;
    for _ in 0..120 {
        run(&ctx, 1);
        crate::tick::run_scheduled_ticks(&ctx);
        spat |= lock(&ctx.entities).iter().any(
            |e| matches!(&e.kind, Kind::Projectile(p) if p.shot == mc_entity::extra::Shot::Spit),
        );
    }
    assert!(spat, "no spit");
    let health = lock(&ctx.vitals)[&uuid].health;
    assert!(health < 20.0, "health {health}");
    // The llama is unhurt by its own spit.
    assert!(with_mob(&ctx, id, |m| m.health) > 0.0);
}

#[test]
fn a_wandering_trader_brings_two_leashed_llamas_that_leave_with_it() {
    use crate::decor::leash::Holder;
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let trader =
        crate::trading::new_merchant(&ctx, "minecraft:wandering_trader", [10.5, -60.0, 10.5])
            .unwrap();
    let tid = trader.id;
    crate::trading::spawn(&ctx, trader);
    crate::trading::bring_llamas(&ctx, tid, [10.5, -60.0, 10.5]);
    let llamas = mobs_of(&ctx, Species::TraderLlama);
    assert_eq!(llamas.len(), 2);
    for l in &llamas {
        assert_eq!(
            lock(&ctx.leashes).links.get(&l.id),
            Some(&Holder::Knot(tid))
        );
    }
    // The trader goes; so do its llamas, and no lead is left behind.
    lock(&ctx.entities).retain(|e| e.id != tid);
    crate::decor::leash::tick(&ctx);
    assert!(
        mobs_of(&ctx, Species::TraderLlama)
            .iter()
            .all(|e| e.removed)
    );
    assert_eq!(items_of(&ctx, "minecraft:lead"), 0);
}

#[test]
fn a_warped_fungus_on_a_stick_boosts_the_strider_and_wears_by_one() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Strider, [0.5, -60.0, 0.5]);
    let saddle = ctx.items.id("minecraft:saddle").unwrap();
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.gear[mc_entity::mob::slot::SADDLE] = Some(mc_entity::mob::Gear {
            item: saddle,
            count: 1,
            damage: 0,
            components: Default::default(),
        });
        m.riders.push(7000);
    }
    // A carrot on a stick does nothing to a strider.
    hold(&ctx, "minecraft:carrot_on_a_stick", 1);
    let actor = actor_at([0.5, -60.0, 0.5]);
    crate::interact::use_item(&ctx, actor, 0, 0.0, 0.0);
    assert_eq!(with_mob(&ctx, id, |m| m.boost), 0);
    hold(&ctx, "minecraft:warped_fungus_on_a_stick", 1);
    crate::interact::use_item(&ctx, actor, 0, 0.0, 0.0);
    let boost = with_mob(&ctx, id, |m| (m.boost, m.boost_total));
    assert!(
        (140..=980).contains(&boost.0) && boost.0 == boost.1,
        "{boost:?}"
    );
    let held = lock(&ctx.inventories)[&STEVE].slots[36].clone().unwrap();
    assert_eq!(held.damage, 1);
}

#[test]
fn a_strider_walks_on_lava_and_is_cold_on_land() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 20.5]);
    for x in -14..=22 {
        for z in -14..=22 {
            set(&ctx, [x, -61, z], "minecraft:lava");
        }
    }
    let id = put(&ctx, Species::Strider, [2.5, -60.1, 2.5]);
    // It stays on the lava (a stroll off it is cut short and it returns).
    let mut on_lava = 0;
    for _ in 0..300 {
        run(&ctx, 1);
        if mob_of(&ctx, id).is_some_and(|e| (e.pos[1] + 60.11).abs() < 0.05) {
            on_lava += 1;
        }
    }
    assert!(on_lava > 200, "on lava {on_lava} of 300");
    assert!(with_mob(&ctx, id, |m| m.health) >= 20.0);
    // Out on the flat ground it shivers.
    let land = put(&ctx, Species::Strider, [30.5, -60.0, 30.5]);
    run(&ctx, 5);
    assert!(with_mob(&ctx, land, |m| m.cold));
}

fn villager_at(ctx: &ServerContext, pos: [f64; 3]) -> i32 {
    let e = crate::trading::new_merchant(ctx, "minecraft:villager", pos).unwrap();
    let id = e.id;
    lock(&ctx.entities).push(e);
    id
}

fn merchant_of(ctx: &ServerContext, id: i32) -> Box<mc_entity::trade::Merchant> {
    let e = mob_of(ctx, id).expect("villager still there");
    let Kind::Merchant(m) = e.kind else { panic!() };
    m
}

/// Sets the time of day (the clocks run on from there).
fn set_day(ctx: &ServerContext, t: i64) {
    let mut time = lock(&ctx.time);
    for c in &mut time.clocks {
        c.ticks = t;
        c.running = false;
    }
}

fn run_all(ctx: &ServerContext, ticks: usize) {
    for _ in 0..ticks {
        lock(&ctx.time).age += 1;
        let mut level = WorldLevel::new(ctx);
        tick(ctx, &mut level);
        drop(level);
        crate::tick::run_scheduled_ticks(ctx);
    }
}

fn bed_state(ctx: &ServerContext, at: [i32; 3]) -> Option<String> {
    let s = ctx.world.block(
        mc_protocol::Position::new(at[0], at[1], at[2]),
        &ctx.data.blocks,
    )?;
    ctx.data.blocks.property(s, "occupied").map(str::to_owned)
}

#[test]
fn villagers_lie_down_in_a_free_bed_at_night_and_get_up_by_day() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let blocks = &ctx.data.blocks;
    let bed = blocks.default_state("minecraft:red_bed").unwrap();
    let foot = blocks.with_property(bed, "facing", "east").unwrap();
    let foot = blocks.with_property(foot, "part", "foot").unwrap();
    let head = blocks.with_property(foot, "part", "head").unwrap();
    set_state(&ctx, [6, -60, 3], foot);
    set_state(&ctx, [7, -60, 3], head);
    let id = villager_at(&ctx, [3.5, -60.0, 3.5]);
    set_day(&ctx, 13000);
    let mut slept = false;
    for _ in 0..600 {
        run_all(&ctx, 1);
        if merchant_of(&ctx, id).walk.asleep {
            slept = true;
            break;
        }
    }
    {
        let m = merchant_of(&ctx, id);
        let e = mob_of(&ctx, id).unwrap();
        assert!(
            slept,
            "never went to bed: {:?} bed {:?} rest {} path {:?} pos {:?} age {}",
            m.walk.asleep,
            m.walk.bed,
            m.walk.rest,
            m.walk.nav.path,
            e.pos,
            lock(&ctx.time).age
        );
    }
    let e = mob_of(&ctx, id).unwrap();
    assert!(
        (e.pos[0] - 7.5).abs() < 0.01 && (e.pos[1] + 59.4375).abs() < 0.01,
        "{:?}",
        e.pos
    );
    assert_eq!(bed_state(&ctx, [7, -60, 3]).as_deref(), Some("true"));
    assert_eq!(bed_state(&ctx, [6, -60, 3]).as_deref(), Some("true"));
    // Day comes: it gets up and the bed is free.
    set_day(&ctx, 24100);
    run_all(&ctx, 2);
    assert!(!merchant_of(&ctx, id).walk.asleep);
    assert_eq!(bed_state(&ctx, [7, -60, 3]).as_deref(), Some("false"));
}

#[test]
fn a_bed_is_free_again_when_its_sleeper_is_gone() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let blocks = &ctx.data.blocks;
    let bed = blocks.default_state("minecraft:red_bed").unwrap();
    let foot = blocks.with_property(bed, "facing", "east").unwrap();
    let foot = blocks.with_property(foot, "part", "foot").unwrap();
    let head = blocks.with_property(foot, "part", "head").unwrap();
    set_state(&ctx, [6, -60, 3], foot);
    set_state(&ctx, [7, -60, 3], head);
    let id = villager_at(&ctx, [5.5, -60.0, 3.5]);
    set_day(&ctx, 13000);
    for _ in 0..400 {
        run_all(&ctx, 1);
        if merchant_of(&ctx, id).walk.asleep {
            break;
        }
    }
    assert!(merchant_of(&ctx, id).walk.asleep);
    lock(&ctx.entities).retain(|e| e.id != id);
    run_all(&ctx, 40);
    assert_eq!(bed_state(&ctx, [7, -60, 3]).as_deref(), Some("false"));
}

#[test]
fn villagers_run_from_zombies() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = villager_at(&ctx, [10.5, -60.0, 10.5]);
    let z = put(&ctx, Species::Zombie, [14.5, -60.0, 10.5]);
    // The zombie stands still: it has no one to hunt but this.
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == z)
        && let Kind::Mob(m) = &mut e.kind
    {
        m.persistent = true;
        m.speed_attr = 0.0;
    }
    run_all(&ctx, 100);
    let e = mob_of(&ctx, id).unwrap();
    let far = e.pos[0] - 14.5;
    assert!(far < -8.0, "villager at {:?}", e.pos);
}

#[test]
fn a_cold_strider_heads_back_to_the_lava() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 20.5]);
    for x in 5..=12 {
        for z in -2..=6 {
            set(&ctx, [x, -61, z], "minecraft:lava");
        }
    }
    let id = put(&ctx, Species::Strider, [2.5, -60.0, 2.5]);
    let mut back = false;
    for _ in 0..800 {
        run(&ctx, 1);
        if mob_of(&ctx, id).is_some_and(|e| e.pos[0] > 5.0 && (e.pos[1] + 60.11).abs() < 0.05) {
            back = true;
            break;
        }
    }
    assert!(back, "still at {:?}", mob_of(&ctx, id).map(|e| e.pos));
}

fn block_name(ctx: &ServerContext, x: i32, y: i32, z: i32) -> String {
    let p = mc_protocol::Position::new(x, y, z);
    let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
    ctx.data.blocks.block_name(s).unwrap_or("").to_owned()
}

/// Farmland in place of the grass, a crop on it, and a mob dropped two
/// blocks onto it. The mob does not wander.
fn drop_on(ctx: &ServerContext, species: Species, x: i32, z: i32) -> i32 {
    let farm = mc_protocol::Position::new(x, -61, z);
    let crop = mc_protocol::Position::new(x, -60, z);
    let farmland = ctx.data.blocks.default_state("minecraft:farmland").unwrap();
    let wheat = ctx.data.blocks.default_state("minecraft:wheat").unwrap();
    WorldLevel::new(ctx).set_block(farm, farmland);
    WorldLevel::new(ctx).set_block(crop, wheat);
    // Farmland stands 15/16 of a block; two blocks above that is the drop.
    let y = -61.0 + 15.0 / 16.0 + 2.0;
    let mut e = new_mob(ctx, species, [f64::from(x) + 0.5, y, f64::from(z) + 0.5]);
    if let Kind::Mob(m) = &mut e.kind {
        m.no_ai = true;
    }
    let id = e.id;
    lock(&ctx.entities).push(e);
    id
}

#[test]
fn a_falling_mob_tramples_farmland() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    lock(&ctx.game_rules).set("spawn_mobs", crate::rules::Value::Bool(false));

    let cow = drop_on(&ctx, Species::Cow, 4, 4);
    run(&ctx, 40);
    assert!(
        mob_of(&ctx, cow).is_some_and(|e| e.on_ground),
        "cow never landed"
    );
    assert_eq!(block_name(&ctx, 4, -61, 4), "minecraft:dirt");
    assert_eq!(block_name(&ctx, 4, -60, 4), "minecraft:air");

    let hen = drop_on(&ctx, Species::Chicken, 6, 4);
    run(&ctx, 40);
    assert!(
        mob_of(&ctx, hen).is_some_and(|e| e.on_ground),
        "chicken never landed"
    );
    assert_eq!(block_name(&ctx, 6, -61, 4), "minecraft:farmland");

    lock(&ctx.game_rules).set("mob_griefing", crate::rules::Value::Bool(false));
    let quiet = drop_on(&ctx, Species::Cow, 8, 4);
    run(&ctx, 40);
    assert!(
        mob_of(&ctx, quiet).is_some_and(|e| e.on_ground),
        "cow never landed"
    );
    assert_eq!(block_name(&ctx, 8, -61, 4), "minecraft:farmland");
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
    ctx.mobs
        .use_on(ctx.dim, zombie, steve([0.5, -60.0, 0.5]), 0);
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
    let _ = steve_at(&ctx, 11.0, false);
    let before = mob_of(&ctx, cow).unwrap().pos[0];
    for _ in 0..20 {
        crate::decor::leash::tick(&ctx);
        run(&ctx, 1);
        let e = mob_of(&ctx, cow).unwrap();
        assert!(e.vel[0] >= 0.0, "pulled, not pushed: {:?}", e.vel);
    }
    let e = mob_of(&ctx, cow).unwrap();
    assert!(e.pos[0] > before + 0.5, "{} -> {}", before, e.pos[0]);
    assert!(
        matches!(&e.kind, Kind::Mob(m) if (m.yaw + 90.0).abs() < 30.0),
        "faces east"
    );
    assert_eq!(leashes(&ctx).len(), 1, "still on at under twelve blocks");
    // Using it again lets go, and the lead drops.
    let at = mob_of(&ctx, cow).unwrap().pos[0];
    ctx.mobs.use_on(ctx.dim, cow, steve_at(&ctx, at, false), 0);
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
    let _ = steve_at(&ctx, 13.5, false);
    crate::decor::leash::tick(&ctx);
    assert_eq!(leashes(&ctx).len(), 1);
    let _ = steve_at(&ctx, 15.0, false);
    crate::decor::leash::tick(&ctx);
    assert!(leashes(&ctx).is_empty(), "snapped");
    assert_eq!(items_of(&ctx, "minecraft:lead"), 1);
    // Sneaking at the pig ties the cow to it.
    lock(&ctx.leashes).links.insert(cow, Holder::Player(STEVE));
    ctx.mobs.use_on(ctx.dim, pig, steve_at(&ctx, 0.5, true), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&cow), Some(&Holder::Knot(pig)));
    // A lead from the pig to the cow is the cow's to keep: taking it with a
    // lead in hand moves it to the player.
    hold(&ctx, "minecraft:lead", 1);
    ctx.mobs.use_on(ctx.dim, cow, steve_at(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&cow), Some(&Holder::Player(STEVE)));
    // Nobody takes another player's: another holder leaves it be.
    lock(&ctx.leashes).links.insert(pig, Holder::Player(0xbeef));
    hold(&ctx, "minecraft:lead", 1);
    ctx.mobs.use_on(ctx.dim, pig, steve_at(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert_eq!(leashes(&ctx).get(&pig), Some(&Holder::Player(0xbeef)));
    // Shears snip every lead of the pig.
    lock(&ctx.leashes).links.insert(cow, Holder::Knot(pig));
    hold(&ctx, "minecraft:shears", 1);
    ctx.mobs.use_on(ctx.dim, pig, steve_at(&ctx, 0.5, false), 0);
    run(&ctx, 1);
    assert!(leashes(&ctx).is_empty(), "{:?}", leashes(&ctx));
}

#[test]
fn a_bucket_milks_a_goat_and_its_horns_round_trip() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Goat, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:bucket", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    let milk = ctx.items.id("minecraft:milk_bucket").unwrap();
    let got = lock(&ctx.inventories)[&STEVE].slots[36]
        .as_ref()
        .map(|s| s.item);
    assert_eq!(got, Some(milk));

    let mob = {
        let e = mob_of(&ctx, id).unwrap();
        let Kind::Mob(m) = e.kind else { panic!() };
        m
    };
    assert!(mob.quirks.land.left_horn && mob.quirks.land.right_horn);
    assert!(!mob.quirks.land.screaming);
    let mut saved = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &mob, &mut saved);
    let mut loaded = Mob::new(Species::Goat, 1);
    loaded.quirks.land.screaming = false;
    loaded.quirks.land.left_horn = true;
    crate::mobs::nbt::apply(&ctx, &mut loaded, &saved);
    // Rewrite with the screaming goat and one horn gone.
    let mut screaming = mob;
    screaming.quirks.land.screaming = true;
    screaming.quirks.land.right_horn = false;
    let mut saved = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &screaming, &mut saved);
    crate::mobs::nbt::apply(&ctx, &mut loaded, &saved);
    assert!(loaded.quirks.land.screaming);
    assert!(loaded.quirks.land.left_horn);
    assert!(!loaded.quirks.land.right_horn);

    let meta = crate::mobs::meta::metadata_of(&ctx, &loaded, 0);
    let flag = |index: u8| {
        meta.iter().find_map(|(i, v)| {
            (*i == index).then_some(matches!(
                v,
                mc_protocol::packets::play::MetadataValue::Boolean(true)
            ))
        })
    };
    assert_eq!(flag(18), Some(true));
    assert_eq!(flag(19), Some(true));
    assert_eq!(flag(20), Some(false));
    assert_eq!(crate::mobs::land::horn_group("minecraft:frozen_peaks"), 2);
    assert_eq!(crate::mobs::land::horn_group("minecraft:desert"), 3);
    assert_eq!(crate::mobs::land::horn_group("minecraft:swamp"), 1);
    assert_eq!(crate::mobs::land::horn_group("minecraft:plains"), 0);
}

#[test]
fn two_screaming_goats_have_a_screaming_baby() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let a = put(&ctx, Species::Goat, [0.5, -60.0, 0.5]);
    let b = put(&ctx, Species::Goat, [2.5, -60.0, 0.5]);
    let mut entities = lock(&ctx.entities);
    for id in [a, b] {
        let Kind::Mob(m) = &mut entities.iter_mut().find(|e| e.id == id).unwrap().kind else {
            panic!()
        };
        m.quirks.land.screaming = true;
        m.fall_in_love(None);
    }
    let baby = crate::mobs::hurt::breed(&ctx, &mut entities, 0, b).expect("baby");
    drop(entities);
    let Kind::Mob(m) = &baby.kind else { panic!() };
    assert!(m.is_baby());
    assert!(m.quirks.land.screaming);
    assert!(m.quirks.land.left_horn && m.quirks.land.right_horn);
}

#[test]
fn a_burning_mob_in_a_water_cauldron_goes_out() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 10.5]);
    let state = ctx
        .data
        .blocks
        .state_id("minecraft:water_cauldron", &[("level", "3")])
        .unwrap();
    WorldLevel::new(&ctx).set_block(mc_protocol::Position::new(0, -60, 0), state);
    let id = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
    if let Some(Kind::Mob(m)) = lock(&ctx.entities).iter_mut().map(|e| &mut e.kind).next() {
        m.burning = 200;
        m.no_ai = true;
    }
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.burning), 0);
    assert_eq!(block_name(&ctx, 0, -60, 0), "minecraft:water_cauldron");
    let s = ctx
        .world
        .block(mc_protocol::Position::new(0, -60, 0), &ctx.data.blocks)
        .unwrap();
    assert_eq!(ctx.shapes.property(s, "level"), Some("2"));
}

#[test]
fn a_wandering_trader_walks_falls_and_dies_to_a_player() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let e = crate::trading::new_merchant(&ctx, "minecraft:wandering_trader", [5.5, -60.0, 5.5])
        .unwrap();
    let id = e.id;
    lock(&ctx.entities).push(e);
    // It strolls about.
    let mut farthest = 0.0f64;
    for _ in 0..800 {
        run_all(&ctx, 1);
        let p = mob_of(&ctx, id).unwrap().pos;
        farthest = farthest.max((p[0] - 5.5).hypot(p[2] - 5.5));
    }
    assert!(farthest > 2.0, "stood still");
    // With the floor gone it falls as anything does, faster and faster.
    let p = mob_of(&ctx, id).unwrap().pos;
    let (x, z) = (p[0].floor() as i32, p[2].floor() as i32);
    let air = ctx.data.blocks.default_state("minecraft:air").unwrap();
    for y in -63..=-61 {
        for dx in -1..=1 {
            for dz in -1..=1 {
                let at = mc_protocol::Position::new(x + dx, y, z + dz);
                ctx.world.set_block(at, air, &ctx.data.blocks, &ctx.rules);
            }
        }
    }
    run_all(&ctx, 8);
    assert!(mob_of(&ctx, id).unwrap().pos[1] < -62.0);
    // A player's blows hurt it: a second one within half a second does
    // not, and it dies at the end of its 20 health.
    let at = mob_of(&ctx, id).unwrap().pos;
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(6.0, at));
    run(&ctx, 1);
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(6.0, at));
    run(&ctx, 1);
    assert!(mob_of(&ctx, id).is_some_and(|e| !e.removed));
    for _ in 0..3 {
        run(&ctx, 10);
        let at = mob_of(&ctx, id).map_or(at, |e| e.pos);
        ctx.mobs.hit(ctx.dim, id, steve(at), blow(6.0, at));
        run(&ctx, 1);
    }
    assert!(mob_of(&ctx, id).is_none_or(|e| e.removed), "still alive");
}

// ---- Land animals (Cursor) ----

#[test]
fn land_animals_have_their_wiki_health_and_size() {
    for (s, hp, size) in [
        (Species::PolarBear, 30.0, (1.4, 1.4)),
        (Species::Ocelot, 10.0, (0.6, 0.7)),
        (Species::SkeletonHorse, 15.0, (1.3964844, 1.6)),
        (Species::ZombieHorse, 15.0, (1.3964844, 1.6)),
        (Species::Hoglin, 40.0, (1.3964844, 1.4)),
        (Species::Zoglin, 40.0, (1.3964844, 1.4)),
        (Species::PiglinBrute, 50.0, (0.6, 1.95)),
    ] {
        assert_eq!(s.max_health(), hp, "{s:?}");
        assert_eq!(s.size(), size, "{s:?}");
        assert_eq!(Species::from_id(s.id()), Some(s));
    }
    assert!(Species::Zoglin.undead() && Species::SkeletonHorse.undead());
    assert!(Species::Hoglin.ageable() && Species::Hoglin.food_tag().is_some());
    assert_eq!(Species::PiglinBrute.xp_reward(), (20, 20));
}

#[test]
fn hoglins_and_brutes_zombify_outside_the_nether() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let hoglin = put(&ctx, Species::Hoglin, [0.5, -60.0, 0.5]);
    let brute = put(&ctx, Species::PiglinBrute, [4.5, -60.0, 0.5]);
    run(&ctx, 290);
    assert!(mob_of(&ctx, hoglin).is_some_and(|e| !e.removed));
    run(&ctx, 20);
    let kinds: Vec<Species> = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) => Some(m.species),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&Species::Zoglin), "{kinds:?}");
    assert!(kinds.contains(&Species::ZombifiedPiglin), "{kinds:?}");
    assert!(!kinds.contains(&Species::Hoglin) && !kinds.contains(&Species::PiglinBrute));
    let _ = brute;
}

#[test]
fn a_brute_spawns_with_a_golden_axe() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut e = new_mob(&ctx, Species::PiglinBrute, [0.5, -60.0, 0.5]);
    crate::mobs::spawn::dress(&ctx, &mut e, Some("minecraft:basalt_deltas"));
    let Kind::Mob(m) = &e.kind else { panic!() };
    let axe = m.gear[mc_entity::mob::slot::MAINHAND]
        .as_ref()
        .map(|g| g.item);
    assert_eq!(axe, ctx.items.id("minecraft:golden_axe"));
}
