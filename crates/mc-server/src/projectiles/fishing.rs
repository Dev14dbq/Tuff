//! Fishing rods: the bobber is cast, floats, a fish bites after a while,
//! and reeling in at the right moment catches what the fishing loot table
//! gives (minecraft.wiki, "Fishing"). A bobber waits 100 to 600 ticks (less
//! 100 per level of Lure; slower out of the sky's sight, quicker in rain),
//! then a fish swims over for 20 to 80 ticks and bites for 20 to 40. Treasure
//! needs open water around the bobber. A catch costs the rod 1, a bobber
//! stuck in a block 2, a hooked item 3 and any other hooked entity 5, which
//! is pulled to the player.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::{MetadataValue, SetEntityData};

use super::{look, spawn};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::hits::{self, Strike, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Right click with a rod: cast, or reel in.
pub fn use_rod(ctx: &ServerContext, actor: Actor, hand: i32, yaw: f32, pitch: f32) -> Outcome {
    let mut outcome = Outcome::default();
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    let cast = lock(&ctx.shooting).bobbers.remove(&actor.uuid);
    let comps = held::held(ctx, actor.uuid, hand).and_then(|h| h.comps);
    if let Some(id) = cast {
        reel(ctx, actor, hand, id);
        return outcome;
    }
    let at = super::muzzle(&actor);
    let id = super::shoot(
        ctx,
        Shot::Bobber {
            hooked: None,
            wait: -1,
            approach: 0,
            bite: 0,
            lure: held::enchant(comps.as_ref(), "lure"),
            luck: held::enchant(comps.as_ref(), "luck_of_the_sea"),
        },
        Some(actor.uuid),
        at,
        look(yaw, pitch),
        0.9,
        1.0,
    );
    lock(&ctx.shooting).bobbers.insert(actor.uuid, id);
    fx::sound(
        ctx,
        "entity.fishing_bobber.throw",
        fx::category::NEUTRAL,
        at,
        0.5,
        0.4 / (hits::rnd() as f32 * 0.4 + 0.8),
    );
    let _ = spawn;
    outcome
}

/// Pulls the bobber in: with a bite the catch comes flying, a hooked
/// entity is dragged over, a bobber stuck in a block just comes back.
fn reel(ctx: &ServerContext, actor: Actor, hand: i32, id: i32) {
    let me = [actor.pose.x, actor.pose.y, actor.pose.z];
    let found = {
        let mut entities = lock(&ctx.entities);
        let found = entities.iter_mut().find(|e| e.id == id).map(|e| {
            e.removed = true;
            let (bite, hooked, luck) = match &e.kind {
                Kind::Projectile(p) => match &p.shot {
                    Shot::Bobber {
                        bite, hooked, luck, ..
                    } => (*bite, *hooked, *luck),
                    _ => (0, None, 0),
                },
                _ => (0, None, 0),
            };
            let stuck = hooked.is_none() && e.vel == [0.0; 3] && !hits::in_water(ctx, e.pos);
            (e.pos, bite, hooked, luck, stuck)
        });
        // The hooked one is drawn to the player.
        if let Some((pos, _, Some(h), _, _)) = &found
            && let Some(target) = entities.iter_mut().find(|e| e.id == *h)
        {
            for i in 0..3 {
                target.vel[i] += (me[i] - pos[i]) * 0.1;
            }
            if matches!(target.kind, Kind::Mob(_)) {
                target.on_ground = false;
            }
        }
        found
    };
    let Some((pos, bite, hooked, luck, stuck)) = found else {
        return;
    };
    let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
    let rod = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:fishing_rod");
    let mut wear = 0;
    if let Some(h) = hooked {
        // Players are pulled by their clients.
        let player = lock(&ctx.players)
            .values()
            .any(|p| p.entity_id == h)
            .then_some(h);
        if let Some(pid) = player {
            let v = [
                (me[0] - pos[0]) * 0.1,
                (me[1] - pos[1]) * 0.1,
                (me[2] - pos[2]) * 0.1,
            ];
            fx::emit(
                ctx,
                fx::Fx::Motion(mc_protocol::packets::play::SetEntityMotion {
                    entity_id: pid,
                    velocity: v,
                }),
            );
        }
        let is_item = lock(&ctx.entities)
            .iter()
            .any(|e| e.id == h && matches!(e.kind, Kind::Item { .. }));
        wear = if is_item { 3 } else { 5 };
    } else if bite > 0 {
        wear = 1;
        catch(ctx, actor, pos, luck, open_water(ctx, pos));
        fx::sound(
            ctx,
            "entity.fishing_bobber.retrieve",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
    } else if stuck {
        wear = 2;
    }
    if let Some(r) = rod
        && wear > 0
        && actor.game_mode != 1
    {
        held::wear(ctx, actor.uuid, r.slot, wear);
    }
}

/// What a block is to the open-water test: air (or a lily pad) above, a
/// source of water below.
#[derive(Clone, Copy, PartialEq)]
enum Layer {
    Air,
    Water,
    Invalid,
}

fn layer_kind(ctx: &ServerContext, p: [i32; 3]) -> Layer {
    let Some(state) = hits::block_at(ctx, p) else {
        return Layer::Invalid;
    };
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state).unwrap_or("");
    match name {
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" | "minecraft:lily_pad" => {
            Layer::Air
        }
        "minecraft:water" if blocks.property(state, "level") == Some("0") => Layer::Water,
        _ if blocks.property(state, "waterlogged") == Some("true") => Layer::Water,
        "minecraft:bubble_column" | "minecraft:kelp" | "minecraft:seagrass"
        | "minecraft:tall_seagrass" | "minecraft:kelp_plant" => Layer::Water,
        _ => Layer::Invalid,
    }
}

