//! Advancement criteria: what happened, tested against the conditions of
//! each criterion of its trigger (minecraft.wiki, "Advancement definition",
//! "Predicate", "Data component predicate"). Each trigger has a function
//! here, called where it happens in the game; the conditions of the
//! criteria are worked through in `commands::progress`.

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::context::ServerContext;
use crate::enchant_effects::is;
use crate::tick::lock;

/// An entity as conditions see it.
#[derive(Clone, Debug, Default)]
pub struct Subject {
    /// `minecraft:cow`.
    pub kind: String,
    pub id: i32,
    pub pos: [f64; 3],
    pub dimension: String,
    pub baby: bool,
    pub on_fire: bool,
    pub sneaking: bool,
    /// Worn and held stacks by slot (`mainhand`, `head`, `body`...).
    pub equipment: Vec<(&'static str, ItemStack)>,
    /// Data components as predicates name them (`minecraft:cat/variant`).
    pub components: Vec<(String, String)>,
    pub vehicle: Option<Box<Subject>>,
    pub passengers: Vec<Subject>,
    /// A player: the entity in their crosshair.
    pub looking_at: Option<Box<Subject>>,
    /// A lightning bolt: how many fires it lit.
    pub fires_lit: Option<i32>,
}

/// Mob gear slots by the names predicates use.
const MOB_SLOTS: [(&str, usize); 7] = [
    ("mainhand", mc_entity::mob::slot::MAINHAND),
    ("offhand", mc_entity::mob::slot::OFFHAND),
    ("feet", mc_entity::mob::slot::FEET),
    ("legs", mc_entity::mob::slot::LEGS),
    ("chest", mc_entity::mob::slot::CHEST),
    ("head", mc_entity::mob::slot::HEAD),
    ("body", mc_entity::mob::slot::BODY),
];

/// An entity of the world as conditions see it.
pub fn entity(ctx: &ServerContext, e: &mc_entity::Entity) -> Subject {
    use mc_entity::Kind;
    let mut s = Subject {
        kind: crate::stats::entity_type(e).unwrap_or_default(),
        id: e.id,
        pos: e.pos,
        dimension: ctx.world.dimension.clone(),
        ..Subject::default()
    };
    match &e.kind {
        Kind::Mob(m) => {
            let ent = crate::mobs::hurt::ent_of(ctx, m);
            s.baby = ent.baby;
            s.on_fire = ent.on_fire;
            s.components = ent.components;
            for (name, slot) in MOB_SLOTS {
                if let Some(g) = &m.gear[slot] {
                    s.equipment.push((
                        name,
                        ItemStack {
                            count: g.count,
                            item: g.item,
                            damage: g.damage,
                            components: g.components.clone(),
                        },
                    ));
                }
            }
        }
        Kind::Merchant(m) => s.baby = m.is_baby(),
        _ => {}
    }
    s
}

/// What an entity rides, if it rides anything (looked up only when the
/// entity list is free, as callers may hold it).
fn vehicle_of(ctx: &ServerContext, id: i32) -> Option<Subject> {
    use mc_entity::Kind;
    let entities = ctx.entities.try_lock().ok()?;
    let e = entities.iter().find(|e| {
        !e.removed
            && match &e.kind {
                Kind::Vehicle(v) => v.riders.contains(&id),
                Kind::Mob(m) => m.riders.contains(&id),
                _ => false,
            }
    })?;
    let mut v = entity(ctx, e);
    let riders: &[i32] = match &e.kind {
        Kind::Vehicle(v) => &v.riders,
        Kind::Mob(m) => &m.riders,
        _ => &[],
    };
    for r in riders {
        if let Some(p) = entities.iter().find(|p| p.id == *r) {
            v.passengers.push(entity(ctx, p));
        } else if let Some(p) = player_by_entity(ctx, *r) {
            v.passengers.push(p);
        }
    }
    Some(v)
}

/// A player as conditions see them, without what they ride.
fn player_plain(ctx: &ServerContext, uuid: u128) -> Option<Subject> {
    let (pose, dimension, id) = {
        let players = lock(&ctx.players);
        let p = players.get(&uuid)?;
        (p.pose?, p.dimension.clone(), p.entity_id)
    };
    let mut s = Subject {
        kind: "minecraft:player".into(),
        id,
        pos: [pose.x, pose.y, pose.z],
        dimension,
        sneaking: crate::stats::sneaking(ctx, uuid),
        ..Subject::default()
    };
    if let Some(h) = lock(&ctx.inventories).get(&uuid) {
        let slots = [
            ("mainhand", 36 + h.selected),
            ("offhand", 45),
            ("head", 5),
            ("chest", 6),
            ("legs", 7),
            ("feet", 8),
        ];
        for (name, i) in slots {
            if let Some(Some(stack)) = h.slots.get(i) {
                s.equipment.push((name, stack.clone()));
            }
        }
    }
    Some(s)
}

fn player_by_entity(ctx: &ServerContext, id: i32) -> Option<Subject> {
    let uuid = lock(&ctx.players)
        .values()
        .find(|p| p.entity_id == id)
        .map(|p| p.profile.uuid)?;
    player_plain(ctx, uuid)
}

/// A player as conditions see them.
pub fn player(ctx: &ServerContext, uuid: u128) -> Option<Subject> {
    let mut s = player_plain(ctx, uuid)?;
    s.vehicle = vehicle_of(ctx, s.id).map(Box::new);
    Some(s)
}

/// A number range: an exact number, `{min, max}` or nothing.
pub fn range(r: &Value, x: f64) -> bool {
    match r {
        Value::Null => true,
        Value::Number(n) => n.as_f64() == Some(x),
        _ => {
            r["min"].as_f64().is_none_or(|lo| x >= lo) && r["max"].as_f64().is_none_or(|hi| x <= hi)
        }
    }
}

/// Whether a stack is what an item predicate asks for.
pub fn item(ctx: &ServerContext, p: &Value, stack: Option<&ItemStack>) -> bool {
    if p.is_null() {
        return true;
    }
    let Some(stack) = stack.filter(|s| s.count > 0) else {
        return false;
    };
    let name = ctx.items.name(stack.item).unwrap_or("");
    if !p["items"].is_null() && !is(ctx, "minecraft:item", &p["items"], name) {
        return false;
    }
    if !range(&p["count"], f64::from(stack.count)) {
        return false;
    }
    for (key, want) in p["components"].as_object().into_iter().flatten() {
        if !component_is(stack, key, want) {
            return false;
        }
    }
    let predicates = &p["predicates"];
    for (key, want) in predicates.as_object().into_iter().flatten() {
        let ok = match key.trim_start_matches("minecraft:") {
            "enchantments" => enchanted(ctx, crate::enchants::levels(stack), want),
            "stored_enchantments" => enchanted(
                ctx,
                crate::enchants::levels_in(stack, "minecraft:stored_enchantments"),
                want,
            ),
            "jukebox_playable" => ctx.items.get(stack.item).is_some_and(|i| {
                i.jukebox_song.is_some()
                    && want["song"]
                        .as_str()
                        .is_none_or(|s| i.jukebox_song.as_deref() == Some(s))
            }),
            "damage" => {
                range(&want["damage"], f64::from(stack.damage))
                    && range(
                        &want["durability"],
                        f64::from(
                            ctx.items.get(stack.item).map_or(0, |i| i.max_damage) - stack.damage,
                        ),
                    )
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Whether a stack carries exactly this component value.
fn component_is(stack: &ItemStack, key: &str, want: &Value) -> bool {
    let key = mc_protocol::components::full_name(key).into_owned();
    if key == "minecraft:damage" {
        return want.as_i64() == Some(i64::from(stack.damage));
    }
    let Some(tag) = mc_data::json_to_nbt(want) else {
        return false;
    };
    let mut c = mc_nbt::Compound::new();
    c.insert(key.as_str(), tag);
    let (_, wanted) = mc_protocol::components::Components::from_nbt(&c);
    wanted
        .get(&key)
        .is_some_and(|w| stack.components.get(&key) == Some(w))
}

/// `minecraft:enchantments`: every listed entry has a match on the stack.
fn enchanted(ctx: &ServerContext, have: Vec<(String, i32)>, want: &Value) -> bool {
    let list = match want {
        Value::Array(a) => a.iter().collect::<Vec<_>>(),
        Value::Null => return true,
        v => vec![v],
    };
    list.into_iter().all(|e| {
        have.iter().any(|(n, l)| {
            (e["enchantments"].is_null() || is(ctx, "minecraft:enchantment", &e["enchantments"], n))
                && range(&e["levels"], f64::from(*l))
        })
    })
}

/// The block at a place: its name and state.
fn block_at(ctx: &ServerContext, p: Position) -> Option<(String, u32)> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    Some((ctx.shapes.block_name(state)?.to_owned(), state))
}

fn state_is(ctx: &ServerContext, state: u32, want: &Value) -> bool {
    want.as_object().into_iter().flatten().all(|(k, v)| {
        ctx.shapes.property(state, k).is_some_and(|pv| match v {
            Value::String(s) => s == pv,
            Value::Bool(b) => pv == b.to_string(),
            Value::Number(n) => pv == n.to_string(),
            r => pv.parse::<f64>().is_ok_and(|x| range(r, x)),
        })
    })
}

pub(crate) fn block_is(ctx: &ServerContext, p: &Value, at: Position) -> bool {
    if p.is_null() {
        return true;
    }
    let Some((name, state)) = block_at(ctx, at) else {
        return false;
    };
    (p["blocks"].is_null() || is(ctx, "minecraft:block", &p["blocks"], &name))
        && state_is(ctx, state, &p["state"])
}

/// A lit campfire up to five blocks below (`smokey`).
fn smokey(ctx: &ServerContext, at: Position) -> bool {
    (1..=5).any(|d| {
        block_at(ctx, Position::new(at.x, at.y - d, at.z)).is_some_and(|(n, state)| {
            n.ends_with("campfire") && ctx.shapes.property(state, "lit") == Some("true")
        })
    })
}

fn block_pos(pos: [f64; 3]) -> Position {
    Position::new(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    )
}

/// Whether a place is what a location predicate asks for.
pub fn location(ctx: &ServerContext, p: &Value, pos: [f64; 3], dimension: &str) -> bool {
    if p.is_null() {
        return true;
    }
    let at = block_pos(pos);
    let pos_ok = p["position"].is_null()
        || range(&p["position"]["x"], pos[0])
            && range(&p["position"]["y"], pos[1])
            && range(&p["position"]["z"], pos[2]);
    if !pos_ok {
        return false;
    }
    if let Some(d) = p["dimension"].as_str()
        && crate::enchant_effects::full(d) != dimension
    {
        return false;
    }
    let here = ctx.in_dimension(dimension);
    let ctx = here.as_deref().unwrap_or(ctx);
    if !p["biomes"].is_null() {
        let Some(biome) = crate::mobs::aqua::biome_at(ctx, pos) else {
            return false;
        };
        if !is(ctx, "minecraft:worldgen/biome", &p["biomes"], &biome) {
            return false;
        }
    }
    if !p["structures"].is_null() {
        let names: Vec<String> = match &p["structures"] {
            Value::String(s) if s.starts_with('#') => ctx
                .commands
                .env
                .tag("minecraft:worldgen/structure", &s[1..]),
            v => v
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(crate::enchant_effects::full))
                        .collect()
                })
                .unwrap_or_else(|| {
                    v.as_str()
                        .map(crate::enchant_effects::full)
                        .into_iter()
                        .collect()
                }),
        };
        let Some(pipeline) = ctx.world.pipeline() else {
            return false;
        };
        if !names.iter().any(|n| pipeline.inside(n, at.x, at.y, at.z)) {
            return false;
        }
    }
    if !block_is(ctx, &p["block"], at) {
        return false;
    }
    if !p["fluid"].is_null() {
        let Some((name, state)) = block_at(ctx, at) else {
            return false;
        };
        let waterlogged = ctx.shapes.property(state, "waterlogged") == Some("true");
        let fluid = match name.as_str() {
            "minecraft:water" | "minecraft:bubble_column" => "minecraft:water",
            "minecraft:lava" => "minecraft:lava",
            _ if waterlogged => "minecraft:water",
            _ => "minecraft:empty",
        };
        if !p["fluid"]["fluids"].is_null()
            && !is(ctx, "minecraft:fluid", &p["fluid"]["fluids"], fluid)
        {
            return false;
        }
    }
    if let Some(want) = p["smokey"].as_bool()
        && smokey(ctx, at) != want
    {
        return false;
    }
    if let Some(want) = p["can_see_sky"].as_bool()
        && (ctx.world.sky_light(at) == 15) != want
    {
        return false;
    }
    true
}

/// Whether an entity is what an entity predicate asks for; distances are
/// measured from `origin`.
pub fn entity_is(ctx: &ServerContext, p: &Value, s: &Subject, origin: [f64; 3]) -> bool {
    let Some(o) = p.as_object() else {
        return p.is_null();
    };
    o.iter()
        .all(|(k, v)| match k.trim_start_matches("minecraft:") {
            "type" | "entity_type" => is(ctx, "minecraft:entity_type", v, &s.kind),
            "flags" => {
                v["is_baby"].as_bool().is_none_or(|w| w == s.baby)
                    && v["is_on_fire"].as_bool().is_none_or(|w| w == s.on_fire)
                    && v["is_sneaking"].as_bool().is_none_or(|w| w == s.sneaking)
            }
            "components" => v.as_object().into_iter().flatten().all(|(name, want)| {
                s.components.iter().any(|(n, have)| {
                    n == name
                        && want
                            .as_str()
                            .is_some_and(|w| w == have || crate::enchant_effects::full(w) == *have)
                })
            }),
            "equipment" => v.as_object().into_iter().flatten().all(|(slot, want)| {
                item(
                    ctx,
                    want,
                    s.equipment
                        .iter()
                        .find(|(n, _)| n == slot)
                        .map(|(_, st)| st),
                )
            }),
            "location" => location(ctx, v, s.pos, &s.dimension),
            "stepping_on" => location(ctx, v, [s.pos[0], s.pos[1] - 0.5, s.pos[2]], &s.dimension),
            "distance" => {
                let d = [
                    s.pos[0] - origin[0],
                    s.pos[1] - origin[1],
                    s.pos[2] - origin[2],
                ];
                range(&v["x"], d[0].abs())
                    && range(&v["y"], d[1].abs())
                    && range(&v["z"], d[2].abs())
                    && range(&v["horizontal"], (d[0] * d[0] + d[2] * d[2]).sqrt())
                    && range(
                        &v["absolute"],
                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt(),
                    )
            }
            "vehicle" => s
                .vehicle
                .as_deref()
                .is_some_and(|r| entity_is(ctx, v, r, origin)),
            "passenger" => s.passengers.iter().any(|r| entity_is(ctx, v, r, origin)),
            "type_specific/player" => {
                v["looking_at"].is_null()
                    || s.looking_at
                        .as_deref()
                        .is_some_and(|l| entity_is(ctx, &v["looking_at"], l, origin))
            }
            "type_specific/lightning" => s
                .fires_lit
                .is_some_and(|n| range(&v["blocks_set_on_fire"], f64::from(n))),
            _ => false,
        })
}

/// The loot condition an object is, if it is one (and not an old-style
/// entity predicate, whose `type` is an entity type).
fn condition_kind(c: &Value) -> Option<&str> {
    let kind = c["condition"].as_str().or(c["type"].as_str())?;
    let kind = kind.trim_start_matches("minecraft:");
    matches!(
        kind,
        "entity_properties" | "inverted" | "any_of" | "alternative" | "all_of"
    )
    .then_some(kind)
}

/// A contextual predicate about one entity: loot conditions with the entity
/// as `this` (or, in older packs, a bare entity predicate). An empty one
/// holds for anything.
pub fn holds(ctx: &ServerContext, c: &Value, s: Option<&Subject>, origin: [f64; 3]) -> bool {
    match c {
        Value::Null => true,
        Value::Array(a) => a.iter().all(|c| holds(ctx, c, s, origin)),
        Value::Object(_) if condition_kind(c).is_some() => match condition_kind(c).unwrap_or("") {
            "entity_properties" => s.is_some_and(|s| entity_is(ctx, &c["predicate"], s, origin)),
            "inverted" => !holds(ctx, &c["term"], s, origin),
            "any_of" | "alternative" => c["terms"]
                .as_array()
                .is_some_and(|t| t.iter().any(|c| holds(ctx, c, s, origin))),
            "all_of" => c["terms"]
                .as_array()
                .is_none_or(|t| t.iter().all(|c| holds(ctx, c, s, origin))),
            _ => false,
        },
        _ => s.is_some_and(|s| entity_is(ctx, c, s, origin)),
    }
}

/// The conditions about a block that was placed or used (`location` of
/// `placed_block`, `item_used_on_block`): checks at the block, by the
/// state it has now, with the stack used as the tool.
pub fn block_holds(ctx: &ServerContext, c: &Value, at: Position, tool: Option<&ItemStack>) -> bool {
    match c {
        Value::Null => true,
        Value::Array(a) => a.iter().all(|c| block_holds(ctx, c, at, tool)),
        _ => {
            let kind = c["condition"].as_str().or(c["type"].as_str()).unwrap_or("");
            match kind.trim_start_matches("minecraft:") {
                "location_check" => {
                    let off = |k: &str| c[k].as_i64().unwrap_or(0) as i32;
                    let p = Position::new(
                        at.x + off("offsetX"),
                        at.y + off("offsetY"),
                        at.z + off("offsetZ"),
                    );
                    let pos = [
                        f64::from(p.x) + 0.5,
                        f64::from(p.y) + 0.5,
                        f64::from(p.z) + 0.5,
                    ];
                    location(ctx, &c["predicate"], pos, &ctx.world.dimension)
                }
                "match_block" => block_is(ctx, c, at),
                "block_state_property" => block_at(ctx, at).is_some_and(|(name, state)| {
                    c["block"]
                        .as_str()
                        .is_some_and(|b| crate::enchant_effects::full(b) == name)
                        && state_is(ctx, state, &c["properties"])
                }),
                "match_tool" => item(ctx, &c["predicate"], tool),
                "inverted" => !block_holds(ctx, &c["term"], at, tool),
                "any_of" | "alternative" => c["terms"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|c| block_holds(ctx, c, at, tool))),
                "all_of" => c["terms"]
                    .as_array()
                    .is_none_or(|t| t.iter().all(|c| block_holds(ctx, c, at, tool))),
                _ => false,
            }
        }
    }
}

