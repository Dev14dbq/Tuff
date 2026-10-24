//! What enchantments on a player's gear do over time, and burning
//! (minecraft.wiki, "Soul Speed", "Frost Walker", "Swift Sneak", "Depth
//! Strider", "Efficiency", "Aqua Affinity", "Fire Protection", "Fire" →
//! "Burning"):
//! - the `attributes` of the enchantments in the slots they act from are
//!   modifiers on the player's attributes, which clients move and dig by;
//!   they are sent again whenever the gear changes;
//! - `location_changed` effects run when the player steps into another
//!   block (or the item arrives): Soul Speed's speed while on soul blocks
//!   (and its wear), Frost Walker's disk of frosted ice;
//! - `tick` effects: Soul Speed's souls and sounds;
//! - a burning player loses a point of health a second and goes out in
//!   water and rain; how long fire lasts is scaled by `burning_time`.

use std::collections::BTreeMap;

use mc_protocol::Position;
use mc_protocol::packets::play::{AttributeModifier, ItemStack, UpdateAttributes};
use serde_json::Value;

use crate::context::{Event, ServerContext};
use crate::enchant_effects::{self as ee, Mover, Situation};
use crate::tick::lock;

/// A location effect that is on: (equipment slot, enchantment, index of
/// the effect) and the modifiers it put on.
type Active = (
    &'static str,
    String,
    usize,
    Vec<(String, AttributeModifier)>,
);
/// What a slot holds: (slot, item, enchantments).
type SlotSig = (&'static str, i32, Vec<(String, i32)>);

/// A player's enchantment state between ticks.
#[derive(Clone, Debug, Default)]
pub struct Worn {
    /// Modifiers enchantments put on now, by attribute (`minecraft:...`).
    mods: BTreeMap<String, Vec<AttributeModifier>>,
    /// Location effects that are on: (equipment slot, enchantment, index of
    /// the effect) and the modifiers they put on.
    active: Vec<Active>,
    /// What was worn last tick: (slot, item, enchantments).
    gear: Vec<SlotSig>,
    last_block: Option<[i32; 3]>,
    last_pos: Option<[f64; 3]>,
    age: i64,
    /// Ticks spent gliding.
    glided: i32,
    /// The equipment others were last shown.
    shown: Option<[Option<ItemStack>; 6]>,
}

impl Worn {
    /// The modifiers enchantments put on an attribute (`minecraft:...`).
    pub fn modifiers(&self, attribute: &str) -> &[AttributeModifier] {
        self.mods.get(attribute).map_or(&[], Vec::as_slice)
    }
}

/// Whether it rains on `p`: the sky open above and rain in the biome.
pub fn rain_at(ctx: &ServerContext, p: [f64; 3]) -> bool {
    if !lock(&ctx.weather).raining {
        return false;
    }
    let (x, y, z) = (
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    );
    if !sees_sky(ctx, [x, y, z]) {
        return false;
    }
    let blocks = &ctx.data.blocks;
    let biome = ctx.world.biome(Position::new(x, y, z), blocks).unwrap_or(0);
    ctx.world.pipeline().is_none_or(|pl| {
        pl.features.has_precipitation(biome)
            && pl.features.temperature(biome, mc_features::P::new(x, y, z)) >= 0.15
    })
}

/// Whether nothing that stops motion or holds fluid is above the block.
pub fn sees_sky(ctx: &ServerContext, p: [i32; 3]) -> bool {
    let blocks = &ctx.data.blocks;
    ctx.world
        .top_where(p[0], p[2], |s| {
            ctx.rules.blocks_motion(s) || blocks.is_fluid(s)
        })
        .is_none_or(|top| p[1] >= top)
}

fn block_name(ctx: &ServerContext, p: [i32; 3]) -> Option<String> {
    let blocks = &ctx.data.blocks;
    ctx.world
        .block(Position::new(p[0], p[1], p[2]), blocks)
        .and_then(|s| blocks.block_name(s))
        .map(str::to_owned)
}

fn in_vehicle(ctx: &ServerContext, entity_id: i32) -> bool {
    lock(&ctx.entities).iter().any(|e| match &e.kind {
        mc_entity::Kind::Vehicle(v) => v.riders.contains(&entity_id),
        _ => false,
    })
}

/// Sets a player alight for `ticks` (lava 300, fire 160): fire lasts
/// `burning_time` as long, and never gets shorter.
pub fn ignite(ctx: &ServerContext, uuid: u128, ticks: i32) {
    let factor = crate::commands::gear::value(ctx, uuid, "burning_time");
    let ticks = (f64::from(ticks) * factor).ceil() as i32;
    let lit = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.fire > 0);
    if lit {
        if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
            v.fire = v.fire.max(ticks);
        }
    } else {
        crate::effects::change(ctx, uuid, |v| v.fire = v.fire.max(ticks));
    }
}

