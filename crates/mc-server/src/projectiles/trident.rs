//! Tridents (minecraft.wiki, "Trident", "Loyalty", "Riptide",
//! "Channeling", "Impaling"):
//! - held for at least 10 ticks and let go, a trident is thrown at 2.5
//!   (it wears by 1; one that would break cannot be used), or with Riptide
//!   launches its holder in water or rain instead, spinning for 20 ticks
//!   and hitting what they run into;
//! - a thrown trident hurts what it hits by 8 plus Impaling against
//!   aquatic mobs, then bounces off and falls; it sticks in blocks;
//! - with Channeling, in a thunderstorm, a hit under the open sky (or on a
//!   lightning rod) calls lightning;
//! - with Loyalty, once it has hit or stuck, it flies back through blocks
//!   to its owner, faster with each level; it drops if the owner is gone;
//! - only its owner picks it up; a player's trident never despawns.

use mc_entity::extra::{Arrow, Pickup, Shot};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::ItemStack;

use super::{hurt_player, look, muzzle};
use crate::context::{Event, ServerContext};
use crate::enchant_effects::{self as ee, Situation};
use crate::fx;
use crate::held;
use crate::hits::{self, Hit, Strike, Tick};
use crate::interact::Actor;
use crate::tick::lock;

/// Base damage of a thrown trident.
const DAMAGE: f32 = 8.0;
/// Ticks a stuck trident that may not be picked up lasts.
const LIFE: i32 = 1200;

/// A value effect of the trident's enchantments (`trident_return_acceleration`,
/// `trident_spin_attack_strength`), from 0.
fn effect(ctx: &ServerContext, stack: &ItemStack, component: &str) -> f32 {
    ee::modify(ctx, component, stack, &Situation::default(), 0.0)
}

/// Whether the player stands in water or rain (Riptide works there).
pub fn wet(ctx: &ServerContext, pose: crate::context::Pose) -> bool {
    let feet = [pose.x, pose.y, pose.z];
    let head = [pose.x, pose.y + 1.62, pose.z];
    hits::in_water(ctx, feet)
        || hits::in_water(ctx, head)
        || crate::worn::rain_at(ctx, feet)
        || crate::worn::rain_at(ctx, [pose.x, pose.y + 1.8, pose.z])
}

/// Whether a trident can be used: not about to break, and with Riptide
/// only when wet.
pub fn usable(ctx: &ServerContext, actor: &Actor, stack: &ItemStack) -> bool {
    let max = ctx.items.max_damage(stack.item);
    if max > 0 && stack.damage >= max - 1 {
        return false;
    }
    effect(ctx, stack, "minecraft:trident_spin_attack_strength") <= 0.0 || wet(ctx, actor.pose)
}

/// The trident sound of its enchantments (`trident_sound`, Riptide's by
/// level), else the throw.
fn sound_of(ctx: &ServerContext, stack: &ItemStack) -> String {
    for (name, lvl) in crate::enchants::levels(stack) {
        if let Some(e) = ctx.enchantments.get().get(&name)
            && let Some(list) = e.effects["minecraft:trident_sound"].as_array()
            && let Some(s) = list
                .get(
                    (lvl as usize)
                        .saturating_sub(1)
                        .min(list.len().saturating_sub(1)),
                )
                .and_then(|v| v.as_str())
        {
            return s.trim_start_matches("minecraft:").to_owned();
        }
    }
    "item.trident.throw".into()
}

/// A riptide spin in progress.
#[derive(Clone, Debug)]
pub struct Spin {
    pub ticks: i32,
    pub damage: f32,
    pub hand: i32,
    /// Where the player was last tick, to sweep their box from.
    pub from: [f64; 3],
}

