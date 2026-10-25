//! Aquatic mobs (Claude): what a squid's ink, a pufferfish's sting, a
//! guardian's beam and curse, a frog's tongue and the eggs and frogspawn do
//! in the world.

use mc_entity::{Entity, Kind, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use super::hurt::{self, Harm};
use super::{Actor, Target};
use crate::context::ServerContext;
use crate::fx;
use crate::level::WorldLevel;

/// The biome at a place.
pub fn biome_at(ctx: &ServerContext, pos: [f64; 3]) -> Option<String> {
    let at = Position::new(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    );
    let id = ctx.world.biome(at, &ctx.data.blocks)?;
    let name = ctx.world.biome_name(id)?;
    Some(if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    })
}

/// A squid squirts ink: the cloud and the sound.
pub fn ink(ctx: &ServerContext, e: &Entity) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let glow = m.species == Species::GlowSquid;
    let (sound, particle) = if glow {
        ("entity.glow_squid.squirt", "glow_squid_ink")
    } else {
        ("entity.squid.squirt", "squid_ink")
    };
    fx::sound(ctx, sound, fx::category::NEUTRAL, e.pos, 0.4, 1.0);
    fx::particles(
        ctx,
        particle,
        [e.pos[0], e.pos[1] + 0.4, e.pos[2]],
        0.3,
        0.05,
        20,
    );
}

/// The player or mob with this id, hurt by `hit` (already scaled by the
/// difficulty for players). Returns whether it took the blow.
#[allow(clippy::too_many_arguments)]
fn hurt_target(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    amount: f32,
    kind: &'static str,
    death_key: &'static str,
    targets: &[Target],
    difficulty: i32,
) -> Option<Option<u128>> {
    hurt_with(
        ctx, entities, i, target, amount, kind, death_key, targets, difficulty, 0.0,
    )
}

/// A mob hurts its target (a player or a mob) with a push this strong;
/// `Some(Some(player))` when it was a player.
#[allow(clippy::too_many_arguments)]
pub(super) fn hurt_with(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    amount: f32,
    kind: &'static str,
    death_key: &'static str,
    targets: &[Target],
    difficulty: i32,
    knockback: f64,
) -> Option<Option<u128>> {
    let (id, pos) = (entities[i].id, entities[i].pos);
    let Kind::Mob(m) = &entities[i].kind else {
        return None;
    };
    let species = m.species;
    if let Some(t) = targets.iter().find(|t| t.id == target) {
        if difficulty == 0 {
            return None;
        }
        let hit = crate::combat::Hit {
            amount: crate::damage::scale_for_difficulty(amount, difficulty),
            kind,
            death_key,
            attacker: Some(species.display_name()),
            by: Some(id),
            from: Some(pos),
            knockback,
            disables_shield: 0.0,
        };
        return crate::combat::hurt_player(ctx, t.uuid, &hit).map(|_| Some(t.uuid));
    }
    let j = entities.iter().position(|e| e.id == target)?;
    if !matches!(entities[j].kind, Kind::Mob(_)) {
        return None;
    }
    let harm = Harm {
        by: Some(id),
        from: Some(pos),
        killer: Some(species.id().to_owned()),
        knockback,
        ..Harm::new(amount, kind)
    };
    hurt::harm(ctx, &mut entities[j], &harm).then_some(None)
}

/// A pufferfish stings whatever touched it: damage, then poison.
#[allow(clippy::too_many_arguments)]
pub fn sting(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    poison: i32,
    targets: &[Target],
    difficulty: i32,
) {
    let hurt = hurt_target(
        ctx,
        entities,
        i,
        target,
        damage,
        "minecraft:mob_attack",
        "death.attack.mob",
        targets,
        difficulty,
    );
    if hurt.is_none() {
        return;
    }
    fx::sound(
        ctx,
        "entity.puffer_fish.sting",
        fx::category::NEUTRAL,
        entities[i].pos,
        1.0,
        1.0,
    );
    let Some(kind) = crate::effects::kind_of("poison") else {
        return;
    };
    let effect = crate::effects::Instance::new(0, poison, true);
    match hurt {
        Some(Some(uuid)) => {
            crate::effects::give(ctx, uuid, kind, effect);
        }
        _ => {
            if let Some(j) = entities.iter().position(|e| e.id == target) {
                crate::mob_effects::give(ctx, &mut entities[j], kind, effect, None);
            }
        }
    }
}

