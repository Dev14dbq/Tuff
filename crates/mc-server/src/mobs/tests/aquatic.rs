//! Fish, squids, dolphins, turtles, axolotls, frogs, tadpoles, guardians
//! and nautiluses in the world (Claude).

use super::*;
use mc_protocol::packets::play::MetadataValue;

fn normal(ctx: &ServerContext) {
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
}

/// Fills a box of cells (inclusive) with a block.
fn fill(ctx: &ServerContext, from: [i32; 3], to: [i32; 3], name: &str) {
    let state = ctx.data.blocks.default_state(name).unwrap();
    for x in from[0]..=to[0] {
        for y in from[1]..=to[1] {
            for z in from[2]..=to[2] {
                set_state(ctx, [x, y, z], state);
            }
        }
    }
}

fn set_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut Mob) -> R) -> R {
    let mut all = lock(&ctx.entities);
    let Kind::Mob(m) = &mut all.iter_mut().find(|e| e.id == id).unwrap().kind else {
        panic!()
    };
    f(m)
}

fn meta(ctx: &ServerContext, m: &Mob) -> Vec<(u8, MetadataValue)> {
    crate::mobs::meta::metadata_of(ctx, m, 0)
}

fn field(v: &[(u8, MetadataValue)], index: u8) -> Option<&MetadataValue> {
    v.iter().find(|(i, _)| *i == index).map(|(_, x)| x)
}

fn health(ctx: &ServerContext) -> f32 {
    lock(&ctx.vitals)[&STEVE].health
}

fn effect_of(ctx: &ServerContext, name: &str) -> Option<(i32, i32)> {
    lock(&ctx.vitals)[&STEVE]
        .effects
        .active
        .get(name)
        .map(|i| (i.amplifier, i.duration))
}

#[test]
fn every_aquatic_species_is_known_by_the_registry() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    for s in [
        Species::Cod,
        Species::Salmon,
        Species::Pufferfish,
        Species::TropicalFish,
        Species::Squid,
        Species::GlowSquid,
        Species::Dolphin,
        Species::Turtle,
        Species::Axolotl,
        Species::Frog,
        Species::Tadpole,
        Species::Guardian,
        Species::ElderGuardian,
        Species::Nautilus,
        Species::ZombieNautilus,
    ] {
        assert_ne!(crate::mobs::meta::type_id(&ctx, s), 0, "{s:?}");
        assert!(
            ctx.items.id(&format!("{}_spawn_egg", s.id())).is_some(),
            "{s:?}"
        );
    }
}

