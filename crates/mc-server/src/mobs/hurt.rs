//! Damage to mobs and by mobs, death and what it leaves (minecraft.wiki,
//! "Damage", "Experience", "Breeding", the pages of each mob):
//! - armor (the kind's own and what it wears) takes its share as for
//!   players; a hit pushes the mob away from where it came from;
//! - hurt and death sounds and the red flash (damage event) go to clients;
//! - a mob a player hurt in the last 100 ticks dies "killed by a player":
//!   experience and the rare drops of the loot tables;
//! - experience: animals 1–3, monsters 5 (plus 1–3 for each piece of
//!   equipment), blazes 10, slimes their size, babies nothing (baby zombies
//!   two and a half times more);
//! - equipment drops with its chance (more with Looting), saddles and horse
//!   armor always;
//! - two animals in love make a baby that takes five minutes to grow up;
//!   the parents wait five minutes, and the player gets 1–7 experience.

use mc_entity::mob::{Category, Sound, Target, slot};
use mc_entity::{Entity, Kind, Mob, Species};
use mc_protocol::packets::play;

use super::{Actor, category, dist_sq, metadata_of};
use crate::combat::Blow;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;

/// A hit on a mob.
#[derive(Clone, Debug)]
pub struct Harm<'a> {
    pub amount: f32,
    /// Damage type (`minecraft:arrow`).
    pub kind: &'a str,
    /// The entity behind it (the player or mob), and the player if one.
    pub by: Option<i32>,
    pub player: Option<u128>,
    /// The projectile that did it.
    pub direct: Option<i32>,
    /// Pushes away from here.
    pub from: Option<[f64; 3]>,
    /// Knockback levels beyond the basic push.
    pub knockback: f64,
    /// Seconds of fire.
    pub fire: i32,
    /// The attacker's weapon and its Looting level, for the drops.
    pub weapon: Option<crate::loot::Tool>,
    /// The attacker's kind (`minecraft:skeleton`), for the drops.
    pub killer: Option<String>,
}

impl<'a> Harm<'a> {
    pub fn new(amount: f32, kind: &'a str) -> Self {
        Self {
            amount,
            kind,
            by: None,
            player: None,
            direct: None,
            from: None,
            knockback: 0.0,
            fire: 0,
            weapon: None,
            killer: None,
        }
    }
}

const FIRE: [&str; 5] = [
    "minecraft:on_fire",
    "minecraft:in_fire",
    "minecraft:lava",
    "minecraft:hot_floor",
    "minecraft:campfire",
];

/// Armor points and toughness a mob has: its own and what it wears.
fn armor_of(ctx: &ServerContext, m: &Mob) -> (f32, f32) {
    m.gear
        .iter()
        .flatten()
        .filter_map(|g| ctx.items.get(g.item))
        .fold((m.species.armor(), 0.0), |a, i| {
            (a.0 + i.armor, a.1 + i.toughness)
        })
}

