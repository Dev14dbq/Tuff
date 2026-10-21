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
//!
//! Eating and drinking ("Food"): holding use with a consumable in hand for
//! its time (1.6 s unless the item says otherwise) eats it; food can be eaten
//! only when hungry unless it says it always can, or in creative. Food adds
//! its nutrition (up to 20) and saturation (up to the food level); the stack
//! loses one (not in creative) and what is left (a bottle) goes in its place.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::SetHealth;

use crate::context::{Event, Pose, ServerContext};
use crate::tick::lock;

const MAX_HEALTH: f32 = 20.0;
const IMMUNE_TICKS: i32 = 10;
/// A fall this soon after being hit is credited to the attacker.
const ASSIST_TICKS: i32 = 100;

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
    /// The damage that started the immunity: only more gets through.
    last_hurt: f32,
    pub dead: bool,
    /// Experience: the level, the part of the next one (0–1) and every
    /// point ever collected (`XpLevel`, `XpP`, `XpTotal`).
    pub xp_level: i32,
    pub xp_progress: f32,
    pub xp_total: i32,
    /// Mob effects (`active_effects`).
    pub effects: crate::effects::Effects,
    /// Extra hearts from absorption (`AbsorptionAmount`).
    pub absorption: f32,
    /// Air left under water, in ticks (`Air`).
    pub air: i32,
    /// Ticks since a player hit them (for "doomed to fall by").
    pub since_attacked: i32,
    pub last_attacker: Option<String>,
}

/// Points from one experience level to the next (wiki, "Experience").
pub fn xp_needed(level: i32) -> i32 {
    if level >= 30 {
        112 + (level - 30) * 9
    } else if level >= 15 {
        37 + (level - 15) * 5
    } else {
        7 + level * 2
    }
}

impl Vitals {
    pub fn add_xp_points(&mut self, points: i32) {
        self.xp_progress += points as f32 / xp_needed(self.xp_level) as f32;
        self.xp_total = self.xp_total.saturating_add(points).max(0);
        while self.xp_progress < 0.0 {
            let f = self.xp_progress * xp_needed(self.xp_level) as f32;
            if self.xp_level > 0 {
                self.add_xp_levels(-1);
                self.xp_progress = 1.0 + f / xp_needed(self.xp_level) as f32;
            } else {
                self.add_xp_levels(-1);
                self.xp_progress = 0.0;
            }
        }
        while self.xp_progress >= 1.0 {
            self.xp_progress = (self.xp_progress - 1.0) * xp_needed(self.xp_level) as f32;
            self.add_xp_levels(1);
            self.xp_progress /= xp_needed(self.xp_level) as f32;
        }
    }

    pub fn add_xp_levels(&mut self, levels: i32) {
        self.xp_level = self.xp_level.saturating_add(levels);
        if self.xp_level < 0 {
            self.xp_level = 0;
            self.xp_progress = 0.0;
            self.xp_total = 0;
        }
    }

    /// Points into the current level.
    pub fn xp_points(&self) -> i32 {
        (self.xp_progress * xp_needed(self.xp_level) as f32).floor() as i32
    }

    pub fn experience_packet(&self) -> mc_protocol::packets::play::SetExperience {
        mc_protocol::packets::play::SetExperience {
            bar: self.xp_progress,
            level: self.xp_level,
            total: self.xp_total,
        }
    }
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
            last_hurt: 0.0,
            dead: false,
            xp_level: 0,
            xp_progress: 0.0,
            xp_total: 0,
            effects: crate::effects::Effects::default(),
            absorption: 0.0,
            air: crate::damage::MAX_AIR,
            since_attacked: i32::MAX,
            last_attacker: None,
        }
    }
}

impl Vitals {
    /// Maximum health, with health boost.
    pub fn max_health(&self) -> f32 {
        self.effects
            .value(crate::effects::Attr::MaxHealth)
            .min(f64::from(f32::MAX)) as f32
    }

    pub fn packet(&self) -> SetHealth {
        SetHealth {
            health: self.health,
            food: self.food,
            saturation: self.saturation,
        }
    }