/// The magic part of a guardian's beam.
pub fn magic(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    targets: &[Target],
    difficulty: i32,
) {
    hurt_target(
        ctx,
        entities,
        i,
        target,
        damage,
        "minecraft:indirect_magic",
        "death.attack.indirectMagic",
        targets,
        difficulty,
    );
}

/// An elder guardian curses every player within fifty blocks who is not
/// cursed already: Mining Fatigue III for five minutes.
pub fn curse(ctx: &ServerContext, e: &Entity, targets: &[Target]) {
    let Some(kind) = crate::effects::kind_of("mining_fatigue") else {
        return;
    };
    for t in targets.iter().filter(|t| t.targetable) {
        if super::dist_sq(t.pos, e.pos) > 50.0 * 50.0 {
            continue;
        }
        let cursed = crate::effects::change(ctx, t.uuid, |v| {
            v.effects
                .active
                .get("mining_fatigue")
                .is_some_and(|i| i.amplifier >= 2 && i.duration >= 1200)
        })
        .unwrap_or(false);
        if cursed {
            continue;
        }
        crate::effects::give(
            ctx,
            t.uuid,
            kind,
            crate::effects::Instance::new(2, 6000, true),
        );
        fx::sound(ctx, "entity.elder_guardian.curse", 5, t.pos, 1.0, 1.0);
        let _ = ctx.events.send(crate::context::Event::Direct {
            uuid: t.uuid,
            packet: crate::context::Direct::GameEvent(mc_protocol::packets::play::GameEvent {
                event: mc_protocol::packets::play::game_event::ELDER_GUARDIAN_APPEARANCE,
                value: 1.0,
            }),
        });
    }
}

/// A frog swallows a small slime or magma cube: a slimeball, or a froglight
/// of the colour of the frog.
pub fn eat(ctx: &ServerContext, entities: &mut [Entity], i: usize, target: i32) {
    let Kind::Mob(frog) = &entities[i].kind else {
        return;
    };
    let variant = frog.variant;
    let Some(j) = entities.iter().position(|e| e.id == target) else {
        return;
    };
    let pos = entities[j].pos;
    let Kind::Mob(prey) = &mut entities[j].kind else {
        return;
    };
    if prey.is_dead() || prey.dying.is_some() || prey.size > 1 {
        return;
    }
    let drop = match prey.species {
        Species::Slime => "minecraft:slime_ball",
        Species::MagmaCube => match super::meta::variant_name(ctx, Species::Frog, variant) {
            Some("minecraft:warm") => "minecraft:pearlescent_froglight",
            Some("minecraft:cold") => "minecraft:verdant_froglight",
            _ => "minecraft:ochre_froglight",
        },
        _ => return,
    };
    // It goes without a death animation or loot of its own.
    entities[j].removed = true;
    if let Some(item) = ctx.items.id(drop) {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], item, 1);
    }
}

/// A turtle lays a clutch in the sand where it stands.
pub fn lay_egg(ctx: &ServerContext, level: &mut WorldLevel, cell: [i32; 3], count: i32) {
    let p = Position::new(cell[0], cell[1], cell[2]);
    let eggs = count.clamp(1, 4).to_string();
    if let Some(state) = ctx.data.blocks.state_id(
        "minecraft:turtle_egg",
        &[("eggs", eggs.as_str()), ("hatch", "0")],
    ) {
        level.set_block(p, state);
    }
}

/// A frog lays frogspawn on the water.
pub fn lay_spawn(ctx: &ServerContext, level: &mut WorldLevel, cell: [i32; 3]) {
    let p = Position::new(cell[0], cell[1], cell[2]);
    if let Some(state) = ctx.data.blocks.default_state("minecraft:frogspawn") {
        level.set_block(p, state);
    }
}

