//! `/advancement` and `/recipe`: what players have achieved and which
//! recipes their book knows. The progress is kept while the server runs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use mc_command::args::full_id;
use mc_nbt::{Compound, Tag};
use mc_protocol::text::{list, translate};

use super::run::err;
use super::target::Target;
use super::{Result, Run};
use crate::context::{Direct, Event, ServerContext};
use crate::tick::lock;

/// Recipe types the recipe book has no entry for.
const HIDDEN_RECIPES: [&str; 11] = [
    "minecraft:brewing",
    "minecraft:crafting_decorated_pot",
    "minecraft:crafting_special_bannerduplicate",
    "minecraft:crafting_special_bookcloning",
    "minecraft:crafting_special_firework_rocket",
    "minecraft:crafting_special_firework_star",
    "minecraft:crafting_special_firework_star_fade",
    "minecraft:crafting_special_mapextending",
    "minecraft:crafting_special_repairitem",
    "minecraft:crafting_special_shielddecoration",
    "minecraft:crafting_special_tippedarrow",
];

#[derive(Debug)]
struct Adv {
    parent: Option<String>,
    criteria: Vec<String>,
    requirements: Vec<Vec<String>>,
    /// Criteria that complete on their own every tick.
    ticks: Vec<String>,
    /// (criterion, recipe): complete once the recipe is known.
    unlocks: Vec<(String, String)>,
    title: Option<Tag>,
    description: Option<Tag>,
    frame: String,
    announce: bool,
    recipes: Vec<String>,
    xp: i32,
}

/// Every advancement and recipe of the data packs.
#[derive(Debug, Default)]
pub struct Book {
    advs: BTreeMap<String, Adv>,
    children: HashMap<String, Vec<String>>,
    recipes: BTreeSet<String>,
}

/// What each player has done.
#[derive(Debug, Default)]
pub struct Progress {
    done: HashMap<u128, HashMap<String, BTreeSet<String>>>,
    known: HashMap<u128, BTreeSet<String>>,
}

fn book(ctx: &ServerContext) -> Arc<Book> {
    if let Some(b) = &lock(&ctx.rare).book {
        return b.clone();
    }
    let pack = ctx.data.data_pack();
    let mut b = Book::default();
    for id in pack
        .registry_entries("minecraft:advancement")
        .unwrap_or_default()
    {
        let Ok(v) = pack.read_entry("minecraft:advancement", &id) else {
            continue;
        };
        let criteria: Vec<String> = v["criteria"]
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        let ticks = v["criteria"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, c)| c["trigger"].as_str() == Some("minecraft:tick"))
            .map(|(k, _)| k.clone())
            .collect();
        let unlocks = v["criteria"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, c)| c["trigger"].as_str() == Some("minecraft:recipe_unlocked"))
            .filter_map(|(k, c)| {
                Some((
                    k.clone(),
                    full_id(
                        c["conditions"]["recipe"]
                            .as_str()
                            .or(c["conditions"]["recipes"].as_str())?,
                    ),
                ))
            })
            .collect();
        let requirements = match v["requirements"].as_array() {
            Some(groups) => groups
                .iter()
                .map(|g| {
                    g.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c.as_str().map(str::to_owned))
                        .collect()
                })
                .collect(),
            None => criteria.iter().map(|c| vec![c.clone()]).collect(),
        };
        let display = &v["display"];
        let component = |k: &str| mc_data::json_to_nbt(&display[k]);
        let adv = Adv {
            parent: v["parent"].as_str().map(full_id),
            criteria,
            requirements,
            ticks,
            unlocks,
            title: component("title"),
            description: component("description"),
            frame: display["frame"].as_str().unwrap_or("task").to_owned(),
            announce: display["announce_to_chat"].as_bool().unwrap_or(true),
            recipes: v["rewards"]["recipes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r.as_str().map(full_id))
                .collect(),
            xp: v["rewards"]["experience"].as_i64().unwrap_or(0) as i32,
        };
        if let Some(p) = &adv.parent {
            b.children.entry(p.clone()).or_default().push(id.clone());
        }
        b.advs.insert(id, adv);
    }
    for id in pack
        .registry_entries("minecraft:recipe")
        .unwrap_or_default()
    {
        let kind = pack
            .read_entry("minecraft:recipe", &id)
            .ok()
            .and_then(|v| v["type"].as_str().map(str::to_owned))
            .unwrap_or_default();
        if !HIDDEN_RECIPES.contains(&kind.as_str()) {
            b.recipes.insert(id);
        }
    }
    let b = Arc::new(b);
    lock(&ctx.rare).book = Some(b.clone());
    b
}

impl Adv {
    fn complete(&self, have: Option<&BTreeSet<String>>) -> bool {
        let Some(have) = have else {
            return self.requirements.is_empty();
        };
        self.requirements
            .iter()
            .all(|g| g.iter().any(|c| have.contains(c)))
    }
}