/// Hurts a mob; whether the hit counted. A death starts [`die`].
pub fn harm(ctx: &ServerContext, e: &mut Entity, h: &Harm) -> bool {
    // The dragon takes hits its own way; the wither shrugs some off.
    if matches!(&e.kind, Kind::Mob(m) if m.species.boss())
        && let Some(done) = crate::bosses::harm(ctx, e, h)
    {
        return done;
    }
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.dying.is_some() || m.is_dead() {
        return false;
    }
    if m.species.fire_immune()
        && (FIRE.contains(&h.kind) || h.kind == "minecraft:fireball" && h.fire > 0)
    {
        return false;
    }
    // Endermen dodge what is thrown at them.
    if m.species == Species::Enderman
        && matches!(
            h.kind,
            "minecraft:arrow" | "minecraft:trident" | "minecraft:thrown"
        )
    {
        let ground = super::Ground::new(ctx);
        if mc_entity::mob::random_teleport(e, &ground, 32) {
            fx::sound(ctx, "entity.enderman.teleport", 5, e.pos, 1.0, 1.0);
        }
        return false;
    }
    // A silverfish hurt by someone, or by magic, calls its friends.
    if m.species == Species::Silverfish
        && (h.by.is_some() || h.player.is_some() || h.kind == "minecraft:magic")
    {
        super::infest::notify_hurt(m);
    }
    let (armor, toughness) = armor_of(ctx, m);
    let amount = crate::damage::armor_reduce(h.amount, armor, toughness, h.kind);
    if !m.hurt(amount, h.by.or(h.player.map(|_| -1))) {
        return false;
    }
    // Hurt by no one (water, a fall, a wall), an enderman mostly teleports.
    if m.species == Species::Enderman && h.by.is_none() && h.player.is_none() && m.below(10) != 0 {
        let ground = super::Ground::new(ctx);
        if mc_entity::mob::random_teleport(e, &ground, 32) {
            fx::sound(ctx, "entity.enderman.teleport", 5, e.pos, 1.0, 1.0);
        }
    }
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if let Some(p) = h.player {
        m.hurt_by_player = Some((p, 100));
    }
    if h.fire > 0 {
        m.burning = m.burning.max(h.fire * 20);
    }
    // Neutral mobs get angry at a player who hits them.
    if matches!(
        m.species,
        Species::ZombifiedPiglin | Species::Wolf | Species::Enderman
    ) && h.by.is_some()
        && !m.is_tame()
    {
        m.anger = 400 + m.below(400) as i32;
    }
    if let Some(from) = h.from {
        let resist = armor_resistance(ctx, m);
        let strength = (0.4 + h.knockback * 0.5) * (1.0 - resist);
        e.vel = crate::combat::knockback(e.vel, e.pos, from, strength, e.on_ground);
        e.on_ground = false;
    }
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    fx::emit(
        ctx,
        fx::Fx::Damage(play::DamageEvent {
            entity_id: id,
            damage_type: ctx.damage_type(h.kind),
            direct: h.direct.or(h.by),
            cause: h.by,
        }),
    );
    let dead = m.is_dead();
    if let Some(mc_entity::mob::MobEvent::Sound {
        name,
        volume,
        pitch,
    }) = m.sound(if dead { Sound::Death } else { Sound::Hurt })
    {
        fx::sound(ctx, &name, category(m.species), e.pos, volume, pitch);
    }
    fx::emit(
        ctx,
        fx::Fx::Data(play::SetEntityData {
            entity_id: id,
            entries: vec![(
                play::metadata::HEALTH,
                play::MetadataValue::Float(m.health.max(0.0)),
            )],
        }),
    );
    if dead {
        let killer = h
            .killer
            .clone()
            .or_else(|| h.player.map(|_| "minecraft:player".to_owned()));
        die(ctx, e, killer.as_deref().map(|k| (k, h.weapon.clone())), 0);
    }
    true
}

fn armor_resistance(ctx: &ServerContext, m: &Mob) -> f64 {
    m.species.knockback_resistance()
        + m.gear
            .iter()
            .flatten()
            .filter_map(|g| ctx.items.get(g.item))
            .map(|i| f64::from(i.knockback_resistance))
            .sum::<f64>()
            .min(1.0)
}