#[test]
fn the_metadata_of_the_aquatic_mobs_follows_the_wiki() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mob = |s: Species| Mob::new(s, 99);
    let v = meta(&ctx, &mob(Species::Cod));
    assert_eq!(field(&v, 16), Some(&MetadataValue::Boolean(false)));
    let mut puffer = mob(Species::Pufferfish);
    puffer.quirks.aqua.puff = 2;
    let v = meta(&ctx, &puffer);
    assert_eq!(field(&v, 17), Some(&MetadataValue::VarInt(2)));
    let mut tropical = mob(Species::TropicalFish);
    tropical.variant = 65536;
    assert_eq!(
        field(&meta(&ctx, &tropical), 17),
        Some(&MetadataValue::VarInt(65536))
    );
    let mut salmon = mob(Species::Salmon);
    salmon.variant = 2;
    assert_eq!(
        field(&meta(&ctx, &salmon), 17),
        Some(&MetadataValue::VarInt(2))
    );
    let mut glow = mob(Species::GlowSquid);
    glow.quirks.aqua.dark = 60;
    assert_eq!(
        field(&meta(&ctx, &glow), 18),
        Some(&MetadataValue::VarInt(60))
    );
    // Squids and dolphins are ageable: the baby flag is field 16.
    let mut squid = mob(Species::Squid);
    squid.set_baby(true);
    assert_eq!(
        field(&meta(&ctx, &squid), 16),
        Some(&MetadataValue::Boolean(true))
    );
    let mut dolphin = mob(Species::Dolphin);
    dolphin.quirks.aqua.got_fish = true;
    let v = meta(&ctx, &dolphin);
    assert_eq!(field(&v, 18), Some(&MetadataValue::Boolean(true)));
    assert_eq!(field(&v, 19), Some(&MetadataValue::VarInt(2400)));
    let mut turtle = mob(Species::Turtle);
    turtle.quirks.aqua.has_egg = true;
    let v = meta(&ctx, &turtle);
    assert_eq!(field(&v, 18), Some(&MetadataValue::Boolean(true)));
    assert_eq!(field(&v, 19), Some(&MetadataValue::Boolean(false)));
    let mut axolotl = mob(Species::Axolotl);
    axolotl.variant = 3;
    axolotl.quirks.aqua.dead_play = 10;
    axolotl.quirks.aqua.from_bucket = true;
    let v = meta(&ctx, &axolotl);
    assert_eq!(field(&v, 18), Some(&MetadataValue::VarInt(3)));
    assert_eq!(field(&v, 19), Some(&MetadataValue::Boolean(true)));
    assert_eq!(field(&v, 20), Some(&MetadataValue::Boolean(true)));
    let frog = mob(Species::Frog);
    let v = meta(&ctx, &frog);
    assert!(matches!(field(&v, 18), Some(MetadataValue::Holder(27, _))));
    assert_eq!(field(&v, 19), Some(&MetadataValue::OptVarInt(None)));
    let tadpole = mob(Species::Tadpole);
    let v = meta(&ctx, &tadpole);
    assert_eq!(field(&v, 16), Some(&MetadataValue::Boolean(false)));
    assert_eq!(field(&v, 17), Some(&MetadataValue::Boolean(false)));
    let mut guardian = mob(Species::Guardian);
    guardian.quirks.aqua.beam = 12;
    guardian.quirks.aqua.retract = true;
    let v = meta(&ctx, &guardian);
    assert_eq!(field(&v, 16), Some(&MetadataValue::Boolean(true)));
    assert_eq!(field(&v, 17), Some(&MetadataValue::VarInt(12)));
    let mut nautilus = mob(Species::Nautilus);
    nautilus.owner = Some(5);
    let v = meta(&ctx, &nautilus);
    assert_eq!(field(&v, 18), Some(&MetadataValue::Byte(4)));
    assert_eq!(field(&v, 19), Some(&MetadataValue::OptUuid(Some(5))));
    assert_eq!(field(&v, 20), Some(&MetadataValue::Boolean(false)));
    let zombie = mob(Species::ZombieNautilus);
    let v = meta(&ctx, &zombie);
    assert!(matches!(field(&v, 21), Some(MetadataValue::Holder(32, _))));
}

#[test]
fn the_aquatic_mobs_are_saved_and_read_back() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let round = |m: &Mob| {
        let mut c = mc_nbt::Compound::new();
        crate::mobs::nbt::write(&ctx, m, &mut c);
        let mut back = Mob::new(m.species, 5);
        crate::mobs::nbt::apply(&ctx, &mut back, &c);
        (c, back)
    };
    let mut p = Mob::new(Species::Pufferfish, 3);
    p.quirks.aqua.puff = 1;
    p.quirks.aqua.from_bucket = true;
    let (c, back) = round(&p);
    assert_eq!(c.get("PuffState"), Some(&mc_nbt::Tag::Int(1)));
    assert_eq!(back.quirks.aqua.puff, 1);
    assert!(back.quirks.aqua.from_bucket && back.persistent);
    let mut s = Mob::new(Species::Salmon, 3);
    s.variant = 2;
    let (c, back) = round(&s);
    assert_eq!(c.get("type"), Some(&mc_nbt::Tag::String("large".into())));
    assert_eq!(back.variant, 2);
    let mut t = Mob::new(Species::TropicalFish, 3);
    t.variant = 918_273;
    assert_eq!(round(&t).1.variant, 918_273);
    let mut a = Mob::new(Species::Axolotl, 3);
    a.variant = 4;
    assert_eq!(round(&a).1.variant, 4);
    let mut turtle = Mob::new(Species::Turtle, 3);
    turtle.quirks.aqua.has_egg = true;
    turtle.quirks.aqua.home = Some([4, -60, 9]);
    let (c, back) = round(&turtle);
    assert_eq!(
        c.get("home_pos"),
        Some(&mc_nbt::Tag::IntArray(vec![4, -60, 9]))
    );
    assert!(back.quirks.aqua.has_egg);
    assert_eq!(back.quirks.aqua.home, Some([4, -60, 9]));
    let mut tad = Mob::new(Species::Tadpole, 3);
    tad.quirks.aqua.growth = 777;
    assert_eq!(round(&tad).1.quirks.aqua.growth, 777);
    let mut d = Mob::new(Species::Dolphin, 3);
    d.quirks.aqua.got_fish = true;
    d.quirks.aqua.air = 1234;
    let back = round(&d).1;
    assert!(back.quirks.aqua.got_fish);
    assert_eq!(back.quirks.aqua.air, 1234);
    // A frog's variant is saved by name.
    let mut f = Mob::new(Species::Frog, 3);
    f.variant = crate::mobs::meta::variant_id(&ctx, Species::Frog, "warm").unwrap();
    let (c, back) = round(&f);
    assert_eq!(
        c.get("variant"),
        Some(&mc_nbt::Tag::String("minecraft:warm".into()))
    );
    assert_eq!(back.variant, f.variant);
}

