//! Health and food of survival and adventure players, from the public
//! descriptions (minecraft.wiki: "Health", "Hunger", "Damage", "Fall
//! damage"):
//! - falling: damage is the fall distance minus 3, rounded up; landing in
//!   water does no harm;
//! - lava hurts 4, fire 1, the void below the world 4; after being hurt a
//!   player is immune for 10 ticks;
//! - with food 18 or more a hurt player heals 1 every 80 ticks (every 10
//!   with full food and saturation left), using 6 exhaustion; with no food
//!   they starve, 1 every 80 ticks (on easy down to 10 health, on normal to
//!   1, on hard to death);
//! - exhaustion: 4 of it costs a point of saturation, or of food.
//!
//! Creative and spectator players take no damage and do not get hungry.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::SetHealth;

use crate::context::{Event, Pose, ServerContext};
use crate::tick::lock;

const MAX_HEALTH: f32 = 20.0;
const SAFE_FALL: f64 = 3.0;
const IMMUNE_TICKS: i32 = 10;

#[derive(Clone, Debug)]
pub struct Vitals {
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    /// Blocks fallen since last on the ground.
    pub fall_distance: f64,
    food_timer: i32,
    immune: i32,
    pub dead: bool,
}

impl Default for Vitals {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            fall_distance: 0.0,
            food_timer: 0,
            immune: 0,
            dead: false,
        }
    }
}

impl Vitals {
    pub fn packet(&self) -> SetHealth {
        SetHealth {
            health: self.health,
            food: self.food,
            saturation: self.saturation,
        }
    }

    fn exhaust(&mut self, amount: f32) {
        self.exhaustion += amount;
        while self.exhaustion >= 4.0 {
            self.exhaustion -= 4.0;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else {
                self.food = (self.food - 1).max(0);
            }
        }
    }
}

pub type AllVitals = HashMap<u128, Vitals>;

fn takes_damage(game_mode: i32) -> bool {
    game_mode == 0 || game_mode == 2
}

/// A translated chat component: `{"translate": key, "with": [name]}`.
fn death_message(key: &str, name: &str) -> Tag {
    let mut player = Compound::new();
    player.insert("text", Tag::String(name.to_owned()));
    let mut message = Compound::new();
    message.insert("translate", Tag::String(key.to_owned()));
    message.insert("with", Tag::List(vec![Tag::Compound(player)]));
    Tag::Compound(message)
}

/// Hurts a player; returns whether they died. Sends what clients need.
pub fn hurt(
    ctx: &ServerContext,
    uuid: u128,
    amount: f32,
    damage_type: &str,
    death_key: &str,
) -> bool {
    let Some((name, entity_id, game_mode)) = lock(&ctx.players)
        .get(&uuid)
        .map(|p| (p.profile.name.clone(), p.entity_id, p.game_mode))
    else {
        return false;
    };
    if !takes_damage(game_mode) || amount <= 0.0 {
        return false;
    }
    let (packet, died) = {
        let mut all = lock(&ctx.vitals);
        let Some(v) = all.get_mut(&uuid) else {
            return false;
        };
        if v.dead || v.immune > 0 {
            return false;
        }
        v.health = (v.health - amount).max(0.0);
        v.immune = IMMUNE_TICKS;
        v.exhaust(0.1);
        let died = v.health <= 0.0;
        v.dead = died;
        (v.packet(), died)
    };
    let _ = ctx.events.send(Event::Health {
        uuid,
        health: packet,
    });
    let _ = ctx.events.send(Event::Hurt {
        uuid,
        entity_id,
        damage_type: ctx.damage_type(damage_type),
    });
    if died {
        let _ = ctx.events.send(Event::Died {
            uuid,
            entity_id,
            message: death_message(death_key, &name),
        });
    }
    died
}