/// The player let go of a trident held for `ticks`.
pub fn release(ctx: &ServerContext, actor: Actor, hand: i32, ticks: i32) {
    let Some(h) = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:trident")
    else {
        return;
    };
    if ticks < 10 || !usable(ctx, &actor, &h.stack) {
        return;
    }
    crate::stats::item(ctx, actor.uuid, crate::stats::USED, h.stack.item, 1);
    let spin = effect(ctx, &h.stack, "minecraft:trident_spin_attack_strength");
    let sound = sound_of(ctx, &h.stack);
    let creative = actor.game_mode == 1;
    if !creative {
        held::wear(ctx, actor.uuid, h.slot, 1);
    }
    let Some(now) = held::held(ctx, actor.uuid, hand) else {
        return;
    };
    let at = [actor.pose.x, actor.pose.y, actor.pose.z];
    if spin <= 0.0 {
        let mut a = Arrow::new(now.stack.item);
        a.trident = true;
        a.damage = f64::from(DAMAGE);
        a.stack = Some(ItemStack {
            count: 1,
            ..now.stack.clone()
        });
        if creative {
            a.pickup = Pickup::Creative;
        }
        let dir = look(actor.pose.yaw, actor.pose.pitch);
        let id = super::shoot(
            ctx,
            Shot::Arrow(a),
            Some(actor.uuid),
            muzzle(&actor),
            dir,
            2.5,
            1.0,
        );
        let _ = id;
        if !creative {
            held::consume(ctx, actor.uuid, now.slot, 1);
        }
        fx::sound(ctx, &sound, fx::category::PLAYERS, at, 1.0, 1.0);
        return;
    }
    // Riptide: the client pushes itself; the server spins and hits.
    lock(&ctx.shooting).spins.insert(
        actor.uuid,
        Spin {
            ticks: 20,
            damage: DAMAGE,
            hand,
            from: at,
        },
    );
    spin_flag(ctx, actor.entity_id, true);
    fx::sound(ctx, &sound, fx::category::PLAYERS, at, 1.0, 1.0);
}

/// The living entity flag of a riptide spin (hand states 0x04).
fn spin_flag(ctx: &ServerContext, entity_id: i32, on: bool) {
    fx::emit(
        ctx,
        fx::Fx::Data(mc_protocol::packets::play::SetEntityData {
            entity_id,
            entries: vec![(
                mc_protocol::packets::play::metadata::HAND_STATES,
                mc_protocol::packets::play::MetadataValue::Byte(if on { 4 } else { 0 }),
            )],
        }),
    );
}

/// One tick of the riptide spins: a spinning player who runs into a mob or
/// another player hits them with the trident and stops.
pub fn tick_spins(ctx: &ServerContext) {
    let spins: Vec<(u128, Spin)> = lock(&ctx.shooting).spins.drain().collect();
    for (uuid, mut spin) in spins {
        let Some((entity_id, pose, mode, name)) = lock(&ctx.players)
            .get(&uuid)
            .and_then(|p| Some((p.entity_id, p.pose?, p.game_mode, p.profile.name.clone())))
        else {
            continue;
        };
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        let to = [pose.x, pose.y, pose.z];
        let swept = mc_entity::Aabb {
            min: [
                spin.from[0].min(to[0]) - 0.3,
                spin.from[1].min(to[1]),
                spin.from[2].min(to[2]) - 0.3,
            ],
            max: [
                spin.from[0].max(to[0]) + 0.3,
                spin.from[1].max(to[1]) + 1.8,
                spin.from[2].max(to[2]) + 0.3,
            ],
        };
        spin.from = to;
        spin.ticks -= 1;
        let overlaps = |b: &mc_entity::Aabb| {
            (0..3).all(|i| b.min[i] < swept.max[i] && b.max[i] > swept.min[i])
        };
        let mob = lock(&ctx.entities)
            .iter()
            .find(|e| matches!(&e.kind, Kind::Mob(m) if m.dying.is_none()) && overlaps(&e.aabb()))
            .map(|e| e.id);
        let player = lock(&ctx.players)
            .values()
            .filter(|p| {
                p.profile.uuid != uuid && p.game_mode != 3 && p.dimension == ctx.world.dimension
            })
            .find(|p| {
                p.pose.is_some_and(|q| {
                    overlaps(&mc_entity::Aabb {
                        min: [q.x - 0.3, q.y, q.z - 0.3],
                        max: [q.x + 0.3, q.y + 1.8, q.z + 0.3],
                    })
                })
            })
            .map(|p| p.profile.uuid);
        let actor = Actor {
            uuid,
            entity_id,
            game_mode: mode,
            pose,
            sneaking: false,
        };
        let hit = if let Some(victim) = player {
            crate::combat::player_hits_player_with(ctx, &actor, &name, victim, Some(spin.damage));
            true
        } else if let Some(target) = mob {
            let blow = crate::combat::swing_with(ctx, uuid, pose, Some(spin.damage));
            ctx.mobs.hit(
                ctx.dim,
                target,
                crate::mobs::Actor {
                    uuid,
                    entity_id,
                    name,
                    pos: to,
                    game_mode: mode,
                    sneaking: false,
                },
                blow,
            );
            true
        } else {
            false
        };
        if hit || spin.ticks <= 0 {
            spin_flag(ctx, entity_id, false);
        } else {
            lock(&ctx.shooting).spins.insert(uuid, spin);
        }
        let _ = spin.hand;
    }
}