#[test]
fn frogs_and_zombie_nautiluses_take_the_look_of_their_biome() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let variant = |species, biome: Option<&str>| {
        let e = new_mob_in(&ctx, species, [0.0; 3], biome);
        let Kind::Mob(m) = &e.kind else { panic!() };
        crate::mobs::meta::variant_name(&ctx, species, m.variant).unwrap()
    };
    assert_eq!(
        variant(Species::Frog, Some("minecraft:swamp")),
        "minecraft:temperate"
    );
    assert_eq!(
        variant(Species::Frog, Some("minecraft:desert")),
        "minecraft:warm"
    );
    assert_eq!(
        variant(Species::Frog, Some("minecraft:snowy_taiga")),
        "minecraft:cold"
    );
    assert_eq!(
        variant(Species::ZombieNautilus, Some("minecraft:warm_ocean")),
        "minecraft:warm"
    );
    assert_eq!(
        variant(Species::ZombieNautilus, Some("minecraft:ocean")),
        "minecraft:temperate"
    );
}

#[test]
fn a_water_bucket_catches_a_fish_and_the_bucket_lets_it_go() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::TropicalFish, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.variant = 459_008);
    hold(&ctx, "minecraft:water_bucket", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 2);
    assert!(mob_of(&ctx, id).is_none_or(|e| e.removed));
    let inv = lock(&ctx.inventories);
    let stack = inv[&STEVE]
        .slots
        .iter()
        .flatten()
        .find(|s| ctx.items.name(s.item) == Some("minecraft:tropical_fish_bucket"))
        .cloned()
        .expect("a bucket of tropical fish");
    drop(inv);
    // Back into the water.
    let before = lock(&ctx.entities).len();
    crate::mobs::aqua::release(
        &ctx,
        "minecraft:tropical_fish_bucket",
        &stack,
        mc_protocol::Position::new(5, -60, 5),
    );
    let all = lock(&ctx.entities);
    assert_eq!(all.len(), before + 1);
    let Kind::Mob(m) = &all.last().unwrap().kind else {
        panic!()
    };
    assert_eq!(m.species, Species::TropicalFish);
    assert_eq!(m.variant, 459_008);
    assert!(m.quirks.aqua.from_bucket && m.persistent);
}

#[test]
fn every_bucketable_creature_has_its_bucket() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    for s in [
        Species::Cod,
        Species::Salmon,
        Species::Pufferfish,
        Species::TropicalFish,
        Species::Axolotl,
        Species::Tadpole,
    ] {
        let bucket = crate::mobs::aqua::bucket_of(s).unwrap();
        assert!(ctx.items.id(bucket).is_some(), "{bucket}");
        assert_eq!(crate::mobs::aqua::bucket_species(bucket), Some(s));
    }
}

#[test]
fn a_tadpole_in_a_bucket_keeps_its_age_and_a_slimeball_speeds_it() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Tadpole, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.aqua.growth = 10_000);
    hold(&ctx, "minecraft:slime_ball", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    // A tenth of the 14000 left, and the tick since.
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.aqua.growth), 11_401);
}

