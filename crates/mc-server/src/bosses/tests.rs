use std::sync::Arc;

use mc_entity::boss::{Part, Phase};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::Tag;
use mc_protocol::Position;

use super::*;
use crate::context::{Event, Pose};
use crate::level::WorldLevel;

const STEVE: u128 = 0x5ee;

/// The flat test world's End, `None` without the data.
fn end() -> Option<Arc<ServerContext>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
    let end = ctx.in_dimension("minecraft:the_end")?;
    for cx in -3..=3 {
        for cz in -3..=3 {
            let _ = end
                .world
                .block(Position::new(cx * 16, 0, cz * 16), &end.data.blocks);
        }
    }
    Some(end)
}

fn overworld() -> Option<Arc<ServerContext>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
    for cx in -2..=2 {
        for cz in -2..=2 {
            let _ = ctx
                .world
                .block(Position::new(cx * 16, 0, cz * 16), &ctx.data.blocks);
        }
    }
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    Some(ctx)
}

fn pose(p: [f64; 3]) -> Pose {
    Pose {
        x: p[0],
        y: p[1],
        z: p[2],
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    }
}

/// A survival player at `pos` in the context's dimension.
fn join(ctx: &ServerContext, pos: [f64; 3]) -> u128 {
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
            pose: Some(pose(pos)),
            dimension: ctx.world.dimension.clone(),
            main_hand: None,
            flying: false,
            address: None,
            extra: Default::default(),
        },
    );
    lock(&ctx.vitals).insert(STEVE, crate::vitals::Vitals::default());
    STEVE
}

fn actor(pos: [f64; 3]) -> crate::interact::Actor {
    crate::interact::Actor {
        uuid: STEVE,
        entity_id: 7000,
        game_mode: 0,
        pose: pose(pos),
        sneaking: false,
    }
}

fn put(ctx: &ServerContext, p: [i32; 3], block: &str) {
    let state = ctx.data.blocks.default_state(block).unwrap();
    WorldLevel::new(ctx).set_block(Position::new(p[0], p[1], p[2]), state);
}

fn name(ctx: &ServerContext, p: [i32; 3]) -> String {
    let b = &ctx.data.blocks;
    ctx.world
        .block(Position::new(p[0], p[1], p[2]), b)
        .and_then(|s| b.block_name(s))
        .unwrap_or("")
        .to_owned()
}

/// Runs the world for `n` ticks.
fn run(ctx: &ServerContext, n: usize) {
    for _ in 0..n {
        lock(&ctx.time).age += 1;
        crate::tick::run_scheduled_ticks(ctx);
    }
}

fn crystals(ctx: &ServerContext) -> Vec<Entity> {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::Crystal(_)) && !e.removed)
        .cloned()
        .collect()
}

fn of_species(ctx: &ServerContext, s: Species) -> Vec<Entity> {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == s) && !e.removed)
        .cloned()
        .collect()
}

fn give(ctx: &ServerContext, item: &str, count: i32) {
    let id = ctx.items.id(item).unwrap();
    let mut inv = lock(&ctx.inventories);
    let hands = inv.entry(STEVE).or_default();
    let slot = hands.hand_slot(0);
    hands.slots[slot] = Some(mc_protocol::packets::play::ItemStack::new(id, count));
}

#[test]
fn crystals_stand_on_obsidian_or_bedrock_only() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [5.5, -60.0, 5.5]);
    give(&ctx, "minecraft:end_crystal", 3);
    put(&ctx, [0, -61, 0], "minecraft:obsidian");
    let mut out = crate::interact::Outcome::default();
    // On grass: nothing.
    assert!(crystal::place(
        &ctx,
        &mut out,
        actor([5.5, -60.0, 5.5]),
        0,
        "minecraft:end_crystal",
        Position::new(2, -61, 2)
    ));
    assert!(crystals(&ctx).is_empty());
    assert!(crystal::place(
        &ctx,
        &mut out,
        actor([5.5, -60.0, 5.5]),
        0,
        "minecraft:end_crystal",
        Position::new(0, -61, 0)
    ));
    let placed = crystals(&ctx);
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].pos, [0.5, -60.0, 0.5]);
    let Kind::Crystal(c) = &placed[0].kind else {
        unreachable!()
    };
    assert!(!c.show_bottom, "a placed crystal hides its base");
    // Not twice in the same spot.
    assert!(crystal::place(
        &ctx,
        &mut out,
        actor([5.5, -60.0, 5.5]),
        0,
        "minecraft:end_crystal",
        Position::new(0, -61, 0)
    ));
    assert_eq!(crystals(&ctx).len(), 1);
    // One crystal used.
    let left = lock(&ctx.inventories)[&STEVE].held(0).map(|s| s.count);
    assert_eq!(left, Some(2));
}

#[test]
fn a_hit_crystal_explodes_and_explosions_do_not_chain() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let a = crystal::new(&ctx, [0.5, -60.0, 0.5], true, None, false);
    let b = crystal::new(&ctx, [3.5, -60.0, 0.5], true, None, false);
    let (a_id, b_id) = (a.id, b.id);
    lock(&ctx.entities).extend([a, b]);
    {
        let mut entities = lock(&ctx.entities);
        let e = entities.iter_mut().find(|e| e.id == a_id).unwrap();
        let Kind::Crystal(c) = &mut e.kind else {
            unreachable!()
        };
        crystal::strike(c, Some(STEVE), "minecraft:player_attack");
    }
    let mut events = ctx.events.subscribe();
    run(&ctx, 1);
    let mut blasts = 0;
    while let Some(ev) = events.try_changed() {
        if let Event::Explosion { radius, .. } = ev {
            assert_eq!(radius, crystal::POWER);
            blasts += 1;
        }
    }
    assert_eq!(blasts, 1);
    // The other was caught in it: it goes, without a blast of its own.
    let mut events = ctx.events.subscribe();
    run(&ctx, 1);
    assert!(crystals(&ctx).iter().all(|e| e.id != a_id && e.id != b_id));
    while let Some(ev) = events.try_changed() {
        assert!(
            !matches!(ev, Event::Explosion { .. }),
            "explosions do not chain"
        );
    }
    // The grass under the first is blown away.
    assert_ne!(name(&ctx, [0, -61, 0]), "minecraft:grass_block");
}