/// A hit as damage predicates see it.
#[derive(Clone, Debug, Default)]
pub struct Hit {
    /// The damage type (`minecraft:arrow`).
    pub kind: String,
    /// What struck (the arrow).
    pub direct: Option<Subject>,
    /// Who is to blame (the archer).
    pub source: Option<Subject>,
    pub dealt: f32,
    pub taken: f32,
    pub blocked: bool,
}

/// A damage source predicate (`killing_blow`, the `type` of a damage
/// predicate).
pub fn source_is(ctx: &ServerContext, p: &Value, h: &Hit, origin: [f64; 3]) -> bool {
    if p.is_null() {
        return true;
    }
    let tags_ok = p["tags"].as_array().into_iter().flatten().all(|t| {
        let id = t["id"].as_str().unwrap_or("");
        let id = id.trim_start_matches('#');
        crate::enchant_effects::damage_is(ctx, &h.kind, id)
            == t["expected"].as_bool().unwrap_or(true)
    });
    let direct_ok = p["direct_entity"].is_null()
        || h.direct
            .as_ref()
            .is_some_and(|d| entity_is(ctx, &p["direct_entity"], d, origin));
    let source_ok = p["source_entity"].is_null()
        || h.source
            .as_ref()
            .is_some_and(|d| entity_is(ctx, &p["source_entity"], d, origin));
    let is_direct_ok = p["is_direct"].as_bool().is_none_or(|w| {
        w == match (&h.direct, &h.source) {
            (Some(d), Some(s)) => d.id == s.id,
            _ => true,
        }
    });
    tags_ok && direct_ok && source_ok && is_direct_ok
}