/// A player's melee blow lands on the mob at `i` (and sweeps those near).
pub fn player_blow(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    by: &Actor,
    blow: &Blow,
    now: i64,
) {
    let _ = now;
    let weapon = crate::held::held(ctx, by.uuid, 0).map(|h| crate::loot::Tool {
        item: h.name.clone(),
        enchants: enchant_list(h.comps.as_ref()),
    });
    let target_pos = entities[i].pos;
    let landed = hit_with_blow(
        ctx,
        &mut entities[i],
        by,
        blow,
        blow.damage,
        blow.knockback,
        weapon.clone(),
    );
    crate::combat::blow_sound(ctx, blow, target_pos, landed);
    if !landed {
        return;
    }
    let (target, on_ground) = (entities[i].id, entities[i].on_ground);
    crate::combat::after_blow(ctx, by.uuid, blow, target, target_pos, on_ground, entities);
    crate::combat::note_hit(ctx, by.uuid, entities[i].id);
    crate::combat::wear_weapon(ctx, by.uuid, by.game_mode);
    if blow.sweep {
        let sweep = 1.0 + blow.sweep_ratio * blow.damage;
        let target_box = entities[i].aabb();
        for (j, o) in entities.iter_mut().enumerate() {
            if j == i || !matches!(o.kind, Kind::Mob(_)) {
                continue;
            }
            let b = o.aabb();
            let close = b.max[0] > target_box.min[0] - 1.0
                && b.min[0] < target_box.max[0] + 1.0
                && b.max[1] > target_box.min[1] - 0.25
                && b.min[1] < target_box.max[1] + 0.25
                && b.max[2] > target_box.min[2] - 1.0
                && b.min[2] < target_box.max[2] + 1.0;
            if close && dist_sq(o.pos, by.pos) < 9.0 {
                hit_with_blow(ctx, o, by, blow, sweep, 0.0, weapon.clone());
            }
        }
    }
}

fn hit_with_blow(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &Actor,
    blow: &Blow,
    damage: f32,
    knockback: f64,
    weapon: Option<crate::loot::Tool>,
) -> bool {
    let Kind::Mob(m) = &e.kind else {
        return false;
    };
    let bonus = if m.species.undead() {
        blow.undead
    } else if m.species.arthropod() {
        blow.arthropod
    } else {
        0.0
    };
    let aquatic = blow.impaling > 0.0
        && ctx
            .commands
            .env
            .tag("minecraft:entity_type", "minecraft:sensitive_to_impaling")
            .iter()
            .any(|t| t == m.species.id());
    let bonus = bonus + if aquatic { blow.impaling } else { 0.0 };
    let h = Harm {
        amount: damage + bonus,
        kind: blow.kind(),
        by: Some(by.entity_id),
        player: Some(by.uuid),
        direct: None,
        from: Some(by.pos),
        knockback,
        fire: blow.fire,
        weapon,
        killer: Some("minecraft:player".into()),
    };
    crate::damage::with_breach(blow.breach, || harm(ctx, e, &h))
}

/// The enchantments of a stack's components, for loot conditions.
pub fn enchant_list(comps: Option<&mc_nbt::Tag>) -> Vec<(String, i32)> {
    let Some(mc_nbt::Tag::Compound(c)) = comps else {
        return Vec::new();
    };
    let Some(mc_nbt::Tag::Compound(e)) = c.get("minecraft:enchantments") else {
        return Vec::new();
    };
    let levels = match e.get("levels") {
        Some(mc_nbt::Tag::Compound(l)) => l,
        _ => e,
    };
    levels
        .iter()
        .filter_map(|(k, v)| match v {
            mc_nbt::Tag::Int(l) => Some((k.to_string(), *l)),
            _ => None,
        })
        .collect()
}