#[test]
fn invulnerable_crystals_and_fireworks_do_nothing() {
    let mut c = mc_entity::boss::Crystal {
        invulnerable: true,
        ..Default::default()
    };
    crystal::strike(&mut c, None, "minecraft:arrow");
    assert!(c.struck.is_none());
    c.invulnerable = false;
    crystal::strike(&mut c, None, "minecraft:fireworks");
    assert!(c.struck.is_none());
    crystal::strike(&mut c, None, "minecraft:explosion");
    assert_eq!(c.struck, Some((None, true)));
}

#[test]
fn crystal_metadata_and_nbt() {
    let mut c = mc_entity::boss::Crystal::default();
    let mut nbt = mc_nbt::Compound::new();
    nbt.insert("beam_target", Tag::IntArray(vec![0, 128, 0]));
    nbt.insert("ShowBottom", Tag::Byte(1));
    crystal::apply_nbt(&mut c, &nbt);
    assert_eq!(c.beam, Some([0, 128, 0]));
    assert!(c.show_bottom);
    let meta = crystal::metadata(&c);
    assert_eq!(meta.len(), 2);
    let mut out = mc_nbt::Compound::new();
    crystal::to_nbt(&c, &mut out);
    assert_eq!(
        out.get("beam_target"),
        Some(&Tag::IntArray(vec![0, 128, 0]))
    );
}

#[test]
fn spikes_carry_crystals_and_cages() {
    let spikes = mc_features::end::spikes_for_seed(12345);
    assert_eq!(spikes.len(), 10);
    assert_eq!(spikes.iter().filter(|s| s.guarded).count(), 2);
    let mut r = mc_features::rand::WorldgenRandom::legacy(1);
    let nbt = mc_features::end::crystal(&mut r, &spikes[0], false, None);
    assert_eq!(
        nbt.get("id"),
        Some(&Tag::String("minecraft:end_crystal".into()))
    );
    assert_eq!(nbt.get("ShowBottom"), Some(&Tag::Byte(1)));
    let Some(Tag::List(p)) = nbt.get("Pos") else {
        panic!()
    };
    assert_eq!(p[1], Tag::Double(f64::from(spikes[0].height + 1)));
}

/// The fight's dragon in the flat End, with a player near; the fight has
/// run its first tick (portal built, dragon made).
fn fight_with_dragon() -> Option<(Arc<ServerContext>, u128)> {
    let ctx = end()?;
    join(&ctx, [0.5, 60.0, 8.5]);
    run(&ctx, 1);
    let dragons = of_species(&ctx, Species::EnderDragon);
    assert_eq!(dragons.len(), 1, "the fight makes a dragon");
    Some((ctx, dragons[0].uuid))
}

#[test]
fn the_fight_builds_the_portal_and_a_circling_dragon() {
    let Some((ctx, uuid)) = fight_with_dragon() else {
        return;
    };
    let fight = lock(&ctx.bosses)
        .fight
        .as_ref()
        .map(|f| (f.dragon, f.exit_portal, f.dragon_killed));
    let (dragon, portal, killed) = fight.unwrap();
    assert_eq!(dragon, Some(uuid));
    assert!(!killed);
    let o = portal.unwrap();
    // Bedrock pillar and ring, no portal yet.
    assert_eq!(name(&ctx, [o[0], o[1] + 3, o[2]]), "minecraft:bedrock");
    assert_eq!(name(&ctx, [o[0] + 3, o[1], o[2]]), "minecraft:bedrock");
    assert_eq!(name(&ctx, [o[0] + 1, o[1], o[2]]), "minecraft:air");
    assert_eq!(
        name(&ctx, [o[0], o[1] + 2, o[2] - 1]),
        "minecraft:wall_torch"
    );
    // It circles and its parts follow.
    let before = of_species(&ctx, Species::EnderDragon)[0].pos;
    run(&ctx, 40);
    let d = of_species(&ctx, Species::EnderDragon).remove(0);
    assert_ne!(d.pos, before);
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    let dr = m.dragon().unwrap();
    assert!(matches!(
        dr.phase,
        Phase::HoldingPattern | Phase::StrafePlayer | Phase::LandingApproach
    ));
    assert_eq!(dr.nodes.len(), 24);
    assert_eq!(dr.part_base, d.id + 1);
    let head = dr.parts[Part::Head as usize];
    assert!(dist_sq(head, d.pos) > 25.0, "the head is out in front");
    // The boss bar for the player near.
    assert_eq!(bar_viewers(&ctx, uuid), vec![STEVE]);
}

#[test]
fn the_head_takes_full_damage_the_body_a_quarter() {
    let Some(ctx) = overworld() else {
        return;
    };
    let mut d = crate::mobs::new_mob(&ctx, Species::EnderDragon, [0.0, 0.0, 0.0]);
    let mut h = crate::mobs::hurt::Harm::new(20.0, "minecraft:player_attack");
    h.player = Some(STEVE);
    dragon::aim_at(&mut d, Part::Head);
    assert!(crate::mobs::hurt::harm(&ctx, &mut d, &h));
    let Kind::Mob(m) = &mut d.kind else {
        unreachable!()
    };
    assert_eq!(m.health, 180.0);
    m.invulnerable = 0;
    // Elsewhere: 20 / 4 + 1.
    assert!(crate::mobs::hurt::harm(&ctx, &mut d, &h));
    let Kind::Mob(m) = &mut d.kind else {
        unreachable!()
    };
    assert_eq!(m.health, 174.0);
    m.invulnerable = 0;
    // Mobs and lava do nothing; explosions do.
    let lava = crate::mobs::hurt::Harm::new(20.0, "minecraft:lava");
    assert!(!crate::mobs::hurt::harm(&ctx, &mut d, &lava));
    let boom = crate::mobs::hurt::Harm::new(8.0, "minecraft:explosion");
    assert!(crate::mobs::hurt::harm(&ctx, &mut d, &boom));
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    assert_eq!(m.health, 171.0);
}