/// A damage predicate (`damage` of `player_hurt_entity`).
pub fn damage_is(ctx: &ServerContext, p: &Value, h: &Hit, origin: [f64; 3]) -> bool {
    if p.is_null() {
        return true;
    }
    range(&p["dealt"], f64::from(h.dealt))
        && range(&p["taken"], f64::from(h.taken))
        && p["blocked"].as_bool().is_none_or(|w| w == h.blocked)
        && (p["source_entity"].is_null()
            || h.source
                .as_ref()
                .is_some_and(|s| entity_is(ctx, &p["source_entity"], s, origin)))
        && source_is(ctx, &p["type"], h, origin)
}

// -- triggers

use crate::commands::criterion;

/// The hit with the player as its source when it names none.
fn from_player(hit: &Hit, me: &Subject) -> Hit {
    let mut h = hit.clone();
    if h.source.is_none() {
        h.source = Some(me.clone());
    }
    if h.direct.is_none() {
        h.direct = h.source.clone();
    }
    h
}

/// `player_killed_entity`: the player killed `victim`.
pub fn killed(ctx: &ServerContext, uuid: u128, victim: &Subject, hit: &Hit) {
    criterion(ctx, uuid, "minecraft:player_killed_entity", |c, me| {
        holds(ctx, &c["entity"], Some(victim), me.pos)
            && source_is(ctx, &c["killing_blow"], &from_player(hit, me), me.pos)
    });
}

/// `player_hurt_entity`: the player hurt `victim`.
pub fn hurt_entity(ctx: &ServerContext, uuid: u128, victim: &Subject, hit: &Hit) {
    criterion(ctx, uuid, "minecraft:player_hurt_entity", |c, me| {
        holds(ctx, &c["entity"], Some(victim), me.pos)
            && damage_is(ctx, &c["damage"], &from_player(hit, me), me.pos)
    });
}

/// `entity_hurt_player`: the player was hurt.
pub fn hurt_by(ctx: &ServerContext, uuid: u128, hit: &Hit) {
    criterion(ctx, uuid, "minecraft:entity_hurt_player", |c, me| {
        damage_is(ctx, &c["damage"], hit, me.pos)
    });
}

/// `entity_killed_player`: the player was killed.
pub fn killed_by(ctx: &ServerContext, uuid: u128, hit: &Hit) {
    criterion(ctx, uuid, "minecraft:entity_killed_player", |c, me| {
        holds(ctx, &c["entity"], hit.source.as_ref(), me.pos)
            && source_is(ctx, &c["killing_blow"], hit, me.pos)
    });
}

/// Triggers with nothing to test beyond the player (`location`,
/// `slept_in_bed`, `hero_of_the_village`, `voluntary_exile`...).
pub fn plain(ctx: &ServerContext, uuid: u128, trigger: &str) {
    criterion(ctx, uuid, trigger, |_, _| true);
}

/// Triggers about one stack (`consume_item`, `filled_bucket`,
/// `shot_crossbow`, `used_totem`, `item_durability_changed`...).
pub fn with_item(ctx: &ServerContext, uuid: u128, trigger: &str, stack: &ItemStack) {
    criterion(ctx, uuid, trigger, |c, _| {
        item(ctx, &c["item"], Some(stack))
    });
}

