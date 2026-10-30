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
    /// Ticks left burning (`Fire`), below 0 the time fire takes to catch.
    pub fire: i32,
    /// Gliding with an elytra.
    pub gliding: bool,
    /// Horizontal speed of the last glide step, in blocks. A wall takes
    /// the greater of this and the step that hits it.
    pub(crate) glide_speed: f64,
    /// The last glide step already ended against a wall.
    pub(crate) struck_wall: bool,
    /// Where a push (a wind burst, a mace smash) launched the player: a
    /// fall counts only below it.
    pub impulse_y: Option<f64>,
    /// A landing whose damage waits until this tick's attacks have run,
    /// so a mace smash can cancel the fall it ends.
    pub pending_fall: Option<f64>,
    /// What the enchantments of the gear do over time.
    pub worn: crate::worn::Worn,
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
            fire: -20,
            gliding: false,
            glide_speed: 0.0,
            struck_wall: false,
            impulse_y: None,
            pending_fall: None,
            worn: crate::worn::Worn::default(),
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
    if let Some(entity_id) = crate::spear::stop(uuid) {
        let _ = ctx.events.send(Event::UsingItem {
            uuid,
            entity_id,
            hand: None,
        });
    }
    crate::archaeology::stop(ctx, uuid);
    crate::criteria::spyglass(uuid, None);
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
    let eaten;
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
        eaten = hands.slots[using.slot].clone();
        if using.game_mode != 1 {
            let stack = hands.slots[using.slot].as_mut().expect("checked");
            stack.count -= 1;
            if stack.count <= 0 {
                hands.slots[using.slot] = None;
            }
            changed.push(using.slot);
            if let Some(id) = info.remainder.as_deref().and_then(|n| ctx.items.id(n)) {
                if hands.slots[using.slot].is_none() {
                    hands.slots[using.slot] = Some(mc_protocol::packets::play::ItemStack {
                        count: 1,
                        item: id,
                        damage: 0,
                        components: Default::default(),
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
                                    components: Default::default(),
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
    if let Some(stack) = &eaten {
        crate::consume::eaten(ctx, uuid, stack);
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
        .enumerate()
        .map(|(i, n)| {
            let mut c = Compound::new();
            if i == 1 && key == "death.attack.badRespawnPoint.message" {
                // A link to the bug report, as the game shows it.
                c.insert(
                    "translate",
                    Tag::String("death.attack.badRespawnPoint.link".into()),
                );
                let mut click = Compound::new();
                click.insert("action", Tag::String("open_url".into()));
                click.insert(
                    "url",
                    Tag::String("https://bugs.mojang.com/browse/MCPE-28723".into()),
                );
                c.insert("click_event", Tag::Compound(click));
                let mut hover = Compound::new();
                hover.insert("action", Tag::String("show_text".into()));
                hover.insert("value", Tag::String("MCPE-28723".into()));
                c.insert("hover_event", Tag::Compound(hover));
                return Tag::Compound(c);
            }
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
    if attacker.is_some_and(|by| !may_harm(ctx, by, &name)) {
        return None;
    }
    // Before the vitals are locked: the attributes read them.
    let (armor, toughness) = crate::damage::worn_armor(ctx, uuid);
    let (packet, died, tally) = {
        let mut all = lock(&ctx.vitals);
        let v = all.get_mut(&uuid)?;
        if v.dead || crate::effects::fire_proof(&v.effects, damage_type) {
            return None;
        }
        if crate::enchant_effects::immune(ctx, uuid, damage_type) {
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
        let before_cuts = amount;
        let amount = crate::damage::armor_reduce(amount, armor, toughness, damage_type);
        let amount = crate::effects::reduce_damage(&v.effects, amount, damage_type);
        let amount = if amount > 0.0 {
            crate::enchant_effects::protect(ctx, uuid, amount, damage_type)
        } else {
            amount
        };
        let resisted = (before_cuts - amount).max(0.0);
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
        let tally = [amount, absorbed, resisted].map(|d| (d * 10.0).round() as i32);
        v.health = (v.health - amount).max(0.0);
        v.exhaust(crate::damage::exhaustion(ctx, damage_type));
        if let Some(by) = attacker {
            v.since_attacked = 0;
            v.last_attacker = Some(by.to_owned());
        }
        let died = v.health <= 0.0;
        v.dead = died;
        (v.packet(), died, tally)
    };
    // A totem in hand cheats death, though not the void or /kill.
    let (packet, died) = if died && !bypass && undying(ctx, uuid, entity_id) {
        let packet = lock(&ctx.vitals).get(&uuid).map(Vitals::packet)?;
        (packet, false)
    } else {
        (packet, died)
    };
    // A hurt sleeper wakes.
    crate::useblocks::wake(ctx, uuid);
    let infested = lock(&ctx.vitals)
        .get(&uuid)
        .is_some_and(|v| v.effects.active.contains_key("infested"));
    let pose = lock(&ctx.players).get(&uuid).and_then(|p| p.pose);
    if let (true, Some(pose)) = (infested, pose) {
        crate::mobs::aftermath::infested(ctx, [pose.x, pose.y, pose.z], 1.8, pose.yaw, pose.pitch);
    }
    for (stat, n) in ["damage_taken", "damage_absorbed", "damage_resisted"]
        .into_iter()
        .zip(tally)
    {
        crate::stats::custom(ctx, uuid, stat, n);
    }
    let hit = crate::criteria::Hit {
        kind: crate::enchant_effects::full(damage_type),
        taken: tally[0] as f32 / 10.0,
        dealt: amount,
        ..Default::default()
    };
    crate::criteria::hurt_by(ctx, uuid, &hit);
    if died {
        crate::criteria::killed_by(ctx, uuid, &hit);
    }
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
        let at = lock(&ctx.players)
            .get(&uuid)
            .and_then(|p| p.pose)
            .map(|p| [p.x, p.y, p.z])
            .unwrap_or([0.0; 3]);
        crate::mobs::anger::note_death(ctx, entity_id, at);
        let carried = lock(&ctx.vitals).get(&uuid).map(|v| v.effects.clone());
        if let Some(effects) = carried {
            crate::mobs::aftermath::killed(ctx, entity_id, at, (0.6, 1.8), true, &effects);
        }
        crate::stats::died(ctx, uuid, attacker);
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

/// A held item with `death_protection` (the totem of undying) saves a
/// dying player: it is used up, they keep 1 health, its death effects
/// apply (minecraft.wiki, "Totem of Undying"). The main hand is looked at
/// first.
fn undying(ctx: &ServerContext, uuid: u128, entity_id: i32) -> bool {
    let Some((held, protection)) = [0, 1].into_iter().find_map(|hand| {
        let h = crate::held::held(ctx, uuid, hand)?;
        let p = ctx
            .items
            .component(&h.stack, "minecraft:death_protection")?;
        Some((h, p))
    }) else {
        return false;
    };
    crate::held::consume(ctx, uuid, held.slot, 1);
    if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
        v.health = 1.0;
        v.dead = false;
    }
    let pos = lock(&ctx.players)
        .get(&uuid)
        .and_then(|p| p.pose)
        .map_or([0.0; 3], |q| [q.x, q.y, q.z]);
    for effect in protection["death_effects"].as_array().into_iter().flatten() {
        crate::consume::consume_effect(ctx, uuid, effect, pos);
    }
    // Clients show the totem and play its sound.
    crate::fx::status(ctx, entity_id, 35);
    crate::stats::add(ctx, uuid, crate::stats::USED, &held.name, 1);
    crate::criteria::with_item(ctx, uuid, "minecraft:used_totem", &held.stack);
    true
}

/// A player moved: track falling and hurt them when they land.
pub fn moved(ctx: &ServerContext, uuid: u128, before: Pose, after: Pose, in_water: bool) {
    crate::criteria::moved(
        ctx,
        uuid,
        [before.x, before.y, before.z],
        before.on_ground,
        [after.x, after.y, after.z],
        after.on_ground,
        in_water,
    );
    // Actually flying, or a spectator: nothing is a fall. A creative
    // player who is not flying still falls, so a mace can smash; the
    // damage itself is refused later. Stopping the flight starts the
    // fall from there.
    let flies = lock(&ctx.players)
        .get(&uuid)
        .is_some_and(|p| p.flying || p.game_mode == 3);
    let mut wall_hit = None;
    let landed_from = {
        let mut all = lock(&ctx.vitals);
        let Some(v) = all.get_mut(&uuid) else {
            return;
        };
        if in_water || flies || v.gliding {
            // Deployed elytra erase the fall. A landing does not hurt;
            // running into a wall at speed does, once, from the speed.
            v.fall_distance = 0.0;
            if v.gliding {
                let (dx, dz) = (after.x - before.x, after.z - before.z);
                let horiz = dx.hypot(dz);
                let against = !after.on_ground && into_wall(ctx, after, dx, dz);
                let fresh = against && !v.struck_wall;
                let speed = horiz.max(v.glide_speed);
                v.glide_speed = horiz;
                v.struck_wall = against;
                if fresh {
                    let hit = speed * 10.0 - 3.0;
                    if hit > 0.0 {
                        wall_hit = Some(hit as f32);
                    }
                }
            } else {
                v.glide_speed = 0.0;
                v.struck_wall = false;
            }
            None
        } else if after.on_ground {
            // The move that lands is part of the fall.
            let mut fell = v.fall_distance + (before.y - after.y).max(0.0);
            if let Some(y) = v.impulse_y.take() {
                fell = fell.min((y - after.y).max(0.0));
            }
            v.fall_distance = 0.0;
            Some(fell)
        } else {
            if after.y < before.y {
                v.fall_distance += before.y - after.y;
            }
            None
        }
    };
    if let Some(hit) = wall_hit {
        hurt(
            ctx,
            uuid,
            hit,
            "minecraft:fly_into_wall",
            "death.attack.flyIntoWall",
        );
    }
    if let Some(fell) = landed_from {
        crate::stats::fell(ctx, uuid, fell);
        // Farmland gives way under a fall, the higher the likelier
        // (wiki, "Farmland" → "Trampling").
        let below = mc_protocol::Position::new(
            after.x.floor() as i32,
            (after.y - 0.2).floor() as i32,
            after.z.floor() as i32,
        );
        let roll = (crate::util::random_u64() >> 40) as f64 / (1u64 << 24) as f64;
        if roll < fell - 0.5 && !crate::adventure::under_spawn_protection(ctx, uuid, below) {
            let mut level = crate::level::WorldLevel::new(ctx);
            let logic = level.logic();
            if mc_tick::random::trample(&mut level, logic, crate::level::to_pos(below)) {
                let changes = level.into_changes();
                let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
            }
        }
        // The hurt waits: a smash queued before this landing may still
        // cancel what was fallen above the blow. A flat step has nothing
        // to cancel and must not clear a smash's mark.
        if fell > 0.0
            && let Some(v) = lock(&ctx.vitals).get_mut(&uuid)
        {
            v.pending_fall = Some(fell);
        }
    }
}

/// Fall damage for landings recorded since the last tick, after attacks.
/// A mace smash sets [`Vitals::impulse_y`] so only the drop below the blow
/// counts.
pub fn apply_pending_falls(ctx: &ServerContext) {
    let due: Vec<(u128, f64, Option<f64>)> = {
        let mut all = lock(&ctx.vitals);
        all.iter_mut()
            .filter_map(|(&uuid, v)| {
                let fell = v.pending_fall.take()?;
                Some((uuid, fell, v.impulse_y.take()))
            })
            .collect()
    };
    for (uuid, mut fell, impulse) in due {
        if let Some(y) = impulse {
            let ground = lock(&ctx.players)
                .get(&uuid)
                .and_then(|p| p.pose)
                .map(|p| p.y)
                .unwrap_or(y);
            fell = fell.min((y - ground).max(0.0));
        }
        hurt_for_fall(ctx, uuid, fell);
    }
}

/// A glide step ends in a solid block ahead of it. The box is the elytra's
/// (0.6 wide, 0.6 tall); the probe sits just past its leading face.
fn into_wall(ctx: &ServerContext, pose: Pose, dx: f64, dz: f64) -> bool {
    let solid = |x: f64, y: f64, z: f64| {
        let at = mc_protocol::Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        ctx.world
            .block(at, &ctx.data.blocks)
            .is_some_and(|s| ctx.rules.blocks_motion(s))
    };
    let ys = [pose.y + 0.1, pose.y + 0.4];
    let face = |along: f64, at: f64| along.signum() * 0.35 + at;
    let mut hit = false;
    if dx.abs() >= 0.05 {
        let x = face(dx, pose.x);
        hit |= ys.iter().any(|&y| solid(x, y, pose.z));
    }
    if dz.abs() >= 0.05 {
        let z = face(dz, pose.z);
        hit |= ys.iter().any(|&y| solid(pose.x, y, z));
    }
    hit
}

/// Whole blocks past the safe distance (minecraft.wiki, "Damage" →
/// "Fall damage"): a jump off two blocks, 3.25 from its top, is safe.
fn hurt_for_fall(ctx: &ServerContext, uuid: u128, fell: f64) {
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
    let damage = (fell - safe).floor();
    if damage > 0.0 && !floating {
        let below = lock(&ctx.players).get(&uuid).and_then(|p| p.pose).map(|p| {
            mc_protocol::Position::new(
                p.x.floor() as i32,
                (p.y - 0.2).floor() as i32,
                p.z.floor() as i32,
            )
        });
        let block = below
            .and_then(|at| ctx.world.block(at, &ctx.data.blocks))
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

/// One tick for every player: immunity wears off, hunger heals or hurts,
/// lava, fire and the void hurt.
pub fn tick(ctx: &ServerContext) {
    let players: Vec<(u128, i32, Pose)> = lock(&ctx.players)
        .values()
        .filter_map(|p| p.pose.map(|pose| (p.profile.uuid, p.game_mode, pose)))
        .collect();
    for (uuid, game_mode, pose) in players {
        let here = ctx.of_player(uuid);
        let ctx = &*here;
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
        } else if let Some(amount) = ctx
            .world
            .block(feet, &ctx.data.blocks)
            .and_then(|s| crate::campfires::damage(ctx, s))
            && !frost_walking(ctx, uuid)
        {
            hurt_by = Some((amount, "minecraft:campfire", "death.attack.inFire"));
        }
        if hurt_by.is_none()
            && let Some(hit) = stepped_on(ctx, uuid, &pose)
        {
            hurt_by = Some(hit);
        }
        if hurt_by.is_none() && smothered(ctx, uuid, &pose) {
            hurt_by = Some((1.0, "minecraft:in_wall", "death.attack.inWall"));
        }
        // A wither rose the body overlaps gives Wither for two seconds,
        // refreshed while they stay in it.
        if crate::hits::overlaps_block(
            ctx,
            [pose.x - 0.3, pose.y, pose.z - 0.3],
            [pose.x + 0.3, pose.y + 1.8, pose.z + 0.3],
            "minecraft:wither_rose",
        ) && let Some(kind) = crate::effects::kind_of("wither")
        {
            crate::effects::give(ctx, uuid, kind, crate::effects::Instance::new(0, 40, true));
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
        if block == Some("minecraft:lava") && game_mode != 3 {
            crate::worn::ignite(ctx, uuid, 300);
        } else if matches!(block, Some("minecraft:fire" | "minecraft:soul_fire")) && game_mode != 3
        {
            crate::worn::in_fire(ctx, uuid);
        }
        let mut drowns = false;
        let mut air_change = None;
        // Respiration: each tick under water keeps the air with a chance of
        // bonus / (bonus + 1).
        let oxygen = if underwater {
            crate::enchant_effects::attribute(ctx, uuid, "minecraft:oxygen_bonus")
        } else {
            0.0
        };
        let holds_breath = oxygen > 0.0
            && (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64
                >= 1.0 / (f64::from(oxygen) + 1.0);
        if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
            let dry = v.effects.has("water_breathing")
                || v.effects.has("conduit_power")
                || v.effects.has("breath_of_the_nautilus");
            let (air, drown) = if underwater && holds_breath {
                (v.air, false)
            } else {
                crate::damage::breathe(v.air, underwater && !dry)
            };
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

/// Eyes inside a full block. The eye height is the pose that fits: standing,
/// sneaking, or crawling in a one-block gap. Crawling keeps the eyes in the
/// air cell, so a ceiling does not suffocate.
fn smothered(ctx: &ServerContext, uuid: u128, pose: &crate::context::Pose) -> bool {
    let eye = pose.y + eye_height(ctx, uuid, pose);
    let half = 0.24;
    let y = eye.floor() as i32;
    let xs = [
        (pose.x - half).floor() as i32,
        (pose.x + half).floor() as i32,
    ];
    let zs = [
        (pose.z - half).floor() as i32,
        (pose.z + half).floor() as i32,
    ];
    xs.iter().any(|&x| {
        zs.iter().any(|&z| {
            ctx.world
                .block(mc_protocol::Position::new(x, y, z), &ctx.data.blocks)
                .is_some_and(|state| crate::mobs::suffocates(ctx, state))
        })
    })
}

/// Standing 1.62, sneaking 1.27, swimming and crawling 0.4, asleep 0.2.
fn eye_height(ctx: &ServerContext, uuid: u128, pose: &crate::context::Pose) -> f64 {
    if lock(&ctx.sleepers).contains_key(&uuid) {
        return 0.2;
    }
    let gliding = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.gliding);
    let swimming = crate::stats::is_sprinting(ctx, uuid)
        && ctx
            .world
            .block(
                mc_protocol::Position::new(
                    pose.x.floor() as i32,
                    (pose.y + 0.4).floor() as i32,
                    pose.z.floor() as i32,
                ),
                &ctx.data.blocks,
            )
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:water");
    let sneaking = crate::stats::sneaking(ctx, uuid);
    let (height, eye) = if gliding || swimming {
        (0.6, 0.4)
    } else if sneaking {
        (1.5, 1.27)
    } else {
        (1.8, 1.62)
    };
    if fits(ctx, pose, height) {
        return eye;
    }
    if height > 1.5 && fits(ctx, pose, 1.5) {
        return 1.27;
    }
    0.4
}

/// Whether a 0.6-wide box of `height` at the feet misses every block.
fn fits(ctx: &ServerContext, pose: &crate::context::Pose, height: f64) -> bool {
    const EPS: f64 = 1.0e-6;
    let min = [pose.x - 0.3 + EPS, pose.y + EPS, pose.z - 0.3 + EPS];
    let max = [
        pose.x + 0.3 - EPS,
        pose.y + height - EPS,
        pose.z + 0.3 - EPS,
    ];
    let x0 = min[0].floor() as i32;
    let y0 = min[1].floor() as i32;
    let z0 = min[2].floor() as i32;
    let x1 = max[0].floor() as i32;
    let y1 = max[1].floor() as i32;
    let z1 = max[2].floor() as i32;
    let mut boxes = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                boxes.clear();
                crate::vehicles::boat::shape_at(ctx, x, y, z, &mut boxes);
                if boxes.iter().any(|b| {
                    min[0] < b.max[0]
                        && max[0] > b.min[0]
                        && min[1] < b.max[1]
                        && max[1] > b.min[1]
                        && min[2] < b.max[2]
                        && max[2] > b.min[2]
                }) {
                    return false;
                }
            }
        }
    }
    true
}

/// Magma under the feet, and a cactus or a grown sweet berry bush the
/// body overlaps. Sneaking keeps magma from hurting; the plants do not care.
fn stepped_on(
    ctx: &ServerContext,
    uuid: u128,
    pose: &crate::context::Pose,
) -> Option<(f32, &'static str, &'static str)> {
    let blocks = &ctx.data.blocks;
    let named = |x: i32, y: i32, z: i32| {
        let state = ctx
            .world
            .block(mc_protocol::Position::new(x, y, z), blocks)?;
        let name = blocks.block_name(state)?;
        Some((state, name))
    };
    if pose.on_ground && !crate::stats::sneaking(ctx, uuid) {
        let under = (pose.y - 0.2).floor() as i32;
        if named(pose.x.floor() as i32, under, pose.z.floor() as i32)
            .is_some_and(|(_, n)| n == "minecraft:magma_block")
        {
            return Some((1.0, "minecraft:hot_floor", "death.attack.hotFloor"));
        }
    }
    let x0 = (pose.x - 0.3).floor() as i32;
    let x1 = (pose.x + 0.3).floor() as i32;
    let z0 = (pose.z - 0.3).floor() as i32;
    let z1 = (pose.z + 0.3).floor() as i32;
    let y0 = pose.y.floor() as i32;
    let y1 = (pose.y + 1.8 - 1.0e-6).floor() as i32;
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let Some((state, name)) = named(x, y, z) else {
                    continue;
                };
                if name == "minecraft:cactus" {
                    return Some((1.0, "minecraft:cactus", "death.attack.cactus"));
                }
                if name == "minecraft:sweet_berry_bush"
                    && blocks.property(state, "age").is_some_and(|a| a != "0")
                {
                    return Some((
                        1.0,
                        "minecraft:sweet_berry_bush",
                        "death.attack.sweetBerryBush",
                    ));
                }
            }
        }
    }
    None
}

/// Frost walker boots keep off the damage of stepping on hot blocks
/// (`burn_from_stepping`).
fn frost_walking(ctx: &ServerContext, uuid: u128) -> bool {
    let boots = lock(&ctx.inventories)
        .get(&uuid)
        .and_then(|h| h.slots[8].clone());
    boots.is_some_and(|b| crate::enchant_effects::has_effect(ctx, &b, "minecraft:damage_immunity"))
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

/// Whether player `by` may hurt player `victim`: always when `by` is not
/// a player or is the victim; else only with `pvp` on and, when they are
/// teammates, friendly fire allowed.
fn may_harm(ctx: &ServerContext, by: &str, victim: &str) -> bool {
    if by == victim || !lock(&ctx.players).values().any(|p| p.profile.name == by) {
        return true;
    }
    if !ctx.game_rule_bool("pvp") {
        return false;
    }
    let board = lock(&ctx.scoreboard);
    match (board.team_of(by), board.team_of(victim)) {
        (Some(a), Some(b)) if a.name == b.name => a.friendly_fire,
        _ => true,
    }
}

/// A dead player respawns with full health; with `keep_inventory` they
/// keep their experience. Until they are at the respawn point they are
/// nowhere, so the lava they died in does not set them alight again.
pub fn respawn(ctx: &ServerContext, uuid: u128) -> SetHealth {
    if let Some(p) = lock(&ctx.players).get_mut(&uuid) {
        p.pose = None;
    }
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

/// With `ender_pearls_vanish_on_death` on, the ender pearls a dying
/// player threw are gone.
pub fn drop_pearls(ctx: &ServerContext, uuid: u128) {
    if !ctx.game_rule_bool("ender_pearls_vanish_on_death") {
        return;
    }
    let mut removed = Vec::new();
    for e in lock(&ctx.entities).iter_mut() {
        if let mc_entity::Kind::Projectile(p) = &e.kind
            && matches!(p.shot, mc_entity::extra::Shot::Pearl)
            && p.owner == Some(uuid)
            && !e.removed
        {
            e.removed = true;
            removed.push(e.id);
        }
    }
    if !removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(removed));
    }
}

/// Tells the players a death message, as the dead player's team allows:
/// everyone, only their team, everyone else, or no one.
pub fn announce_death(ctx: &ServerContext, uuid: u128, message: Tag) {
    let name = lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone());
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

#[cfg(test)]
mod totem_tests {
    use super::*;
    use crate::testkit::{flat, join, stack, wear};

    const U: u128 = 0x7073;

    #[test]
    fn a_totem_in_the_off_hand_cheats_death_but_not_the_void() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 3, [0.5, -60.0, 0.5], 0);
        wear(&ctx, U, 45, stack(&ctx, "totem_of_undying", &[]));
        assert_eq!(
            try_hurt(
                &ctx,
                U,
                100.0,
                "minecraft:generic",
                "death.attack.generic",
                None
            ),
            Some(false)
        );
        {
            let all = lock(&ctx.vitals);
            let v = &all[&U];
            assert_eq!(v.health, 1.0);
            assert!(!v.dead);
            for effect in ["regeneration", "absorption", "fire_resistance"] {
                let kind = crate::effects::kind_of(effect).unwrap();
                assert!(v.effects.get(kind).is_some(), "no {effect}");
            }
        }
        assert!(lock(&ctx.inventories)[&U].slots[45].is_none(), "totem kept");
        // With another, the void still takes them.
        wear(&ctx, U, 45, stack(&ctx, "totem_of_undying", &[]));
        if let Some(v) = lock(&ctx.vitals).get_mut(&U) {
            v.immune = 0;
        }
        assert_eq!(
            try_hurt(
                &ctx,
                U,
                1000.0,
                "minecraft:out_of_world",
                "death.attack.outOfWorld",
                None
            ),
            Some(true)
        );
        assert!(lock(&ctx.inventories)[&U].slots[45].is_some());
    }

    #[test]
    fn a_jump_off_two_blocks_is_safe_and_five_blocks_hurt() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        crate::testkit::join(&ctx, 5, 5, [0.5, -50.0, 0.5], 0);
        let at = |y: f64, on_ground: bool| crate::context::Pose {
            x: 0.5,
            y,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground,
        };
        // Down from a jump's top 3.25 blocks, in steps, then landing.
        let fall = |from: f64, to: f64| {
            let mut y = from;
            while y - 0.5 > to {
                moved(&ctx, 5, at(y, false), at(y - 0.5, false), false);
                y -= 0.5;
            }
            moved(&ctx, 5, at(y, false), at(to, true), false);
        };
        let health = || lock(&ctx.vitals)[&5].health;
        fall(-50.0, -53.25);
        apply_pending_falls(&ctx);
        assert_eq!(health(), 20.0);
        fall(-53.25, -58.25);
        apply_pending_falls(&ctx);
        assert_eq!(health(), 18.0);
        // A drop started in creative still counts when they land in survival.
        lock(&ctx.players).get_mut(&5).unwrap().game_mode = 1;
        let mut y = -58.25;
        while y < -30.0 {
            moved(&ctx, 5, at(y, false), at(y + 1.0, false), false);
            y += 1.0;
        }
        moved(&ctx, 5, at(y, false), at(y - 20.0, false), false);
        lock(&ctx.players).get_mut(&5).unwrap().game_mode = 0;
        fall(y - 20.0, y - 21.0);
        apply_pending_falls(&ctx);
        assert!(health() < 18.0, "the creative drop was not forgiven");
    }

    fn air(x: f64, y: f64, on_ground: bool) -> crate::context::Pose {
        crate::context::Pose {
            x,
            y,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground,
        }
    }

    #[test]
    fn elytra_land_safely_and_a_fast_wall_hurts_once() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 8, 8, [0.5, -60.0, 0.5], 0);
        lock(&ctx.vitals).get_mut(&8).unwrap().gliding = true;
        let mut y = -40.0;
        while y > -59.5 {
            moved(&ctx, 8, air(0.5, y, false), air(0.5, y - 0.5, false), false);
            y -= 0.5;
        }
        moved(&ctx, 8, air(0.5, y, false), air(0.5, -60.0, true), false);
        apply_pending_falls(&ctx);
        assert_eq!(lock(&ctx.vitals)[&8].health, 20.0, "a glide lands softly");

        lock(&ctx.vitals).get_mut(&8).unwrap().gliding = true;
        crate::testkit::set(&ctx, [2, -59, 0], "minecraft:stone");
        moved(
            &ctx,
            8,
            air(0.5, -59.0, false),
            air(1.7, -59.0, false),
            false,
        );
        assert_eq!(lock(&ctx.vitals)[&8].health, 11.0, "speed into a wall");
        moved(
            &ctx,
            8,
            air(1.7, -59.0, false),
            air(1.72, -59.0, false),
            false,
        );
        assert_eq!(
            lock(&ctx.vitals)[&8].health,
            11.0,
            "scraping is not another hit"
        );

        if let Some(v) = lock(&ctx.vitals).get_mut(&8) {
            v.gliding = true;
            v.struck_wall = false;
            v.glide_speed = 0.0;
            v.health = 20.0;
            v.immune = 0;
        }
        moved(
            &ctx,
            8,
            air(1.5, -59.0, false),
            air(1.7, -59.0, false),
            false,
        );
        assert_eq!(lock(&ctx.vitals)[&8].health, 20.0, "a slow touch");
    }

    #[test]
    fn taking_the_elytra_off_in_flight_folds_the_wings() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 9, 9, [0.5, -40.0, 0.5], 0);
        crate::testkit::place(&ctx, 9, [0.5, -40.0, 0.5], false);
        wear(&ctx, 9, 6, stack(&ctx, "elytra", &[]));
        crate::worn::start_gliding(&ctx, 9);
        assert!(lock(&ctx.vitals)[&9].gliding);
        lock(&ctx.vitals).get_mut(&9).unwrap().fall_distance = 40.0;
        lock(&ctx.inventories).get_mut(&9).unwrap().slots[6] = None;
        crate::worn::settle_gliding(&ctx, 9);
        let v = &lock(&ctx.vitals)[&9];
        assert!(!v.gliding);
        assert_eq!(v.fall_distance, 0.0);
    }

    #[test]
    fn a_player_who_died_in_lava_respawns_without_burning() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, U, 3, [0.5, -60.0, 0.5], 0);
        crate::testkit::set(&ctx, [0, -60, 0], "minecraft:lava");
        while !lock(&ctx.vitals)[&U].dead {
            tick(&ctx);
        }
        respawn(&ctx, U);
        for _ in 0..40 {
            tick(&ctx);
        }
        let v = &lock(&ctx.vitals)[&U];
        assert!(
            v.fire <= 0 && v.health == 20.0,
            "fire {} health {}",
            v.fire,
            v.health
        );
    }

    fn put(ctx: &ServerContext, at: [i32; 3], name: &str, props: &[(&str, &str)]) {
        let id = format!("minecraft:{name}");
        let state = if props.is_empty() {
            ctx.data.blocks.default_state(&id).unwrap()
        } else {
            ctx.data.blocks.state_id(&id, props).unwrap()
        };
        ctx.world.set_block(
            mc_protocol::Position::new(at[0], at[1], at[2]),
            state,
            &ctx.data.blocks,
            &ctx.rules,
        );
    }

    fn health(ctx: &ServerContext, uuid: u128) -> f32 {
        lock(&ctx.vitals).get(&uuid).unwrap().health
    }

    #[test]
    fn magma_cactus_and_berry_bushes_hurt_and_sneaking_spares_magma() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 11, 11, [0.5, -60.0, 0.5], 0);
        put(&ctx, [0, -61, 0], "magma_block", &[]);
        tick(&ctx);
        assert_eq!(health(&ctx, 11), 19.0);
        crate::stats::load(&ctx, 11);
        crate::stats::set_sneaking(&ctx, 11, true);
        if let Some(v) = lock(&ctx.vitals).get_mut(&11) {
            v.immune = 0;
            v.health = 20.0;
        }
        tick(&ctx);
        assert_eq!(health(&ctx, 11), 20.0);

        join(&ctx, 12, 12, [4.5, -60.0, 0.5], 0);
        put(&ctx, [4, -60, 0], "cactus", &[]);
        tick(&ctx);
        assert_eq!(health(&ctx, 12), 19.0);

        join(&ctx, 13, 13, [8.5, -60.0, 0.5], 0);
        put(&ctx, [8, -60, 0], "sweet_berry_bush", &[("age", "0")]);
        tick(&ctx);
        assert_eq!(health(&ctx, 13), 20.0);
        put(&ctx, [8, -60, 0], "sweet_berry_bush", &[("age", "2")]);
        tick(&ctx);
        assert_eq!(health(&ctx, 13), 19.0);

        join(&ctx, 15, 15, [16.5, -60.0, 0.5], 0);
        put(&ctx, [16, -60, 0], "wither_rose", &[]);
        tick(&ctx);
        assert!(
            lock(&ctx.vitals)[&15].effects.has("wither"),
            "a wither rose gives wither"
        );

        join(&ctx, 14, 14, [12.5, -60.0, 0.5], 0);
        put(&ctx, [12, -61, 0], "magma_block", &[]);
        wear(
            &ctx,
            14,
            8,
            stack(&ctx, "iron_boots", &[("frost_walker", 2)]),
        );
        tick(&ctx);
        assert_eq!(health(&ctx, 14), 20.0);

        join(&ctx, 16, 16, [20.5, -60.0, 0.5], 0);
        put(&ctx, [20, -60, 0], "stone", &[]);
        put(&ctx, [20, -59, 0], "stone", &[]);
        tick(&ctx);
        assert_eq!(health(&ctx, 16), 19.0, "stone around the body suffocates");

        join(&ctx, 17, 17, [24.5, -60.0, 0.5], 0);
        put(&ctx, [24, -59, 0], "stone", &[]);
        tick(&ctx);
        assert_eq!(health(&ctx, 17), 20.0, "a one-block gap is a crawl");

        join(&ctx, 18, 18, [28.5, -60.0, 0.5], 0);
        put(&ctx, [28, -60, 0], "glass", &[]);
        put(&ctx, [28, -59, 0], "glass", &[]);
        tick(&ctx);
        assert_eq!(health(&ctx, 18), 20.0, "glass does not suffocate");
    }
}