#[test]
fn perched_it_shrugs_off_arrows_and_takes_off_after_a_quarter() {
    let Some(ctx) = overworld() else {
        return;
    };
    let mut d = crate::mobs::new_mob(&ctx, Species::EnderDragon, [0.0, 0.0, 0.0]);
    let Kind::Mob(m) = &mut d.kind else {
        unreachable!()
    };
    m.dragon_mut().unwrap().phase = Phase::SittingScanning;
    let mut arrow = crate::mobs::hurt::Harm::new(9.0, "minecraft:arrow");
    arrow.player = Some(STEVE);
    dragon::aim_at(&mut d, Part::Head);
    assert!(!crate::mobs::hurt::harm(&ctx, &mut d, &arrow));
    let mut blow = crate::mobs::hurt::Harm::new(30.0, "minecraft:player_attack");
    blow.player = Some(STEVE);
    for _ in 0..2 {
        dragon::aim_at(&mut d, Part::Head);
        crate::mobs::hurt::harm(&ctx, &mut d, &blow);
        if let Kind::Mob(m) = &mut d.kind {
            m.invulnerable = 0;
        }
    }
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    assert_eq!(m.health, 140.0);
    assert_eq!(m.dragon().unwrap().phase, Phase::TakingOff);
}

#[test]
fn a_fatal_blow_sends_it_to_die_and_the_fight_ends() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    let mut events = ctx.events.subscribe();
    {
        let mut entities = lock(&ctx.entities);
        let d = entities
            .iter_mut()
            .find(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::EnderDragon))
            .unwrap();
        let mut h = crate::mobs::hurt::Harm::new(500.0, "minecraft:player_attack");
        h.player = Some(STEVE);
        // Struck low over the island.
        d.pos = [20.0, 30.0, 0.0];
        dragon::aim_at(d, Part::Head);
        crate::mobs::hurt::harm(&ctx, d, &h);
        let Kind::Mob(m) = &d.kind else {
            unreachable!()
        };
        assert_eq!(m.health, 1.0);
        assert_eq!(m.dragon().unwrap().phase, Phase::Dying);
    }
    // It flies to the portal, dies there over 200 ticks.
    let mut xp = 0;
    let mut done = false;
    for _ in 0..1500 {
        run(&ctx, 1);
        while let Some(ev) = events.try_changed() {
            if let Event::EntitySpawned(e) = ev
                && let Kind::ExperienceOrb { value, count } = e.kind
            {
                xp += value * count;
            }
        }
        if of_species(&ctx, Species::EnderDragon).is_empty() {
            done = true;
            break;
        }
    }
    assert!(done, "the dragon died");
    assert_eq!(xp, dragon::FIRST_XP);
    let fight = lock(&ctx.bosses);
    let f = fight.fight.as_ref().unwrap();
    assert!(f.dragon_killed && f.previously_killed);
    assert_eq!(f.gateways.len(), 19);
    let o = f.exit_portal.unwrap();
    let next = f.gateways.clone();
    drop(fight);
    assert_eq!(name(&ctx, [o[0] + 1, o[1], o[2]]), "minecraft:end_portal");
    assert_eq!(name(&ctx, [o[0], o[1] + 4, o[2]]), "minecraft:dragon_egg");
    // The gateway that came is the one the shuffle left out.
    let all = fight::shuffled_gateways(0);
    let made = all.iter().find(|i| !next.contains(i)).copied().unwrap();
    let g = fight::gateway_pos(made);
    assert_eq!(name(&ctx, g), "minecraft:end_gateway");
}

#[test]
fn gateways_follow_the_ring() {
    let ring: Vec<[i32; 3]> = (0..20).map(fight::gateway_pos).collect();
    assert_eq!(ring[0], [96, 75, 0]);
    assert_eq!(ring[1], [91, 75, 29]);
    assert_eq!(ring[5], [-1, 75, 96]);
    assert_eq!(ring[19], [91, 75, -30]);
    let mut order = fight::shuffled_gateways(12345);
    assert_eq!(order, fight::shuffled_gateways(12345));
    order.sort_unstable();
    assert_eq!(order, (0..20).collect::<Vec<_>>());
}

#[test]
fn the_fight_is_saved_and_read_back() {
    let Some(ctx) = end() else {
        return;
    };
    let mut f = fight::Fight::load(&ctx);
    f.dragon_killed = true;
    f.previously_killed = true;
    f.needs_state_scanning = false;
    f.dragon = Some(0x1234_5678_9abc_def0_1122_3344_5566_7788);
    f.exit_portal = Some([0, 60, 0]);
    f.gateways = vec![3, 1, 4];
    f.respawn = Some(fight::Stage::SummoningPillars);
    f.respawn_time = 17;
    f.respawn_crystals = vec![7, 8];
    let saved = f.write();
    assert_eq!(
        saved.get("exit_portal_location"),
        Some(&Tag::IntArray(vec![0, 60, 0]))
    );
    assert_eq!(
        saved.get("respawn_stage"),
        Some(&Tag::String("summoning_pillars".into()))
    );
    let mut g = fight::Fight::load(&ctx);
    g.read(&saved, false);
    assert!(g.dragon_killed && g.previously_killed && !g.needs_state_scanning);
    assert_eq!(g.dragon, f.dragon);
    assert_eq!(g.exit_portal, f.exit_portal);
    assert_eq!(g.gateways, f.gateways);
    assert_eq!(g.respawn, f.respawn);
    assert_eq!(g.respawn_time, 17);
    assert_eq!(g.respawn_crystals, vec![7, 8]);
    // The old level.dat names.
    let mut old = mc_nbt::Compound::new();
    old.insert("DragonKilled", Tag::Byte(1));
    let mut pos = mc_nbt::Compound::new();
    pos.insert("X", Tag::Int(0));
    pos.insert("Y", Tag::Int(61));
    pos.insert("Z", Tag::Int(0));
    old.insert("ExitPortalLocation", Tag::Compound(pos));
    let mut h = fight::Fight::load(&ctx);
    h.read(&old, true);
    assert!(h.dragon_killed);
    assert_eq!(h.exit_portal, Some([0, 61, 0]));
}