/// The mob at `i` bites, hits or touches the entity `target`.
#[allow(clippy::too_many_arguments)]
pub fn mob_attacks(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    target: i32,
    damage: f32,
    targets: &[Target],
    difficulty: i32,
    now: i64,
) {
    let (id, pos) = (entities[i].id, entities[i].pos);
    let Kind::Mob(m) = &entities[i].kind else {
        return;
    };
    let species = m.species;
    let weapon = m.gear_item(slot::MAINHAND).and_then(|w| ctx.items.get(w));
    let damage = damage + weapon.map_or(0.0, |w| w.attack_damage);
    let disables = weapon.and_then(|w| w.weapon).map_or(0.0, |w| w.1);
    let burning = m.burning > 0;
    if let Some(t) = targets.iter().find(|t| t.id == target) {
        let amount = crate::damage::scale_for_difficulty(damage, difficulty);
        let hit = crate::combat::Hit {
            amount,
            kind: "minecraft:mob_attack",
            death_key: "death.attack.mob",
            attacker: Some(species.display_name()),
            by: Some(id),
            from: Some(pos),
            knockback: 0.0,
            disables_shield: disables,
        };
        if crate::combat::hurt_player(ctx, t.uuid, &hit).is_some() {
            poison(ctx, t.uuid, species, difficulty);
            if burning && crate::hits::rnd() < f64::from(difficulty) * 0.3 {
                // Burning zombies set what they hit alight.
                crate::worn::ignite(ctx, t.uuid, 40 * difficulty);
            }
        }
        return;
    }
    let Some(j) = entities.iter().position(|e| e.id == target) else {
        return;
    };
    let _ = now;
    match &entities[j].kind {
        Kind::Mob(_) => {
            let harm = Harm {
                by: Some(id),
                from: Some(pos),
                killer: Some(species.id().to_owned()),
                ..Harm::new(damage, "minecraft:mob_attack")
            };
            harm_at(ctx, entities, j, &harm);
            // An iron golem throws what it hits up into the air.
            if species == Species::IronGolem
                && let Kind::Mob(t) = &entities[j].kind
            {
                let lift = 0.4 * (1.0 - armor_resistance(ctx, t)).max(0.0);
                entities[j].vel[1] += lift;
                entities[j].on_ground = false;
            }
        }
        Kind::Merchant(_) => {
            villager_hit(ctx, entities, j, species, damage, difficulty);
            // The golems of the village go for whoever it was.
            super::golems::defend(entities, j, id);
        }
        _ => {}
    }
}

fn harm_at(ctx: &ServerContext, entities: &mut [Entity], j: usize, h: &Harm) {
    harm(ctx, &mut entities[j], h);
}

/// Effects some mobs give with their bite.
fn poison(ctx: &ServerContext, uuid: u128, species: Species, difficulty: i32) {
    let (effect, seconds) = match species {
        Species::CaveSpider if difficulty >= 2 => ("poison", if difficulty == 3 { 15 } else { 7 }),
        Species::Husk => ("hunger", 7 * difficulty),
        Species::WitherSkeleton => ("wither", 10),
        _ => return,
    };
    if seconds <= 0 {
        return;
    }
    if let Some(kind) = crate::effects::kind_of(effect) {
        crate::effects::give(
            ctx,
            uuid,
            kind,
            crate::effects::Instance::new(0, seconds * 20, true),
        );
    }
}

/// A zombie hits a villager: it dies at the end of its health, and on
/// normal (half the time) or hard turns into a zombie villager.
fn villager_hit(
    ctx: &ServerContext,
    entities: &mut [Entity],
    j: usize,
    by: Species,
    damage: f32,
    difficulty: i32,
) {
    let id = entities[j].id;
    let left = {
        let mut hp = lock(&ctx.mobs.villagers);
        let h = hp.entry(id).or_insert(20.0);
        *h -= damage;
        *h
    };
    fx::emit(
        ctx,
        fx::Fx::Damage(play::DamageEvent {
            entity_id: id,
            damage_type: ctx.damage_type("minecraft:mob_attack"),
            direct: None,
            cause: None,
        }),
    );
    let pos = entities[j].pos;
    fx::sound(
        ctx,
        "entity.villager.hurt",
        fx::category::NEUTRAL,
        pos,
        1.0,
        1.0,
    );
    if left > 0.0 {
        return;
    }
    lock(&ctx.mobs.villagers).remove(&id);
    let infect = by.zombie() && (difficulty == 3 || difficulty == 2 && crate::hits::rnd() < 0.5);
    let Kind::Merchant(v) = &entities[j].kind else {
        return;
    };
    let data = (v.variant.clone(), v.profession.clone(), v.level);
    entities[j].removed = true;
    if infect {
        let mut z = super::new_mob(ctx, Species::ZombieVillager, pos);
        if let Kind::Mob(m) = &mut z.kind {
            m.villager = Some(data);
            m.persistent = true;
        }
        fx::sound(ctx, "entity.zombie.infect", 5, pos, 2.0, 1.0);
        let _ = ctx.events.send(Event::EntitySpawned(z.clone()));
        lock(&ctx.entities).push(z);
    } else {
        fx::status(ctx, id, 3);
        fx::sound(
            ctx,
            "entity.villager.death",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
    }
    let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
}

/// The loot context entity for a mob.
fn ent_of(ctx: &ServerContext, m: &Mob) -> crate::loot::Ent {
    let mut components = Vec::new();
    if m.species == Species::Sheep {
        components.push((
            "minecraft:sheep/color".to_owned(),
            super::WOOL_COLORS[usize::from(m.color & 15)].to_owned(),
        ));
    }
    if let Some(name) = super::meta::variant_name(ctx, m.species, m.variant) {
        components.push((
            format!("minecraft:{}/variant", m.species.path()),
            name.to_owned(),
        ));
    }
    crate::loot::Ent {
        kind: m.species.id().to_owned(),
        on_fire: m.burning > 0,
        baby: m.is_baby(),
        size: matches!(m.species, Species::Slime | Species::MagmaCube).then_some(m.size),
        components,
        sheared: (m.species == Species::Sheep).then_some(m.sheared),
        vehicle: None,
        mainhand: m
            .gear_item(slot::MAINHAND)
            .and_then(|i| ctx.items.name(i))
            .map(|n| crate::loot::Tool {
                item: n.to_owned(),
                enchants: Vec::new(),
            }),
        open_water: None,
    }
}

struct Env<'a>(&'a ServerContext);