/// A dolphin's grace on a swimmer: five seconds.
pub fn grace(ctx: &ServerContext, player: i32, targets: &[Target]) {
    let Some(t) = targets.iter().find(|t| t.id == player) else {
        return;
    };
    if let Some(kind) = crate::effects::kind_of("dolphins_grace") {
        crate::effects::give(
            ctx,
            t.uuid,
            kind,
            crate::effects::Instance::new(0, 100, true),
        );
    }
}

/// A guardian with its spikes out hurts whoever hits it in melee.
pub fn thorns(ctx: &ServerContext, e: &Entity, victim: u128) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    if !matches!(m.species, Species::Guardian | Species::ElderGuardian)
        || m.quirks.aqua.retract
        || ctx.difficulty() == 0
    {
        return;
    }
    let hit = crate::combat::Hit {
        amount: crate::damage::scale_for_difficulty(2.0, ctx.difficulty()),
        kind: "minecraft:thorns",
        death_key: "death.attack.thorns",
        attacker: Some(m.species.display_name()),
        by: Some(e.id),
        from: Some(e.pos),
        knockback: 0.0,
        disables_shield: 0.0,
    };
    crate::combat::hurt_player(ctx, victim, &hit);
    fx::sound(ctx, "entity.guardian.hurt_land", 5, e.pos, 1.0, 1.0);
}

/// A player kills a mob axolotls were fighting: they are thanked with
/// Regeneration for a hundred ticks per axolotl (twelve hundred at most
/// in all) and no more Mining Fatigue.
pub fn reward(ctx: &ServerContext, entities: &[Entity], i: usize, player: u128) {
    let dead = &entities[i];
    let Kind::Mob(d) = &dead.kind else {
        return;
    };
    if !d.is_dead() {
        return;
    }
    let fighting = entities
        .iter()
        .filter(|e| {
            matches!(&e.kind, Kind::Mob(m)
                if m.species == Species::Axolotl && m.target == Some(dead.id) && !m.is_dead())
                && super::dist_sq(e.pos, dead.pos) < 20.0 * 20.0
        })
        .count() as i32;
    if fighting == 0 {
        return;
    }
    if let Some(kind) = crate::effects::kind_of("mining_fatigue") {
        crate::effects::remove(ctx, player, Some(kind));
    }
    let Some(kind) = crate::effects::kind_of("regeneration") else {
        return;
    };
    let left = crate::effects::change(ctx, player, |v| {
        v.effects
            .active
            .get("regeneration")
            .map_or(0, |i| i.duration.max(0))
    })
    .unwrap_or(0);
    let ticks = (left + 100 * fighting).min(2400);
    crate::effects::give(
        ctx,
        player,
        kind,
        crate::effects::Instance::new(0, ticks, true),
    );
}

/// The creature in a bucket.
pub fn bucket_species(item: &str) -> Option<Species> {
    Some(match item {
        "minecraft:cod_bucket" => Species::Cod,
        "minecraft:salmon_bucket" => Species::Salmon,
        "minecraft:pufferfish_bucket" => Species::Pufferfish,
        "minecraft:tropical_fish_bucket" => Species::TropicalFish,
        "minecraft:axolotl_bucket" => Species::Axolotl,
        "minecraft:tadpole_bucket" => Species::Tadpole,
        _ => return None,
    })
}

/// The bucket a creature is caught in.
pub fn bucket_of(species: Species) -> Option<&'static str> {
    Some(match species {
        Species::Cod => "minecraft:cod_bucket",
        Species::Salmon => "minecraft:salmon_bucket",
        Species::Pufferfish => "minecraft:pufferfish_bucket",
        Species::TropicalFish => "minecraft:tropical_fish_bucket",
        Species::Axolotl => "minecraft:axolotl_bucket",
        Species::Tadpole => "minecraft:tadpole_bucket",
        _ => return None,
    })
}