/// For the `advancements` selector option: whether a player has the
/// advancement `id` (`criterion` `None`), or its criterion, done.
pub fn has_done(ctx: &ServerContext, uuid: u128, id: &str, criterion: Option<&str>) -> bool {
    let b = book(ctx);
    let id = full_id(id);
    let state = lock(&ctx.rare);
    let have = state.progress.done.get(&uuid).and_then(|m| m.get(&id));
    match criterion {
        Some(c) => have.is_some_and(|h| h.contains(c)),
        None => b.advs.get(&id).is_some_and(|a| a.complete(have)),
    }
}

/// How an advancement is named in messages.
fn name(id: &str, adv: &Adv) -> Tag {
    let (Some(title), Some(description)) = (&adv.title, &adv.description) else {
        return Tag::String(id.to_owned());
    };
    let color = if adv.frame == "challenge" {
        "dark_purple"
    } else {
        "green"
    };
    let mut hover_value = title.clone();
    if let Tag::Compound(c) = &mut hover_value {
        c.insert("color", color);
        let mut nl = Compound::new();
        nl.insert("", "\n");
        c.insert("extra", list(vec![Tag::Compound(nl), description.clone()]));
    }
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", hover_value);
    let mut shown = title.clone();
    if let Tag::Compound(c) = &mut shown {
        c.insert("hover_event", Tag::Compound(hover));
    }
    let mut out = translate("chat.square_brackets", vec![shown]);
    if let Tag::Compound(c) = &mut out {
        c.insert("color", color);
    }
    out
}

/// Gives a player what completing an advancement earns, and tells everyone.
fn rewarded(r: &mut Run, b: &Book, p: &Target, id: &str, adv: &Adv) {
    let uuid = p.uuid();
    for recipe in &adv.recipes {
        unlock_recipe(r, b, p, recipe);
    }
    if adv.xp > 0 {
        let packet = {
            let mut all = lock(&r.ctx.vitals);
            all.get_mut(&uuid).map(|v| {
                v.add_xp_points(adv.xp);
                v.experience_packet()
            })
        };
        if let Some(packet) = packet {
            r.out.events.push(Event::Direct {
                uuid,
                packet: Direct::Experience(packet),
            });
        }
    }
    if adv.title.is_some() && adv.announce && r.ctx.game_rule_bool("show_advancement_messages") {
        let kind = match adv.frame.as_str() {
            "challenge" => "challenge",
            "goal" => "goal",
            _ => "task",
        };
        let line = translate(
            &format!("chat.type.advancement.{kind}"),
            vec![p.display(), name(id, adv)],
        );
        let everyone: Vec<u128> = lock(&r.ctx.players).keys().copied().collect();
        for u in everyone {
            r.out.events.push(Event::Message {
                uuid: u,
                line: line.clone(),
                overlay: false,
            });
        }
    }
}

/// A recipe becomes known: advancements waiting for it progress.
fn unlock_recipe(r: &mut Run, b: &Book, p: &Target, recipe: &str) -> bool {
    let new = lock(&r.ctx.rare)
        .progress
        .known
        .entry(p.uuid())
        .or_default()
        .insert(recipe.to_owned());
    if new {
        for (id, adv) in &b.advs {
            for (criterion, wanted) in &adv.unlocks {
                if wanted == recipe {
                    grant(r, b, p, id, Some(criterion));
                }
            }
        }
    }
    new
}

/// Completes the criteria that complete by themselves.
fn tick_awards(r: &mut Run, b: &Book, p: &Target) {
    for (id, adv) in &b.advs {
        if adv.ticks.is_empty() {
            continue;
        }
        let (was, now) = {
            let mut st = lock(&r.ctx.rare);
            let have = st
                .progress
                .done
                .entry(p.uuid())
                .or_default()
                .entry(id.clone())
                .or_default();
            let was = adv.complete(Some(have));
            have.extend(adv.ticks.iter().cloned());
            (was, adv.complete(Some(have)))
        };
        if !was && now {
            rewarded(r, b, p, id, adv);
        }
    }
}

fn grant(r: &mut Run, b: &Book, p: &Target, id: &str, only: Option<&str>) -> bool {
    let adv = &b.advs[id];
    let (was, changed) = {
        let mut st = lock(&r.ctx.rare);
        let have = st
            .progress
            .done
            .entry(p.uuid())
            .or_default()
            .entry(id.to_owned())
            .or_default();
        let was = adv.complete(Some(have));
        let before = have.len();
        match only {
            Some(c) => {
                have.insert(c.to_owned());
            }
            None if was => {}
            None => have.extend(adv.criteria.iter().cloned()),
        }
        (was, have.len() != before)
    };
    let now = lock(&r.ctx.rare)
        .progress
        .done
        .get(&p.uuid())
        .and_then(|d| d.get(id))
        .is_some_and(|h| adv.complete(Some(h)));
    if now && !was {
        rewarded(r, b, p, id, adv);
    }
    changed
}