/// A player in a fire block: the fire catches after a while and burns
/// for 8 seconds.
pub fn in_fire(ctx: &ServerContext, uuid: u128) {
    let fire = lock(&ctx.vitals).get(&uuid).map_or(0, |v| v.fire);
    if fire < 0 {
        if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
            v.fire += 1;
        }
        if fire + 1 < 0 {
            return;
        }
    } else if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
        v.fire += 1 + (crate::util::random_u64() % 2) as i32;
    }
    ignite(ctx, uuid, 160);
}

/// Puts a burning player out.
pub fn extinguish(ctx: &ServerContext, uuid: u128) {
    let lit = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.fire > 0);
    if lit {
        crate::effects::change(ctx, uuid, |v| v.fire = 0);
    }
}

/// What others see a player hold and wear, by equipment slot: main hand,
/// off hand, feet, legs, chest, head.
pub fn equipment(ctx: &ServerContext, uuid: u128) -> [Option<ItemStack>; 6] {
    let inventories = lock(&ctx.inventories);
    let Some(h) = inventories.get(&uuid) else {
        return Default::default();
    };
    let main = h.hand_slot(0);
    [main, 45, 8, 7, 6, 5].map(|i| h.slots[i].clone())
}

/// Tells others what changed of a player's equipment (a stack that wore,
/// broke, was put on or taken off).
fn show_equipment(ctx: &ServerContext, uuid: u128, entity_id: i32, worn: &mut Worn) {
    let now = equipment(ctx, uuid);
    let slots: Vec<(u8, Option<ItemStack>)> = match &worn.shown {
        None => Vec::new(),
        Some(was) => (0..6)
            .filter(|&i| was[i] != now[i])
            .map(|i| (i as u8, now[i].clone()))
            .collect(),
    };
    worn.shown = Some(now);
    if !slots.is_empty() {
        let _ = ctx.events.send(Event::Equipment {
            uuid,
            entity_id,
            slots,
        });
    }
}

/// The chest slot holds a glider (an elytra) that is not about to break.
fn has_glider(ctx: &ServerContext, uuid: u128) -> bool {
    lock(&ctx.inventories)
        .get(&uuid)
        .and_then(|h| h.slots[6].clone())
        .is_some_and(|s| {
            let glides = s.components.get("minecraft:glider").is_some()
                || ctx.items.default_component(s.item, "glider").is_some()
                    && !s.components.is_removed("minecraft:glider");
            let max = ctx.items.max_damage(s.item);
            glides && (max == 0 || s.damage < max - 1)
        })
}

/// The player asks to glide (Player Command 6): in the air, out of water,
/// not levitating, with a glider on.
pub fn start_gliding(ctx: &ServerContext, uuid: u128) {
    let Some(pose) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose) else {
        return;
    };
    let levitating = lock(&ctx.vitals)
        .get(&uuid)
        .is_none_or(|v| v.gliding || v.effects.has("levitation"));
    if pose.on_ground || levitating || crate::hits::in_water(ctx, [pose.x, pose.y, pose.z]) {
        return;
    }
    if has_glider(ctx, uuid) {
        crate::effects::change(ctx, uuid, |v| v.gliding = true);
    }
}

/// One tick of gliding: it ends on the ground, in water or without a
/// glider; the elytra wears by 1 a second.
fn glide(
    ctx: &ServerContext,
    uuid: u128,
    mode: i32,
    pose: crate::context::Pose,
    flying: bool,
    worn: &mut Worn,
) {
    let gliding = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.gliding);
    if !gliding {
        worn.glided = 0;
        return;
    }
    if pose.on_ground
        || flying
        || crate::hits::in_water(ctx, [pose.x, pose.y, pose.z])
        || !has_glider(ctx, uuid)
    {
        worn.glided = 0;
        crate::effects::change(ctx, uuid, |v| v.gliding = false);
        return;
    }
    worn.glided += 1;
    if worn.glided % 20 == 0 && mode != 1 {
        crate::held::wear(ctx, uuid, 6, 1);
    }
}