/// Open water: in the box from 2 blocks either side and one below to two
/// above the bobber's block, every layer is all air or all water source
/// (water below, air above), and there are no bubble columns.
pub fn open_water(ctx: &ServerContext, at: [f64; 3]) -> bool {
    let c = [
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    ];
    let mut seen_air = false;
    for dy in -1..=2 {
        let mut kind: Option<Layer> = None;
        for dx in -2..=2 {
            for dz in -2..=2 {
                let p = [c[0] + dx, c[1] + dy, c[2] + dz];
                if hits::block_at(ctx, p)
                    .and_then(|s| ctx.data.blocks.block_name(s))
                    .is_some_and(|n| n == "minecraft:bubble_column")
                {
                    return false;
                }
                let k = layer_kind(ctx, p);
                if k == Layer::Invalid || kind.is_some_and(|prev| prev != k) {
                    return false;
                }
                kind = Some(k);
            }
        }
        match kind {
            Some(Layer::Water) if seen_air => return false,
            Some(Layer::Air) => seen_air = true,
            _ => {}
        }
    }
    true
}

/// Rolls the fishing table and throws the result at the player.
fn catch(ctx: &ServerContext, actor: Actor, at: [f64; 3], luck: i32, open_water: bool) {
    let Some(table) = ctx.loot.table("minecraft:gameplay/fishing").cloned() else {
        return;
    };
    let cx = crate::loot::LootCtx {
        origin: Some(at),
        luck: luck as f32,
        this: Some(crate::loot::Ent {
            kind: "minecraft:fishing_bobber".into(),
            open_water: Some(open_water),
            ..crate::loot::Ent::default()
        }),
        ..crate::loot::LootCtx::default()
    };
    let env = crate::commands::loot::Env(ctx);
    for drop in ctx.loot.roll(&env, &table, &cx) {
        let d = [
            actor.pose.x - at[0],
            actor.pose.y - at[1],
            actor.pose.z - at[2],
        ];
        let flat = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let entity = Entity {
            id: ctx.next_entity_id(),
            uuid: crate::util::random_uuid(),
            kind: Kind::stack(&drop.stack(), 10),
            pos: at,
            vel: [d[0] * 0.1, d[1] * 0.1 + flat.sqrt() * 0.08, d[2] * 0.1],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        };
        let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
        lock(&ctx.entities).push(entity);
    }
    // 1 to 6 points of experience, as an orb at the player.
    let points = 1 + (hits::rnd() * 6.0) as i32;
    crate::xp::spawn(ctx, [actor.pose.x, actor.pose.y + 0.5, actor.pose.z], points);
}