impl crate::loot::LootEnv for Env<'_> {
    fn tag(&self, registry: &str, tag: &str) -> Vec<String> {
        self.0.commands.env.tag(registry, tag)
    }
    fn max_stack(&self, item: i32) -> i32 {
        self.0.items.max_stack(item)
    }
    fn max_damage(&self, item: i32) -> i32 {
        self.0.items.get(item).map_or(0, |i| i.max_damage)
    }
    fn item_name(&self, item: i32) -> String {
        self.0.items.name(item).unwrap_or("").to_owned()
    }
    fn smelt(&self, item: i32) -> Option<i32> {
        self.0
            .recipes
            .cooking
            .iter()
            .find(|c| c.kind == "minecraft:smelting" && c.input.contains(&item))
            .map(|c| c.result.item)
    }
}

/// Rolls a loot table for a mob; `killer` is the attacker's kind and weapon.
pub fn roll(
    ctx: &ServerContext,
    e: &Entity,
    table: &str,
    by_player: bool,
    killer: Option<(&str, Option<crate::loot::Tool>)>,
) -> Vec<crate::loot::Drop> {
    let Kind::Mob(m) = &e.kind else {
        return Vec::new();
    };
    let env = Env(ctx);
    let cx = crate::loot::LootCtx {
        origin: Some(e.pos),
        this: Some(ent_of(ctx, m)),
        killer: killer.as_ref().map(|(k, _)| crate::loot::Ent {
            kind: (*k).to_owned(),
            ..Default::default()
        }),
        tool: killer.and_then(|(_, w)| w),
        player_kill: by_player,
        ..Default::default()
    };
    match ctx.loot.table(table) {
        Some(t) => ctx.loot.roll(&env, t, &cx),
        None => Vec::new(),
    }
}

/// Experience a player gets for killing this mob.
fn experience(m: &mut Mob) -> i32 {
    let (lo, hi) = m.species.xp_reward();
    let mut xp = m.rand_range(lo, hi);
    match m.species {
        Species::Slime | Species::MagmaCube => return m.size,
        _ => {}
    }
    if m.species.category() == Category::Monster {
        for s in 0..6 {
            if m.gear[s].is_some() && m.drop_chance[s] <= 1.0 {
                xp += 1 + m.below(3) as i32;
            }
        }
    }
    if m.is_baby() {
        xp = if m.species.category() == Category::Monster {
            (xp as f32 * 2.5) as i32
        } else {
            0
        };
    }
    xp
}