#[test]
fn a_pufferfish_stings_a_player_and_poisons_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    fill(&ctx, [-4, -60, -4], [4, -55, 4], "minecraft:water");
    let id = put(&ctx, Species::Pufferfish, [0.6, -59.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    run(&ctx, 60);
    assert!(health(&ctx) < 20.0, "{}", health(&ctx));
    let (amp, ticks) = effect_of(&ctx, "poison").expect("poisoned");
    assert_eq!(amp, 0);
    assert!(ticks > 0 && ticks <= 120, "{ticks}");
}

#[test]
fn nothing_stings_on_peaceful() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(0, std::sync::atomic::Ordering::Relaxed);
    join(&ctx, [0.5, -60.0, 0.5]);
    fill(&ctx, [-4, -60, -4], [4, -55, 4], "minecraft:water");
    let id = put(&ctx, Species::Pufferfish, [0.6, -59.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    run(&ctx, 60);
    assert_eq!(health(&ctx), 20.0);
    assert!(effect_of(&ctx, "poison").is_none());
}

#[test]
fn a_guardian_beam_hurts_and_its_spikes_hurt_back() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Guardian, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    // Spikes out: whoever strikes it is struck.
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    run(&ctx, 1);
    assert_eq!(health(&ctx), 18.0);
    // Pulled in while it swims: no thorns.
    run(&ctx, 25);
    set_mob(&ctx, id, |m| m.quirks.aqua.retract = true);
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    run(&ctx, 1);
    assert_eq!(health(&ctx), 18.0);
}

#[test]
fn an_elder_guardian_lays_mining_fatigue_on_players_near() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::ElderGuardian, [20.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.aqua.curse_in = 3);
    let mut events = ctx.events.subscribe();
    run(&ctx, 6);
    let (amp, ticks) = effect_of(&ctx, "mining_fatigue").expect("cursed");
    assert_eq!(amp, 2);
    assert!(ticks > 5900, "{ticks}");
    // And its ghost fills the screen.
    let mut ghost = false;
    while let Some(e) = events.try_changed() {
        if let crate::context::Event::Direct {
            packet: crate::context::Direct::GameEvent(g),
            ..
        } = e
        {
            ghost |= g.event == mc_protocol::packets::play::game_event::ELDER_GUARDIAN_APPEARANCE;
        }
    }
    assert!(ghost);
}

#[test]
fn an_elder_guardian_does_not_curse_players_far_away() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::ElderGuardian, [70.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.aqua.curse_in = 3);
    run(&ctx, 6);
    assert!(effect_of(&ctx, "mining_fatigue").is_none());
}

#[test]
fn a_frog_eats_a_small_magma_cube_and_leaves_a_froglight() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [30.5, -60.0, 30.5]);
    let frog = put(&ctx, Species::Frog, [0.5, -60.0, 0.5]);
    let cwarm = crate::mobs::meta::variant_id(&ctx, Species::Frog, "warm").unwrap();
    set_mob(&ctx, frog, |m| m.variant = cold_or(m.variant, cwarm));
    let cube = put(&ctx, Species::MagmaCube, [3.5, -60.0, 0.5]);
    set_mob(&ctx, cube, |m| {
        m.set_size(1);
        m.no_ai = true;
    });
    run(&ctx, 200);
    assert!(mob_of(&ctx, cube).is_none_or(|e| e.removed), "still there");
    assert_eq!(items_of(&ctx, "minecraft:pearlescent_froglight"), 1);
}

fn cold_or(_: i32, v: i32) -> i32 {
    v
}

#[test]
fn a_frog_eats_a_slime_and_it_leaves_a_slimeball() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [30.5, -60.0, 30.5]);
    put(&ctx, Species::Frog, [0.5, -60.0, 0.5]);
    let slime = put(&ctx, Species::Slime, [3.5, -60.0, 0.5]);
    set_mob(&ctx, slime, |m| {
        m.set_size(1);
        m.no_ai = true;
    });
    run(&ctx, 200);
    assert_eq!(items_of(&ctx, "minecraft:slime_ball"), 1);
}

#[test]
fn a_dolphin_fed_a_fish_is_marked_and_a_baby_just_grows() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Dolphin, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:cod", 3);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.quirks.aqua.got_fish));
    let baby = put(&ctx, Species::Dolphin, [2.5, -60.0, 2.5]);
    set_mob(&ctx, baby, |m| m.baby_age = -1000);
    ctx.mobs.use_on(ctx.dim, baby, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    let age = with_mob(&ctx, baby, |m| m.baby_age);
    assert!(age > -950 && age < -850, "{age}");
    assert!(!with_mob(&ctx, baby, |m| m.quirks.aqua.got_fish));
    // Dolphins do not breed.
    assert!(!with_mob(&ctx, id, |m| m.in_love()));
}