/// One tick of every player: burning, gear and its effects.
pub fn tick(ctx: &ServerContext) {
    let players: Vec<(u128, i32, i32, crate::context::Pose, bool)> = lock(&ctx.players)
        .values()
        .filter_map(|p| {
            p.pose
                .map(|pose| (p.profile.uuid, p.entity_id, p.game_mode, pose, p.flying))
        })
        .collect();
    for (uuid, entity_id, mode, pose, flying) in players {
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        if lock(&ctx.vitals).get(&uuid).is_none_or(|v| v.dead) {
            continue;
        }
        burn(ctx, uuid, mode, pose);
        gear_tick(ctx, uuid, entity_id, mode, pose, flying);
    }
}

fn burn(ctx: &ServerContext, uuid: u128, mode: i32, pose: crate::context::Pose) {
    let at = [pose.x, pose.y, pose.z];
    let fire = lock(&ctx.vitals).get(&uuid).map_or(0, |v| v.fire);
    if fire <= 0 {
        return;
    }
    let wet = crate::hits::in_water(ctx, at) || rain_at(ctx, at);
    if wet || mode == 3 {
        extinguish(ctx, uuid);
        if wet {
            crate::fx::sound(
                ctx,
                "entity.generic.extinguish_fire",
                crate::fx::category::PLAYERS,
                at,
                0.7,
                1.6 + (crate::hits::rnd() as f32 - crate::hits::rnd() as f32) * 0.4,
            );
        }
        return;
    }
    let feet = [
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    ];
    let in_lava = block_name(ctx, feet).as_deref() == Some("minecraft:lava");
    if fire % 20 == 0 && !in_lava {
        crate::vitals::hurt(ctx, uuid, 1.0, "minecraft:on_fire", "death.attack.onFire");
    }
    if fire == 1 {
        crate::effects::change(ctx, uuid, |v| v.fire = 0);
    } else if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
        v.fire -= 1;
    }
}