/// Triggers about one entity (`tame_animal`, `summoned_entity`,
/// `started_riding`...): `entity` is tested.
pub fn with_entity(ctx: &ServerContext, uuid: u128, trigger: &str, e: &Subject) {
    criterion(ctx, uuid, trigger, |c, me| {
        holds(ctx, &c["entity"], Some(e), me.pos)
    });
}

/// `player_interacted_with_entity`, `player_sheared_equipment`,
/// `thrown_item_picked_up_by_entity`, `thrown_item_picked_up_by_player`:
/// an entity and a stack.
pub fn entity_and_item(
    ctx: &ServerContext,
    uuid: u128,
    trigger: &str,
    e: &Subject,
    stack: Option<&ItemStack>,
) {
    criterion(ctx, uuid, trigger, |c, me| {
        holds(ctx, &c["entity"], Some(e), me.pos) && item(ctx, &c["item"], stack)
    });
}

/// `bred_animals`: two parents made a child.
pub fn bred(
    ctx: &ServerContext,
    uuid: u128,
    parent: &Subject,
    partner: &Subject,
    child: Option<&Subject>,
) {
    criterion(ctx, uuid, "minecraft:bred_animals", |c, me| {
        let ok = |k: &str, s: Option<&Subject>| c[k].is_null() || holds(ctx, &c[k], s, me.pos);
        ok("child", child)
            && (ok("parent", Some(parent)) && ok("partner", Some(partner))
                || ok("parent", Some(partner)) && ok("partner", Some(parent)))
    });
}

/// `placed_block`, `item_used_on_block`, `allay_drop_item_on_block`: a
/// block at `at` with a stack.
pub fn on_block(
    ctx: &ServerContext,
    uuid: u128,
    trigger: &str,
    at: Position,
    stack: Option<&ItemStack>,
) {
    criterion(ctx, uuid, trigger, |c, _| {
        block_holds(ctx, &c["location"], at, stack)
    });
}

/// `bee_nest_destroyed`: a player broke a hive or nest with `tool`, with
/// `bees` inside.
pub fn bee_nest(
    ctx: &ServerContext,
    uuid: u128,
    block: &str,
    tool: Option<&ItemStack>,
    bees: usize,
) {
    criterion(ctx, uuid, "minecraft:bee_nest_destroyed", |c, _| {
        (c["blocks"].is_null() || is(ctx, "minecraft:block", &c["blocks"], block))
            && item(ctx, &c["item"], tool)
            && range(&c["num_bees_inside"], bees as f64)
    });
}

/// `recipe_crafted`, `crafter_recipe_crafted`: a recipe was made from these
/// ingredients.
pub fn crafted(
    ctx: &ServerContext,
    uuid: u128,
    trigger: &str,
    recipe: &str,
    ingredients: &[ItemStack],
) {
    criterion(ctx, uuid, trigger, |c, _| {
        c["recipe_id"]
            .as_str()
            .is_none_or(|r| crate::enchant_effects::full(r) == recipe)
            && c["recipes"]
                .as_str()
                .is_none_or(|r| crate::enchant_effects::full(r) == recipe)
            && each_matched(ctx, &c["ingredients"], ingredients)
    });
}

/// Whether every predicate of the list matches an ingredient of its own.
fn each_matched(ctx: &ServerContext, wants: &Value, ingredients: &[ItemStack]) -> bool {
    let mut left: Vec<&ItemStack> = ingredients.iter().collect();
    wants.as_array().into_iter().flatten().all(|p| {
        match left.iter().position(|s| item(ctx, p, Some(s))) {
            Some(i) => {
                left.swap_remove(i);
                true
            }
            None => false,
        }
    })
}

/// `player_generates_container_loot`: a loot table was rolled into a
/// container the player opened.
pub fn container_loot(ctx: &ServerContext, uuid: u128, table: &str) {
    criterion(
        ctx,
        uuid,
        "minecraft:player_generates_container_loot",
        |c, _| {
            c["loot_table"]
                .as_str()
                .is_none_or(|t| crate::enchant_effects::full(t) == table)
                && (c["loot_tables"].is_null()
                    || is(ctx, "minecraft:loot_table", &c["loot_tables"], table))
        },
    );
}

/// Where each player last left the overworld for the Nether.
static ENTERED_NETHER: std::sync::Mutex<Option<std::collections::HashMap<u128, [f64; 3]>>> =
    std::sync::Mutex::new(None);

/// `changed_dimension`, and `nether_travel` on coming back from the Nether:
/// the player left `from` at `left`.
pub fn changed_dimension(ctx: &ServerContext, uuid: u128, from: &str, to: &str, left: [f64; 3]) {
    criterion(ctx, uuid, "minecraft:changed_dimension", |c, _| {
        c["from"]
            .as_str()
            .is_none_or(|d| crate::enchant_effects::full(d) == from)
            && c["to"]
                .as_str()
                .is_none_or(|d| crate::enchant_effects::full(d) == to)
    });
    let mut entered = lock(&ENTERED_NETHER);
    let entered = entered.get_or_insert_with(Default::default);
    match (from, to) {
        ("minecraft:overworld", "minecraft:the_nether") => {
            entered.insert(uuid, left);
        }
        ("minecraft:the_nether", "minecraft:overworld") => {
            if let Some(start) = entered.remove(&uuid) {
                travelled(
                    ctx,
                    uuid,
                    "minecraft:nether_travel",
                    (start, "minecraft:overworld"),
                    None,
                    0,
                );
            }
        }
        _ => {}
    }
}

/// `fishing_rod_hooked`: the rod pulled in an entity or items.
pub fn fished(
    ctx: &ServerContext,
    uuid: u128,
    rod: &ItemStack,
    hooked: Option<&Subject>,
    caught: &[ItemStack],
) {
    criterion(ctx, uuid, "minecraft:fishing_rod_hooked", |c, me| {
        item(ctx, &c["rod"], Some(rod))
            && (c["entity"].is_null() || holds(ctx, &c["entity"], hooked, me.pos))
            && (c["item"].is_null() || caught.iter().any(|s| item(ctx, &c["item"], Some(s))))
    });
}

/// `effects_changed`: the player's effects changed, given by `source`.
pub fn effects_changed(ctx: &ServerContext, uuid: u128, have: &[String], source: Option<&Subject>) {
    criterion(ctx, uuid, "minecraft:effects_changed", |c, me| {
        c["effects"]
            .as_object()
            .into_iter()
            .flatten()
            .all(|(k, _)| have.iter().any(|e| *e == crate::enchant_effects::full(k)))
            && (c["source"].is_null() || holds(ctx, &c["source"], source, me.pos))
    });
}

/// `construct_beacon`: a beacon came to this level.
pub fn beacon(ctx: &ServerContext, uuid: u128, level: i32) {
    criterion(ctx, uuid, "minecraft:construct_beacon", |c, _| {
        range(&c["level"], f64::from(level))
    });
}

/// `using_item`: the player uses a stack, looking at `looking_at`.
pub fn using(ctx: &ServerContext, uuid: u128, stack: &ItemStack, looking_at: Option<&Subject>) {
    crate::commands::criterion_as(
        ctx,
        uuid,
        "minecraft:using_item",
        |me| me.looking_at = looking_at.cloned().map(Box::new),
        |c, _| item(ctx, &c["item"], Some(stack)),
    );
}

/// `villager_trade`.
pub fn traded(ctx: &ServerContext, uuid: u128, villager: &Subject, bought: &ItemStack) {
    criterion(ctx, uuid, "minecraft:villager_trade", |c, me| {
        holds(ctx, &c["villager"], Some(villager), me.pos) && item(ctx, &c["item"], Some(bought))
    });
}