#[test]
fn a_nautilus_is_tamed_with_pufferfish_and_healed_with_fish() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Nautilus, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    hold(&ctx, "minecraft:pufferfish", 64);
    for _ in 0..60 {
        ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
        run(&ctx, 1);
        if with_mob(&ctx, id, |m| m.is_tame()) {
            break;
        }
    }
    assert_eq!(with_mob(&ctx, id, |m| m.owner), Some(STEVE));
    set_mob(&ctx, id, |m| m.health = 5.0);
    hold(&ctx, "minecraft:cooked_cod", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    // Cooked cod gives 5 of food: ten points.
    assert_eq!(with_mob(&ctx, id, |m| m.health), 15.0);
}

#[test]
fn a_tame_nautilus_takes_a_saddle_and_armor_and_carries_a_rider() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Nautilus, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    let by = || steve([0.5, -60.0, 0.5]);
    // Wild, it takes no saddle.
    hold(&ctx, "minecraft:saddle", 1);
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 1);
    assert!(!with_mob(&ctx, id, |m| m.saddled()));
    set_mob(&ctx, id, |m| m.owner = Some(STEVE));
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.saddled()));
    hold(&ctx, "minecraft:iron_nautilus_armor", 1);
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.gear[mc_entity::mob::slot::BODY].is_some()));
    // Empty-handed it is mounted, and its rider keeps their breath.
    hold(&ctx, "minecraft:stick", 1);
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 25);
    assert!(with_mob(&ctx, id, |m| m.riders == vec![7000]));
    assert!(effect_of(&ctx, "breath_of_the_nautilus").is_some());
    // Shears take the armor first, then the saddle (and the rider with it).
    hold(&ctx, "minecraft:shears", 1);
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.gear[mc_entity::mob::slot::BODY]
        .is_none()
        && m.saddled()));
    ctx.mobs.use_on(ctx.dim, id, by(), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| !m.saddled() && m.riders.is_empty()));
}

#[test]
fn a_golden_dandelion_holds_a_baby_and_a_second_one_lets_it_grow() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Turtle, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| {
        m.no_ai = true;
        m.set_baby(true);
        m.baby_age = -100;
    });
    hold(&ctx, "minecraft:golden_dandelion", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 50);
    let (locked, age, kept) = set_mob(&ctx, id, |m| {
        (m.quirks.aqua.age_locked, m.baby_age, m.persistent)
    });
    assert!(locked && kept);
    assert_eq!(age, -24000, "the timer starts over and does not run");
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 10);
    assert!(!set_mob(&ctx, id, |m| m.quirks.aqua.age_locked));
    assert!(set_mob(&ctx, id, |m| m.baby_age) > -24000);
}

#[test]
fn an_axolotl_playing_dead_is_not_there_for_hunters() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut e = new_mob(&ctx, Species::Axolotl, [0.5, -60.0, 0.5]);
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.aqua.dead_play = 100;
    }
    let mut all = [e];
    assert!(!super::super::others(&all)[0].alive);
    if let Kind::Mob(m) = &mut all[0].kind {
        m.quirks.aqua.dead_play = 0;
    }
    assert!(super::super::others(&all)[0].alive);
}