/// The data a bucket keeps of its creature (`bucket_entity_data`).
fn bucket_data(m: &mc_entity::Mob) -> Compound {
    let mut c = Compound::new();
    match m.species {
        Species::TropicalFish => {
            c.insert("BucketVariantTag", Tag::Int(m.variant));
        }
        Species::Salmon => {
            let kind = ["small", "medium", "large"][m.variant.clamp(0, 2) as usize];
            c.insert("type", Tag::String(kind.into()));
        }
        Species::Pufferfish => {
            c.insert("PuffState", Tag::Int(i32::from(m.quirks.aqua.puff)));
        }
        Species::Axolotl => {
            c.insert("Variant", Tag::Int(m.variant));
            c.insert("Age", Tag::Int(m.baby_age));
            c.insert("Health", Tag::Float(m.health));
        }
        Species::Tadpole => {
            c.insert("Age", Tag::Int(m.quirks.aqua.growth));
        }
        _ => {}
    }
    if let Some(name) = &m.custom_name {
        c.insert("CustomName", Tag::String(name.clone()));
    }
    c
}

/// Lets the creature in a bucket out at the water poured at `at`.
pub fn release(ctx: &ServerContext, bucket: &str, stack: &ItemStack, at: Position) {
    let Some(species) = bucket_species(bucket) else {
        return;
    };
    let pos = [
        f64::from(at.x) + 0.5,
        f64::from(at.y) + 0.2,
        f64::from(at.z) + 0.5,
    ];
    let mut e = super::new_mob(ctx, species, pos);
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.aqua.from_bucket = true;
        m.persistent = true;
        if let Some(Tag::Compound(d)) = stack.components.get("minecraft:bucket_entity_data") {
            let int = |k: &str| match d.get(k) {
                Some(Tag::Int(v)) => Some(*v),
                _ => None,
            };
            match species {
                Species::TropicalFish => {
                    if let Some(v) = int("BucketVariantTag") {
                        m.variant = v;
                    }
                }
                Species::Salmon => {
                    m.variant = match d.get("type") {
                        Some(Tag::String(s)) if s == "small" => 0,
                        Some(Tag::String(s)) if s == "large" => 2,
                        _ => 1,
                    };
                }
                Species::Pufferfish => {
                    if let Some(v) = int("PuffState") {
                        m.quirks.aqua.puff = v.clamp(0, 2) as u8;
                    }
                }
                Species::Axolotl => {
                    if let Some(v) = int("Variant") {
                        m.variant = v.clamp(0, 4);
                    }
                    if let Some(v) = int("Age") {
                        m.baby_age = v;
                    }
                    if let Some(Tag::Float(h)) = d.get("Health") {
                        m.health = h.clamp(1.0, m.max_health);
                    }
                }
                Species::Tadpole => {
                    if let Some(v) = int("Age") {
                        m.quirks.aqua.growth = v.max(0);
                    }
                }
                _ => {}
            }
            if let Some(Tag::String(n)) = d.get("CustomName") {
                m.custom_name = Some(n.clone());
            }
        }
    }
    let _ = ctx
        .events
        .send(crate::context::Event::EntitySpawned(e.clone()));
    crate::tick::lock(&ctx.entities).push(e);
    fx::sound(
        ctx,
        "item.bucket.empty_fish",
        fx::category::PLAYERS,
        pos,
        1.0,
        1.0,
    );
}

fn consume(ctx: &ServerContext, by: &Actor, slot: usize) {
    if by.game_mode != 1 {
        crate::held::consume(ctx, by.uuid, slot, 1);
    }
}

/// Gives a player a stack, dropping what does not fit.
fn give(ctx: &ServerContext, by: &Actor, stack: &ItemStack) {
    let left = crate::held::give(ctx, by.uuid, stack);
    if left > 0 {
        crate::tick::spawn_item_at(ctx, by.pos, stack.item, left);
    }
}

fn item_tagged(ctx: &ServerContext, name: &str, tag: &str) -> bool {
    ctx.commands
        .env
        .tag("minecraft:item", tag)
        .iter()
        .any(|n| n == name)
}