/// The item and enchantments worn in each slot, to see what changed.
fn signature(gear: &[(usize, &'static str, ItemStack)]) -> Vec<SlotSig> {
    gear.iter()
        .map(|(_, slot, s)| (*slot, s.item, crate::enchants::levels(s)))
        .collect()
}

fn gear_tick(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    mode: i32,
    pose: crate::context::Pose,
    flying: bool,
) {
    let gear = ee::gear(ctx, uuid);
    let Some((mut worn, fall_distance, gliding, food)) =
        lock(&ctx.vitals).get_mut(&uuid).map(|v| {
            (
                std::mem::take(&mut v.worn),
                v.fall_distance,
                v.gliding,
                v.food,
            )
        })
    else {
        return;
    };
    worn.age += 1;
    glide(ctx, uuid, mode, pose, flying, &mut worn);
    show_equipment(ctx, uuid, entity_id, &mut worn);
    let pos = [pose.x, pose.y, pose.z];
    let block = pos.map(|c| c.floor() as i32);
    let below = [block[0], (pose.y - 0.500_001).floor() as i32, block[2]];
    let horizontal = worn
        .last_pos
        .map_or(0.0, |l| (pos[0] - l[0]).hypot(pos[2] - l[2]));
    let mover = Mover {
        on_ground: pose.on_ground,
        flying,
        fall_flying: gliding,
        in_water: crate::hits::in_water(ctx, pos),
        in_vehicle: in_vehicle(ctx, entity_id),
        below: block_name(ctx, below),
        fall_distance,
        horizontal_speed: horizontal,
        can_see_sky: sees_sky(ctx, block),
        age: worn.age,
        creative: mode == 1,
        food,
    };
    let (raining, thundering) = {
        let w = lock(&ctx.weather);
        (w.raining, w.thundering)
    };
    let base = Situation {
        this: Some("minecraft:player"),
        mover: Some(&mover),
        raining,
        thundering,
        ..Situation::default()
    };

    // Gear that went or changed stops its location effects.
    let now = signature(&gear);
    let changed: Vec<&'static str> = ["head", "chest", "legs", "feet", "offhand", "mainhand"]
        .into_iter()
        .filter(|slot| now.iter().find(|g| g.0 == *slot) != worn.gear.iter().find(|g| g.0 == *slot))
        .collect();
    worn.active.retain(|a| !changed.contains(&a.0));
    let moved = worn.last_block != Some(block);
    let mut wear: Vec<(usize, i32)> = Vec::new();
    let mut disks: Vec<(Value, i32)> = Vec::new();
    for (slot_index, slot, stack) in &gear {
        if !(moved || changed.contains(slot)) {
            continue;
        }
        let item = ctx.items.name(stack.item).unwrap_or("").to_owned();
        for (name, level) in crate::enchants::levels(stack) {
            if !ee::acts_from(ctx, &name, slot) {
                continue;
            }
            let Some(e) = ctx.enchantments.get(&name) else {
                continue;
            };
            let entries = ee::effects_of(&e.effects["minecraft:location_changed"]);
            for (k, entry) in entries.iter().enumerate() {
                let at = worn
                    .active
                    .iter()
                    .position(|a| a.0 == *slot && a.1 == name && a.2 == k);
                let s = Situation {
                    level,
                    active: at.is_some(),
                    ..base
                };
                let ok = entry
                    .get("requirements")
                    .is_none_or(|r| ee::holds(ctx, r, &item, &s));
                if ok {
                    let mut added = Vec::new();
                    run_location(
                        &entry["effect"],
                        level,
                        slot,
                        &mut added,
                        &mut |what| match what {
                            Location::Wear(n) => wear.push((*slot_index, n)),
                            Location::Disk(d) => disks.push((d, level)),
                        },
                    );
                    if at.is_none() {
                        worn.active.push((slot, name.clone(), k, added));
                    }
                } else if let Some(i) = at {
                    worn.active.remove(i);
                }
            }
        }
    }
    worn.gear = now;
    worn.last_block = Some(block);
    worn.last_pos = Some(pos);

    // Every tick: Soul Speed's souls and sounds.
    for (_, slot, stack) in &gear {
        let item = ctx.items.name(stack.item).unwrap_or("").to_owned();
        for (name, level) in crate::enchants::levels(stack) {
            if !ee::acts_from(ctx, &name, slot) {
                continue;
            }
            let Some(e) = ctx.enchantments.get(&name) else {
                continue;
            };
            for entry in ee::effects_of(&e.effects["minecraft:tick"]) {
                let s = Situation { level, ..base };
                if entry
                    .get("requirements")
                    .is_none_or(|r| ee::holds(ctx, r, &item, &s))
                {
                    tick_effect(ctx, &entry["effect"], pos);
                }
            }
        }
    }

    // The modifiers now, and the attributes whose modifiers changed.
    let mut mods: BTreeMap<String, Vec<AttributeModifier>> = BTreeMap::new();
    for (attribute, m) in ee::gear_attributes(ctx, &gear)
        .into_iter()
        .chain(worn.active.iter().flat_map(|a| a.3.iter().cloned()))
    {
        mods.entry(attribute).or_default().push(m);
    }
    let names: Vec<String> = mods
        .keys()
        .chain(worn.mods.keys())
        .filter(|k| mods.get(*k) != worn.mods.get(*k))
        .cloned()
        .collect();
    worn.mods = mods;
    if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
        v.worn = worn;
    }
    let mut names = names;
    names.sort();
    names.dedup();
    let attributes: Vec<_> = names
        .iter()
        .filter_map(|n| crate::commands::gear::snapshot(ctx, uuid, n))
        .collect();
    if !attributes.is_empty() {
        let _ = ctx.events.send(Event::Attributes {
            uuid,
            packet: UpdateAttributes {
                entity_id,
                attributes,
            },
        });
    }
    if mode != 1 {
        for (slot, n) in wear {
            crate::held::wear(ctx, uuid, slot, n);
        }
    }
    for (disk, level) in disks {
        replace_disk(ctx, &disk, level, pos);
    }
}

/// What a location effect asks the world for.
enum Location {
    Wear(i32),
    Disk(Value),
}

/// Runs a `location_changed` effect: attribute modifiers are collected in
/// `added`, the rest is handed to `act`.
fn run_location(
    effect: &Value,
    level: i32,
    slot: &str,
    added: &mut Vec<(String, AttributeModifier)>,
    act: &mut dyn FnMut(Location),
) {
    match effect["type"].as_str().unwrap_or("") {
        "minecraft:all_of" => {
            for e in effect["effects"].as_array().into_iter().flatten() {
                run_location(e, level, slot, added, act);
            }
        }
        "minecraft:attribute" => added.push(ee::modifier(effect, level, slot)),
        "minecraft:change_item_damage" => {
            act(Location::Wear(ee::value(&effect["amount"], level) as i32));
        }
        "minecraft:replace_disk" => act(Location::Disk(effect.clone())),
        _ => {}
    }
}

/// A `tick` effect: a sound or particles at the player's feet.
fn tick_effect(ctx: &ServerContext, effect: &Value, pos: [f64; 3]) {
    match effect["type"].as_str().unwrap_or("") {
        "minecraft:all_of" => {
            for e in effect["effects"].as_array().into_iter().flatten() {
                tick_effect(ctx, e, pos);
            }
        }
        "minecraft:play_sound" => {
            let sound = match &effect["sound"] {
                Value::Array(l) if !l.is_empty() => {
                    l[(crate::util::random_u64() % l.len() as u64) as usize].as_str()
                }
                v => v.as_str(),
            };
            let Some(sound) = sound else {
                return;
            };
            let pitch = uniform(&effect["pitch"]);
            let volume = uniform(&effect["volume"]);
            crate::fx::sound(
                ctx,
                sound.trim_start_matches("minecraft:"),
                crate::fx::category::PLAYERS,
                pos,
                volume,
                pitch,
            );
        }
        "minecraft:spawn_particles" => {
            let Some(name) = effect["particle"]["type"].as_str() else {
                return;
            };
            let y = effect["vertical_position"]["offset"]
                .as_f64()
                .unwrap_or(0.0);
            crate::fx::particles(
                ctx,
                &ee::full(name),
                [pos[0], pos[1] + y, pos[2]],
                0.3,
                effect["speed"].as_f64().unwrap_or(0.0) as f32 * 0.1,
                1,
            );
        }
        _ => {}
    }
}

/// A float provider: a number or `uniform`.
fn uniform(v: &Value) -> f32 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(1.0) as f32,
        Value::Object(o) => {
            let lo = o["min_inclusive"].as_f64().unwrap_or(0.0);
            let hi = o["max_exclusive"].as_f64().unwrap_or(lo);
            (lo + crate::hits::rnd() * (hi - lo)) as f32
        }
        _ => 1.0,
    }
}