#[test]
fn a_drowned_with_a_trident_may_ride_a_zombie_nautilus() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let trident = ctx.items.id("minecraft:trident").unwrap();
    let dressed = |ctx: &ServerContext| {
        let mut e = new_mob(ctx, Species::Drowned, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut e.kind {
            m.set_baby(false);
            m.gear[mc_entity::mob::slot::MAINHAND] = Some(mc_entity::mob::Gear {
                item: trident,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
        }
        e
    };
    // Bare-handed ones never do, and not in rivers.
    let mut bare = new_mob(&ctx, Species::Drowned, [0.5, -60.0, 0.5]);
    if let Kind::Mob(m) = &mut bare.kind {
        m.set_baby(false);
        m.gear[mc_entity::mob::slot::MAINHAND] = None;
    }
    assert!((0..40).all(|_| crate::mobs::spawn::jockey(&ctx, &mut bare, None).is_none()));
    let mut river = dressed(&ctx);
    assert!(
        (0..40).all(
            |_| crate::mobs::spawn::jockey(&ctx, &mut river, Some("minecraft:river")).is_none()
        )
    );
    // With a trident it happens about one time in two.
    let mut found = None;
    for _ in 0..60 {
        let mut rider = dressed(&ctx);
        if let Some(mount) = crate::mobs::spawn::jockey(&ctx, &mut rider, None) {
            found = Some((rider, mount));
            break;
        }
    }
    let (rider, mount) = found.expect("a jockey in sixty tries");
    let (rider_id, mount_id) = (rider.id, mount.id);
    let mut all = vec![rider, mount];
    all[1].pos = [10.5, -58.0, 3.5];
    crate::mobs::aqua::carry(&ctx, &mut all);
    assert_eq!(all[0].pos[0], 10.5);
    assert!(all[0].pos[1] > -58.0);
    let Kind::Mob(m) = &all[1].kind else { panic!() };
    assert_eq!(m.species, Species::ZombieNautilus);
    assert_eq!(m.riders, vec![rider_id]);
    // When the mount goes, the rider falls off.
    all[1].removed = true;
    crate::mobs::aqua::carry(&ctx, &mut all);
    let Kind::Mob(r) = &all[0].kind else { panic!() };
    assert_eq!(r.quirks.aqua.mount, None);
    let _ = mount_id;
}

#[test]
fn an_axolotl_is_bred_with_a_bucket_of_tropical_fish_and_the_bucket_comes_back() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Axolotl, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    hold(&ctx, "minecraft:tropical_fish_bucket", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.in_love()));
    let inv = lock(&ctx.inventories);
    let water = inv[&STEVE]
        .slots
        .iter()
        .flatten()
        .any(|s| ctx.items.name(s.item) == Some("minecraft:water_bucket"));
    assert!(water, "no empty bucket back");
}

#[test]
fn killing_what_axolotls_fight_rewards_the_player() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let zombie = put(&ctx, Species::Zombie, [2.5, -60.0, 0.5]);
    set_mob(&ctx, zombie, |m| m.no_ai = true);
    for x in [4.5, 5.5] {
        let axolotl = put(&ctx, Species::Axolotl, [x, -60.0, 0.5]);
        set_mob(&ctx, axolotl, |m| {
            m.no_ai = true;
            m.target = Some(zombie);
        });
    }
    let (give, _) = (crate::effects::kind_of("mining_fatigue").unwrap(), ());
    crate::effects::give(
        &ctx,
        STEVE,
        give,
        crate::effects::Instance::new(0, 1000, true),
    );
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, zombie, steve(at), blow(100.0, at));
    run(&ctx, 2);
    assert!(effect_of(&ctx, "mining_fatigue").is_none());
    let (_, ticks) = effect_of(&ctx, "regeneration").expect("regenerating");
    assert!(ticks > 150 && ticks <= 200, "{ticks}");
}

#[test]
fn two_turtles_in_love_leave_an_egg_inside_one_and_no_baby() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let a = put(&ctx, Species::Turtle, [0.5, -60.0, 0.5]);
    let b = put(&ctx, Species::Turtle, [1.5, -60.0, 0.5]);
    for id in [a, b] {
        set_mob(&ctx, id, |m| m.fall_in_love(Some(STEVE)));
    }
    run(&ctx, 200);
    let eggs: Vec<_> = [a, b]
        .iter()
        .map(|id| with_mob(&ctx, *id, |m| m.quirks.aqua.has_egg))
        .collect();
    assert_eq!(eggs.iter().filter(|e| **e).count(), 1, "{eggs:?}");
    let babies = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Turtle && m.is_baby()))
        .count();
    assert_eq!(babies, 0);
}

#[test]
fn a_turtle_lays_its_clutch_in_the_sand() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    fill(&ctx, [-3, -61, -3], [3, -61, 3], "minecraft:sand");
    let id = put(&ctx, Species::Turtle, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| {
        m.quirks.aqua.has_egg = true;
        m.quirks.aqua.home = Some([0, -60, 0]);
    });
    run(&ctx, 400);
    assert!(!with_mob(&ctx, id, |m| m.quirks.aqua.has_egg));
    let mut eggs = 0;
    for x in -3..=3 {
        for z in -3..=3 {
            if block(&ctx, [x, -60, z]) == "minecraft:turtle_egg" {
                eggs += 1;
            }
        }
    }
    assert_eq!(eggs, 1, "no nest");
}