/// A beaten fight: portal open, no dragon.
fn beaten() -> Option<(Arc<ServerContext>, [i32; 3])> {
    let ctx = end()?;
    join(&ctx, [0.5, 60.0, 8.5]);
    let o = [0, fight::surface(&ctx, 0, 0) - 1, 0];
    {
        let mut level = WorldLevel::new(&ctx);
        fight::build_podium(&ctx, &mut level, o, true);
    }
    let mut f = fight::Fight::load(&ctx);
    f.needs_state_scanning = false;
    f.dragon_killed = true;
    f.previously_killed = true;
    f.exit_portal = Some(o);
    lock(&ctx.bosses).fight = Some(f);
    Some((ctx, o))
}

#[test]
fn four_crystals_on_the_portal_bring_the_dragon_back() {
    let Some((ctx, o)) = beaten() else {
        return;
    };
    give(&ctx, "minecraft:end_crystal", 4);
    for (dx, dz) in [(0, -3), (3, 0), (0, 3), (-3, 0)] {
        let mut out = crate::interact::Outcome::default();
        crystal::place(
            &ctx,
            &mut out,
            actor([0.5, 60.0, 8.5]),
            0,
            "minecraft:end_crystal",
            Position::new(o[0] + dx, o[1], o[2] + dz),
        );
    }
    assert_eq!(name(&ctx, [o[0] + 3, o[1], o[2]]), "minecraft:bedrock");
    assert_eq!(name(&ctx, [o[0] + 3, o[1] + 1, o[2]]), "minecraft:air");
    assert_eq!(crystals(&ctx).len(), 4);
    run(&ctx, 2);
    let stage = lock(&ctx.bosses).fight.as_ref().unwrap().respawn;
    assert!(stage.is_some(), "the respawn started");
    // The portal is closed while it goes on.
    assert_eq!(name(&ctx, [o[0] + 1, o[1], o[2]]), "minecraft:air");
    let mut came = false;
    for _ in 0..800 {
        run(&ctx, 1);
        if !of_species(&ctx, Species::EnderDragon).is_empty() {
            came = true;
            break;
        }
    }
    assert!(came, "a dragon came back");
    let guard = lock(&ctx.bosses);
    let f = guard.fight.as_ref().unwrap();
    assert!(!f.dragon_killed && f.respawn.is_none());
    drop(guard);
    // The four are gone; the pillars have crystals again.
    let left = crystals(&ctx);
    assert_eq!(left.len(), 10);
    for c in &left {
        let Kind::Crystal(c) = &c.kind else {
            unreachable!()
        };
        assert!(!c.invulnerable && c.beam.is_none());
    }
}

#[test]
fn destroying_a_summoning_crystal_stops_the_respawn() {
    let Some((ctx, o)) = beaten() else {
        return;
    };
    give(&ctx, "minecraft:end_crystal", 4);
    for (dx, dz) in [(0, -3), (3, 0), (0, 3), (-3, 0)] {
        let mut out = crate::interact::Outcome::default();
        crystal::place(
            &ctx,
            &mut out,
            actor([0.5, 60.0, 8.5]),
            0,
            "minecraft:end_crystal",
            Position::new(o[0] + dx, o[1], o[2] + dz),
        );
    }
    run(&ctx, 5);
    assert!(lock(&ctx.bosses).fight.as_ref().unwrap().respawn.is_some());
    {
        let mut entities = lock(&ctx.entities);
        let c = entities
            .iter_mut()
            .find(|e| matches!(e.kind, Kind::Crystal(_)))
            .unwrap();
        let Kind::Crystal(c) = &mut c.kind else {
            unreachable!()
        };
        crystal::strike(c, Some(STEVE), "minecraft:arrow");
    }
    run(&ctx, 2);
    let guard = lock(&ctx.bosses);
    let f = guard.fight.as_ref().unwrap();
    assert!(f.respawn.is_none() && f.dragon_killed);
    drop(guard);
    // The portal is open again.
    assert_eq!(name(&ctx, [o[0] + 1, o[1], o[2]]), "minecraft:end_portal");
    assert!(of_species(&ctx, Species::EnderDragon).is_empty());
}