/// Uses of items on the aquatic mobs. Returns whether one was used.
pub fn used(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &Actor,
    name: &str,
    slot: usize,
    food: bool,
) -> bool {
    let (id, pos) = (e.id, e.pos);
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.dying.is_some() || m.is_dead() {
        return false;
    }
    let species = m.species;
    // A water bucket takes up what can be bucketed.
    if name == "minecraft:water_bucket"
        && let Some(bucket) = bucket_of(species)
        && let Some(item) = ctx.items.id(bucket)
    {
        let mut stack = ItemStack::new(item, 1);
        stack.components.set(
            "minecraft:bucket_entity_data",
            Tag::Compound(bucket_data(m)),
        );
        e.removed = true;
        consume(ctx, by, slot);
        give(ctx, by, &stack);
        fx::sound(
            ctx,
            "item.bucket.fill_fish",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        return true;
    }
    // A slimeball speeds a tadpole on its way.
    if species == Species::Tadpole && item_tagged(ctx, name, "minecraft:frog_food") {
        let a = &mut m.quirks.aqua;
        a.growth += (mc_entity::mob::aqua::TADPOLE_GROWTH - a.growth) / 10;
        consume(ctx, by, slot);
        fx::particles(
            ctx,
            "happy_villager",
            [pos[0], pos[1] + 0.3, pos[2]],
            0.3,
            0.0,
            5,
        );
        return true;
    }
    // A fish for a grown dolphin: it leads to a treasure.
    if species == Species::Dolphin && !m.is_baby() && item_tagged(ctx, name, "minecraft:fishes") {
        m.quirks.aqua.got_fish = true;
        m.dirty = true;
        consume(ctx, by, slot);
        fx::sound(
            ctx,
            "entity.dolphin.eat",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
        let from = (
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        );
        let wanted = ctx
            .commands
            .env
            .tag("minecraft:worldgen/structure", "minecraft:dolphin_located");
        if let Some((_, (x, y, z))) = crate::commands::places::nearest_structure(ctx, &wanted, from)
        {
            m.quirks.aqua.treasure = Some([x, y, z]);
            fx::particles(
                ctx,
                "happy_villager",
                [pos[0], pos[1] + 0.3, pos[2]],
                0.5,
                0.0,
                7,
            );
        }
        return true;
    }
    // Nautiluses: pufferfish tame, fish heal and (in buckets) breed.
    let is_nautilus = matches!(species, Species::Nautilus | Species::ZombieNautilus);
    if is_nautilus
        && !m.is_tame()
        && !m.is_baby()
        && item_tagged(ctx, name, "minecraft:nautilus_taming_items")
    {
        consume(ctx, by, slot);
        if name.ends_with("_bucket") {
            give_empty_bucket(ctx, by);
        }
        if m.below(3) == 0 {
            m.owner = Some(by.uuid);
            m.tame = true;
            m.persistent = true;
            m.nav.stop();
            m.target = None;
            m.revenge = None;
            m.dirty = true;
            fx::status(ctx, id, 7);
        } else {
            fx::status(ctx, id, 6);
        }
        return true;
    }
    if food && (is_nautilus || species == Species::Axolotl) {
        if m.is_baby() {
            m.baby_age += -m.baby_age / 10;
        } else if is_nautilus && m.health < m.max_health {
            // Two points of health for each of the food's.
            let nutrition = ctx
                .items
                .id(name)
                .and_then(|i| ctx.items.get(i))
                .and_then(|i| i.food.as_ref())
                .map_or(1, |f| f.nutrition);
            m.health = (m.health + 2.0 * nutrition as f32).min(m.max_health);
        } else if m.baby_age == 0 && !m.in_love() {
            m.fall_in_love(Some(by.uuid));
            fx::status(ctx, id, 18);
        } else {
            return false;
        }
        m.dirty = true;
        consume(ctx, by, slot);
        if name.ends_with("_bucket") {
            give_empty_water_bucket(ctx, by);
        }
        let eat = if is_nautilus {
            format!("entity.{}.eat", species.path())
        } else {
            "entity.generic.eat".to_owned()
        };
        fx::sound(ctx, &eat, fx::category::NEUTRAL, pos, 1.0, 1.0);
        return true;
    }
    false
}

fn give_empty_bucket(ctx: &ServerContext, by: &Actor) {
    if let Some(item) = ctx.items.id("minecraft:water_bucket") {
        give(ctx, by, &ItemStack::new(item, 1));
    }
}

fn give_empty_water_bucket(ctx: &ServerContext, by: &Actor) {
    give_empty_bucket(ctx, by);
}

/// A mob that lands on a turtle egg breaks one a third of the time, one
/// that stands on it a hundredth of the time each tick. Turtles and what
/// flies leave them alone, and so does a world that forbids mob griefing.
pub fn trample_eggs(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    m: &mc_entity::Mob,
    pos: [f64; 3],
    fell: Option<f64>,
) {
    if matches!(m.species, Species::Turtle)
        || m.species.flies()
        || m.is_dead()
        || !ctx.game_rule_bool("mob_griefing")
    {
        return;
    }
    let chance = if fell.is_some() { 3 } else { 100 };
    if !crate::util::random_u64().is_multiple_of(chance) {
        return;
    }
    let at = mc_protocol::Position::new(
        pos[0].floor() as i32,
        (pos[1] + 0.01).floor() as i32,
        pos[2].floor() as i32,
    );
    let logic = level.logic();
    mc_tick::hatching::trample_egg(level, logic, crate::level::to_pos(at));
}

/// Mobs that ride a mob sit on it: they follow it, are announced to the
/// clients once, and fall off when it goes.
pub fn carry(ctx: &ServerContext, entities: &mut [Entity]) {
    let riding: Vec<(usize, i32)> = entities
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            Kind::Mob(m) => m.quirks.aqua.mount.map(|v| (i, v)),
            _ => None,
        })
        .collect();
    for (i, vehicle) in riding {
        let mount = entities
            .iter()
            .find(|e| e.id == vehicle && !e.removed)
            .filter(|e| matches!(&e.kind, Kind::Mob(m) if !m.is_dead()))
            .map(|e| (e.pos, e.size().1));
        let rider = entities[i].id;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        let Some((pos, height)) = mount else {
            m.quirks.aqua.mount = None;
            m.quirks.aqua.mount_sent = false;
            fx::emit(
                ctx,
                fx::Fx::Passengers(mc_protocol::packets::play::SetPassengers {
                    vehicle,
                    passengers: Vec::new(),
                }),
            );
            continue;
        };
        let announce = !m.quirks.aqua.mount_sent;
        m.quirks.aqua.mount_sent = true;
        entities[i].pos = [pos[0], pos[1] + height * 0.7, pos[2]];
        entities[i].vel = [0.0; 3];
        entities[i].on_ground = false;
        if announce {
            fx::emit(
                ctx,
                fx::Fx::Passengers(mc_protocol::packets::play::SetPassengers {
                    vehicle,
                    passengers: vec![rider],
                }),
            );
        }
    }
    // A mount forgets riders that got off or died.
    let seated: Vec<(i32, Option<i32>)> = entities
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if !e.removed => Some((e.id, m.quirks.aqua.mount)),
            _ => None,
        })
        .collect();
    for e in entities.iter_mut() {
        let id = e.id;
        if let Kind::Mob(m) = &mut e.kind
            && m.species == Species::ZombieNautilus
            && !m.riders.is_empty()
        {
            m.riders.retain(|r| {
                seated
                    .iter()
                    .find(|(i, _)| i == r)
                    .is_none_or(|(_, v)| *v == Some(id))
            });
        }
    }
}