/// A turtle egg cracks on random ticks at night and hatches at the third
/// crack, the whole clutch at once.
#[test]
fn turtle_eggs_crack_and_hatch_into_baby_turtles_at_dusk() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    fill(&ctx, [0, -61, 0], [0, -61, 0], "minecraft:sand");
    let egg = ctx
        .data
        .blocks
        .state_id("minecraft:turtle_egg", &[("eggs", "3"), ("hatch", "0")])
        .unwrap();
    let p = mc_protocol::Position::new(0, -60, 0);
    ctx.world.set_block(p, egg, &ctx.data.blocks, &ctx.rules);
    // The night comes round to the hour eggs crack in.
    for c in &mut lock(&ctx.time).clocks {
        c.ticks = 21_800;
    }
    let tick = |ctx: &ServerContext| {
        let mut level = WorldLevel::new(ctx);
        let logic = level.logic();
        let at = mc_tick::dir::BlockPos::new(0, -60, 0);
        let state = mc_tick::Level::block(&level, at);
        let mut rng = mc_tick::rng::Rng::new(5);
        mc_tick::random::tick(&mut level, logic, at, state, &mut rng);
    };
    let hatch = |ctx: &ServerContext| {
        ctx.world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.property(s, "hatch").map(str::to_owned))
    };
    tick(&ctx);
    assert_eq!(hatch(&ctx).as_deref(), Some("1"));
    tick(&ctx);
    assert_eq!(hatch(&ctx).as_deref(), Some("2"));
    let before = lock(&ctx.entities).len();
    tick(&ctx);
    assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:air");
    let all = lock(&ctx.entities);
    let babies: Vec<_> = all
        .iter()
        .skip(before)
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if m.species == Species::Turtle && m.is_baby() => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(babies.len(), 3);
    assert!(
        babies
            .iter()
            .all(|m| m.quirks.aqua.home == Some([0, -60, 0]))
    );
}

#[test]
fn turtle_eggs_rest_by_day_and_off_the_sand() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    // Noon, grass underfoot: nothing happens in a thousand ticks.
    let egg = ctx
        .data
        .blocks
        .state_id("minecraft:turtle_egg", &[("eggs", "1"), ("hatch", "2")])
        .unwrap();
    let p = mc_protocol::Position::new(0, -60, 0);
    ctx.world.set_block(p, egg, &ctx.data.blocks, &ctx.rules);
    for c in &mut lock(&ctx.time).clocks {
        c.ticks = 6000;
    }
    let mut level = WorldLevel::new(&ctx);
    let logic = level.logic();
    let at = mc_tick::dir::BlockPos::new(0, -60, 0);
    let mut rng = mc_tick::rng::Rng::new(5);
    for _ in 0..200 {
        let state = mc_tick::Level::block(&level, at);
        mc_tick::random::tick(&mut level, logic, at, state, &mut rng);
    }
    drop(level);
    assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:turtle_egg");
}

/// Mobs break turtle eggs one at a time by standing on them; turtles do not.
#[test]
fn walkers_trample_turtle_eggs_one_by_one_but_turtles_do_not() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let egg = ctx
        .data
        .blocks
        .state_id("minecraft:turtle_egg", &[("eggs", "2"), ("hatch", "0")])
        .unwrap();
    let p = mc_protocol::Position::new(0, -60, 0);
    ctx.world.set_block(p, egg, &ctx.data.blocks, &ctx.rules);
    let eggs = |ctx: &ServerContext| {
        ctx.world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.property(s, "eggs").map(str::to_owned))
    };
    let mut level = WorldLevel::new(&ctx);
    let turtle = Mob::new(Species::Turtle, 1);
    for _ in 0..2000 {
        crate::mobs::aqua::trample_eggs(&ctx, &mut level, &turtle, [0.5, -60.0, 0.5], None);
    }
    drop(level);
    assert_eq!(eggs(&ctx).as_deref(), Some("2"));
    let cow = Mob::new(Species::Cow, 1);
    let mut level = WorldLevel::new(&ctx);
    for _ in 0..3000 {
        crate::mobs::aqua::trample_eggs(&ctx, &mut level, &cow, [0.5, -60.0, 0.5], None);
    }
    drop(level);
    assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:air");
}