/// A player moved: track falling and hurt them when they land.
pub fn moved(ctx: &ServerContext, uuid: u128, before: Pose, after: Pose, in_water: bool) {
    let landed_from = {
        let mut all = lock(&ctx.vitals);
        let Some(v) = all.get_mut(&uuid) else {
            return;
        };
        if in_water {
            v.fall_distance = 0.0;
            None
        } else if after.on_ground {
            // The move that lands is part of the fall.
            let fell = v.fall_distance + (before.y - after.y).max(0.0);
            v.fall_distance = 0.0;
            Some(fell)
        } else {
            if after.y < before.y {
                v.fall_distance += before.y - after.y;
            }
            None
        }
    };
    if let Some(fell) = landed_from {
        let damage = (fell - SAFE_FALL).ceil();
        if damage > 0.0 {
            hurt(
                ctx,
                uuid,
                damage as f32,
                "minecraft:fall",
                "death.attack.fall",
            );
        }
    }
}

/// One tick for every player: immunity wears off, hunger heals or hurts,
/// lava, fire and the void hurt.
pub fn tick(ctx: &ServerContext) {
    let players: Vec<(u128, i32, Pose)> = lock(&ctx.players)
        .values()
        .filter_map(|p| p.pose.map(|pose| (p.profile.uuid, p.game_mode, pose)))
        .collect();
    for (uuid, game_mode, pose) in players {
        let mut hurt_by: Option<(f32, &str, &str)> = None;
        let mut changed = None;
        {
            let mut all = lock(&ctx.vitals);
            let Some(v) = all.get_mut(&uuid) else {
                continue;
            };
            if v.immune > 0 {
                v.immune -= 1;
            }
            if v.dead || !takes_damage(game_mode) {
                continue;
            }
            let before = (v.health, v.food, v.saturation);
            v.food_timer += 1;
            let full = v.food >= 20 && v.saturation > 0.0;
            if v.health < MAX_HEALTH
                && (full && v.food_timer >= 10 || v.food >= 18 && v.food_timer >= 80)
            {
                v.health = (v.health + 1.0).min(MAX_HEALTH);
                v.exhaust(6.0);
                v.food_timer = 0;
            } else if v.food <= 0 && v.food_timer >= 80 {
                v.food_timer = 0;
                let floor = match ctx.difficulty {
                    1 => 10.0,
                    2 => 1.0,
                    3 => 0.0,
                    _ => MAX_HEALTH,
                };
                if v.health > floor {
                    hurt_by = Some((1.0, "minecraft:starve", "death.attack.starve"));
                }
            } else if v.food_timer >= 80 {
                v.food_timer = 0;
            }
            if (v.health, v.food, v.saturation) != before {
                changed = Some(v.packet());
            }
        }
        if let Some(packet) = changed {
            let _ = ctx.events.send(Event::Health {
                uuid,
                health: packet,
            });
        }
        // Where the player is.
        let feet = mc_protocol::Position::new(
            pose.x.floor() as i32,
            pose.y.floor() as i32,
            pose.z.floor() as i32,
        );
        let block = ctx
            .world
            .block(feet, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s));
        if pose.y < f64::from(ctx.world.min_y - 64) {
            hurt_by = Some((4.0, "minecraft:out_of_world", "death.attack.outOfWorld"));
        } else if block == Some("minecraft:lava") {
            hurt_by = Some((4.0, "minecraft:lava", "death.attack.lava"));
        } else if matches!(block, Some("minecraft:fire" | "minecraft:soul_fire")) {
            hurt_by = Some((1.0, "minecraft:in_fire", "death.attack.inFire"));
        }
        if let Some((amount, kind, key)) = hurt_by {
            hurt(ctx, uuid, amount, kind, key);
        }
    }
}

/// A dead player respawns with full health.
pub fn respawn(ctx: &ServerContext, uuid: u128) -> SetHealth {
    let mut all = lock(&ctx.vitals);
    let v = all.entry(uuid).or_default();
    *v = Vitals::default();
    v.packet()
}