    pub(crate) fn exhaust(&mut self, amount: f32) {
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

/// A player eating or drinking.
#[derive(Clone, Debug)]
pub struct Using {
    pub entity_id: i32,
    /// 0 main hand, 1 off hand.
    pub hand: i32,
    /// Inventory slot of the item and the item.
    pub slot: usize,
    pub item: i32,
    pub ticks_left: i32,
    pub game_mode: i32,
}

/// Starts using what is in `hand` if it can be eaten or drunk now. Returns
/// whether it started.
pub fn start_using(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    hand: i32,
    game_mode: i32,
) -> bool {
    let Some((slot, item)) = lock(&ctx.inventories).get(&uuid).and_then(|h| {
        let slot = h.hand_slot(hand);
        h.slots[slot].as_ref().map(|s| (slot, s.item))
    }) else {
        return false;
    };
    let Some(info) = ctx.items.get(item) else {
        return false;
    };
    let Some(ticks) = info.use_ticks else {
        return false;
    };
    if let Some(food) = &info.food {
        let hungry = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.food < 20);
        if !(hungry || food.always || game_mode == 1) {
            return false;
        }
    }
    lock(&ctx.using).insert(
        uuid,
        Using {
            entity_id,
            hand,
            slot,
            item,
            ticks_left: ticks,
            game_mode,
        },
    );
    let _ = ctx.events.send(Event::UsingItem {
        uuid,
        entity_id,
        hand: Some(hand),
    });
    true
}

/// The player let go of use (or changed what they hold).
pub fn stop_using(ctx: &ServerContext, uuid: u128) {
    if let Some(using) = lock(&ctx.using).remove(&uuid) {
        let _ = ctx.events.send(Event::UsingItem {
            uuid,
            entity_id: using.entity_id,
            hand: None,
        });
    }
}

/// One tick of eating and drinking: finished items are consumed.
pub fn tick_using(ctx: &ServerContext) {
    let done: Vec<(u128, Using)> = {
        let mut all = lock(&ctx.using);
        for using in all.values_mut() {
            using.ticks_left -= 1;
        }
        let uuids: Vec<u128> = all
            .iter()
            .filter(|(_, u)| u.ticks_left <= 0)
            .map(|(&id, _)| id)
            .collect();
        uuids
            .into_iter()
            .filter_map(|id| all.remove(&id).map(|u| (id, u)))
            .collect()
    };
    for (uuid, using) in done {
        finish_using(ctx, uuid, &using);
        let _ = ctx.events.send(Event::UsingItem {
            uuid,
            entity_id: using.entity_id,
            hand: None,
        });
    }
}

fn finish_using(ctx: &ServerContext, uuid: u128, using: &Using) {
    let Some(info) = ctx.items.get(using.item) else {
        return;
    };
    // The item must still be there.
    let mut changed = Vec::new();
    {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            return;
        };
        let still = hands.slots[using.slot]
            .as_ref()
            .is_some_and(|s| s.item == using.item);
        if !still {
            return;
        }
        if using.game_mode != 1 {
            let stack = hands.slots[using.slot].as_mut().expect("checked");
            stack.count -= 1;
            if stack.count <= 0 {
                hands.slots[using.slot] = None;
                hands.components.remove(&using.slot);
            }
            changed.push(using.slot);
            if let Some(id) = info.remainder.as_deref().and_then(|n| ctx.items.id(n)) {
                if hands.slots[using.slot].is_none() {
                    hands.slots[using.slot] = Some(mc_protocol::packets::play::ItemStack {
                        count: 1,
                        item: id,
                        damage: 0,
                        has_component_changes: false,
                    });
                } else {
                    let (taken, slots) = hands.insert(id, 1, ctx.items.max_stack(id));
                    changed.extend(slots);
                    if taken == 0 {
                        // No room: it falls at the player's feet.
                        let pose = lock(&ctx.players).get(&uuid).and_then(|p| p.pose);
                        if let Some(pose) = pose {
                            drop(inventories);
                            crate::tick::throw_item(
                                ctx,
                                pose,
                                mc_protocol::packets::play::ItemStack {
                                    count: 1,
                                    item: id,
                                    damage: 0,
                                    has_component_changes: false,
                                },
                            );
                            inventories = lock(&ctx.inventories);
                        }
                    }
                }
            }
        }
        let hands = inventories.get(&uuid);
        for slot in changed {
            let _ = ctx.events.send(Event::InventorySlot {
                uuid,
                slot: slot as i16,
                item: hands.and_then(|h| h.slots[slot].clone()),
            });
        }
    }
    if let Some(food) = &info.food {
        let packet = {
            let mut all = lock(&ctx.vitals);
            let Some(v) = all.get_mut(&uuid) else { return };
            v.food = (v.food + food.nutrition).min(20);
            v.saturation = (v.saturation + food.saturation).min(v.food as f32);
            v.packet()
        };
        let _ = ctx.events.send(Event::Health {
            uuid,
            health: packet,
        });
    }
    let _ = ctx.events.send(Event::UsedItem {
        uuid,
        entity_id: using.entity_id,
    });
}