fn revoke(r: &mut Run, b: &Book, p: &Target, id: &str, only: Option<&str>) -> bool {
    let _ = b;
    let mut st = lock(&r.ctx.rare);
    let Some(have) = st
        .progress
        .done
        .get_mut(&p.uuid())
        .and_then(|d| d.get_mut(id))
    else {
        return false;
    };
    let before = have.len();
    match only {
        Some(c) => {
            have.remove(c);
        }
        None => have.clear(),
    }
    have.len() != before
}

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["advancement", action @ ("grant" | "revoke"), ..] => {
            advancement(r, path, *action == "grant")
        }
        ["recipe", action @ ("give" | "take"), ..] => recipe(r, *action == "give"),
        _ => return None,
    })
}

/// The advancements a mode names around `id`, parents first.
fn selection(b: &Book, mode: &str, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if mode == "everything" {
        return b.advs.keys().cloned().collect();
    }
    if matches!(mode, "through" | "until") {
        let mut chain = Vec::new();
        let mut at = b.advs[id].parent.clone();
        while let Some(p) = at {
            at = b.advs.get(&p).and_then(|a| a.parent.clone());
            chain.push(p);
        }
        out.extend(chain);
    }
    out.push(id.to_owned());
    if matches!(mode, "through" | "from") {
        fn below(b: &Book, id: &str, out: &mut Vec<String>) {
            for c in b.children.get(id).into_iter().flatten() {
                out.push(c.clone());
                below(b, c, out);
            }
        }
        below(b, id, &mut out);
    }
    out
}

fn advancement(r: &mut Run, path: &[&str], giving: bool) -> Result<i32> {
    let b = book(r.ctx);
    let players = r.targets("targets", true)?;
    let mode = path[3];
    let id = r.string("advancement").map(|s| full_id(&s));
    if let Some(id) = &id
        && !b.advs.contains_key(id)
    {
        return Err(err(
            "advancement.advancementNotFound",
            vec![Tag::from(id.as_str())],
        ));
    }
    let criterion = r.string("criterion");
    if let (Some(id), Some(c)) = (&id, &criterion)
        && !b.advs[id].criteria.contains(c)
    {
        return Err(err(
            "commands.advancement.criterionNotFound",
            vec![name(id, &b.advs[id]), Tag::from(c.as_str())],
        ));
    }
    let ids = match &id {
        Some(id) => selection(&b, mode, id),
        None => selection(&b, "everything", ""),
    };
    let verb = if giving { "grant" } else { "revoke" };
    let mut count = 0;
    for p in &players {
        tick_awards(r, &b, p);
        for i in &ids {
            let changed = if giving {
                grant(r, &b, p, i, criterion.as_deref())
            } else {
                revoke(r, &b, p, i, criterion.as_deref())
            };
            count += i32::from(changed);
        }
    }
    let who = if players.len() == 1 {
        ("one", players[0].display())
    } else {
        ("many", Tag::Int(players.len() as i32))
    };
    let (shape, mut args) = match (&id, &criterion) {
        (Some(id), Some(c)) => (
            "criterion",
            vec![Tag::from(c.as_str()), name(id, &b.advs[id])],
        ),
        (Some(id), None) if mode == "only" => ("one", vec![name(id, &b.advs[id])]),
        _ => ("many", vec![Tag::Int(ids.len() as i32)]),
    };
    args.push(who.1);
    let to = if players.len() == 1 { "one" } else { "many" };
    let key = format!("commands.advancement.{verb}.{shape}.to.{to}");
    let _ = who.0;
    if count == 0 {
        return Err(err(&format!("{key}.failure"), args));
    }
    r.success(translate(&format!("{key}.success"), args), true);
    Ok(count)
}

fn recipe(r: &mut Run, giving: bool) -> Result<i32> {
    let b = book(r.ctx);
    let players = r.targets("targets", true)?;
    let wanted: Option<String> = r.string("recipe").filter(|s| s != "*").map(|s| full_id(&s));
    if let Some(id) = &wanted
        && !b.recipes.contains(id)
    {
        return Err(err("recipe.notFound", vec![Tag::from(id.as_str())]));
    }
    let mut count = 0;
    for p in &players {
        tick_awards(r, &b, p);
        let ids: Vec<String> = match &wanted {
            Some(id) => vec![id.clone()],
            None => b.recipes.iter().cloned().collect(),
        };
        for id in ids {
            let changed = if giving {
                unlock_recipe(r, &b, p, &id)
            } else {
                lock(&r.ctx.rare)
                    .progress
                    .known
                    .entry(p.uuid())
                    .or_default()
                    .remove(&id)
            };
            count += i32::from(changed);
        }
    }
    let verb = if giving { "give" } else { "take" };
    if count == 0 {
        return Err(err(&format!("commands.recipe.{verb}.failed"), Vec::new()));
    }
    let msg = if players.len() == 1 {
        translate(
            &format!("commands.recipe.{verb}.success.single"),
            vec![Tag::Int(count), players[0].display()],
        )
    } else {
        translate(
            &format!("commands.recipe.{verb}.success.multiple"),
            vec![Tag::Int(count), Tag::Int(players.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(count)
}