#[test]
fn a_gateway_links_to_the_outer_islands_and_back() {
    let Some(ctx) = end() else {
        return;
    };
    let g = fight::gateway_pos(0);
    for cx in (g[0] >> 4) - 1..=(g[0] >> 4) + 1 {
        for cz in (g[2] >> 4) - 1..=(g[2] >> 4) + 1 {
            let _ = ctx
                .world
                .block(Position::new(cx * 16, 0, cz * 16), &ctx.data.blocks);
        }
    }
    {
        let mut level = WorldLevel::new(&ctx);
        fight::build_gateway(&ctx, &mut level, g, None, false);
    }
    join(
        &ctx,
        [
            f64::from(g[0]) + 0.5,
            f64::from(g[1]),
            f64::from(g[2]) + 0.5,
        ],
    );
    let mut events = ctx.events.subscribe();
    run(&ctx, 1);
    let mut to = None;
    while let Some(ev) = events.try_changed() {
        if let Event::Teleport { uuid, pose } = ev {
            assert_eq!(uuid, STEVE);
            to = Some(pose);
        }
    }
    let to = to.expect("teleported");
    assert!(to.x.hypot(to.z) > 700.0, "out to the islands: {to:?}");
    let be = ctx
        .world
        .block_entity(Position::new(g[0], g[1], g[2]), &ctx.data.blocks)
        .unwrap();
    let Some(Tag::IntArray(exit)) = be.data.get("exit_portal") else {
        panic!("the gateway remembers its exit");
    };
    assert_eq!(
        name(&ctx, [exit[0], exit[1], exit[2]]),
        "minecraft:end_gateway"
    );
    let back = ctx
        .world
        .block_entity(Position::new(exit[0], exit[1], exit[2]), &ctx.data.blocks)
        .unwrap();
    assert_eq!(
        back.data.get("exit_portal"),
        Some(&Tag::IntArray(g.to_vec()))
    );
    assert_eq!(back.data.get("ExactTeleport"), Some(&Tag::Byte(1)));
}

#[test]
fn a_pearl_thrown_into_a_gateway_sends_its_thrower() {
    let Some(ctx) = end() else {
        return;
    };
    let g = fight::gateway_pos(1);
    for cx in (g[0] >> 4) - 1..=(g[0] >> 4) + 1 {
        for cz in (g[2] >> 4) - 1..=(g[2] >> 4) + 1 {
            let _ = ctx
                .world
                .block(Position::new(cx * 16, 0, cz * 16), &ctx.data.blocks);
        }
    }
    {
        let mut level = WorldLevel::new(&ctx);
        fight::build_gateway(&ctx, &mut level, g, None, false);
    }
    join(&ctx, [0.5, 80.0, 0.5]);
    let mut events = ctx.events.subscribe();
    crate::projectiles::spawn(
        &ctx,
        mc_entity::extra::Shot::Pearl,
        Some(STEVE),
        [
            f64::from(g[0]) + 0.5,
            f64::from(g[1]) + 0.5,
            f64::from(g[2]) + 1.2,
        ],
        [0.0, 0.0, -1.0],
    );
    run(&ctx, 1);
    let mut to = None;
    while let Some(ev) = events.try_changed() {
        if let Event::Teleport { uuid, pose } = ev {
            assert_eq!(uuid, STEVE);
            to = Some(pose);
        }
    }
    let to = to.expect("the pearl sends its thrower through");
    assert!(to.x.hypot(to.z) > 700.0, "out to the islands: {to:?}");
    assert!(
        !lock(&ctx.entities)
            .iter()
            .any(|e| matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, mc_entity::extra::Shot::Pearl)))
    );
    assert_eq!(lock(&ctx.vitals).get(&STEVE).unwrap().health, 20.0);
}

/// Builds the wither's T at x 0..2, y -60..-58 (stem foot at 1, -60, 0) but
/// the last skull.
fn wither_t(ctx: &ServerContext) {
    for x in 0..3 {
        put(ctx, [x, -59, 0], "minecraft:soul_sand");
    }
    put(ctx, [1, -60, 0], "minecraft:soul_soil");
    put(ctx, [0, -58, 0], "minecraft:wither_skeleton_skull");
    put(ctx, [1, -58, 0], "minecraft:wither_skeleton_skull");
}

#[test]
fn the_last_skull_of_the_t_summons_the_wither() {
    let Some(ctx) = overworld() else {
        return;
    };
    wither_t(&ctx);
    let mut events = Vec::new();
    // Not yet complete.
    let skull = ctx
        .data
        .blocks
        .default_state("minecraft:wither_skeleton_skull")
        .unwrap();
    wither::placed(&ctx, &mut events, Position::new(1, -58, 0), skull);
    assert!(of_species(&ctx, Species::Wither).is_empty());
    put(&ctx, [2, -58, 0], "minecraft:wither_skeleton_skull");
    wither::placed(&ctx, &mut events, Position::new(2, -58, 0), skull);
    let withers = of_species(&ctx, Species::Wither);
    assert_eq!(withers.len(), 1);
    assert_eq!(withers[0].pos, [1.5, -59.45, 0.5]);
    let Kind::Mob(m) = &withers[0].kind else {
        unreachable!()
    };
    assert_eq!(m.health, 100.0);
    assert_eq!(m.wither().unwrap().invulnerable, wither::SPAWN_TICKS);
    for x in 0..3 {
        assert_eq!(name(&ctx, [x, -58, 0]), "minecraft:air");
        assert_eq!(name(&ctx, [x, -59, 0]), "minecraft:air");
    }
    assert_eq!(name(&ctx, [1, -60, 0]), "minecraft:air");
}

#[test]
fn the_t_lying_down_works_and_peaceful_stops_it() {
    let Some(ctx) = overworld() else {
        return;
    };
    // Flat on the ground: skulls along z = 2, stem towards z = 0.
    for x in 0..3 {
        put(&ctx, [x, -60, 1], "minecraft:soul_sand");
        put(&ctx, [x, -60, 2], "minecraft:wither_skeleton_skull");
    }
    put(&ctx, [1, -60, 0], "minecraft:soul_sand");
    let skull = ctx
        .data
        .blocks
        .default_state("minecraft:wither_skeleton_skull")
        .unwrap();
    ctx.difficulty
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let mut events = Vec::new();
    wither::placed(&ctx, &mut events, Position::new(0, -60, 2), skull);
    assert!(
        of_species(&ctx, Species::Wither).is_empty(),
        "not on peaceful"
    );
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    wither::placed(&ctx, &mut events, Position::new(0, -60, 2), skull);
    assert_eq!(of_species(&ctx, Species::Wither).len(), 1);
}