/// Starts a mob's death: the animation, and what it leaves. `killer` is the
/// kind that killed it (`minecraft:player`) and its weapon.
pub fn die(
    ctx: &ServerContext,
    e: &mut Entity,
    killer: Option<(&str, Option<crate::loot::Tool>)>,
    _looting: i32,
) {
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.dying.is_some() {
        return;
    }
    m.dying = Some(20);
    m.health = 0.0;
    // Its lead falls where it died.
    crate::decor::leash::unleash(ctx, id, e.pos, true);
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    m.target = None;
    m.nav.stop();
    let by_player = m.hurt_by_player.map(|(p, _)| p);
    if !m.riders.is_empty() {
        super::ride::eject(ctx, e);
    }
    fx::status(ctx, id, 3);
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    let killer = killer.or_else(|| by_player.map(|_| ("minecraft:player", None)));
    let drops_on = ctx.game_rule_bool("minecraft:mob_drops");
    if by_player.is_some() && drops_on {
        let xp = experience(m);
        if xp > 0 {
            crate::xp::spawn(ctx, [pos[0], pos[1] + 0.3, pos[2]], xp);
        }
    }
    let looting = killer
        .as_ref()
        .and_then(|(_, w)| w.as_ref())
        .and_then(|w| w.enchants.iter().find(|(n, _)| n == "minecraft:looting"))
        .map_or(0, |(_, l)| *l);
    // Equipment.
    let mut gear_drops = Vec::new();
    for s in 0..8 {
        let Some(g) = m.gear[s].clone() else {
            continue;
        };
        let chance = m.drop_chance[s];
        let always = chance > 1.0;
        if always || by_player.is_some() && (m.next_f64() as f32) < chance + 0.01 * looting as f32 {
            let mut damage = g.damage;
            if !always {
                let max = ctx.items.max_damage(g.item);
                if max > 0 {
                    let worn = max - max.min(1 + m.below(max.max(1) as u32 / 2) as i32);
                    damage = damage.max(worn.min(max - 1));
                }
            }
            gear_drops.push((g.item, g.count, damage, g.components));
            m.gear[s] = None;
        }
    }
    let path = m.species.path();
    let table = format!("minecraft:entities/{path}");
    let drops = if drops_on {
        roll(ctx, e, &table, by_player.is_some(), killer)
    } else {
        gear_drops.clear();
        Vec::new()
    };
    for drop in drops {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], drop.item, drop.count);
    }
    // The wither's star is not in its loot table.
    if drops_on && path == "wither" {
        crate::bosses::wither::drop_star(ctx, pos);
    }
    for (item, count, damage, components) in gear_drops {
        spawn_stack(ctx, pos, item, count, damage, components);
    }
    // A chest on its back spills.
    if let Kind::Mob(m) = &e.kind
        && m.chested
    {
        super::chest::spill(ctx, id, pos);
    }
    // An enderman lets its block fall.
    if let Kind::Mob(m) = &mut e.kind
        && let Some(state) = m.carried.take()
        && drops_on
        && let Some(item) = ctx
            .data
            .blocks
            .block_name(state)
            .and_then(|n| ctx.items.id(n))
    {
        spawn_stack(ctx, pos, item, 1, 0, Default::default());
    }
    // Big slimes split into smaller ones.
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if matches!(m.species, Species::Slime | Species::MagmaCube) && m.size > 1 {
        let n = 2 + m.below(3);
        for k in 0..n {
            let dx = (f64::from(k % 2) - 0.5) * f64::from(m.size) / 4.0;
            let dz = (f64::from(k / 2) - 0.5) * f64::from(m.size) / 4.0;
            let mut s = super::new_mob(ctx, m.species, [pos[0] + dx, pos[1] + 0.5, pos[2] + dz]);
            if let Kind::Mob(sm) = &mut s.kind {
                sm.set_size(m.size / 2);
                sm.persistent = m.persistent;
            }
            let _ = ctx.events.send(Event::EntitySpawned(s.clone()));
            lock(&ctx.entities).push(s);
        }
    }
}