#[test]
fn frogspawn_hatches_into_two_to_five_tadpoles() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let spawn = ctx
        .data
        .blocks
        .default_state("minecraft:frogspawn")
        .unwrap();
    let at = mc_tick::dir::BlockPos::new(0, -60, 0);
    let before = lock(&ctx.entities).len();
    {
        let mut level = WorldLevel::new(&ctx);
        let logic = level.logic();
        mc_tick::set_block(&mut level, logic, at, spawn, mc_tick::Flags::ALL);
        let kind = ctx.shapes.block_index(spawn);
        assert!(mc_tick::Level::is_block_tick_scheduled(&level, at, kind));
        mc_tick::updates::block_tick(&mut level, logic, at, kind);
    }
    assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:air");
    let all = lock(&ctx.entities);
    let tadpoles = all
        .iter()
        .skip(before)
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Tadpole))
        .count();
    assert!((2..=5).contains(&tadpoles), "{tadpoles}");
}

#[test]
fn a_tadpole_that_has_grown_is_a_frog_where_it_stands() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let id = put(&ctx, Species::Tadpole, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| {
        m.quirks.aqua.growth = mc_entity::mob::aqua::TADPOLE_GROWTH - 2
    });
    run(&ctx, 5);
    assert!(mob_of(&ctx, id).is_none_or(|e| e.removed));
    let frogs = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Frog && !m.is_baby()))
        .count();
    assert_eq!(frogs, 1);
}

#[test]
fn fish_squids_and_axolotls_drop_what_the_wiki_says() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    let at = [0.5, -60.0, 0.5];
    for (species, item) in [
        (Species::Cod, "minecraft:cod"),
        (Species::Salmon, "minecraft:salmon"),
        (Species::Pufferfish, "minecraft:pufferfish"),
        (Species::TropicalFish, "minecraft:tropical_fish"),
        (Species::Squid, "minecraft:ink_sac"),
        (Species::GlowSquid, "minecraft:glow_ink_sac"),
    ] {
        let id = put(&ctx, species, [2.5, -60.0, 0.5]);
        set_mob(&ctx, id, |m| m.no_ai = true);
        ctx.mobs.hit(ctx.dim, id, steve(at), blow(100.0, at));
        run(&ctx, 1);
        assert!(items_of(&ctx, item) >= 1, "{species:?} dropped no {item}");
    }
    // An axolotl gives nothing, a guardian shards.
    let id = put(&ctx, Species::Axolotl, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    let items_before = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::Item { .. }))
        .count();
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(100.0, at));
    run(&ctx, 1);
    let items_after = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::Item { .. }))
        .count();
    assert_eq!(items_before, items_after);
}

#[test]
fn a_baby_turtle_that_grows_up_drops_a_scute_in_the_world() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let id = put(&ctx, Species::Turtle, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| {
        m.set_baby(true);
        m.baby_age = -3;
        m.no_ai = true;
    });
    run(&ctx, 6);
    assert_eq!(items_of(&ctx, "minecraft:turtle_scute"), 1);
}

#[test]
fn a_hurt_squid_squirts_ink_into_the_world_without_trouble() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    join(&ctx, [0.5, -60.0, 0.5]);
    fill(&ctx, [-6, -60, -6], [6, -50, 6], "minecraft:water");
    let id = put(&ctx, Species::Squid, [3.5, -56.0, 0.5]);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    run(&ctx, 20);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 9.0);
    assert!(!with_mob(&ctx, id, |m| m.quirks.aqua.ink));
}

#[test]
fn a_cod_in_a_pool_swims_about_in_it_for_a_minute() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    fill(&ctx, [-6, -60, -6], [6, -52, 6], "minecraft:water");
    let id = put(&ctx, Species::Cod, [0.5, -56.0, 0.5]);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1200 {
        run(&ctx, 1);
        let p = mob_of(&ctx, id).unwrap().pos;
        assert!(
            (-6.0..=7.0).contains(&p[0]) && (-60.0..=-51.0).contains(&p[1]),
            "{p:?}"
        );
        seen.insert((p[0].floor() as i32, p[2].floor() as i32));
    }
    assert!(seen.len() > 4, "it hardly moved: {seen:?}");
    assert_eq!(with_mob(&ctx, id, |m| m.health), 3.0);
}