/// Whoever rides a nautilus does not lose breath under water.
pub fn breath(ctx: &ServerContext, m: &mc_entity::Mob, targets: &[Target]) {
    if !matches!(m.species, Species::Nautilus | Species::ZombieNautilus) {
        return;
    }
    let Some(kind) = crate::effects::kind_of("breath_of_the_nautilus") else {
        return;
    };
    for t in targets.iter().filter(|t| m.riders.contains(&t.id)) {
        crate::effects::give(
            ctx,
            t.uuid,
            kind,
            crate::effects::Instance::new(0, 40, true),
        );
    }
}

/// Dolphins play with what floats in the water: they find an item, take it
/// up, carry it a while and spit it out again.
pub fn play(ctx: &ServerContext, entities: &mut [Entity], now: i64) {
    use mc_entity::mob::slot;
    use mc_entity::nav::{Liquid, Terrain};
    let dolphins: Vec<usize> = entities
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(&e.kind, Kind::Mob(m) if m.species == Species::Dolphin
                && m.dying.is_none() && !e.removed && !m.is_baby())
        })
        .map(|(i, _)| i)
        .collect();
    if dolphins.is_empty() {
        return;
    }
    let ground = super::Ground::new(ctx);
    for i in dolphins {
        let pos = entities[i].pos;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        // One it was given on a reload is let go too.
        if m.gear[slot::MAINHAND].is_some() && m.quirks.aqua.hold == 0 {
            m.quirks.aqua.hold = 100;
        }
        // Carrying one: it is let go after a while.
        if m.quirks.aqua.hold > 0 {
            m.quirks.aqua.hold -= 1;
            if m.quirks.aqua.hold == 0
                && let Some(g) = m.gear[slot::MAINHAND].take()
            {
                m.quirks.aqua.play_cd = 300;
                let yaw = f64::from(m.yaw).to_radians();
                let at = [
                    pos[0] - yaw.sin() * 1.2,
                    pos[1] + 0.3,
                    pos[2] + yaw.cos() * 1.2,
                ];
                crate::tick::spawn_item_at(ctx, at, g.item, g.count);
                fx::sound(
                    ctx,
                    "entity.dolphin.play",
                    fx::category::NEUTRAL,
                    pos,
                    1.0,
                    1.0,
                );
                send_worn(ctx, &entities[i]);
            }
            continue;
        }
        if m.gear[slot::MAINHAND].is_some() || m.quirks.aqua.play_cd > 0 || now % 10 != 0 {
            continue;
        }
        // The nearest item in the water within twelve blocks.
        let mut best: Option<(f64, usize)> = None;
        for (j, o) in entities.iter().enumerate() {
            let Kind::Item { pickup_delay, .. } = o.kind else {
                continue;
            };
            let d = super::dist_sq(o.pos, pos);
            let wet = ground.liquid(
                o.pos[0].floor() as i32,
                o.pos[1].floor() as i32,
                o.pos[2].floor() as i32,
            ) == Liquid::Water;
            if o.removed || pickup_delay > 0 || !wet || d > 144.0 {
                continue;
            }
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, j));
            }
        }
        let Some((d, j)) = best else {
            if let Kind::Mob(m) = &mut entities[i].kind {
                m.quirks.aqua.play_at = None;
            }
            continue;
        };
        let target = entities[j].pos;
        if d > 2.25 {
            if let Kind::Mob(m) = &mut entities[i].kind {
                m.quirks.aqua.play_at = Some(target);
            }
            continue;
        }
        // Close enough: it takes the stack.
        let Kind::Item {
            item,
            count,
            damage,
            ref components,
            ..
        } = entities[j].kind
        else {
            continue;
        };
        let components = components.clone();
        let (item_id, collector) = (entities[j].id, entities[i].id);
        entities[j].removed = true;
        if let Kind::Mob(m) = &mut entities[i].kind {
            m.gear[slot::MAINHAND] = Some(mc_entity::mob::Gear {
                item,
                count,
                damage,
                components,
            });
            m.quirks.aqua.play_at = None;
            m.quirks.aqua.hold = 100 + m.below(100) as i32;
            m.drop_chance[slot::MAINHAND] = 2.0;
        }
        let _ = ctx.events.send(crate::context::Event::ItemTaken {
            item_entity: item_id,
            collector,
            count,
            slots: Vec::new(),
        });
        let _ = ctx
            .events
            .send(crate::context::Event::EntitiesRemoved(vec![item_id]));
        send_worn(ctx, &entities[i]);
    }
}

fn send_worn(ctx: &ServerContext, e: &Entity) {
    if let Kind::Mob(m) = &e.kind {
        fx::emit(
            ctx,
            fx::Fx::Equip(mc_protocol::packets::play::SetEquipment {
                entity_id: e.id,
                slots: super::meta::equipment_of(m),
            }),
        );
    }
}