fn spawn_stack(
    ctx: &ServerContext,
    pos: [f64; 3],
    item: i32,
    count: i32,
    damage: i32,
    components: mc_protocol::components::Components,
) {
    let stack = mc_protocol::components::ItemStack {
        count,
        item,
        damage,
        components,
    };
    let mut e = Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::stack(&stack, 10),
        pos: [pos[0], pos[1] + 0.3, pos[2]],
        vel: [0.0, 0.2, 0.0],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    e.vel[0] = (crate::hits::rnd() - 0.5) * 0.2;
    e.vel[2] = (crate::hits::rnd() - 0.5) * 0.2;
    let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    lock(&ctx.entities).push(e);
}

/// A chicken lays: the `gameplay/chicken_lay` table (its colour of egg).
pub fn lay(ctx: &ServerContext, e: &Entity, fallback: &str) {
    let mut drops = roll(ctx, e, "minecraft:gameplay/chicken_lay", false, None);
    if drops.is_empty()
        && let Some(item) = ctx.items.id(fallback)
    {
        drops.push(crate::loot::Drop {
            item,
            count: 1,
            components: Default::default(),
        });
    }
    for d in drops {
        crate::tick::spawn_item_at(ctx, [e.pos[0], e.pos[1] + 0.3, e.pos[2]], d.item, d.count);
    }
}

/// Two mobs in love make a baby; returns it (not yet in the world).
pub fn breed(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    partner: i32,
) -> Option<Entity> {
    let j = entities.iter().position(|e| e.id == partner)?;
    let (a, b) = (&entities[i], &entities[j]);
    let (Kind::Mob(ma), Kind::Mob(mb)) = (&a.kind, &b.kind) else {
        return None;
    };
    if !ma.in_love() || !mb.in_love() || ma.is_baby() || mb.is_baby() {
        return None;
    }
    let kind = match (ma.species, mb.species) {
        (Species::Horse, Species::Donkey) | (Species::Donkey, Species::Horse) => Species::Mule,
        (k, _) => k,
    };
    let by = ma.love_by.or(mb.love_by);
    let pos = a.pos;
    let (va, vb) = (ma.variant, mb.variant);
    let owner = ma.owner.or(mb.owner);
    let mut baby = super::new_mob(ctx, kind, pos);
    if let Kind::Mob(m) = &mut baby.kind {
        m.set_baby(true);
        m.variant = if m.below(2) == 0 { va } else { vb };
        if kind.llama() {
            // The mean of the parents' strengths, now and then one more.
            let both = ma.strength + mb.strength;
            m.strength = ((both + 1) / 2 + i32::from(m.below(10) == 0)).clamp(1, 5);
        }
        if kind == Species::Sheep {
            let pick = m.below(2) == 0;
            m.set_color(mix_wool(ma.color, mb.color, pick));
        }
        if matches!(kind, Species::Wolf | Species::Cat) && owner.is_some() {
            m.owner = owner;
            m.set_max_health(if kind == Species::Wolf { 40.0 } else { 10.0 });
        }
        m.persistent = true;
    }
    for k in [i, j] {
        if let Kind::Mob(m) = &mut entities[k].kind {
            m.love = 0;
            m.love_by = None;
            m.baby_age = 6000;
        }
    }
    fx::status(ctx, entities[i].id, 18);
    if let Some(p) = by {
        let _ = p;
        let xp = 1 + (crate::util::random_u64() % 7) as i32;
        crate::xp::spawn(ctx, pos, xp);
    }
    Some(baby)
}

/// A lamb takes one parent's colour, or the mix where two dyes make one.
fn mix_wool(a: u8, b: u8, first: bool) -> u8 {
    let mix = match (a.min(b), a.max(b)) {
        (0, 15) => Some(7),   // white + black = gray
        (0, 7) => Some(8),    // white + gray = light gray
        (0, 14) => Some(6),   // white + red = pink
        (0, 11) => Some(3),   // white + blue = light blue
        (0, 13) => Some(5),   // white + green = lime
        (4, 14) => Some(1),   // yellow + red = orange
        (11, 14) => Some(10), // blue + red = purple
        (11, 13) => Some(9),  // blue + green = cyan
        (6, 10) => Some(2),   // pink + purple = magenta
        _ => None,
    };
    mix.unwrap_or(if first { a } else { b })
}