#[test]
fn the_wither_grows_then_explodes() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [20.5, -60.0, 20.5]);
    wither_t(&ctx);
    put(&ctx, [2, -58, 0], "minecraft:wither_skeleton_skull");
    let skull = ctx
        .data
        .blocks
        .default_state("minecraft:wither_skeleton_skull")
        .unwrap();
    wither::placed(&ctx, &mut Vec::new(), Position::new(2, -58, 0), skull);
    run(&ctx, 100);
    let w = of_species(&ctx, Species::Wither).remove(0);
    let Kind::Mob(m) = &w.kind else {
        unreachable!()
    };
    assert!(m.wither().unwrap().invulnerable > 0);
    assert!(m.health > 100.0);
    // Can't be hurt yet.
    let mut e = w.clone();
    let mut h = crate::mobs::hurt::Harm::new(10.0, "minecraft:player_attack");
    h.player = Some(STEVE);
    assert!(!crate::mobs::hurt::harm(&ctx, &mut e, &h));
    let mut events = ctx.events.subscribe();
    let mut boom = false;
    for _ in 0..130 {
        run(&ctx, 1);
        while let Some(ev) = events.try_changed() {
            if let Event::Explosion { radius, .. } = ev
                && radius == 7.0
            {
                boom = true;
            }
        }
    }
    assert!(boom, "the spawn explosion");
    let w = of_species(&ctx, Species::Wither).remove(0);
    let Kind::Mob(m) = &w.kind else {
        unreachable!()
    };
    assert_eq!(m.wither().unwrap().invulnerable, 0);
}

#[test]
fn below_half_health_arrows_bounce_off_the_wither() {
    let Some(ctx) = overworld() else {
        return;
    };
    let mut w = crate::mobs::new_mob(&ctx, Species::Wither, [0.0, -60.0, 0.0]);
    let mut arrow = crate::mobs::hurt::Harm::new(6.0, "minecraft:arrow");
    arrow.player = Some(STEVE);
    assert!(crate::mobs::hurt::harm(&ctx, &mut w, &arrow));
    let Kind::Mob(m) = &mut w.kind else {
        unreachable!()
    };
    m.health = 140.0;
    m.invulnerable = 0;
    assert!(!crate::mobs::hurt::harm(&ctx, &mut w, &arrow));
    // Nor do skeletons hurt it, nor its own skulls.
    let mut bone = crate::mobs::hurt::Harm::new(6.0, "minecraft:mob_attack");
    bone.killer = Some("minecraft:skeleton".into());
    assert!(!crate::mobs::hurt::harm(&ctx, &mut w, &bone));
    let mut own = crate::mobs::hurt::Harm::new(6.0, "minecraft:explosion");
    own.killer = Some("minecraft:wither".into());
    assert!(!crate::mobs::hurt::harm(&ctx, &mut w, &own));
    // A sword does, and it will break blocks for it.
    let mut sword = crate::mobs::hurt::Harm::new(6.0, "minecraft:player_attack");
    sword.player = Some(STEVE);
    assert!(crate::mobs::hurt::harm(&ctx, &mut w, &sword));
    let Kind::Mob(m) = &w.kind else {
        unreachable!()
    };
    assert_eq!(m.wither().unwrap().destroy_blocks, 20);
}

#[test]
fn a_dead_wither_drops_a_nether_star() {
    let Some(ctx) = overworld() else {
        return;
    };
    let mut w = crate::mobs::new_mob(&ctx, Species::Wither, [0.5, -60.0, 0.5]);
    let mut h = crate::mobs::hurt::Harm::new(1000.0, "minecraft:player_attack");
    h.player = Some(STEVE);
    assert!(crate::mobs::hurt::harm(&ctx, &mut w, &h));
    let star = ctx.items.id("minecraft:nether_star").unwrap();
    let stars = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::Item { item, .. } if item == star))
        .count();
    assert_eq!(stars, 1);
}

#[test]
fn the_wither_hunts_and_shoots_skulls() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [8.5, -60.0, 0.5]);
    let mut w = crate::mobs::new_mob(&ctx, Species::Wither, [0.5, -55.0, 0.5]);
    if let Kind::Mob(m) = &mut w.kind {
        m.wither_mut().unwrap().invulnerable = 0;
    }
    let wid = w.id;
    lock(&ctx.entities).push(w);
    let mut skulls = std::collections::HashSet::new();
    let mut aimed: Option<(Option<i32>, i32)> = None;
    for _ in 0..200 {
        // Keep the player alive under the skulls.
        if let Some(v) = lock(&ctx.vitals).get_mut(&STEVE) {
            v.health = 20.0;
        }
        run(&ctx, 1);
        for e in lock(&ctx.entities).iter() {
            if let Kind::Projectile(p) = &e.kind
                && matches!(p.shot, mc_entity::extra::Shot::WitherSkull { .. })
            {
                assert_eq!(p.mob.map(|m| m.0), Some(wid));
                skulls.insert(e.id);
            }
        }
        // Whom it goes after (its skulls may kill the player later).
        let w = of_species(&ctx, Species::Wither).remove(0);
        let Kind::Mob(m) = &w.kind else {
            unreachable!()
        };
        if m.target == Some(7000) && m.wither().unwrap().heads[0] == 7000 {
            aimed = Some((m.target, 7000));
        }
    }
    assert!(skulls.len() >= 2, "skulls shot: {}", skulls.len());
    assert_eq!(aimed, Some((Some(7000), 7000)));
    let w = of_species(&ctx, Species::Wither).remove(0);
    // It shows its bar.
    assert_eq!(bar_viewers(&ctx, w.uuid), vec![STEVE]);
}