/// Damage of a trident hit on an entity of `kind`: 8 and Impaling.
fn damage_to(ctx: &ServerContext, stack: Option<&ItemStack>, kind: &str) -> f32 {
    let Some(stack) = stack else {
        return DAMAGE;
    };
    let s = Situation {
        this: Some(kind),
        ..Situation::default()
    };
    ee::modify(ctx, "minecraft:damage", stack, &s, DAMAGE)
}

/// Channeling: in a thunderstorm, the trident calls lightning on what it
/// hit (`post_attack`) or on the lightning rod it struck (`hit_block`).
/// Calls lightning down if the trident channels it; whether it did.
fn channel(t: &mut Tick, stack: Option<&ItemStack>, at: [f64; 3], on_rod: bool) -> bool {
    let Some(stack) = stack else {
        return false;
    };
    let ctx = t.ctx;
    let thundering = lock(&ctx.weather).thundering;
    if !thundering {
        return false;
    }
    let block = at.map(|c| c.floor() as i32);
    let sky = crate::worn::sees_sky(ctx, block);
    let mover = ee::Mover {
        can_see_sky: sky,
        ..ee::Mover::default()
    };
    let s = Situation {
        this: Some("minecraft:trident"),
        direct_attacker: Some("minecraft:trident"),
        thundering,
        raining: true,
        mover: Some(&mover),
        ..Situation::default()
    };
    let calls = if on_rod {
        sky && crate::enchants::levels(stack).iter().any(|(n, l)| {
            ctx.enchantments.get().get(n).is_some_and(|e| {
                ee::effects_of(&e.effects["minecraft:hit_block"])
                    .iter()
                    .any(|en| {
                        let s = Situation { level: *l, ..s };
                        en.get("requirements").is_none_or(|r| {
                            // The rod is the block it hit.
                            rod_condition(ctx, r, &s)
                        })
                    })
            })
        })
    } else {
        let gear = [(0usize, "mainhand", stack.clone())];
        ee::post_attack(ctx, &gear, "attacker", "victim", &s)
            .iter()
            .any(|p| p.lightning)
    };
    if calls {
        super::lightning::strike(
            t,
            [
                block[0] as f64 + 0.5,
                block[1] as f64,
                block[2] as f64 + 0.5,
            ],
        );
        fx::sound(ctx, "item.trident.thunder", 10, at, 5.0, 1.0);
    }
    calls
}

/// The `hit_block` requirements of Channeling, with its `location_check`
/// on the struck block (a lightning rod under the open sky).
fn rod_condition(ctx: &ServerContext, r: &serde_json::Value, s: &Situation) -> bool {
    match r["condition"].as_str().or(r["type"].as_str()).unwrap_or("") {
        "minecraft:all_of" => r["terms"]
            .as_array()
            .is_some_and(|t| t.iter().all(|t| rod_condition(ctx, t, s))),
        // Checked by the caller: the block is a lightning rod with sky.
        "minecraft:location_check" => true,
        _ => ee::holds(ctx, r, "minecraft:trident", s),
    }
}