/// Whether a block predicate of `replace_disk` holds at `p`.
fn block_predicate(ctx: &ServerContext, pr: &Value, p: [i32; 3]) -> bool {
    let off = |pr: &Value| {
        let o = pr["offset"].as_array();
        let at = |i: usize| {
            o.and_then(|o| o.get(i))
                .and_then(Value::as_i64)
                .unwrap_or(0) as i32
        };
        [p[0] + at(0), p[1] + at(1), p[2] + at(2)]
    };
    let blocks = &ctx.data.blocks;
    let state_at = |q: [i32; 3]| ctx.world.block(Position::new(q[0], q[1], q[2]), blocks);
    match pr["type"].as_str().unwrap_or("") {
        "minecraft:all_of" => pr["predicates"]
            .as_array()
            .is_some_and(|l| l.iter().all(|x| block_predicate(ctx, x, p))),
        "minecraft:any_of" => pr["predicates"]
            .as_array()
            .is_some_and(|l| l.iter().any(|x| block_predicate(ctx, x, p))),
        "minecraft:not" => !block_predicate(ctx, &pr["predicate"], p),
        "minecraft:matching_block_tag" => {
            let tag = pr["tag"].as_str().unwrap_or("");
            state_at(off(pr))
                .and_then(|s| blocks.block_name(s))
                .is_some_and(|n| {
                    ctx.commands
                        .env
                        .tag("minecraft:block", tag)
                        .iter()
                        .any(|m| m == n)
                })
        }
        "minecraft:matching_blocks" => {
            let want = &pr["blocks"];
            state_at(off(pr))
                .and_then(|s| blocks.block_name(s))
                .is_some_and(|n| match want {
                    Value::String(w) => ee::full(w) == n,
                    Value::Array(l) => l
                        .iter()
                        .any(|w| w.as_str().is_some_and(|w| ee::full(w) == n)),
                    _ => false,
                })
        }
        "minecraft:matching_fluids" => {
            // `water` is a source; flowing water is `flowing_water`.
            let want = pr["fluids"].as_str().map(ee::full).unwrap_or_default();
            state_at(off(pr)).is_some_and(|s| {
                let name = blocks.block_name(s).unwrap_or("");
                let source = blocks.property(s, "level") == Some("0");
                match want.as_str() {
                    "minecraft:water" => name == "minecraft:water" && source,
                    "minecraft:flowing_water" => name == "minecraft:water" && !source,
                    "minecraft:lava" => name == "minecraft:lava" && source,
                    _ => false,
                }
            })
        }
        "minecraft:unobstructed" => {
            let q = off(pr);
            let cell = mc_entity::Aabb {
                min: [f64::from(q[0]), f64::from(q[1]), f64::from(q[2])],
                max: [
                    f64::from(q[0] + 1),
                    f64::from(q[1] + 1),
                    f64::from(q[2] + 1),
                ],
            };
            let hits = |b: &mc_entity::Aabb| {
                (0..3).all(|i| b.min[i] < cell.max[i] && b.max[i] > cell.min[i])
            };
            let players = lock(&ctx.players).values().any(|pl| {
                pl.game_mode != 3
                    && pl.dimension == ctx.world.dimension
                    && pl.pose.is_some_and(|po| {
                        hits(&mc_entity::Aabb {
                            min: [po.x - 0.3, po.y, po.z - 0.3],
                            max: [po.x + 0.3, po.y + 1.8, po.z + 0.3],
                        })
                    })
            });
            !players
                && !lock(&ctx.entities)
                    .iter()
                    .any(|e| matches!(e.kind, mc_entity::Kind::Mob(_)) && hits(&e.aabb()))
        }
        _ => true,
    }
}