/// `target_hit`: a projectile hit a target block for this signal.
pub fn target_hit(ctx: &ServerContext, uuid: u128, projectile: &Subject, signal: i32) {
    criterion(ctx, uuid, "minecraft:target_hit", |c, me| {
        range(&c["signal_strength"], f64::from(signal))
            && holds(ctx, &c["projectile"], Some(projectile), me.pos)
    });
}

/// `enchanted_item`: the player enchanted a stack for `levels` levels.
pub fn enchanted_item(ctx: &ServerContext, uuid: u128, stack: &ItemStack, levels: i32) {
    criterion(ctx, uuid, "minecraft:enchanted_item", |c, _| {
        item(ctx, &c["item"], Some(stack)) && range(&c["levels"], f64::from(levels))
    });
}

/// `brewed_potion`: the player took a potion out of a brewing stand.
pub fn brewed(ctx: &ServerContext, uuid: u128, potion: &str) {
    criterion(ctx, uuid, "minecraft:brewed_potion", |c, _| {
        c["potion"]
            .as_str()
            .is_none_or(|p| crate::enchant_effects::full(p) == potion)
    });
}

/// `cured_zombie_villager`.
pub fn cured(ctx: &ServerContext, uuid: u128, zombie: &Subject, villager: &Subject) {
    criterion(ctx, uuid, "minecraft:cured_zombie_villager", |c, me| {
        holds(ctx, &c["zombie"], Some(zombie), me.pos)
            && holds(ctx, &c["villager"], Some(villager), me.pos)
    });
}

/// `channeled_lightning`: a trident called down lightning on these.
pub fn channeled(ctx: &ServerContext, uuid: u128, victims: &[Subject]) {
    criterion(ctx, uuid, "minecraft:channeled_lightning", |c, me| {
        c["victims"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|p| victims.iter().any(|v| holds(ctx, p, Some(v), me.pos)))
    });
}

/// `lightning_strike`: lightning struck near the player, with these
/// bystanders.
pub fn lightning(ctx: &ServerContext, uuid: u128, bolt: &Subject, bystanders: &[Subject]) {
    criterion(ctx, uuid, "minecraft:lightning_strike", |c, me| {
        holds(ctx, &c["lightning"], Some(bolt), me.pos)
            && (c["bystander"].is_null()
                || bystanders
                    .iter()
                    .any(|b| holds(ctx, &c["bystander"], Some(b), me.pos)))
    });
}

/// `killed_by_arrow`: one shot killed these, fired from `weapon`.
pub fn killed_by_arrow(
    ctx: &ServerContext,
    uuid: u128,
    victims: &[Subject],
    weapon: Option<&ItemStack>,
) {
    criterion(ctx, uuid, "minecraft:killed_by_arrow", |c, me| {
        let kinds: std::collections::BTreeSet<&str> =
            victims.iter().map(|v| v.kind.as_str()).collect();
        item(ctx, &c["fired_from_weapon"], weapon)
            && range(&c["unique_entity_types"], kinds.len() as f64)
            && c["victims"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|p| victims.iter().any(|v| holds(ctx, p, Some(v), me.pos)))
    });
}

/// `spear_mobs`: one spear charge struck this many mobs so far.
pub fn spear_mobs(ctx: &ServerContext, uuid: u128, count: i32) {
    criterion(ctx, uuid, "minecraft:spear_mobs", |c, _| {
        range(&c["count"], f64::from(count))
    });
}

/// The distance triggers (`fall_from_height`, `ride_entity_in_lava`,
/// `nether_travel`, `levitation`, `fall_after_explosion`): the player went
/// from `start` to where they are; `cause` is what launched them.
pub fn travelled(
    ctx: &ServerContext,
    uuid: u128,
    trigger: &str,
    start: ([f64; 3], &str),
    cause: Option<&Subject>,
    ticks: i32,
) {
    criterion(ctx, uuid, trigger, |c, me| {
        let d = [
            me.pos[0] - start.0[0],
            me.pos[1] - start.0[1],
            me.pos[2] - start.0[2],
        ];
        let dist = &c["distance"];
        range(&dist["x"], d[0].abs())
            && range(&dist["y"], d[1].abs())
            && range(&dist["z"], d[2].abs())
            && range(&dist["horizontal"], (d[0] * d[0] + d[2] * d[2]).sqrt())
            && range(
                &dist["absolute"],
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt(),
            )
            && location(ctx, &c["start_position"], start.0, start.1)
            && range(&c["duration"], f64::from(ticks))
            && (c["cause"].is_null() || holds(ctx, &c["cause"], cause, me.pos))
    });
}

/// Players of this dimension within `reach` blocks of a point.
pub fn players_near(ctx: &ServerContext, at: [f64; 3], reach: f64) -> Vec<u128> {
    let dimension = ctx.world.dimension.clone();
    lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == dimension)
        .filter_map(|p| {
            let pose = p.pose?;
            let d = [pose.x - at[0], pose.y - at[1], pose.z - at[2]];
            (d.iter().all(|c| c.abs() <= reach)).then_some(p.profile.uuid)
        })
        .collect()
}

/// `summoned_entity`: a golem, wither or dragon was made; it counts for the
/// players within five blocks of it.
pub fn summoned(ctx: &ServerContext, e: &mc_entity::Entity) {
    let s = entity(ctx, e);
    for uuid in players_near(ctx, e.pos, 5.0) {
        with_entity(ctx, uuid, "minecraft:summoned_entity", &s);
    }
}

/// Players looking through a spyglass, and with which hand.
static SPYGLASS: std::sync::Mutex<Option<std::collections::HashMap<u128, i32>>> =
    std::sync::Mutex::new(None);

/// The player raised a spyglass (`hand`) or let go of what they used.
pub fn spyglass(uuid: u128, hand: Option<i32>) {
    let mut all = lock(&SPYGLASS);
    let all = all.get_or_insert_with(Default::default);
    match hand {
        Some(h) => {
            all.insert(uuid, h);
        }
        None => {
            all.remove(&uuid);
        }
    }
}