/// The owner's eyes, if they can take the trident back.
fn owner_eyes(t: &Tick, owner: Option<u128>) -> Option<[f64; 3]> {
    let w = t.players.iter().find(|w| Some(w.uuid) == owner)?;
    (w.mode != 3).then_some([w.pose.x, w.pose.y + 1.62, w.pose.z])
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let owner = p.owner;
    let Shot::Arrow(a) = &mut p.shot else {
        return;
    };
    let ctx = t.ctx;
    e.age += 1;
    let loyalty = a.stack.as_ref().map_or(0.0, |s| {
        effect(ctx, s, "minecraft:trident_return_acceleration")
    });
    let below_world = e.pos[1] < f64::from(ctx.world.min_y);
    if loyalty > 0.0 && owner.is_some() && (a.dealt || a.stuck > 4 || a.returning || below_world) {
        match owner_eyes(t, owner) {
            None => {
                // Nobody to go back to: it drops.
                if a.pickup == Pickup::Yes
                    && let Some(stack) = &a.stack
                {
                    t.out.spawn.push(Entity {
                        id: ctx.next_entity_id(),
                        uuid: crate::util::random_uuid(),
                        kind: Kind::stack(stack, 10),
                        pos: e.pos,
                        vel: [0.0, 0.2, 0.0],
                        on_ground: false,
                        age: 0,
                        removed: false,
                        tags: Vec::new(),
                    });
                }
                e.removed = true;
                return;
            }
            Some(eyes) => {
                if !a.returning {
                    a.returning = true;
                    a.stuck = 0;
                    fx::sound(
                        ctx,
                        "item.trident.return",
                        fx::category::NEUTRAL,
                        e.pos,
                        10.0,
                        1.0,
                    );
                    t.out.data.push(mc_protocol::packets::play::SetEntityData {
                        entity_id: e.id,
                        entries: crate::fx::arrow_meta(a),
                    });
                }
                let l = f64::from(loyalty);
                let v = [eyes[0] - e.pos[0], eyes[1] - e.pos[1], eyes[2] - e.pos[2]];
                e.pos[1] += v[1] * 0.015 * l;
                let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
                let k = 0.05 * l / len;
                e.vel = [
                    e.vel[0] * 0.95 + v[0] * k,
                    e.vel[1] * 0.95 + v[1] * k,
                    e.vel[2] * 0.95 + v[2] * k,
                ];
                e.pos = [
                    e.pos[0] + e.vel[0],
                    e.pos[1] + e.vel[1],
                    e.pos[2] + e.vel[2],
                ];
                e.vel = e.vel.map(|c| c * 0.99);
                pick_up(t, e);
                return;
            }
        }
    }
    if a.stuck > 0 {
        if !super::arrow::held_by_block(ctx, e.pos) {
            a.stuck = 0;
            e.vel = [0.0, -0.02, 0.0];
            return;
        }
        a.stuck += 1;
        if a.pickup != Pickup::Yes && a.stuck >= LIFE {
            e.removed = true;
        } else if a.stuck > 7 {
            pick_up(t, e);
        }
        return;
    }
    if e.pos[1] < f64::from(ctx.world.min_y - 64) {
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let ignore = if e.age < 5 { owner } else { None };
    let skip: Vec<i32> = if a.dealt {
        // It hurts nothing more: only blocks stop it.
        t.players
            .iter()
            .map(|w| w.entity_id)
            .chain(t.targets.iter().map(|g| g.id))
            .collect()
    } else {
        a.hit.clone()
    };
    let stack = a.stack.clone();
    match hits::trace(t, from, to, 0.25, ignore, &skip) {
        None => e.pos = to,
        Some((f, strike)) => {
            e.pos = [
                from[0] + e.vel[0] * f,
                from[1] + e.vel[1] * f,
                from[2] + e.vel[2] * f,
            ];
            match strike {
                Strike::Block { .. } => {
                    hits::hit_block(ctx, &strike, e.pos, true);
                    crate::pots::projectile(
                        ctx,
                        from,
                        to,
                        crate::pots::Impact {
                            by: crate::pots::Launcher::of(owner, t.shooter.is_some()),
                            fast_trident: a.trident
                                && e.vel.iter().map(|v| v * v).sum::<f64>() > 0.36,
                            can_break: a
                                .stack
                                .as_ref()
                                .and_then(|s| ctx.items.component(s, "minecraft:can_break")),
                        },
                    );
                    fx::sound(
                        ctx,
                        "item.trident.hit_ground",
                        fx::category::NEUTRAL,
                        e.pos,
                        1.0,
                        1.0,
                    );
                    a.stuck = 1;
                    e.vel = [0.0; 3];
                    t.out.data.push(mc_protocol::packets::play::SetEntityData {
                        entity_id: e.id,
                        entries: crate::fx::arrow_meta(a),
                    });
                    // The block it went into.
                    let inside = [
                        e.pos[0] + (to[0] - from[0]) * 0.01,
                        e.pos[1] + (to[1] - from[1]) * 0.01,
                        e.pos[2] + (to[2] - from[2]) * 0.01,
                    ];
                    let cell = inside.map(|c| c.floor() as i32);
                    let rod = hits::block_at(ctx, cell)
                        .and_then(|s| ctx.data.blocks.block_name(s))
                        .is_some_and(|n| {
                            ctx.commands
                                .env
                                .tag("minecraft:block", "minecraft:lightning_rods")
                                .iter()
                                .any(|m| m == n)
                        });
                    if rod {
                        let _ = channel(
                            t,
                            stack.as_ref(),
                            [
                                cell[0] as f64 + 0.5,
                                cell[1] as f64 + 1.0,
                                cell[2] as f64 + 0.5,
                            ],
                            true,
                        );
                    }
                    return;
                }
                Strike::Player(i) => {
                    let victim = t.players[i];
                    let amount = damage_to(ctx, stack.as_ref(), "minecraft:player");
                    hurt_player(
                        t,
                        &victim,
                        amount,
                        "minecraft:trident",
                        "death.attack.trident",
                        owner,
                        e.id,
                    );
                    let _ = channel(t, stack.as_ref(), victim.pose_vec(), false);
                }
                Strike::Target(i) => {
                    let target = t.targets[i];
                    let amount = damage_to(ctx, stack.as_ref(), target.kind.unwrap_or(""));
                    t.out.hits.push(Hit {
                        target: target.id,
                        amount,
                        by: owner,
                        with: Some(e.id),
                        kind: "minecraft:trident",
                    });
                    if let Some(kind) = target.kind {
                        let c = target.aabb;
                        let called = channel(
                            t,
                            stack.as_ref(),
                            [
                                (c.min[0] + c.max[0]) / 2.0,
                                c.min[1],
                                (c.min[2] + c.max[2]) / 2.0,
                            ],
                            false,
                        );
                        if called && let Some(o) = owner {
                            let victim = crate::criteria::Subject {
                                kind: kind.to_owned(),
                                id: target.id,
                                ..Default::default()
                            };
                            crate::criteria::channeled(ctx, o, &[victim]);
                        }
                    }
                }
            }
            // Bounces off what it hit and falls.
            a.dealt = true;
            e.vel = [e.vel[0] * -0.01, e.vel[1] * -0.1, e.vel[2] * -0.01];
            fx::sound(
                ctx,
                "item.trident.hit",
                fx::category::NEUTRAL,
                e.pos,
                1.0,
                1.0,
            );
        }
    }
    // Tridents keep their speed in water.
    e.vel = e.vel.map(|c| c * 0.99);
    e.vel[1] -= 0.05;
}

/// The owner (or anyone, for a trident nobody threw) takes it back.
fn pick_up(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let owner = p.owner;
    let Shot::Arrow(a) = &p.shot else {
        return;
    };
    let b = e.aabb();
    for who in t.players {
        if who.mode == 3 || owner.is_some_and(|o| o != who.uuid) {
            continue;
        }
        let w = who.aabb();
        let near = (0..3).all(|i| {
            let m = if i == 1 { 0.5 } else { 1.0 };
            w.min[i] - m < b.max[i] && w.max[i] + m > b.min[i]
        });
        if !near {
            continue;
        }
        let creative = who.mode == 1;
        let stack = a.stack.clone().unwrap_or_else(|| ItemStack::new(a.item, 1));
        let taken = match a.pickup {
            Pickup::Yes => held::give(t.ctx, who.uuid, &stack) == 0,
            Pickup::Creative => creative,
            Pickup::No => false,
        };
        // A returning trident is taken back whatever its pickup says.
        let taken = taken
            || a.returning
                && owner == Some(who.uuid)
                && (creative || held::give(t.ctx, who.uuid, &stack) == 0);
        if !taken {
            continue;
        }
        let _ = t.ctx.events.send(Event::ItemTaken {
            item_entity: e.id,
            collector: who.entity_id,
            count: 1,
            slots: Vec::new(),
        });
        e.removed = true;
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{flat, join, set, stack, wear};

    const U: u128 = 0x7001;

    fn actor(ctx: &ServerContext) -> Actor {
        let p = lock(&ctx.players).get(&U).cloned().unwrap();
        Actor {
            uuid: U,
            entity_id: p.entity_id,
            game_mode: p.game_mode,
            pose: p.pose.unwrap(),
            sneaking: false,
        }
    }

    fn take_trident(ctx: &ServerContext) -> Entity {
        let mut all = lock(&ctx.entities);
        let i = all
            .iter()
            .position(|e| matches!(&e.kind, Kind::Projectile(p) if matches!(&p.shot, Shot::Arrow(a) if a.trident)))
            .expect("a trident");
        all.swap_remove(i)
    }

    fn who(ctx: &ServerContext, uuid: u128) -> hits::Who {
        let p = lock(&ctx.players).get(&uuid).cloned().unwrap();
        hits::Who {
            uuid,
            entity_id: p.entity_id,
            mode: p.game_mode,
            pose: p.pose.unwrap(),
        }
    }

    fn step(
        ctx: &ServerContext,
        e: &mut Entity,
        players: &[hits::Who],
        targets: &[hits::Target],
    ) -> hits::Out {
        let mut out = hits::Out::default();
        let mut t = Tick {
            ctx,
            players,
            targets,
            out: &mut out,
            shooter: None,
        };
        tick(&mut t, e);
        out
    }

    fn arrow(e: &Entity) -> &Arrow {
        let Kind::Projectile(p) = &e.kind else {
            panic!()
        };
        let Shot::Arrow(a) = &p.shot else { panic!() };
        a
    }

    #[test]
    fn a_thrown_trident_leaves_the_hand_and_hurts_by_impaling() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 7101, [0.5, -60.0, 0.5], 0);
        wear(&ctx, U, 36, stack(&ctx, "trident", &[("impaling", 5)]));
        // Too short a hold does nothing.
        release(&ctx, actor(&ctx), 0, 9);
        assert!(lock(&ctx.inventories).get(&U).unwrap().slots[36].is_some());
        release(&ctx, actor(&ctx), 0, 10);
        assert!(lock(&ctx.inventories).get(&U).unwrap().slots[36].is_none());
        let mut e = take_trident(&ctx);
        let a = arrow(&e);
        assert_eq!(a.stack.as_ref().unwrap().damage, 1);
        let speed = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt();
        assert!((speed - 2.5).abs() < 0.2, "{speed}");
        // A cod in its way takes 8 and 12.5; a zombie only 8.
        let at = e.pos;
        let target = |kind| hits::Target {
            id: 55,
            aabb: mc_entity::Aabb {
                min: [at[0] - 1.0, at[1] - 1.0, at[2] + 1.0],
                max: [at[0] + 1.0, at[1] + 1.0, at[2] + 2.0],
            },
            kind: Some(kind),
        };
        e.age = 10;
        e.vel = [0.0, 0.0, 2.5];
        let mut z = e.clone();
        let out = step(&ctx, &mut e, &[], &[target("minecraft:cod")]);
        assert_eq!(out.hits[0].amount, 20.5);
        assert_eq!(out.hits[0].kind, "minecraft:trident");
        assert!(arrow(&e).dealt && e.vel[2] < 0.0 && !e.removed);
        let out = step(&ctx, &mut z, &[], &[target("minecraft:zombie")]);
        assert_eq!(out.hits[0].amount, 8.0);
    }

    #[test]
    fn loyalty_brings_it_back_and_only_the_owner_takes_it() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 7102, [0.5, -60.0, 0.5], 0);
        join(&ctx, 0x7002, 7103, [10.5, -60.0, 0.5], 0);
        wear(&ctx, U, 36, stack(&ctx, "trident", &[]));
        release(&ctx, actor(&ctx), 0, 20);
        let mut plain = take_trident(&ctx);
        // Stuck next to the other player: not theirs to take.
        plain.pos = [10.5, -60.0, 0.5];
        plain.vel = [0.0; 3];
        if let Kind::Projectile(p) = &mut plain.kind
            && let Shot::Arrow(a) = &mut p.shot
        {
            a.stuck = 20;
        }
        let other = [who(&ctx, 0x7002)];
        step(&ctx, &mut plain, &other, &[]);
        assert!(!plain.removed);
        // A player's trident does not despawn.
        for _ in 0..1300 {
            step(&ctx, &mut plain, &[], &[]);
        }
        assert!(!plain.removed);

        wear(&ctx, U, 36, stack(&ctx, "trident", &[("loyalty", 3)]));
        release(&ctx, actor(&ctx), 0, 20);
        let mut e = take_trident(&ctx);
        e.pos = [8.5, -60.0, 0.5];
        e.vel = [0.0; 3];
        if let Kind::Projectile(p) = &mut e.kind
            && let Shot::Arrow(a) = &mut p.shot
        {
            a.stuck = 5;
        }
        let me = [who(&ctx, U)];
        let mut ticks = 0;
        while !e.removed && ticks < 200 {
            step(&ctx, &mut e, &me, &[]);
            ticks += 1;
            if ticks == 1 {
                assert!(arrow(&e).returning);
            }
        }
        assert!(e.removed, "came back in {ticks}: {:?}", e.pos);
        let back = lock(&ctx.inventories)
            .get(&U)
            .unwrap()
            .slots
            .iter()
            .flatten()
            .any(|s| {
                crate::enchants::levels(s)
                    .iter()
                    .any(|(n, _)| n == "minecraft:loyalty")
            });
        assert!(back);

        // With the owner gone it drops as an item.
        wear(&ctx, U, 36, stack(&ctx, "trident", &[("loyalty", 1)]));
        lock(&ctx.inventories)
            .get_mut(&U)
            .unwrap()
            .slots
            .iter_mut()
            .skip(37)
            .for_each(|s| *s = None);
        release(&ctx, actor(&ctx), 0, 20);
        let mut e = take_trident(&ctx);
        if let Kind::Projectile(p) = &mut e.kind
            && let Shot::Arrow(a) = &mut p.shot
        {
            a.dealt = true;
        }
        let out = step(&ctx, &mut e, &[], &[]);
        assert!(e.removed);
        assert!(matches!(out.spawn[0].kind, Kind::Item { .. }));
    }

    #[test]
    fn riptide_needs_water_and_keeps_the_trident() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 7104, [0.5, -60.0, 0.5], 0);
        wear(&ctx, U, 36, stack(&ctx, "trident", &[("riptide", 3)]));
        assert!(!usable(
            &ctx,
            &actor(&ctx),
            &lock(&ctx.inventories).get(&U).unwrap().slots[36]
                .clone()
                .unwrap()
        ));
        release(&ctx, actor(&ctx), 0, 20);
        assert!(lock(&ctx.shooting).spins.is_empty());
        set(&ctx, [0, -60, 0], "minecraft:water");
        release(&ctx, actor(&ctx), 0, 20);
        assert_eq!(lock(&ctx.shooting).spins.get(&U).map(|s| s.ticks), Some(20));
        let held = lock(&ctx.inventories).get(&U).unwrap().slots[36]
            .clone()
            .unwrap();
        assert_eq!(held.damage, 1);
        // The spin wears off after 20 ticks.
        for _ in 0..20 {
            tick_spins(&ctx);
        }
        assert!(lock(&ctx.shooting).spins.is_empty());
    }

    #[test]
    fn channeling_calls_lightning_in_a_storm() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 7105, [0.5, -60.0, 0.5], 0);
        wear(&ctx, U, 36, stack(&ctx, "trident", &[("channeling", 1)]));
        release(&ctx, actor(&ctx), 0, 20);
        let mut e = take_trident(&ctx);
        e.age = 10;
        e.pos = [20.5, -59.0, 0.5];
        e.vel = [0.0, 0.0, 2.0];
        let target = [hits::Target {
            id: 77,
            aabb: mc_entity::Aabb {
                min: [20.0, -60.0, 1.5],
                max: [21.0, -58.0, 2.5],
            },
            kind: Some("minecraft:zombie"),
        }];
        let mut calm = e.clone();
        let out = step(&ctx, &mut calm, &[], &target);
        assert!(out.spawn.is_empty());
        {
            let mut w = lock(&ctx.weather);
            w.raining = true;
            w.thundering = true;
        }
        let out = step(&ctx, &mut e, &[], &target);
        assert_eq!(out.hits.len(), 1);
        assert!(out.spawn.iter().any(
            |b| matches!(&b.kind, Kind::Projectile(p) if matches!(p.shot, Shot::Lightning { .. }))
        ));
    }
}