/// `replace_disk` (Frost Walker): blocks in a disk around the player that
/// pass its predicate become `block_state`.
fn replace_disk(ctx: &ServerContext, d: &Value, level: i32, origin: [f64; 3]) {
    let radius = ee::value(&d["radius"], level) as i32;
    let height = ee::value(&d["height"], level) as i32;
    let off = d["offset"].as_array();
    let o = |i: usize| {
        off.and_then(|o| o.get(i))
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32
    };
    let c = [
        origin[0].floor() as i32 + o(0),
        origin[1].floor() as i32 + o(1),
        origin[2].floor() as i32 + o(2),
    ];
    let blocks = &ctx.data.blocks;
    let Some(name) = d["block_state"]["id"].as_str() else {
        return;
    };
    let Some(mut state) = blocks.default_state(&ee::full(name)) else {
        return;
    };
    if let Some(props) = d["block_state"]["Properties"]
        .as_object()
        .or(d["block_state"]["properties"].as_object())
    {
        for (k, v) in props {
            state = ctx.shapes.with(state, k, v.as_str().unwrap_or(""));
        }
    }
    let mut level = crate::level::WorldLevel::new(ctx);
    let mut placed = Vec::new();
    for y in c[1]..=c[1] + (height - 1).min(0) {
        for x in c[0] - radius..=c[0] + radius {
            for z in c[2] - radius..=c[2] + radius {
                let (dx, dz) = (
                    f64::from(x) + 0.5 - origin[0],
                    f64::from(z) + 0.5 - origin[2],
                );
                if dx * dx + dz * dz >= f64::from(radius * radius) {
                    continue;
                }
                let p = [x, y, z];
                if d.get("predicate")
                    .is_none_or(|pr| block_predicate(ctx, pr, p))
                    && level.set_block(Position::new(x, y, z), state)
                {
                    placed.push(p);
                }
            }
        }
    }
    {
        let logic = level.logic();
        let mut rng = mc_tick::rng::Rng::new(crate::util::random_u64());
        for p in &placed {
            mc_tick::frosted_ice::placed(
                &mut level,
                logic,
                mc_tick::BlockPos::new(p[0], p[1], p[2]),
                &mut rng,
            );
        }
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{block, flat, join, place, set, stack, wear};

    const U: u128 = 0xabc;

    fn mods(ctx: &ServerContext, attr: &str) -> Vec<AttributeModifier> {
        lock(&ctx.vitals)
            .get(&U)
            .unwrap()
            .worn
            .modifiers(attr)
            .to_vec()
    }

    #[test]
    fn gear_enchantments_become_attribute_modifiers() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 900, [0.5, -60.0, 0.5], 0);
        wear(
            &ctx,
            U,
            7,
            stack(&ctx, "iron_leggings", &[("swift_sneak", 3)]),
        );
        wear(
            &ctx,
            U,
            8,
            stack(&ctx, "iron_boots", &[("depth_strider", 3)]),
        );
        wear(
            &ctx,
            U,
            5,
            stack(&ctx, "iron_helmet", &[("aqua_affinity", 1)]),
        );
        wear(
            &ctx,
            U,
            36,
            stack(&ctx, "iron_pickaxe", &[("efficiency", 5)]),
        );
        // Boots in the hand do nothing.
        wear(
            &ctx,
            U,
            45,
            stack(&ctx, "iron_boots", &[("depth_strider", 3)]),
        );
        tick(&ctx);
        let sneak = mods(&ctx, "minecraft:sneaking_speed");
        assert_eq!(sneak.len(), 1);
        assert_eq!(sneak[0].id, "minecraft:enchantment.swift_sneak/legs");
        assert!((sneak[0].amount - 0.45).abs() < 1e-6);
        let water = mods(&ctx, "minecraft:water_movement_efficiency");
        assert_eq!(water.len(), 1);
        assert!((water[0].amount - 1.0).abs() < 1e-6);
        let mining = mods(&ctx, "minecraft:mining_efficiency");
        assert_eq!(mining[0].amount, 26.0);
        assert_eq!(mining[0].id, "minecraft:enchantment.efficiency/mainhand");
        let under = mods(&ctx, "minecraft:submerged_mining_speed");
        assert_eq!((under[0].amount, under[0].operation), (4.0, 2));
        let snap = crate::commands::gear::snapshot(&ctx, U, "sneaking_speed").unwrap();
        assert!((snap.base - 0.3).abs() < 1e-9 && snap.modifiers == sneak);
        // Taken off, the modifier goes.
        wear(
            &ctx,
            U,
            7,
            ItemStack::new(ctx.items.id("minecraft:iron_leggings").unwrap(), 1),
        );
        tick(&ctx);
        assert!(mods(&ctx, "minecraft:sneaking_speed").is_empty());
    }

    #[test]
    fn soul_speed_is_on_while_on_soul_blocks() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 901, [0.5, -60.0, 0.5], 0);
        set(&ctx, [0, -61, 0], "minecraft:soul_sand");
        wear(&ctx, U, 8, stack(&ctx, "iron_boots", &[("soul_speed", 3)]));
        place(&ctx, U, [0.5, -60.125, 0.5], true);
        tick(&ctx);
        let speed = mods(&ctx, "minecraft:movement_speed");
        assert_eq!(speed.len(), 1, "{speed:?}");
        assert!((speed[0].amount - 0.0615).abs() < 1e-6);
        assert_eq!(mods(&ctx, "minecraft:movement_efficiency")[0].amount, 1.0);
        // A jump keeps it, stepping onto grass ends it.
        place(&ctx, U, [0.5, -59.0, 0.5], false);
        tick(&ctx);
        assert_eq!(mods(&ctx, "minecraft:movement_speed").len(), 1);
        place(&ctx, U, [2.5, -60.0, 0.5], true);
        tick(&ctx);
        assert!(mods(&ctx, "minecraft:movement_speed").is_empty());
    }

    #[test]
    fn frost_walker_freezes_water_around_and_ignores_magma() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 902, [0.5, -60.0, 0.5], 0);
        for x in -4..=4 {
            for z in -4..=4 {
                set(&ctx, [x, -61, z], "minecraft:water");
            }
        }
        wear(
            &ctx,
            U,
            8,
            stack(&ctx, "iron_boots", &[("frost_walker", 2)]),
        );
        tick(&ctx);
        // Radius 4 around the feet: (3, 0) is in, (4, 0) and (3, 3) out.
        assert_eq!(block(&ctx, [0, -61, 0]), "minecraft:frosted_ice");
        assert_eq!(block(&ctx, [3, -61, 0]), "minecraft:frosted_ice");
        assert_eq!(block(&ctx, [4, -61, 0]), "minecraft:water");
        assert_eq!(block(&ctx, [3, -61, 3]), "minecraft:water");
        // In the air nothing freezes.
        set(&ctx, [0, -61, 6], "minecraft:water");
        place(&ctx, U, [0.5, -58.0, 6.5], false);
        tick(&ctx);
        assert_eq!(block(&ctx, [0, -61, 6]), "minecraft:water");
        assert!(crate::enchant_effects::immune(
            &ctx,
            U,
            "minecraft:hot_floor"
        ));
        assert!(!crate::enchant_effects::immune(&ctx, U, "minecraft:lava"));
    }

    #[test]
    fn fire_protection_shortens_burning_and_water_puts_it_out() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 903, [0.5, -60.0, 0.5], 0);
        let fire = |ctx: &ServerContext| lock(&ctx.vitals).get(&U).unwrap().fire;
        ignite(&ctx, U, 300);
        assert_eq!(fire(&ctx), 300);
        crate::effects::change(&ctx, U, |v| v.fire = 0);
        wear(
            &ctx,
            U,
            6,
            stack(&ctx, "iron_chestplate", &[("fire_protection", 4)]),
        );
        tick(&ctx);
        ignite(&ctx, U, 300);
        assert_eq!(fire(&ctx), 120);
        let health = lock(&ctx.vitals).get(&U).unwrap().health;
        for _ in 0..20 {
            tick(&ctx);
        }
        assert_eq!(fire(&ctx), 100);
        assert!(lock(&ctx.vitals).get(&U).unwrap().health < health);
        set(&ctx, [0, -60, 0], "minecraft:water");
        tick(&ctx);
        assert_eq!(fire(&ctx), 0);
    }

    #[test]
    fn blast_protection_resists_explosion_knockback() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 904, [0.5, -60.0, 0.5], 0);
        wear(
            &ctx,
            U,
            8,
            stack(&ctx, "iron_boots", &[("blast_protection", 2)]),
        );
        wear(
            &ctx,
            U,
            6,
            stack(&ctx, "iron_chestplate", &[("blast_protection", 4)]),
        );
        tick(&ctx);
        let r = crate::commands::gear::value(&ctx, U, "explosion_knockback_resistance");
        // 0.3 + 0.6, at most 1.
        assert!((r - 0.9).abs() < 1e-6, "{r}");
    }

    #[test]
    fn thorns_hurts_the_attacker_and_wears_the_armor() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 905, [0.5, -60.0, 0.5], 0);
        join(&ctx, 0xdef, 906, [1.5, -60.0, 0.5], 0);
        // Level 7 never misses (15% a level).
        wear(&ctx, U, 6, stack(&ctx, "iron_chestplate", &[("thorns", 7)]));
        let hit = crate::combat::Hit {
            amount: 2.0,
            kind: "minecraft:player_attack",
            death_key: "death.attack.player",
            attacker: Some("P906"),
            by: Some(906),
            from: Some([1.5, -60.0, 0.5]),
            knockback: 0.0,
            disables_shield: 0.0,
        };
        assert!(crate::combat::hurt_player(&ctx, U, &hit).is_some());
        let other = lock(&ctx.vitals).get(&0xdef).unwrap().health;
        assert!((15.0..=19.0).contains(&other), "{other}");
        let worn = lock(&ctx.inventories).get(&U).unwrap().slots[6]
            .clone()
            .unwrap();
        // Thorns' 2 and the hit's 1.
        assert_eq!(worn.damage, 3);
    }

    #[test]
    fn others_see_armor_and_the_off_hand_change() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 906, [0.5, -60.0, 0.5], 0);
        tick(&ctx);
        let mut events = ctx.events.subscribe();
        let helmet = stack(&ctx, "iron_helmet", &[("protection", 1)]);
        wear(&ctx, U, 5, helmet.clone());
        wear(&ctx, U, 45, stack(&ctx, "shield", &[]));
        tick(&ctx);
        let mut seen = Vec::new();
        while let Some(e) = events.try_changed() {
            if let Event::Equipment { slots, .. } = e {
                seen.extend(slots);
            }
        }
        assert!(seen.contains(&(5, Some(helmet))));
        assert!(seen.iter().any(|(i, s)| *i == 1 && s.is_some()));
        assert_eq!(seen.len(), 2);
    }
}