/// The entity nearest along the player's line of sight, up to `reach`.
fn in_sight(
    ctx: &ServerContext,
    me: &Subject,
    yaw: f32,
    pitch: f32,
    reach: f64,
) -> Option<Subject> {
    let eye = [
        me.pos[0],
        me.pos[1] + if me.sneaking { 1.27 } else { 1.62 },
        me.pos[2],
    ];
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    let dir = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let entities = ctx.entities.try_lock().ok()?;
    let mut best: Option<(f64, &mc_entity::Entity)> = None;
    for e in entities.iter().filter(|e| !e.removed) {
        let (w, h) = e.size();
        let lo = [e.pos[0] - w / 2.0, e.pos[1], e.pos[2] - w / 2.0];
        let hi = [e.pos[0] + w / 2.0, e.pos[1] + h, e.pos[2] + w / 2.0];
        // The slab test: where the ray enters and leaves the box.
        let (mut near, mut far) = (0.0_f64, reach);
        let mut hit = true;
        for k in 0..3 {
            if dir[k].abs() < 1e-9 {
                if eye[k] < lo[k] || eye[k] > hi[k] {
                    hit = false;
                }
                continue;
            }
            let (a, b) = ((lo[k] - eye[k]) / dir[k], (hi[k] - eye[k]) / dir[k]);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        if hit && near <= far && best.is_none_or(|(d, _)| near < d) {
            best = Some((near, e));
        }
    }
    best.map(|(_, e)| entity(ctx, e))
}

/// What the movement triggers keep of each player: where a fall began,
/// where an explosion or wind burst launched them and by what, where they
/// began to levitate (and for how long) or to ride through lava.
#[derive(Default)]
struct Motion {
    fall_start: Option<[f64; 3]>,
    launched: Option<([f64; 3], Option<String>)>,
    levitating: Option<([f64; 3], i32)>,
    lava: Option<[f64; 3]>,
    mounted: bool,
}

static MOTION: std::sync::Mutex<Option<std::collections::HashMap<u128, Motion>>> =
    std::sync::Mutex::new(None);

fn motion<R>(uuid: u128, f: impl FnOnce(&mut Motion) -> R) -> R {
    let mut all = lock(&MOTION);
    f(all
        .get_or_insert_with(Default::default)
        .entry(uuid)
        .or_default())
}

/// The player moved: a fall begins or ends (`fall_from_height`, and
/// `fall_after_explosion` after a launch).
/// `enter_block`: the blocks (not air) a player's box is in.
fn entered(ctx: &ServerContext, uuid: u128, feet: [f64; 3]) {
    criterion(ctx, uuid, "minecraft:enter_block", |c, _| {
        let cells = |lo: f64, hi: f64| lo.floor() as i32..=hi.floor() as i32;
        cells(feet[0] - 0.3, feet[0] + 0.3).any(|x| {
            cells(feet[1], feet[1] + 1.8).any(|y| {
                cells(feet[2] - 0.3, feet[2] + 0.3).any(|z| {
                    let at = Position::new(x, y, z);
                    block_at(ctx, at).is_some_and(|(_, s)| !ctx.shapes.is_air(s))
                        && block_is(ctx, c, at)
                })
            })
        })
    });
}

/// `slide_down_block`: falling pressed against the side of a block whose
/// faces stand a sixteenth in (a honey block), the player slides down it.
fn slid(ctx: &ServerContext, uuid: u128, feet: [f64; 3]) {
    // The box is 0.6 wide; a honey block's side is a sixteenth in.
    let reach = 0.3 + 1.0 / 16.0 + 0.01;
    let sides = [(reach, 0.0), (-reach, 0.0), (0.0, reach), (0.0, -reach)];
    for (dx, dz) in sides {
        let at = block_pos([feet[0] + dx, feet[1] + 0.1, feet[2] + dz]);
        let Some((name, state)) = block_at(ctx, at) else {
            continue;
        };
        if name != "minecraft:honey_block" {
            continue;
        }
        criterion(ctx, uuid, "minecraft:slide_down_block", |c, _| {
            c["block"]
                .as_str()
                .is_none_or(|b| crate::enchant_effects::full(b) == name)
                && (c["blocks"].is_null() || is(ctx, "minecraft:block", &c["blocks"], &name))
                && state_is(ctx, state, &c["state"])
        });
        return;
    }
}

pub fn moved(
    ctx: &ServerContext,
    uuid: u128,
    before: [f64; 3],
    was_on_ground: bool,
    after: [f64; 3],
    on_ground: bool,
    in_water: bool,
) {
    // Entering is looked at when the feet or the head go to another cell.
    let cell = |p: [f64; 3], up: f64| [p[0].floor(), (p[1] + up).floor(), p[2].floor()];
    if cell(before, 0.0) != cell(after, 0.0) || cell(before, 1.8) != cell(after, 1.8) {
        entered(ctx, uuid, after);
    }
    if !on_ground && before[1] - after[1] > 0.03 {
        slid(ctx, uuid, after);
    }
    let landed = motion(uuid, |m| {
        if in_water {
            m.fall_start = None;
            m.launched = None;
            return None;
        }
        if was_on_ground && !on_ground && m.fall_start.is_none() {
            m.fall_start = Some(before);
        }
        if on_ground && !was_on_ground {
            return Some((m.fall_start.take(), m.launched.take()));
        }
        if on_ground {
            m.fall_start = None;
        }
        None
    });
    let Some((start, launch)) = landed else {
        return;
    };
    let dimension = ctx.world.dimension.clone();
    let _ = after;
    if let Some(start) = start {
        travelled(
            ctx,
            uuid,
            "minecraft:fall_from_height",
            (start, &dimension),
            None,
            0,
        );
    }
    if let Some((at, cause)) = launch {
        let cause = cause.map(|kind| Subject {
            kind,
            ..Subject::default()
        });
        travelled(
            ctx,
            uuid,
            "minecraft:fall_after_explosion",
            (at, &dimension),
            cause.as_ref(),
            0,
        );
    }
}

/// The kinds of the mobs that threw items (allays), by uuid, for
/// `thrown_item_picked_up_by_player`: the item keeps only its thrower's
/// uuid, and the mob may be gone or out of reach by the time it is picked
/// up.
static THROWERS: std::sync::Mutex<Option<std::collections::HashMap<u128, String>>> =
    std::sync::Mutex::new(None);

/// A mob of `kind` threw an item.
pub fn note_thrower(uuid: u128, kind: &str) {
    lock(&THROWERS)
        .get_or_insert_with(Default::default)
        .insert(uuid, kind.to_owned());
}

/// `thrown_item_picked_up_by_player`: the player picked up `stack`, thrown
/// by `thrower`.
pub fn picked_up_thrown(ctx: &ServerContext, uuid: u128, thrower: u128, stack: &ItemStack) {
    let Some(kind) = lock(&THROWERS)
        .as_ref()
        .and_then(|t| t.get(&thrower).cloned())
    else {
        return;
    };
    let by = Subject {
        kind,
        ..Subject::default()
    };
    entity_and_item(
        ctx,
        uuid,
        "minecraft:thrown_item_picked_up_by_player",
        &by,
        Some(stack),
    );
}

/// An explosion or wind burst launched the player from `at`.
pub fn launched(uuid: u128, at: [f64; 3], cause: Option<&str>) {
    motion(uuid, |m| m.launched = Some((at, cause.map(str::to_owned))));
}

/// The player got on something (`started_riding` is tested next tick, when
/// what they ride can be looked at).
pub fn started_riding(uuid: u128) {
    motion(uuid, |m| m.mounted = true);
}

/// The player left the game: what was kept of them goes.
pub fn forget(uuid: u128) {
    if let Some(all) = lock(&MOTION).as_mut() {
        all.remove(&uuid);
    }
    spyglass(uuid, None);
}

static TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Each tick: the triggers that watch players over time (`using_item`
/// through a spyglass, `levitation`, `ride_entity_in_lava`,
/// `started_riding`).
pub fn tick(ctx: &ServerContext) {
    let n = TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let online: Vec<u128> = lock(&ctx.players).keys().copied().collect();
    for uuid in online {
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        let Some(pos) = lock(&ctx.players)
            .get(&uuid)
            .and_then(|p| p.pose)
            .map(|p| [p.x, p.y, p.z])
        else {
            continue;
        };
        if motion(uuid, |m| std::mem::take(&mut m.mounted)) {
            plain(ctx, uuid, "minecraft:started_riding");
        }
        let levitating = lock(&ctx.vitals)
            .get(&uuid)
            .is_some_and(|v| v.effects.has("levitation"));
        let since = motion(uuid, |m| {
            if !levitating {
                m.levitating = None;
                return None;
            }
            let l = m.levitating.get_or_insert((pos, 0));
            l.1 += 1;
            Some(*l)
        });
        if let Some((start, ticks)) = since {
            let dimension = ctx.world.dimension.clone();
            travelled(
                ctx,
                uuid,
                "minecraft:levitation",
                (start, &dimension),
                None,
                ticks,
            );
        }
        // Riding through lava, looked at now and then.
        if n.is_multiple_of(10) {
            let in_lava = player(ctx, uuid)
                .and_then(|me| me.vehicle)
                .is_some_and(|v| {
                    block_at(ctx, block_pos(v.pos))
                        .is_some_and(|(name, _)| name == "minecraft:lava")
                });
            let start = motion(uuid, |m| {
                if !in_lava {
                    m.lava = None;
                    return None;
                }
                Some(*m.lava.get_or_insert(pos))
            });
            if let Some(start) = start {
                let dimension = ctx.world.dimension.clone();
                travelled(
                    ctx,
                    uuid,
                    "minecraft:ride_entity_in_lava",
                    (start, &dimension),
                    None,
                    0,
                );
            }
        }
    }
    tick_spyglasses(ctx);
}

/// `using_item` for those looking through a spyglass, with what they look
/// at.
fn tick_spyglasses(ctx: &ServerContext) {
    let users: Vec<(u128, i32)> = lock(&SPYGLASS)
        .as_ref()
        .map(|m| m.iter().map(|(u, h)| (*u, *h)).collect())
        .unwrap_or_default();
    for (uuid, hand) in users {
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        let Some(held) =
            crate::held::held(ctx, uuid, hand).filter(|h| h.name == "minecraft:spyglass")
        else {
            spyglass(uuid, None);
            continue;
        };
        let Some(pose) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose) else {
            continue;
        };
        let Some(me) = player(ctx, uuid) else {
            continue;
        };
        let seen = in_sight(ctx, &me, pose.yaw, pose.pitch, 100.0);
        using(ctx, uuid, &held.stack, seen.as_ref());
    }
}

/// What each arrow killed so far, for `killed_by_arrow`.
static ARROW_KILLS: std::sync::Mutex<Option<std::collections::HashMap<i32, Vec<Subject>>>> =
    std::sync::Mutex::new(None);

/// An arrow the player shot (from a crossbow or a bow) killed `victim`:
/// `killed_by_arrow` with everything it killed so far.
pub fn arrow_killed(ctx: &ServerContext, uuid: u128, arrow: i32, crossbow: bool, victim: Subject) {
    let victims = {
        let mut all = lock(&ARROW_KILLS);
        let all = all.get_or_insert_with(Default::default);
        // Arrows come and go: keep only a few hundred about.
        if all.len() > 256 {
            all.clear();
        }
        let list = all.entry(arrow).or_default();
        list.push(victim);
        list.clone()
    };
    let weapon = ctx
        .items
        .id(if crossbow {
            "minecraft:crossbow"
        } else {
            "minecraft:bow"
        })
        .map(|i| ItemStack::new(i, 1));
    killed_by_arrow(ctx, uuid, &victims, weapon.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::progress::has_done;
    use crate::testkit;
    use mc_entity::mob::Species;

    const ME: u128 = 0xadd5;

    fn setup() -> Option<std::sync::Arc<ServerContext>> {
        let ctx = testkit::flat()?;
        testkit::join(&ctx, ME, 77, [0.5, -60.0, 0.5], 0);
        crate::commands::load_progress(&ctx, ME, &[], &crate::players::BookState::default());
        Some(ctx)
    }

    fn stack(ctx: &ServerContext, name: &str) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), 1)
    }

    #[test]
    fn falling_against_a_honey_block_is_sliding_down_it() {
        let Some(ctx) = setup() else { return };
        let id = "minecraft:adventure/honey_block_slide";
        // Pressed against the west face of the block east of the player.
        let x = 1.0 + 1.0 / 16.0 - 0.3;
        moved(
            &ctx,
            ME,
            [x, -59.3, 0.5],
            false,
            [x, -59.5, 0.5],
            false,
            false,
        );
        assert!(!has_done(&ctx, ME, id, None));
        testkit::set(&ctx, [1, -60, 0], "minecraft:honey_block");
        // Standing still against it is not sliding.
        moved(
            &ctx,
            ME,
            [x, -59.5, 0.5],
            false,
            [x, -59.5, 0.5],
            false,
            false,
        );
        assert!(!has_done(&ctx, ME, id, None));
        moved(
            &ctx,
            ME,
            [x, -59.3, 0.5],
            false,
            [x, -59.5, 0.5],
            false,
            false,
        );
        assert!(has_done(&ctx, ME, id, None));
    }

    #[test]
    fn what_an_allay_brings_counts_when_picked_up_and_at_a_note_block() {
        let Some(ctx) = setup() else { return };
        let allay = 0xa11a;
        let cake = stack(&ctx, "minecraft:cake");
        // Thrown by someone not known to be an allay: nothing.
        picked_up_thrown(&ctx, ME, 0xbeef, &cake);
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:husbandry/allay_deliver_item_to_player",
            None
        ));
        note_thrower(allay, "minecraft:allay");
        picked_up_thrown(&ctx, ME, allay, &cake);
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:husbandry/allay_deliver_item_to_player",
            None
        ));
        let note = "minecraft:husbandry/allay_deliver_cake_to_note_block";
        testkit::set(&ctx, [4, -60, 4], "minecraft:note_block");
        let at = Position::new(4, -60, 4);
        on_block(
            &ctx,
            ME,
            "minecraft:allay_drop_item_on_block",
            at,
            Some(&stack(&ctx, "minecraft:apple")),
        );
        assert!(!has_done(&ctx, ME, note, None));
        on_block(
            &ctx,
            ME,
            "minecraft:allay_drop_item_on_block",
            at,
            Some(&cake),
        );
        assert!(has_done(&ctx, ME, note, None));
    }

    #[test]
    fn five_mobs_in_one_charge_is_a_mob_kabob() {
        let Some(ctx) = setup() else { return };
        spear_mobs(&ctx, ME, 4);
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:adventure/spear_many_mobs",
            None
        ));
        spear_mobs(&ctx, ME, 5);
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:adventure/spear_many_mobs",
            None
        ));
    }

    #[test]
    fn walking_into_an_end_gateway_is_entering_it() {
        let Some(ctx) = setup() else { return };
        testkit::set(&ctx, [5, -60, 0], "minecraft:end_gateway");
        moved(
            &ctx,
            ME,
            [3.5, -60.0, 0.5],
            true,
            [4.5, -60.0, 0.5],
            true,
            false,
        );
        assert!(!has_done(&ctx, ME, "minecraft:end/enter_end_gateway", None));
        moved(
            &ctx,
            ME,
            [4.5, -60.0, 0.5],
            true,
            [5.5, -60.0, 0.5],
            true,
            false,
        );
        assert!(has_done(&ctx, ME, "minecraft:end/enter_end_gateway", None));
    }

    #[test]
    fn a_nest_broken_with_silk_touch_and_three_bees_inside() {
        let Some(ctx) = setup() else { return };
        let mut pick = stack(&ctx, "minecraft:wooden_pickaxe");
        crate::enchants::set_levels(
            &mut pick,
            "minecraft:enchantments",
            &[("minecraft:silk_touch".into(), 1)],
        );
        bee_nest(&ctx, ME, "minecraft:bee_nest", Some(&pick), 2);
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:husbandry/silk_touch_nest",
            None
        ));
        bee_nest(&ctx, ME, "minecraft:beehive", Some(&pick), 3);
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:husbandry/silk_touch_nest",
            None
        ));
        bee_nest(&ctx, ME, "minecraft:bee_nest", Some(&pick), 3);
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:husbandry/silk_touch_nest",
            None
        ));
    }

    #[test]
    fn killing_a_mob_counts_by_its_kind() {
        let Some(ctx) = setup() else { return };
        let mut zombie = crate::mobs::new_mob(&ctx, Species::Zombie, [3.5, -60.0, 0.5]);
        let mut h = crate::mobs::hurt::Harm::new(100.0, "minecraft:player_attack");
        h.player = Some(ME);
        crate::mobs::hurt::harm(&ctx, &mut zombie, &h);
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:adventure/kill_a_mob",
            Some("minecraft:zombie")
        ));
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:adventure/kill_a_mob",
            Some("minecraft:skeleton")
        ));
    }

    #[test]
    fn a_skeleton_shot_from_afar_is_a_sniper_duel_and_from_near_is_not() {
        let Some(ctx) = setup() else { return };
        let arrow = |pos: [f64; 3]| Hit {
            kind: "minecraft:arrow".into(),
            direct: Some(Subject {
                kind: "minecraft:arrow".into(),
                pos,
                ..Subject::default()
            }),
            ..Hit::default()
        };
        let skeleton = |x: f64| Subject {
            kind: "minecraft:skeleton".into(),
            pos: [x, -60.0, 0.5],
            dimension: "minecraft:overworld".into(),
            ..Subject::default()
        };
        killed(&ctx, ME, &skeleton(20.0), &arrow([20.0, -59.0, 0.5]));
        assert!(!has_done(&ctx, ME, "minecraft:adventure/sniper_duel", None));
        // Melee from afar is no shot.
        let melee = Hit {
            kind: "minecraft:player_attack".into(),
            ..Hit::default()
        };
        killed(&ctx, ME, &skeleton(60.0), &melee);
        assert!(!has_done(&ctx, ME, "minecraft:adventure/sniper_duel", None));
        killed(&ctx, ME, &skeleton(60.0), &arrow([60.0, -59.0, 0.5]));
        assert!(has_done(&ctx, ME, "minecraft:adventure/sniper_duel", None));
    }

    #[test]
    fn eating_counts_toward_a_balanced_diet() {
        let Some(ctx) = setup() else { return };
        crate::consume::eaten(&ctx, ME, &stack(&ctx, "minecraft:apple"));
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:husbandry/balanced_diet",
            Some("apple")
        ));
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:husbandry/balanced_diet",
            Some("beef")
        ));
    }

    #[test]
    fn a_planted_crop_is_a_seed_planted() {
        let Some(ctx) = setup() else { return };
        testkit::set(&ctx, [2, -60, 2], "minecraft:wheat");
        on_block(
            &ctx,
            ME,
            "minecraft:placed_block",
            Position::new(2, -60, 2),
            Some(&stack(&ctx, "minecraft:wheat_seeds")),
        );
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:husbandry/plant_seed",
            Some("wheat")
        ));
        assert!(!has_done(
            &ctx,
            ME,
            "minecraft:husbandry/plant_seed",
            Some("beetroots")
        ));
    }

    #[test]
    fn where_the_player_stands_is_checked_by_biome() {
        let Some(ctx) = setup() else { return };
        let biome = crate::mobs::aqua::biome_at(&ctx, [0.5, -60.0, 0.5]).unwrap();
        plain(&ctx, ME, "minecraft:location");
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:adventure/adventuring_time",
            Some(&biome)
        ));
    }

    #[test]
    fn a_long_way_through_the_nether_is_fast_travel() {
        let Some(ctx) = setup() else { return };
        changed_dimension(
            &ctx,
            ME,
            "minecraft:overworld",
            "minecraft:the_nether",
            [0.5, -60.0, 0.5],
        );
        assert!(has_done(&ctx, ME, "minecraft:story/enter_the_nether", None));
        testkit::place(&ctx, ME, [8000.5, -60.0, 0.5], true);
        changed_dimension(
            &ctx,
            ME,
            "minecraft:the_nether",
            "minecraft:overworld",
            [1000.0, 70.0, 0.0],
        );
        assert!(has_done(&ctx, ME, "minecraft:nether/fast_travel", None));
    }

    #[test]
    fn items_match_by_tag_count_and_enchantment() {
        let Some(ctx) = setup() else { return };
        let pick = testkit::stack(&ctx, "diamond_pickaxe", &[("efficiency", 3)]);
        let p = serde_json::json!({
            "items": "#minecraft:pickaxes",
            "predicates": {"minecraft:enchantments": [{"enchantments": "minecraft:efficiency", "levels": {"min": 2}}]}
        });
        assert!(item(&ctx, &p, Some(&pick)));
        let p =
            serde_json::json!({"predicates": {"minecraft:enchantments": [{"levels": {"min": 4}}]}});
        assert!(!item(&ctx, &p, Some(&pick)));
        assert!(!item(
            &ctx,
            &serde_json::json!({"count": {"min": 2}}),
            Some(&pick)
        ));
        assert!(!item(
            &ctx,
            &serde_json::json!({"items": "minecraft:apple"}),
            None
        ));
    }

    #[test]
    fn a_ghast_killed_by_its_own_fireball_is_a_return_to_sender() {
        let Some(ctx) = setup() else { return };
        let ghast = Subject {
            kind: "minecraft:ghast".into(),
            ..Subject::default()
        };
        let hit = Hit {
            kind: "minecraft:fireball".into(),
            direct: Some(Subject {
                kind: "minecraft:fireball".into(),
                ..Subject::default()
            }),
            ..Hit::default()
        };
        killed(&ctx, ME, &ghast, &hit);
        assert!(has_done(
            &ctx,
            ME,
            "minecraft:nether/return_to_sender",
            None
        ));
    }

    /// Every trigger the 26.3 advancement set uses. `impossible` is in the
    /// set and is never granted: that is what it means.
    const TRIGGERS_26_3: &[&str] = &[
        "minecraft:allay_drop_item_on_block",
        "minecraft:avoid_vibration",
        "minecraft:bee_nest_destroyed",
        "minecraft:bred_animals",
        "minecraft:brewed_potion",
        "minecraft:changed_dimension",
        "minecraft:channeled_lightning",
        "minecraft:construct_beacon",
        "minecraft:consume_item",
        "minecraft:crafter_recipe_crafted",
        "minecraft:cured_zombie_villager",
        "minecraft:effects_changed",
        "minecraft:enchanted_item",
        "minecraft:enter_block",
        "minecraft:entity_hurt_player",
        "minecraft:entity_killed_player",
        "minecraft:fall_after_explosion",
        "minecraft:fall_from_height",
        "minecraft:filled_bucket",
        "minecraft:fishing_rod_hooked",
        "minecraft:hero_of_the_village",
        "minecraft:impossible",
        "minecraft:inventory_changed",
        "minecraft:item_durability_changed",
        "minecraft:item_used_on_block",
        "minecraft:kill_mob_near_sculk_catalyst",
        "minecraft:killed_by_arrow",
        "minecraft:levitation",
        "minecraft:lightning_strike",
        "minecraft:location",
        "minecraft:nether_travel",
        "minecraft:placed_block",
        "minecraft:player_generates_container_loot",
        "minecraft:player_hurt_entity",
        "minecraft:player_interacted_with_entity",
        "minecraft:player_killed_entity",
        "minecraft:player_sheared_equipment",
        "minecraft:recipe_crafted",
        "minecraft:recipe_unlocked",
        "minecraft:ride_entity_in_lava",
        "minecraft:shot_crossbow",
        "minecraft:slept_in_bed",
        "minecraft:slide_down_block",
        "minecraft:spear_mobs",
        "minecraft:started_riding",
        "minecraft:summoned_entity",
        "minecraft:tame_animal",
        "minecraft:target_hit",
        "minecraft:thrown_item_picked_up_by_entity",
        "minecraft:thrown_item_picked_up_by_player",
        "minecraft:tick",
        "minecraft:used_totem",
        "minecraft:using_item",
        "minecraft:villager_trade",
    ];

    #[test]
    fn every_advancement_trigger_in_26_3_is_one_the_game_knows() {
        let Some(ctx) = testkit::flat() else {
            return;
        };
        let pack = ctx.data.data_pack();
        let mut used = std::collections::BTreeSet::new();
        for id in pack
            .registry_entries("minecraft:advancement")
            .unwrap_or_default()
        {
            let Ok(v) = pack.read_entry("minecraft:advancement", &id) else {
                continue;
            };
            for c in v["criteria"].as_object().into_iter().flatten() {
                if let Some(t) = c.1["trigger"].as_str() {
                    used.insert(t.to_owned());
                }
            }
        }
        let known: std::collections::BTreeSet<_> =
            TRIGGERS_26_3.iter().map(|s| (*s).to_owned()).collect();
        let missing: Vec<_> = used.difference(&known).cloned().collect();
        let extra: Vec<_> = known.difference(&used).cloned().collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "missing {missing:?}\nextra {extra:?}"
        );
    }
}