/// The y of the water surface above `p`.
fn surface(ctx: &ServerContext, p: [f64; 3]) -> f64 {
    let (x, z) = (p[0].floor() as i32, p[2].floor() as i32);
    let mut y = p[1].floor() as i32;
    while hits::in_water(
        ctx,
        [
            f64::from(x) + 0.5,
            f64::from(y + 1) + 0.5,
            f64::from(z) + 0.5,
        ],
    ) && y < p[1] as i32 + 8
    {
        y += 1;
    }
    f64::from(y) + 0.9
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Some(owner) = p.owner else {
        e.removed = true;
        return;
    };
    let Shot::Bobber {
        hooked,
        wait,
        approach,
        bite,
        lure,
        ..
    } = &mut p.shot
    else {
        return;
    };
    let ctx = t.ctx;
    e.age += 1;
    let me = t.players.iter().find(|w| w.uuid == owner);
    // The line goes when the thrower does, lets go of the rod, or is far.
    let rod = me.is_some_and(|w| {
        [0, 1]
            .iter()
            .any(|&h| held::held(ctx, w.uuid, h).is_some_and(|h| h.name == "minecraft:fishing_rod"))
    });
    let far = me.is_none_or(|w| {
        (w.pose.x - e.pos[0]).powi(2)
            + (w.pose.y - e.pos[1]).powi(2)
            + (w.pose.z - e.pos[2]).powi(2)
            > 1024.0
    });
    if !rod || far {
        e.removed = true;
        lock(&ctx.shooting).bobbers.remove(&owner);
        return;
    }
    if let Some(id) = *hooked {
        // Carried by what it hooked.
        let at = t
            .players
            .iter()
            .find(|w| w.entity_id == id)
            .map(|w| [w.pose.x, w.pose.y + 0.9, w.pose.z])
            .or_else(|| {
                t.targets.iter().find(|g| g.id == id).map(|g| {
                    [
                        (g.aabb.min[0] + g.aabb.max[0]) / 2.0,
                        (g.aabb.min[1] + g.aabb.max[1]) / 2.0,
                        (g.aabb.min[2] + g.aabb.max[2]) / 2.0,
                    ]
                })
            });
        match at {
            Some(at) => {
                e.pos = at;
                e.vel = [0.0; 3];
                return;
            }
            None => *hooked = None,
        }
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    match hits::trace(t, from, to, 0.125, Some(owner), &[]) {
        None => e.pos = to,
        Some((f, strike)) => {
            e.pos = [
                from[0] + e.vel[0] * f,
                from[1] + e.vel[1] * f,
                from[2] + e.vel[2] * f,
            ];
            match strike {
                Strike::Block => e.vel = [0.0; 3],
                Strike::Player(i) => {
                    let id = t.players[i].entity_id;
                    *hooked = Some(id);
                    t.out.data.push(SetEntityData {
                        entity_id: e.id,
                        entries: vec![(8, MetadataValue::VarInt(id + 1))],
                    });
                    return;
                }
                Strike::Target(i) => {
                    let id = t.targets[i].id;
                    *hooked = Some(id);
                    t.out.data.push(SetEntityData {
                        entity_id: e.id,
                        entries: vec![(8, MetadataValue::VarInt(id + 1))],
                    });
                    return;
                }
            }
        }
    }
    if hits::in_water(ctx, e.pos) {
        // Floats at the surface, drifting little.
        let top = surface(ctx, e.pos);
        e.vel[0] *= 0.5;
        e.vel[2] *= 0.5;
        e.vel[1] = ((top - 0.2 - e.pos[1]) * 0.2).clamp(-0.1, 0.1);
        let cell = [
            e.pos[0].floor() as i32,
            (e.pos[1] + 0.5).floor() as i32,
            e.pos[2].floor() as i32,
        ];
        let sky = ctx
            .world
            .sky_light(mc_protocol::Position::new(cell[0], cell[1] + 1, cell[2]))
            >= 15;
        // Each tick the wait runs down by one; rain overhead now and then
        // by two, and cover over the bobber half the time by none.
        let mut step = 1;
        if hits::rnd() < 0.25 && sky && lock(&ctx.weather).raining {
            step += 1;
        }
        if hits::rnd() < 0.5 && !sky {
            step -= 1;
        }
        if *bite > 0 {
            *bite -= 1;
            if *bite == 0 {
                t.out.data.push(SetEntityData {
                    entity_id: e.id,
                    entries: vec![(9, MetadataValue::Boolean(false))],
                });
            }
        } else if *approach > 0 {
            *approach -= step;
            if *approach > 0 {
                // The fish stirs the water on its way.
                if hits::rnd() < 0.15 {
                    fx::particles(ctx, "minecraft:bubble", e.pos, 0.2, 0.0, 2);
                }
                if hits::rnd() < 0.3 {
                    fx::particles(ctx, "minecraft:fishing", e.pos, 0.3, 0.0, 3);
                }
            } else {
                *approach = 0;
                *bite = 20 + (hits::rnd() * 21.0) as i32;
                fx::sound(
                    ctx,
                    "entity.fishing_bobber.splash",
                    fx::category::NEUTRAL,
                    e.pos,
                    0.25,
                    1.0 + (hits::rnd() - hits::rnd()) as f32 * 0.4,
                );
                fx::particles(ctx, "minecraft:fishing", e.pos, 0.3, 0.1, 8);
                fx::particles(ctx, "minecraft:bubble", e.pos, 0.3, 0.1, 6);
                t.out.data.push(SetEntityData {
                    entity_id: e.id,
                    entries: vec![(9, MetadataValue::Boolean(true))],
                });
                // The bobber dips.
                e.vel[1] -= 0.2;
            }
        } else if *wait > 0 {
            *wait -= step;
            if *wait <= 0 {
                *wait = 0;
                *approach = 20 + (hits::rnd() * 61.0) as i32;
            }
        } else {
            // Lure takes 5 seconds off both ends of the wait; what is left
            // under nothing is a bite at once.
            let lo = (100 - 100 * *lure).max(0);
            let hi = (600 - 100 * *lure).max(lo);
            *wait = (lo + (hits::rnd() * f64::from(hi - lo + 1)) as i32).max(1);
        }
    } else {
        *wait = -1;
        *approach = 0;
        *bite = 0;
        if e.vel != [0.0; 3] {
            e.vel = e.vel.map(|c| c * 0.92);
            e.vel[1] -= 0.03;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;
    use mc_protocol::Position;

    const ME: u128 = 0xf15;

    fn set(ctx: &ServerContext, at: [i32; 3], name: &str) {
        let state = ctx.data.blocks.default_state(name).unwrap();
        ctx.world
            .set_block(Position::new(at[0], at[1], at[2]), state, &ctx.data.blocks, &ctx.rules)
            .unwrap();
    }

    /// A pool 7 wide and 2 deep cut into the flat world's surface (y = -61).
    fn pool(ctx: &ServerContext) {
        for x in -3..=3 {
            for z in -3..=3 {
                for y in -62..=-61 {
                    set(ctx, [x, y, z], "minecraft:water");
                }
            }
        }
    }

    fn actor() -> Actor {
        Actor {
            uuid: ME,
            entity_id: 9001,
            game_mode: 0,
            pose: crate::context::Pose {
                x: 0.5,
                y: -60.0,
                z: 8.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        }
    }

    fn equip_rod(ctx: &ServerContext) {
        let rod = ctx.items.id("minecraft:fishing_rod").unwrap();
        let mut inv = lock(&ctx.inventories);
        let hands = inv.entry(ME).or_default();
        let slot = hands.hand_slot(0);
        hands.slots[slot] = Some(mc_protocol::packets::play::ItemStack::new(rod, 1));
    }

    #[test]
    fn treasure_needs_open_water() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        pool(&ctx);
        assert!(open_water(&ctx, [0.5, -60.8, 0.5]));
        // A one-block hole in the ground is not open water.
        set(&ctx, [10, -61, 10], "minecraft:water");
        assert!(!open_water(&ctx, [10.5, -60.8, 10.5]));
        // Nor is a pool with a roof or a bubble column in it.
        set(&ctx, [0, -59, 0], "minecraft:stone");
        assert!(!open_water(&ctx, [0.5, -60.8, 0.5]));
    }

    fn bobber(lure: i32) -> Entity {
        Entity {
            id: 5000,
            uuid: 1,
            kind: Kind::Projectile(mc_entity::extra::Projectile {
                shot: Shot::Bobber {
                    hooked: None,
                    wait: -1,
                    approach: 0,
                    bite: 0,
                    lure,
                    luck: 0,
                },
                owner: Some(ME),
                mob: None,
                yaw: 0.0,
                pitch: 0.0,
            }),
            pos: [0.5, -60.8, 0.5],
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    /// Ticks until the first bite; the ticks the wait and the approach took.
    fn first_bite(ctx: &ServerContext, lure: i32) -> (i32, i32) {
        let mut e = bobber(lure);
        let who = [crate::hits::Who {
            uuid: ME,
            entity_id: 9001,
            mode: 0,
            pose: actor().pose,
        }];
        let mut out = crate::hits::Out::default();
        let (mut lured, mut waited) = (None, 0);
        for n in 0..2000 {
            let mut t = Tick {
                ctx,
                players: &who,
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
            let Kind::Projectile(p) = &e.kind else { panic!() };
            let Shot::Bobber { approach, bite, .. } = &p.shot else { panic!() };
            if *approach > 0 && lured.is_none() {
                lured = Some(n);
            }
            if *bite > 0 {
                assert!((20..=40).contains(bite), "{bite}");
                return (lured.unwrap(), n - lured.unwrap());
            }
            waited = n;
        }
        panic!("no bite in {waited} ticks");
    }

    #[test]
    fn a_fish_bites_after_the_wait_and_lure_shortens_it() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        pool(&ctx);
        equip_rod(&ctx);
        let (mut slow, mut quick) = (0, 0);
        for _ in 0..20 {
            let (wait, approach) = first_bite(&ctx, 0);
            // 100 to 600 ticks to notice the bait (a few more to get going
            // and a few fewer with rain), then 20 to 80 to swim over.
            assert!((90..=700).contains(&wait), "{wait}");
            assert!((19..=82).contains(&approach), "{approach}");
            slow += wait;
            quick += first_bite(&ctx, 3).0;
        }
        assert!(quick * 2 < slow, "{quick} vs {slow}");
    }

    #[test]
    fn reeling_in_a_bite_gives_a_catch_and_wears_the_rod() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        pool(&ctx);
        equip_rod(&ctx);
        let before = lock(&ctx.entities).len();
        let mut e = bobber(0);
        if let Kind::Projectile(p) = &mut e.kind
            && let Shot::Bobber { bite, .. } = &mut p.shot
        {
            *bite = 10;
        }
        lock(&ctx.entities).push(e);
        lock(&ctx.shooting).bobbers.insert(ME, 5000);
        use_rod(&ctx, actor(), 0, 0.0, 0.0);
        let after = lock(&ctx.entities);
        // The catch and the orb.
        assert!(after.iter().any(|e| matches!(e.kind, Kind::Item { .. })));
        assert!(after.iter().any(|e| matches!(e.kind, Kind::ExperienceOrb { .. })));
        assert!(after.len() >= before + 2);
        let damage = lock(&ctx.inventories)[&ME].slots[36].as_ref().unwrap().damage;
        assert_eq!(damage, 1);
        assert!(!lock(&ctx.shooting).bobbers.contains_key(&ME));
    }
}