/// A mob turns into another kind: a new entity in its place (returned), the
/// old one removed.
pub fn convert(ctx: &ServerContext, e: &mut Entity, to: Species) -> Option<Entity> {
    let Kind::Mob(old) = &e.kind else {
        return None;
    };
    let mut new = super::new_mob(ctx, to, e.pos);
    let Kind::Mob(m) = &mut new.kind else {
        return None;
    };
    m.yaw = old.yaw;
    m.head_yaw = old.head_yaw;
    m.baby_age = old.baby_age;
    m.custom_name = old.custom_name.clone();
    m.persistent = old.persistent;
    m.gear = old.gear.clone();
    m.drop_chance = old.drop_chance;
    m.can_pick_up = old.can_pick_up;
    m.no_ai = old.no_ai;
    let sound = match (old.species, to) {
        (Species::Zombie, Species::Drowned) => Some("entity.zombie.converted_to_drowned"),
        (Species::Husk, Species::Zombie) => Some("entity.husk.converted_to_zombie"),
        (Species::Skeleton, Species::Stray) => Some("entity.skeleton.converted_to_stray"),
        _ => None,
    };
    if let Some(s) = sound {
        fx::sound(ctx, s, 5, e.pos, 1.0, 1.0);
    }
    e.removed = true;
    Some(new)
}

/// A zombie villager is cured: a villager with what it was.
pub fn cure(ctx: &ServerContext, e: &mut Entity) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let data = m.villager.clone();
    let Some(mut v) = crate::trading::new_merchant(ctx, "minecraft:villager", e.pos) else {
        return;
    };
    if let (Kind::Merchant(mv), Some((kind, profession, level))) = (&mut v.kind, data) {
        mv.variant = kind;
        mv.profession = profession;
        mv.level = level;
    }
    e.removed = true;
    fx::status(ctx, v.id, 16);
    fx::sound(ctx, "entity.zombie_villager.converted", 5, e.pos, 1.0, 1.0);
    let id = v.id;
    let packet = match &v.kind {
        Kind::Merchant(mv) => Some(crate::trading::villager_data(ctx, id, mv)),
        _ => None,
    };
    crate::trading::spawn(ctx, v);
    if let Some(p) = packet {
        let _ = ctx.events.send(Event::EntitiesData(vec![p]));
    }
}

/// Wears a mob's equipment in `slot`; it breaks at its limit.
pub fn wear_gear(ctx: &ServerContext, e: &mut Entity, s: usize, amount: i32) {
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    let Some(g) = &mut m.gear[s] else {
        return;
    };
    let max = ctx.items.max_damage(g.item);
    if max == 0 {
        return;
    }
    g.damage += amount;
    if g.damage >= max {
        m.gear[s] = None;
        let status = match s {
            slot::MAINHAND => 47,
            slot::OFFHAND => 48,
            slot::HEAD => 49,
            slot::CHEST => 50,
            slot::LEGS => 51,
            slot::FEET => 52,
            _ => 65,
        };
        fx::status(ctx, id, status);
        fx::emit(
            ctx,
            fx::Fx::Equip(play::SetEquipment {
                entity_id: id,
                slots: vec![(s as u8, None)],
            }),
        );
    }
}

/// Sends a mob's metadata now (outside its tick).
pub fn send_data(ctx: &ServerContext, e: &Entity) {
    if let Kind::Mob(m) = &e.kind {
        fx::emit(
            ctx,
            fx::Fx::Data(play::SetEntityData {
                entity_id: e.id,
                entries: metadata_of(ctx, m, lock(&ctx.time).age),
            }),
        );
    }
}