#[test]
fn blue_skulls_break_almost_anything() {
    let Some(ctx) = overworld() else {
        return;
    };
    // The weak table caps obsidian, not bedrock.
    put(&ctx, [0, -60, 0], "minecraft:obsidian");
    put(&ctx, [1, -60, 0], "minecraft:bedrock");
    let blast = crate::explosion::Blast {
        center: [0.5, -59.5, 0.5],
        power: 1.0,
        source: 0,
        owner: None,
        mob: true,
        dangerous: true,
        by: Some("minecraft:wither"),
        fire: false,
        respawn: None,
    };
    let mut level = WorldLevel::new(&ctx);
    let mut entities = Vec::new();
    for _ in 0..5 {
        crate::explosion::explode_all(
            &ctx,
            &mut level,
            &mut entities,
            std::slice::from_ref(&blast),
        );
    }
    drop(level);
    assert_eq!(name(&ctx, [0, -60, 0]), "minecraft:air");
    assert_eq!(name(&ctx, [1, -60, 0]), "minecraft:bedrock");
}

#[test]
fn a_dragon_fireball_leaves_breath() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    crate::projectiles::spawn_by_mob(
        &ctx,
        mc_entity::extra::Shot::DragonFireball,
        (99, "Ender Dragon"),
        [0.5, -55.0, 0.5],
        [0.0, -0.1, 0.0],
    );
    run(&ctx, 40);
    let clouds: Vec<_> = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, mc_entity::extra::Shot::Breath { .. })))
        .cloned()
        .collect();
    assert_eq!(clouds.len(), 1);
    let Kind::Projectile(p) = &clouds[0].kind else {
        unreachable!()
    };
    let mc_entity::extra::Shot::Breath {
        radius, amplifier, ..
    } = &p.shot
    else {
        unreachable!()
    };
    assert!(*radius >= 3.0);
    assert_eq!(*amplifier, 1);
}

#[test]
fn a_bottle_scoops_dragon_breath_and_the_cloud_shrinks() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    give(&ctx, "minecraft:glass_bottle", 1);
    let cloud = projectile::breath(&ctx, 99, [0.5, -60.0, 0.5], 3.0, 0.0, 600, 0);
    let id = cloud.id;
    lock(&ctx.entities).push(cloud);
    let out = crate::hits::use_entity(&ctx, actor([0.5, -60.0, 0.5]), id, 0, false, None, None);
    assert!(
        out.events
            .iter()
            .any(|e| matches!(e, Event::InventorySlot { .. })),
        "the bottle is replaced"
    );
    let held = lock(&ctx.inventories)[&STEVE].slots[36].clone();
    let item = held.map(|s| ctx.items.name(s.item).unwrap_or("").to_owned());
    assert_eq!(item.as_deref(), Some("minecraft:dragon_breath"));
    let entities = lock(&ctx.entities);
    let e = entities.iter().find(|e| e.id == id).unwrap();
    let Kind::Projectile(p) = &e.kind else {
        unreachable!()
    };
    let mc_entity::extra::Shot::Breath { radius, .. } = &p.shot else {
        unreachable!()
    };
    assert!((*radius - 2.5).abs() < 1e-4, "half a block less, {radius}");
}

#[test]
fn the_dragon_egg_falls_when_nothing_is_under_it() {
    let Some(ctx) = overworld() else {
        return;
    };
    let under = [0, -60, 0];
    let at = [0, -59, 0];
    put(&ctx, under, "minecraft:end_stone");
    put(&ctx, at, "minecraft:dragon_egg");
    put(&ctx, under, "minecraft:air");
    lock(&ctx.time).age += 2;
    run(&ctx, 1);
    assert_eq!(name(&ctx, at), "minecraft:air");
    let falling = lock(&ctx.entities).iter().any(|e| {
        matches!(
            &e.kind,
            Kind::FallingBlock { state } if ctx.data.blocks.block_name(*state) == Some("minecraft:dragon_egg")
        )
    });
    assert!(falling, "the egg becomes a falling block");
}

#[test]
fn breath_hurts_who_stands_in_it() {
    let Some(ctx) = overworld() else {
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let cloud = projectile::breath(&ctx, 99, [0.5, -60.0, 0.5], 3.0, 0.0, 600, 1);
    lock(&ctx.entities).push(cloud);
    let before = lock(&ctx.vitals)[&STEVE].health;
    run(&ctx, 30);
    let after = lock(&ctx.vitals)[&STEVE].health;
    assert_eq!(before - after, 6.0, "harming II at half strength");
}

fn with_dragon(ctx: &ServerContext, f: impl FnOnce(&mut Entity)) {
    let mut entities = lock(&ctx.entities);
    let d = entities
        .iter_mut()
        .find(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::EnderDragon))
        .unwrap();
    f(d);
}

fn phase_of(ctx: &ServerContext) -> Phase {
    let d = of_species(ctx, Species::EnderDragon).remove(0);
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    m.dragon().unwrap().phase
}

#[test]
fn it_lands_roars_and_breathes_on_a_player_near_the_portal() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    let top = fight::surface(&ctx, 0, 0);
    join(&ctx, [6.5, f64::from(top), 0.5]);
    with_dragon(&ctx, |d| {
        d.pos = [0.5, f64::from(top) + 12.0, 0.5];
        let Kind::Mob(m) = &mut d.kind else {
            unreachable!()
        };
        m.dragon_mut().unwrap().phase = Phase::Landing;
    });
    let mut seen = Vec::new();
    let mut cloud = false;
    for _ in 0..1200 {
        run(&ctx, 1);
        let p = phase_of(&ctx);
        if seen.last() != Some(&p) {
            seen.push(p);
        }
        cloud |= lock(&ctx.entities).iter().any(|e| {
            matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, mc_entity::extra::Shot::Breath { radius, .. } if radius == 5.0))
        });
        if cloud && p == Phase::SittingFlaming {
            break;
        }
    }
    assert!(cloud, "breath on the portal: {seen:?}");
    assert_eq!(
        &seen[..4],
        &[
            Phase::Landing,
            Phase::SittingScanning,
            Phase::SittingAttacking,
            Phase::SittingFlaming
        ],
    );
}