fn takes_damage(game_mode: i32) -> bool {
    game_mode == 0 || game_mode == 2
}

/// A translated chat component: `{"translate": key, "with": [names...]}`.
fn death_message(key: &str, names: &[&str]) -> Tag {
    let with = names
        .iter()
        .map(|n| {
            let mut c = Compound::new();
            c.insert("text", Tag::String((*n).to_owned()));
            Tag::Compound(c)
        })
        .collect();
    let mut message = Compound::new();
    message.insert("translate", Tag::String(key.to_owned()));
    message.insert("with", Tag::List(with));
    Tag::Compound(message)
}

/// Melee damage of what is in the hand: 1 plus the item's attack damage
/// (its `attribute_modifiers`).
pub fn melee_damage(ctx: &ServerContext, item: Option<i32>) -> f32 {
    1.0 + item
        .and_then(|i| ctx.items.get(i))
        .map_or(0.0, |i| i.attack_damage)
}

/// Hurts a player; returns whether they died. Sends what clients need.
pub fn hurt(
    ctx: &ServerContext,
    uuid: u128,
    amount: f32,
    damage_type: &str,
    death_key: &str,
) -> bool {
    hurt_by(ctx, uuid, amount, damage_type, death_key, None)
}

/// [`hurt`], naming who did it in the death message.
pub fn hurt_by(
    ctx: &ServerContext,
    uuid: u128,
    amount: f32,
    damage_type: &str,
    death_key: &str,
    attacker: Option<&str>,
) -> bool {
    try_hurt(ctx, uuid, amount, damage_type, death_key, attacker).unwrap_or(false)
}

/// Damage types that hurt even creative players and the immune
/// (`bypasses_invulnerability`).
const BYPASSES_INVULNERABILITY: [&str; 2] = ["minecraft:out_of_world", "minecraft:generic_kill"];

/// [`hurt_by`]: `None` when the player took no damage (creative, immune,
/// nothing left after effects), else whether they died.
pub fn try_hurt(
    ctx: &ServerContext,
    uuid: u128,
    amount: f32,
    damage_type: &str,
    death_key: &str,
    attacker: Option<&str>,
) -> Option<bool> {
    try_hurt_by_entity(ctx, uuid, amount, damage_type, death_key, attacker, None)
}