#[test]
fn perched_it_does_not_shove_someone_standing_under_it() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    let top = fight::surface(&ctx, 0, 0);
    let feet = [0.5, f64::from(top), 0.5];
    lock(&ctx.players).get_mut(&STEVE).unwrap().pose = Some(pose(feet));
    with_dragon(&ctx, |d| {
        d.pos = feet;
        let Kind::Mob(m) = &mut d.kind else {
            unreachable!()
        };
        let dragon = m.dragon_mut().unwrap();
        dragon.phase = Phase::SittingScanning;
        dragon.since_hurt = 100;
    });
    let mut events = ctx.events.subscribe();
    let before = lock(&ctx.vitals)[&STEVE].health;
    run(&ctx, 5);
    let mut shoved = false;
    while let Some(ev) = events.try_changed() {
        if let Event::Direct {
            packet: crate::context::Direct::Motion(m),
            ..
        } = ev
            && (m.velocity[0].abs() > 0.5 || m.velocity[2].abs() > 0.5)
        {
            shoved = true;
        }
    }
    assert!(!shoved, "the fountain under a perched dragon stays open");
    assert_eq!(lock(&ctx.vitals)[&STEVE].health, before);
}

#[test]
fn with_nobody_near_the_portal_it_charges() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    let top = fight::surface(&ctx, 0, 0);
    with_dragon(&ctx, |d| {
        d.pos = [0.5, f64::from(top), 0.5];
        let Kind::Mob(m) = &mut d.kind else {
            unreachable!()
        };
        m.dragon_mut().unwrap().phase = Phase::SittingScanning;
    });
    run(&ctx, 101);
    assert_eq!(phase_of(&ctx), Phase::ChargingPlayer);
}

#[test]
fn crystals_heal_it_and_the_healing_one_hurts_when_it_goes() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    let c = crystal::new(&ctx, [0.5, 120.0, 10.5], true, None, false);
    let cid = c.id;
    lock(&ctx.entities).push(c);
    with_dragon(&ctx, |d| {
        let Kind::Mob(m) = &mut d.kind else {
            unreachable!()
        };
        m.health = 150.0;
        m.dragon_mut().unwrap().phase = Phase::Hovering;
        d.pos = [0.5, 120.0, 0.5];
    });
    run(&ctx, 200);
    let d = of_species(&ctx, Species::EnderDragon).remove(0);
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    assert!(m.health > 150.0, "healed to {}", m.health);
    assert_eq!(m.dragon().unwrap().crystal, Some(cid));
    let before = m.health;
    {
        let mut entities = lock(&ctx.entities);
        let e = entities.iter_mut().find(|e| e.id == cid).unwrap();
        let Kind::Crystal(c) = &mut e.kind else {
            unreachable!()
        };
        crystal::strike(c, Some(STEVE), "minecraft:arrow");
    }
    run(&ctx, 1);
    let d = of_species(&ctx, Species::EnderDragon).remove(0);
    let Kind::Mob(m) = &d.kind else {
        unreachable!()
    };
    assert!(m.health <= before - 9.0, "{} after {}", m.health, before);
}

#[test]
fn a_dragon_killed_again_gives_500_and_no_egg() {
    let Some((ctx, _)) = fight_with_dragon() else {
        return;
    };
    lock(&ctx.bosses).fight.as_mut().unwrap().previously_killed = true;
    with_dragon(&ctx, |d| {
        d.pos = [0.5, 20.0, 0.5];
        let Kind::Mob(m) = &mut d.kind else {
            unreachable!()
        };
        m.dragon_mut().unwrap().phase = Phase::SittingScanning;
        m.health = 0.0;
    });
    let mut events = ctx.events.subscribe();
    let mut xp = 0;
    for _ in 0..220 {
        run(&ctx, 1);
        while let Some(ev) = events.try_changed() {
            if let Event::EntitySpawned(e) = ev
                && let Kind::ExperienceOrb { value, count } = e.kind
            {
                xp += value * count;
            }
        }
    }
    assert!(of_species(&ctx, Species::EnderDragon).is_empty());
    assert_eq!(xp, dragon::LATER_XP);
    let o = lock(&ctx.bosses)
        .fight
        .as_ref()
        .unwrap()
        .exit_portal
        .unwrap();
    assert_ne!(name(&ctx, [o[0], o[1] + 4, o[2]]), "minecraft:dragon_egg");
}

#[test]
fn generated_pillars_carry_their_crystals() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let Ok(data) = mc_data::McData::load(&dir) else {
        return;
    };
    let folder = std::env::temp_dir().join(format!("mc-spikes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let mut config = crate::Config::new(dir);
    config.world = Some(folder.clone());
    config.seed = Some(12345);
    let ctx = ServerContext::new(config, data).unwrap();
    let end = ctx.dimension(2).unwrap();
    let spike = mc_features::end::spikes_for_seed(12345)[0];
    let _ = end.world.block(
        Position::new(spike.x, spike.height, spike.z),
        &end.data.blocks,
    );
    let made = end.world.take_generated_entities();
    let crystal = made
        .iter()
        .find(|n| match n.get("Pos") {
            Some(Tag::List(p)) => p[0] == Tag::Double(f64::from(spike.x) + 0.5),
            _ => false,
        })
        .expect("a crystal on the pillar");
    assert_eq!(crystal.get("ShowBottom"), Some(&Tag::Byte(1)));
    assert_eq!(
        name(&end, [spike.x, spike.height, spike.z]),
        "minecraft:bedrock"
    );
    // And the crystals tick brings them into the world.
    lock(&end.players).clear();
    let mut level = WorldLevel::new(&end);
    let mut entities = Vec::new();
    for nbt in made {
        if let Some(e) = crate::summon::from_saved(&end, &nbt) {
            entities.push(e);
        }
    }
    crystal::tick_all(&end, &mut level, &mut entities);
    drop(level);
    assert!(entities.iter().any(|e| matches!(e.kind, Kind::Crystal(_))));
    let _ = std::fs::remove_dir_all(folder);
}