/// [`try_hurt`] by an entity that is not a player (`direct`, its entity id:
/// primed TNT), which clients see in the damage event.
pub fn try_hurt_by_entity(
    ctx: &ServerContext,
    uuid: u128,
    amount: f32,
    damage_type: &str,
    death_key: &str,
    attacker: Option<&str>,
    direct: Option<i32>,
) -> Option<bool> {
    let (name, entity_id, game_mode) = lock(&ctx.players)
        .get(&uuid)
        .map(|p| (p.profile.name.clone(), p.entity_id, p.game_mode))?;
    let bypass = BYPASSES_INVULNERABILITY.contains(&damage_type);
    if !(takes_damage(game_mode) || bypass && game_mode == 1) || amount <= 0.0 {
        return None;
    }
    if crate::damage::ruled_out(ctx, damage_type) {
        return None;
    }
    let (packet, died) = {
        let mut all = lock(&ctx.vitals);
        let v = all.get_mut(&uuid)?;
        if v.dead || crate::effects::fire_proof(&v.effects, damage_type) {
            return None;
        }
        // While immune only a harder hit gets through, by what it adds.
        let amount = if v.immune > 0 && !bypass {
            if amount <= v.last_hurt {
                return None;
            }
            let more = amount - v.last_hurt;
            v.last_hurt = amount;
            more
        } else {
            v.last_hurt = amount;
            v.immune = IMMUNE_TICKS;
            amount
        };
        let amount = if crate::damage::scales_always(ctx, damage_type) {
            crate::damage::scale_for_difficulty(amount, ctx.difficulty())
        } else {
            amount
        };
        let (armor, toughness) = crate::damage::worn_armor(ctx, uuid);
        let amount = crate::damage::armor_reduce(amount, armor, toughness, damage_type);
        let amount = crate::effects::reduce_damage(&v.effects, amount, damage_type);
        // Fully resisted still counts as a hit (vanilla's /damage succeeds).
        if amount <= 0.0 {
            return Some(false);
        }
        let absorbed = amount.min(v.absorption);
        if absorbed > 0.0 {
            v.absorption -= absorbed;
            let _ = ctx.events.send(Event::EntityData {
                uuid,
                packet: mc_protocol::packets::play::SetEntityData {
                    entity_id,
                    entries: vec![(
                        mc_protocol::packets::play::metadata::PLAYER_ABSORPTION,
                        mc_protocol::packets::play::MetadataValue::Float(v.absorption),
                    )],
                },
            });
        }
        let amount = amount - absorbed;
        v.health = (v.health - amount).max(0.0);
        v.exhaust(crate::damage::exhaustion(ctx, damage_type));
        if let Some(by) = attacker {
            v.since_attacked = 0;
            v.last_attacker = Some(by.to_owned());
        }
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
        direct,
    });
    if died {
        let _ = ctx.events.send(Event::Died {
            uuid,
            entity_id,
            message: match attacker {
                Some(by) => death_message(death_key, &[&name, by]),
                None => death_message(death_key, &[&name]),
            },
        });
    }
    Some(died)
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
        let (safe, floating, assist) = {
            let all = lock(&ctx.vitals);
            all.get(&uuid).map_or((3.0, false, None), |v| {
                (
                    v.effects.value(crate::effects::Attr::SafeFallDistance),
                    v.effects.has("slow_falling") || v.effects.has("levitation"),
                    (v.since_attacked < ASSIST_TICKS)
                        .then(|| v.last_attacker.clone())
                        .flatten(),
                )
            })
        };
        let damage = (fell - safe).ceil();
        if damage > 0.0 && !floating {
            let below = mc_protocol::Position::new(
                after.x.floor() as i32,
                (after.y - 0.2).floor() as i32,
                after.z.floor() as i32,
            );
            let block = ctx
                .world
                .block(below, &ctx.data.blocks)
                .and_then(|s| ctx.data.blocks.block_name(s));
            let key = crate::damage::fall_key(block, assist.is_some());
            hurt_by(
                ctx,
                uuid,
                damage as f32,
                "minecraft:fall",
                key,
                assist.as_deref(),
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
            v.since_attacked = v.since_attacked.saturating_add(1);
            if v.dead || !takes_damage(game_mode) {
                continue;
            }
            let before = (v.health, v.food, v.saturation);
            v.food_timer += 1;
            let full = v.food >= 20 && v.saturation > 0.0;
            let max_health = v.max_health();
            if v.health < max_health
                && ctx.game_rule_bool("natural_health_regeneration")
                && (full && v.food_timer >= 10 || v.food >= 18 && v.food_timer >= 80)
            {
                v.health = (v.health + 1.0).min(max_health);
                v.exhaust(6.0);
                v.food_timer = 0;
            } else if v.food <= 0 && v.food_timer >= 80 {
                v.food_timer = 0;
                let floor = match ctx.difficulty() {
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
        // Breathing: the head under water runs the air down.
        let eye = mc_protocol::Position::new(
            pose.x.floor() as i32,
            (pose.y + 1.62).floor() as i32,
            pose.z.floor() as i32,
        );
        let underwater = ctx
            .world
            .block(eye, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:water");
        let mut drowns = false;
        let mut air_change = None;
        if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
            let dry = v.effects.has("water_breathing") || v.effects.has("conduit_power");
            let (air, drown) = crate::damage::breathe(v.air, underwater && !dry);
            drowns = drown;
            if air != v.air {
                v.air = air;
                air_change = Some(air);
            }
        }
        if let (Some(air), Some(entity_id)) = (
            air_change,
            lock(&ctx.players).get(&uuid).map(|p| p.entity_id),
        ) {
            let _ = ctx.events.send(Event::EntityData {
                uuid,
                packet: mc_protocol::packets::play::SetEntityData {
                    entity_id,
                    entries: vec![(
                        mc_protocol::packets::play::metadata::AIR_SUPPLY,
                        mc_protocol::packets::play::MetadataValue::VarInt(air),
                    )],
                },
            });
        }
        if drowns {
            hurt(ctx, uuid, 2.0, "minecraft:drown", "death.attack.drown");
        }
        if let Some((amount, kind, key)) = hurt_by {
            hurt(ctx, uuid, amount, kind, key);
        }
    }
}

/// Kills a player whatever their game mode or immunity (`/kill`).
pub fn kill(ctx: &ServerContext, uuid: u128, damage_type: &str, death_key: &str) {
    if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
        v.immune = 0;
        v.health = f32::MIN_POSITIVE;
    }
    let mode = lock(&ctx.players).get(&uuid).map(|p| p.game_mode);
    if let Some(p) = lock(&ctx.players).get_mut(&uuid) {
        p.game_mode = 0;
    }
    hurt(ctx, uuid, f32::MAX, damage_type, death_key);
    if let (Some(mode), Some(p)) = (mode, lock(&ctx.players).get_mut(&uuid)) {
        p.game_mode = mode;
    }
}

/// A dead player respawns with full health; with `keep_inventory` they
/// keep their experience.
pub fn respawn(ctx: &ServerContext, uuid: u128) -> SetHealth {
    let keep = ctx.game_rule_bool("keep_inventory");
    let mut all = lock(&ctx.vitals);
    let v = all.entry(uuid).or_default();
    let old = std::mem::take(v);
    if keep {
        (v.xp_level, v.xp_progress, v.xp_total) = (old.xp_level, old.xp_progress, old.xp_total);
    }
    v.packet()
}

/// The experience a dying player drops (seven points a level, at most
/// 100), unless they are a spectator or `mob_drops` is off.
pub fn drop_death_xp(ctx: &ServerContext, uuid: u128, game_mode: i32, pos: [f64; 3]) {
    if game_mode == 3 || !ctx.game_rule_bool("mob_drops") {
        return;
    }
    let level = lock(&ctx.vitals).get(&uuid).map_or(0, |v| v.xp_level);
    let amount = (level * 7).min(100);
    if amount > 0 {
        crate::xp::spawn(ctx, pos, amount);
    }
}

/// Tells the players a death message, as the dead player's team allows:
/// everyone, only their team, everyone else, or no one.
pub fn announce_death(ctx: &ServerContext, uuid: u128, message: Tag) {
    let name = lock(&ctx.players).get(&uuid).map(|p| p.profile.name.clone());
    let (visibility, team) = {
        let board = lock(&ctx.scoreboard);
        match name.as_deref().and_then(|n| board.team_of(n)) {
            Some(t) => (t.death_messages, Some(t.name.clone())),
            None => (0, None),
        }
    };
    if visibility == 0 {
        let _ = ctx.events.send(Event::Chat(message));
        return;
    }
    let players: Vec<(u128, String)> = lock(&ctx.players)
        .values()
        .map(|p| (p.profile.uuid, p.profile.name.clone()))
        .collect();
    let board = lock(&ctx.scoreboard);
    for (to, n) in players {
        let same = board.team_of(&n).map(|t| &t.name) == team.as_ref();
        let sees = match visibility {
            2 => same,
            3 => !same,
            _ => false,
        };
        if sees {
            let _ = ctx.events.send(Event::Message {
                uuid: to,
                line: message.clone(),
                overlay: false,
            });
        }
    }
}
