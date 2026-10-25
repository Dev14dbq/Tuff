//! `/advancement` and `/recipe`: what players have achieved and which
//! recipes their book knows. The progress is kept while the server runs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use mc_command::args::full_id;
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{
    Advancement, AdvancementDisplay, CriteriaProgress, ItemStack, RecipeBookAdd, RecipeBookRemove,
    RecipeBookSettings, UpdateAdvancements,
};
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
    /// How it shows in its tab, if it does.
    icon: Option<ItemStack>,
    background: Option<String>,
    show_toast: bool,
    hidden: bool,
    telemetry: bool,
    /// Where it sits in its tab (column, row).
    at: (f32, f32),
    recipes: Vec<String>,
    xp: i32,
}

/// Every advancement and recipe of the data packs.
#[derive(Debug, Default)]
pub struct Book {
    advs: BTreeMap<String, Adv>,
    children: HashMap<String, Vec<String>>,
    recipes: BTreeSet<String>,
    /// `minecraft:inventory_changed` criteria.
    inventory: Vec<InventoryCriterion>,
    /// Those criteria by an item that can complete them.
    by_item: HashMap<i32, Vec<usize>>,
    /// (advancement, criterion) completed by entering water.
    water: Vec<(String, String)>,
    /// (advancement, criterion) waiting for each recipe.
    waiting: HashMap<String, Vec<(String, String)>>,
    /// (advancement, criterion, conditions) of each trigger.
    by_trigger: HashMap<String, Vec<(String, String, serde_json::Value)>>,
}

/// A criterion that completes once the inventory holds every item a
/// predicate asks for (each an item or the items of a tag).
#[derive(Debug)]
struct InventoryCriterion {
    advancement: String,
    criterion: String,
    wants: Vec<HashSet<i32>>,
}

/// Where what an award causes is sent: the player's events.
pub struct Sink<'a> {
    pub ctx: &'a ServerContext,
    pub events: &'a mut Vec<Event>,
}

impl Sink<'_> {
    /// Tells a player's recipe book of a recipe it gained; those told one
    /// after the other share a packet.
    fn book_added(&mut self, uuid: u128, id: i32) {
        let Some(entry) = self.ctx.recipes.book_entry(id, true, true) else {
            return;
        };
        if let Some(Event::Direct {
            uuid: last,
            packet: Direct::RecipeAdd(add),
        }) = self.events.last_mut()
            && *last == uuid
            && !add.replace
        {
            add.entries.push(entry);
            return;
        }
        self.events.push(Event::Direct {
            uuid,
            packet: Direct::RecipeAdd(RecipeBookAdd {
                entries: vec![entry],
                replace: false,
            }),
        });
    }

    fn book_removed(&mut self, uuid: u128, id: i32) {
        if let Some(Event::Direct {
            uuid: last,
            packet: Direct::RecipeRemove(remove),
        }) = self.events.last_mut()
            && *last == uuid
        {
            remove.recipes.push(id);
            return;
        }
        self.events.push(Event::Direct {
            uuid,
            packet: Direct::RecipeRemove(RecipeBookRemove { recipes: vec![id] }),
        });
    }
}

impl Sink<'_> {
    /// Tells a player's screen of what changed in the advancement `id`: its
    /// progress, and what now shows or no longer does. Changes one after
    /// the other share a packet.
    fn advancement_changed(&mut self, b: &Book, uuid: u128, id: &str) {
        let (added, removed, progress) = {
            let mut state = lock(&self.ctx.rare);
            let done = state.progress.done.get(&uuid);
            let now = b.visible(done);
            let when = now_millis();
            let old = state.progress.shown.get(&uuid).cloned().unwrap_or_default();
            let added: Vec<String> = now.difference(&old).cloned().collect();
            let removed: Vec<String> = old.difference(&now).cloned().collect();
            let mut progress: Vec<(String, CriteriaProgress)> = added
                .iter()
                .map(|i| (i.clone(), b.progress(i, done.and_then(|d| d.get(i)), when)))
                .collect();
            if now.contains(id) && !added.iter().any(|i| i == id) {
                progress.push((
                    id.to_owned(),
                    b.progress(id, done.and_then(|d| d.get(id)), when),
                ));
            }
            state.progress.shown.insert(uuid, now);
            (added, removed, progress)
        };
        if added.is_empty() && removed.is_empty() && progress.is_empty() {
            return;
        }
        let added: Vec<(String, Advancement)> = added
            .into_iter()
            .map(|i| {
                let a = b.for_client(&i);
                (i, a)
            })
            .collect();
        if let Some(Event::Direct {
            uuid: last,
            packet: Direct::Advancements(u),
        }) = self.events.last_mut()
            && *last == uuid
        {
            u.added.extend(added);
            u.removed.extend(removed);
            for (i, p) in progress {
                u.progress.retain(|(j, _)| *j != i);
                u.progress.push((i, p));
            }
            return;
        }
        self.events.push(Event::Direct {
            uuid,
            packet: Direct::Advancements(UpdateAdvancements {
                reset: false,
                added,
                removed,
                progress,
                show: true,
            }),
        });
    }
}

fn sink<'a>(r: &'a mut Run<'_>) -> Sink<'a> {
    Sink {
        ctx: r.ctx,
        events: &mut r.out.events,
    }
}

/// What each player has done.
#[derive(Debug, Default)]
pub struct Progress {
    done: HashMap<u128, HashMap<String, BTreeSet<String>>>,
    known: HashMap<u128, BTreeSet<String>>,
    /// Known recipes the player has not looked at yet.
    highlight: HashMap<u128, BTreeSet<String>>,
    /// The items the inventory held when it was last looked at.
    held: HashMap<u128, Vec<i32>>,
    /// Players who were in water on the last look.
    wet: HashSet<u128>,
    /// Each player's recipe books: whether open, and whether filtered to
    /// what can be crafted.
    settings: HashMap<u128, ([bool; 4], [bool; 4])>,
    /// Players whose self-completing criteria were awarded since joining.
    ticked: HashSet<u128>,
    /// The advancements each player's screen shows.
    shown: HashMap<u128, BTreeSet<String>>,
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
        for (k, c) in v["criteria"].as_object().into_iter().flatten() {
            if let Some(t) = c["trigger"].as_str() {
                b.by_trigger.entry(full_id(t)).or_default().push((
                    id.clone(),
                    k.clone(),
                    c["conditions"].clone(),
                ));
            }
            match c["trigger"].as_str() {
                Some("minecraft:inventory_changed") => {
                    let wants: Vec<HashSet<i32>> = c["conditions"]["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|p| items_of(ctx, &p["items"]))
                        .collect();
                    if wants.is_empty() || wants.iter().any(HashSet::is_empty) {
                        continue;
                    }
                    let at = b.inventory.len();
                    for &item in &wants[0] {
                        b.by_item.entry(item).or_default().push(at);
                    }
                    b.inventory.push(InventoryCriterion {
                        advancement: id.clone(),
                        criterion: k.clone(),
                        wants,
                    });
                }
                Some("minecraft:enter_block")
                    if c["conditions"]["blocks"].as_str() == Some("minecraft:water") =>
                {
                    b.water.push((id.clone(), k.clone()));
                }
                _ => {}
            }
        }
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
            icon: display["icon"]["id"].as_str().and_then(|item| {
                let comps = match mc_data::json_to_nbt(&display["icon"]["components"]) {
                    Some(Tag::Compound(c)) => Some(c),
                    _ => None,
                };
                Some(crate::items::stack_with(
                    ctx.items.id(&full_id(item))?,
                    display["icon"]["count"].as_i64().unwrap_or(1) as i32,
                    comps.as_ref(),
                ))
            }),
            background: display["background"].as_str().map(full_id),
            show_toast: display["show_toast"].as_bool().unwrap_or(true),
            hidden: display["hidden"].as_bool().unwrap_or(false),
            telemetry: v["sends_telemetry_event"].as_bool().unwrap_or(false),
            at: (0.0, 0.0),
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
        for (criterion, recipe) in &adv.unlocks {
            b.waiting
                .entry(recipe.clone())
                .or_default()
                .push((id.clone(), criterion.clone()));
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
    for kids in b.children.values_mut() {
        kids.sort();
    }
    lay_out(&mut b);
    let b = Arc::new(b);
    lock(&ctx.rare).book = Some(b.clone());
    b
}

/// Places the advancements of each tab: a column per step from the root,
/// each leaf on a row of its own and a parent level with the middle of its
/// children.
fn lay_out(b: &mut Book) {
    fn place(
        b: &Book,
        id: &str,
        column: f32,
        next_row: &mut f32,
        at: &mut Vec<(String, (f32, f32))>,
    ) -> f32 {
        let kids: Vec<&String> = b
            .children
            .get(id)
            .into_iter()
            .flatten()
            .filter(|k| b.advs[k.as_str()].icon.is_some())
            .collect();
        let row = if kids.is_empty() {
            let row = *next_row;
            *next_row += 1.0;
            row
        } else {
            let rows: Vec<f32> = kids
                .iter()
                .map(|k| place(b, k, column + 1.0, next_row, at))
                .collect();
            (rows[0] + rows[rows.len() - 1]) / 2.0
        };
        at.push((id.to_owned(), (column, row)));
        row
    }
    let mut at = Vec::new();
    for (id, adv) in &b.advs {
        if adv.icon.is_some() && b.root_of(id) == id.as_str() {
            place(b, id, 0.0, &mut 0.0, &mut at);
        }
    }
    for (id, pos) in at {
        if let Some(a) = b.advs.get_mut(&id) {
            a.at = pos;
        }
    }
}

impl Book {
    /// The first advancement up the parents that has no shown parent.
    fn root_of<'a>(&'a self, mut id: &'a str) -> &'a str {
        while let Some(p) = self.advs[id].parent.as_deref()
            && self.advs.get(p).is_some_and(|a| a.icon.is_some())
        {
            id = p;
        }
        id
    }

    /// The advancement as the client is told of it.
    fn for_client(&self, id: &str) -> Advancement {
        let a = &self.advs[id];
        let display = a.icon.as_ref().map(|icon| AdvancementDisplay {
            title: a
                .title
                .clone()
                .unwrap_or_else(|| Tag::String(id.to_owned())),
            description: a
                .description
                .clone()
                .unwrap_or_else(|| Tag::String(String::new())),
            icon: icon.clone(),
            frame: match a.frame.as_str() {
                "challenge" => 1,
                "goal" => 2,
                _ => 0,
            },
            background: a.background.clone(),
            show_toast: a.show_toast,
            hidden: a.hidden,
            x: a.at.0,
            y: a.at.1,
        });
        Advancement {
            parent: a.parent.clone().filter(|p| self.advs.contains_key(p)),
            display,
            requirements: a.requirements.clone(),
            telemetry: a.telemetry,
        }
    }

    /// What of an advancement is done: each criterion, with when if it is.
    fn progress(
        &self,
        id: &str,
        have: Option<&BTreeSet<String>>,
        when: i64,
    ) -> Vec<(String, Option<i64>)> {
        self.advs[id]
            .criteria
            .iter()
            .map(|c| {
                (
                    c.clone(),
                    have.is_some_and(|h| h.contains(c)).then_some(when),
                )
            })
            .collect()
    }

    /// The advancements a player sees in the screen: those done and the
    /// ones up to two steps past them, with the parents of what is shown; a
    /// hidden one only once done. A tab shows once anything in it is done.
    fn visible(&self, done: Option<&HashMap<String, BTreeSet<String>>>) -> BTreeSet<String> {
        let is_done = |id: &str| {
            done.and_then(|d| d.get(id))
                .is_some_and(|h| self.advs[id].complete(Some(h)))
        };
        let mut out = BTreeSet::new();
        for (id, adv) in &self.advs {
            if adv.icon.is_none() {
                continue;
            }
            let near = is_done(id)
                || adv.parent.as_deref().is_some_and(|p| {
                    is_done(p)
                        || self
                            .advs
                            .get(p)
                            .and_then(|a| a.parent.as_deref())
                            .is_some_and(is_done)
                });
            if !near {
                continue;
            }
            // Up the parents, so the tree stays whole.
            let mut at = Some(id.as_str());
            while let Some(i) = at {
                let Some(a) = self.advs.get(i).filter(|a| a.icon.is_some()) else {
                    break;
                };
                if a.hidden && !is_done(i) {
                    break;
                }
                out.insert(i.to_owned());
                at = a.parent.as_deref();
            }
        }
        // What lost a hidden parent on the way stays out.
        let kept: BTreeSet<String> = out
            .iter()
            .filter(|id| {
                let mut at = self.advs[id.as_str()].parent.as_deref();
                while let Some(p) = at {
                    let Some(a) = self.advs.get(p).filter(|a| a.icon.is_some()) else {
                        break;
                    };
                    if !out.contains(p) {
                        return false;
                    }
                    at = a.parent.as_deref();
                }
                true
            })
            .cloned()
            .collect();
        kept
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The whole advancements screen a joining player starts with.
pub fn initial_advancements(ctx: &ServerContext, uuid: u128) -> UpdateAdvancements {
    let b = book(ctx);
    let mut state = lock(&ctx.rare);
    let done = state.progress.done.get(&uuid);
    let shown = b.visible(done);
    let when = now_millis();
    let packet = UpdateAdvancements {
        reset: true,
        added: shown
            .iter()
            .map(|id| (id.clone(), b.for_client(id)))
            .collect(),
        removed: Vec::new(),
        progress: shown
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    b.progress(id, done.and_then(|d| d.get(id)), when),
                )
            })
            .collect(),
        show: false,
    };
    state.progress.shown.insert(uuid, shown);
    packet
}

/// The advancements screen opened on a tab or closed: the server agrees on
/// the tab.
pub fn seen_tab(tab: Option<String>) -> Option<mc_protocol::packets::play::SelectAdvancementsTab> {
    tab.map(|t| mc_protocol::packets::play::SelectAdvancementsTab { tab: Some(t) })
}

/// The items a predicate's `items` names: an item, a `#tag`, or a list.
fn items_of(ctx: &ServerContext, v: &serde_json::Value) -> HashSet<i32> {
    let one = |s: &str| -> Vec<i32> {
        match s.strip_prefix('#') {
            Some(tag) => ctx.recipes.item_tags.get(tag).cloned().unwrap_or_default(),
            None => ctx.items.id(s).into_iter().collect(),
        }
    };
    match v {
        serde_json::Value::String(s) => one(s).into_iter().collect(),
        serde_json::Value::Array(l) => l.iter().filter_map(|s| s.as_str()).flat_map(one).collect(),
        _ => HashSet::new(),
    }
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

/// A player's known recipes, sorted, for their file.
pub fn known_recipes(ctx: &ServerContext, uuid: u128) -> Vec<String> {
    lock(&ctx.rare)
        .progress
        .known
        .get(&uuid)
        .map(|k| k.iter().cloned().collect())
        .unwrap_or_default()
}

/// The advancements file of a player: `players/advancements/<uuid>.json`.
fn advancements_file(ctx: &ServerContext, uuid: u128) -> Option<std::path::PathBuf> {
    let world = ctx.config.world.as_ref()?;
    Some(
        world
            .join("players/advancements")
            .join(format!("{}.json", crate::util::uuid_string(uuid))),
    )
}

/// What a joining player had done and knew, from their files.
pub fn load_progress(
    ctx: &ServerContext,
    uuid: u128,
    recipes: &[String],
    saved: &crate::players::BookState,
) {
    // Reading every advancement takes a while: do it here, not in a tick.
    book(ctx);
    let mut done: HashMap<String, BTreeSet<String>> = HashMap::new();
    if let Some(file) = advancements_file(ctx, uuid)
        && let Ok(text) = std::fs::read_to_string(&file)
        && let Ok(serde_json::Value::Object(all)) = serde_json::from_str(&text)
    {
        for (id, entry) in all {
            if let Some(criteria) = entry["criteria"].as_object() {
                done.insert(id, criteria.keys().cloned().collect());
            }
        }
    }
    let mut state = lock(&ctx.rare);
    state.progress.done.insert(uuid, done);
    state.progress.known.insert(
        uuid,
        recipes
            .iter()
            .map(|r| full_id(r))
            .filter(|r| ctx.recipes.book_id(r).is_some() || book_hidden(ctx, r))
            .collect(),
    );
    state
        .progress
        .settings
        .insert(uuid, (saved.open, saved.filter));
    state.progress.highlight.insert(
        uuid,
        saved
            .highlight
            .iter()
            .map(|r| full_id(r))
            .filter(|r| ctx.recipes.book_id(r).is_some())
            .collect(),
    );
    // Everything in the inventory counts as new again.
    state.progress.held.remove(&uuid);
    state.progress.wet.remove(&uuid);
    state.progress.ticked.remove(&uuid);
}

/// Whether `recipe` is one the data pack has but the book does not show
/// (its file is still kept in the player's list).
fn book_hidden(ctx: &ServerContext, recipe: &str) -> bool {
    ctx.data
        .data_pack()
        .read_entry("minecraft:recipe", recipe)
        .is_ok()
}

/// The recipes a player has not looked at yet, sorted, for their file.
pub fn highlighted(ctx: &ServerContext, uuid: u128) -> Vec<String> {
    lock(&ctx.rare)
        .progress
        .highlight
        .get(&uuid)
        .map(|k| k.iter().cloned().collect())
        .unwrap_or_default()
}

/// The recipe book state of a player, for their file.
pub fn book_state(ctx: &ServerContext, uuid: u128) -> crate::players::BookState {
    let (open, filter) = lock(&ctx.rare)
        .progress
        .settings
        .get(&uuid)
        .copied()
        .unwrap_or_default();
    crate::players::BookState {
        open,
        filter,
        highlight: highlighted(ctx, uuid),
    }
}

/// A book of a player was opened, closed or switched to what can be
/// crafted (Recipe Book Change Settings).
pub fn book_settings(ctx: &ServerContext, uuid: u128, book: i32, open: bool, filter: bool) {
    let Ok(book @ 0..4) = usize::try_from(book) else {
        return;
    };
    let mut state = lock(&ctx.rare);
    let (opens, filters) = state.progress.settings.entry(uuid).or_default();
    opens[book] = open;
    filters[book] = filter;
}

/// The packet that tells a joining player's client which books are open.
pub fn settings_packet(ctx: &ServerContext, uuid: u128) -> RecipeBookSettings {
    let (open, filter) = lock(&ctx.rare)
        .progress
        .settings
        .get(&uuid)
        .copied()
        .unwrap_or_default();
    RecipeBookSettings {
        books: std::array::from_fn(|i| (open[i], filter[i])),
    }
}

/// A recipe the player made is added to their book.
pub fn unlock(ctx: &ServerContext, uuid: u128, recipe: &str) {
    if knows_recipe(ctx, uuid, recipe) {
        return;
    }
    trigger(ctx, uuid, |r, b, p| {
        unlock_recipe(r, b, p, recipe);
    });
}

/// Looks at what every player carries: picking up an ingredient unlocks
/// the recipes whose advancement wants it.
pub fn watch(ctx: &ServerContext) {
    let held: Vec<(u128, Vec<i32>)> = {
        let players = lock(&ctx.players);
        let inventories = lock(&ctx.inventories);
        players
            .keys()
            .filter_map(|&uuid| {
                let mut items: Vec<i32> = inventories
                    .get(&uuid)?
                    .slots
                    .iter()
                    .flatten()
                    .map(|s| s.item)
                    .collect();
                items.sort_unstable();
                items.dedup();
                Some((uuid, items))
            })
            .collect()
    };
    for (uuid, items) in held {
        if lock(&ctx.rare).progress.ticked.insert(uuid) {
            trigger(ctx, uuid, tick_awards);
        }
        inventory_changed(ctx, uuid, &items);
    }
}

/// Whether a player's recipe book has `recipe`.
pub fn knows_recipe(ctx: &ServerContext, uuid: u128, recipe: &str) -> bool {
    lock(&ctx.rare)
        .progress
        .known
        .get(&uuid)
        .is_some_and(|k| k.contains(recipe))
}

/// The packet that gives a joining player's book what they know, with
/// the unseen recipes marked.
pub fn initial_book(ctx: &ServerContext, uuid: u128) -> RecipeBookAdd {
    let state = lock(&ctx.rare);
    let none = BTreeSet::new();
    let known = state.progress.known.get(&uuid).unwrap_or(&none);
    let unseen = state.progress.highlight.get(&uuid).unwrap_or(&none);
    RecipeBookAdd {
        entries: known
            .iter()
            .filter_map(|r| {
                let id = ctx.recipes.book_id(r)?;
                ctx.recipes.book_entry(id, false, unseen.contains(r))
            })
            .collect(),
        replace: true,
    }
}

/// The client showed a recipe's highlight: it is not new any more.
pub fn recipe_seen(ctx: &ServerContext, uuid: u128, id: i32) {
    let Some(r) = ctx.recipes.book_recipe(id) else {
        return;
    };
    if let Some(h) = lock(&ctx.rare).progress.highlight.get_mut(&uuid) {
        h.remove(&r.name);
    }
}

/// Runs a trigger for a player and sends what it caused.
fn trigger(ctx: &ServerContext, uuid: u128, award: impl FnOnce(&mut Sink, &Book, &Target)) {
    let Some(target) = super::target::player(ctx, uuid) else {
        return;
    };
    let b = book(ctx);
    let mut events = Vec::new();
    award(
        &mut Sink {
            ctx,
            events: &mut events,
        },
        &b,
        &target,
    );
    for e in events {
        let _ = ctx.events.send(e);
    }
}

/// A trigger fired for a player: every criterion of it not done yet whose
/// conditions `test` passes, and whose `player` conditions the player
/// meets, is done. `test` is given the conditions and the player.
pub fn criterion(
    ctx: &ServerContext,
    uuid: u128,
    kind: &str,
    test: impl Fn(&serde_json::Value, &crate::criteria::Subject) -> bool,
) {
    criterion_as(ctx, uuid, kind, |_| {}, test);
}

/// [`criterion`] with the player as conditions see them changed first (what
/// they look at, say).
pub fn criterion_as(
    ctx: &ServerContext,
    uuid: u128,
    kind: &str,
    change: impl FnOnce(&mut crate::criteria::Subject),
    test: impl Fn(&serde_json::Value, &crate::criteria::Subject) -> bool,
) {
    let b = book(ctx);
    let Some(list) = b.by_trigger.get(kind) else {
        return;
    };
    let undone: Vec<&(String, String, serde_json::Value)> = {
        let state = lock(&ctx.rare);
        let done = state.progress.done.get(&uuid);
        list.iter()
            .filter(|(a, c, _)| !done.and_then(|d| d.get(a)).is_some_and(|d| d.contains(c)))
            .collect()
    };
    if undone.is_empty() {
        return;
    }
    let Some(mut me) = crate::criteria::player(ctx, uuid) else {
        return;
    };
    change(&mut me);
    let hits: Vec<(String, String)> = undone
        .into_iter()
        .filter(|(_, _, c)| {
            crate::criteria::holds(ctx, &c["player"], Some(&me), me.pos) && test(c, &me)
        })
        .map(|(a, c, _)| (a.clone(), c.clone()))
        .collect();
    if hits.is_empty() {
        return;
    }
    trigger(ctx, uuid, |r, b, p| {
        for (a, c) in hits {
            grant(r, b, p, &a, Some(&c));
        }
    });
}

/// The player's inventory holds `items` (every kind in it) now that it
/// changed: the recipes whose first ingredient is among them unlock.
pub fn inventory_changed(ctx: &ServerContext, uuid: u128, items: &[i32]) {
    {
        let mut state = lock(&ctx.rare);
        if state.progress.held.get(&uuid).is_some_and(|h| h == items) {
            return;
        }
        state.progress.held.insert(uuid, items.to_vec());
    }
    let b = book(ctx);
    let mut hits: BTreeSet<usize> = BTreeSet::new();
    for item in items {
        for &at in b.by_item.get(item).into_iter().flatten() {
            let c = &b.inventory[at];
            if c.wants.iter().all(|w| items.iter().any(|i| w.contains(i))) {
                hits.insert(at);
            }
        }
    }
    // What is already done is not worked through again.
    let fresh: Vec<usize> = {
        let state = lock(&ctx.rare);
        let done = state.progress.done.get(&uuid);
        hits.into_iter()
            .filter(|&at| {
                let c = &b.inventory[at];
                !done
                    .and_then(|d| d.get(&c.advancement))
                    .is_some_and(|d| d.contains(&c.criterion))
            })
            .collect()
    };
    if fresh.is_empty() {
        return;
    }
    trigger(ctx, uuid, |r, b, p| {
        for at in fresh {
            let c = &b.inventory[at];
            grant(r, b, p, &c.advancement, Some(&c.criterion));
        }
    });
}

/// Whether the player is in water now: entering it completes the
/// criteria of the boat recipes.
pub fn in_water(ctx: &ServerContext, uuid: u128, wet: bool) {
    let was = {
        let mut state = lock(&ctx.rare);
        if wet {
            !state.progress.wet.insert(uuid)
        } else {
            state.progress.wet.remove(&uuid);
            return;
        }
    };
    if was {
        return;
    }
    trigger(ctx, uuid, |r, b, p| {
        for (id, criterion) in &b.water {
            grant(r, b, p, id, Some(criterion));
        }
    });
}

/// `yyyy-MM-dd HH:mm:ss +0000` for now, as vanilla dates criteria.
fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rest) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} +0000",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// Writes a player's advancements file: each advancement with criteria
/// done, when they were done (kept from the file for the old ones) and
/// whether it is complete.
pub fn save_advancements(ctx: &ServerContext, uuid: u128) {
    let Some(file) = advancements_file(ctx, uuid) else {
        return;
    };
    let Some(done) = lock(&ctx.rare).progress.done.get(&uuid).cloned() else {
        return;
    };
    let old: serde_json::Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let b = book(ctx);
    let now = now_stamp();
    let mut out = serde_json::Map::new();
    let sorted: BTreeMap<&String, &BTreeSet<String>> = done.iter().collect();
    for (id, criteria) in sorted {
        if criteria.is_empty() {
            continue;
        }
        let mut c = serde_json::Map::new();
        for name in criteria {
            let when = old[id.as_str()]["criteria"][name.as_str()]
                .as_str()
                .map_or_else(|| now.clone(), str::to_owned);
            c.insert(name.clone(), serde_json::Value::String(when));
        }
        let complete = b.advs.get(id).is_some_and(|a| a.complete(Some(criteria)));
        out.insert(
            id.clone(),
            serde_json::json!({ "criteria": c, "done": complete }),
        );
    }
    out.insert(
        "DataVersion".into(),
        serde_json::Value::from(ctx.data.version.world_version),
    );
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(out)).unwrap_or_default();
    if let Err(e) = std::fs::write(&file, text) {
        tracing::warn!("saving advancements: {e:#}");
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
fn rewarded(r: &mut Sink, b: &Book, p: &Target, id: &str, adv: &Adv) {
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
            r.events.push(Event::Direct {
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
            r.events.push(Event::Message {
                uuid: u,
                line: line.clone(),
                overlay: false,
            });
        }
    }
}

/// A recipe becomes known: advancements waiting for it progress.
fn unlock_recipe(r: &mut Sink, b: &Book, p: &Target, recipe: &str) -> bool {
    let uuid = p.uuid();
    let new = lock(&r.ctx.rare)
        .progress
        .known
        .entry(uuid)
        .or_default()
        .insert(recipe.to_owned());
    if new {
        if let Some(id) = r.ctx.recipes.book_id(recipe) {
            lock(&r.ctx.rare)
                .progress
                .highlight
                .entry(uuid)
                .or_default()
                .insert(recipe.to_owned());
            r.book_added(uuid, id);
        }
        for (id, criterion) in b.waiting.get(recipe).into_iter().flatten() {
            grant(r, b, p, id, Some(criterion));
        }
    }
    new
}

/// A recipe leaves the book.
fn forget_recipe(r: &mut Sink, p: &Target, recipe: &str) -> bool {
    let uuid = p.uuid();
    let mut state = lock(&r.ctx.rare);
    let had = state
        .progress
        .known
        .get_mut(&uuid)
        .is_some_and(|k| k.remove(recipe));
    if let Some(h) = state.progress.highlight.get_mut(&uuid) {
        h.remove(recipe);
    }
    drop(state);
    if had && let Some(id) = r.ctx.recipes.book_id(recipe) {
        r.book_removed(uuid, id);
    }
    had
}

/// Completes the criteria that complete by themselves.
fn tick_awards(r: &mut Sink, b: &Book, p: &Target) {
    for (id, adv) in &b.advs {
        if adv.ticks.is_empty() {
            continue;
        }
        let (was, now, changed) = {
            let mut st = lock(&r.ctx.rare);
            let have = st
                .progress
                .done
                .entry(p.uuid())
                .or_default()
                .entry(id.clone())
                .or_default();
            let was = adv.complete(Some(have));
            let before = have.len();
            have.extend(adv.ticks.iter().cloned());
            (was, adv.complete(Some(have)), have.len() != before)
        };
        if changed {
            r.advancement_changed(b, p.uuid(), id);
        }
        if !was && now {
            rewarded(r, b, p, id, adv);
        }
    }
}

fn grant(r: &mut Sink, b: &Book, p: &Target, id: &str, only: Option<&str>) -> bool {
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
    if changed {
        r.advancement_changed(b, p.uuid(), id);
    }
    if now && !was {
        rewarded(r, b, p, id, adv);
    }
    changed
}

fn revoke(r: &mut Sink, b: &Book, p: &Target, id: &str, only: Option<&str>) -> bool {
    let changed = take_back(r, p, id, only);
    if changed {
        r.advancement_changed(b, p.uuid(), id);
    }
    changed
}

fn take_back(r: &mut Sink, p: &Target, id: &str, only: Option<&str>) -> bool {
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
        tick_awards(&mut sink(r), &b, p);
        for i in &ids {
            let changed = if giving {
                grant(&mut sink(r), &b, p, i, criterion.as_deref())
            } else {
                revoke(&mut sink(r), &b, p, i, criterion.as_deref())
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
        tick_awards(&mut sink(r), &b, p);
        let ids: Vec<String> = match &wanted {
            Some(id) => vec![id.clone()],
            None => b.recipes.iter().cloned().collect(),
        };
        for id in ids {
            let changed = if giving {
                unlock_recipe(&mut sink(r), &b, p, &id)
            } else {
                forget_recipe(&mut sink(r), p, &id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Pose;
    use crate::players::{BookState, Saved, Store};
    use crate::testkit;
    use mc_protocol::packets::play::ItemStack;

    const ME: u128 = 0xb00b;

    fn setup() -> Option<Arc<ServerContext>> {
        let ctx = testkit::flat()?;
        testkit::join(&ctx, ME, 52, [0.5, -60.0, 0.5], 0);
        load_progress(&ctx, ME, &[], &BookState::default());
        Some(ctx)
    }

    fn run(ctx: &ServerContext, line: &str) -> super::super::Output {
        let mut control = crate::tick::TickControl::default();
        super::super::execute(ctx, &mut control, &super::super::Source::Console, line)
    }

    /// The recipe ids a player's packets added and removed.
    fn changes(events: &[Event]) -> (Vec<i32>, Vec<i32>) {
        let (mut added, mut removed) = (Vec::new(), Vec::new());
        for e in events {
            match e {
                Event::Direct {
                    packet: Direct::RecipeAdd(add),
                    ..
                } => added.extend(add.entries.iter().map(|e| e.id)),
                Event::Direct {
                    packet: Direct::RecipeRemove(remove),
                    ..
                } => removed.extend(&remove.recipes),
                _ => {}
            }
        }
        (added, removed)
    }

    fn drain(rx: &mut crate::context::EventReceiver) -> Vec<Event> {
        std::iter::from_fn(|| rx.try_changed()).collect()
    }

    /// The advancement updates among a player's packets, as one.
    fn screen(events: &[Event]) -> UpdateAdvancements {
        let mut all = UpdateAdvancements {
            reset: false,
            added: vec![],
            removed: vec![],
            progress: vec![],
            show: false,
        };
        for e in events {
            if let Event::Direct {
                packet: Direct::Advancements(u),
                ..
            } = e
            {
                all.added.extend(u.added.iter().cloned());
                all.removed.extend(u.removed.iter().cloned());
                all.progress.extend(u.progress.iter().cloned());
                all.show |= u.show;
            }
        }
        all
    }

    #[test]
    fn the_screen_shows_what_is_done_and_two_steps_past_it() {
        let Some(ctx) = setup() else { return };
        // Nothing done: no tab yet.
        let start = initial_advancements(&ctx, ME);
        assert!(start.reset && start.added.is_empty());
        let out = screen(&run(&ctx, "advancement grant P52 only minecraft:story/root").events);
        let ids: Vec<&str> = out.added.iter().map(|(i, _)| i.as_str()).collect();
        for shown in [
            "minecraft:story/root",
            "minecraft:story/mine_stone",
            "minecraft:story/upgrade_tools",
        ] {
            assert!(ids.contains(&shown), "{shown} in {ids:?}");
        }
        assert!(!ids.contains(&"minecraft:story/smelt_iron"));
        assert!(out.show);
        let root = &out
            .added
            .iter()
            .find(|(i, _)| i == "minecraft:story/root")
            .unwrap()
            .1;
        let d = root.display.as_ref().unwrap();
        assert_eq!(
            d.background.as_deref(),
            Some("minecraft:gui/advancements/backgrounds/stone")
        );
        assert_eq!(d.x, 0.0);
        assert!(root.parent.is_none());
        let stone = &out
            .added
            .iter()
            .find(|(i, _)| i == "minecraft:story/mine_stone")
            .unwrap()
            .1;
        assert_eq!(stone.display.as_ref().unwrap().x, 1.0);
        assert_eq!(stone.parent.as_deref(), Some("minecraft:story/root"));
        let (_, crit) = out
            .progress
            .iter()
            .find(|(i, _)| i == "minecraft:story/root")
            .unwrap();
        assert!(crit.iter().all(|(_, when)| when.is_some()));
        // A rejoin sees the same.
        let again = initial_advancements(&ctx, ME);
        assert_eq!(again.added.len(), out.added.len());
        // Taking it back empties the tab.
        let out = screen(&run(&ctx, "advancement revoke P52 only minecraft:story/root").events);
        assert!(out.removed.iter().any(|i| i == "minecraft:story/root"));
        assert!(out.added.is_empty());
    }

    #[test]
    fn a_hidden_advancement_shows_only_once_done() {
        let Some(ctx) = setup() else { return };
        let b = book(&ctx);
        let (id, adv) = b
            .advs
            .iter()
            .find(|(_, a)| a.hidden && a.icon.is_some())
            .unwrap();
        let parent = adv.parent.clone().unwrap();
        run(&ctx, &format!("advancement grant P52 only {parent}"));
        assert!(
            !initial_advancements(&ctx, ME)
                .added
                .iter()
                .any(|(i, _)| i == id)
        );
        run(&ctx, &format!("advancement grant P52 only {id}"));
        assert!(
            initial_advancements(&ctx, ME)
                .added
                .iter()
                .any(|(i, _)| i == id)
        );
    }

    #[test]
    fn recipe_give_and_take_change_the_book_and_tell_the_client() {
        let Some(ctx) = setup() else { return };
        let stick = ctx.recipes.book_id("minecraft:stick").unwrap();
        let out = run(&ctx, "recipe give P52 minecraft:stick");
        let (added, removed) = changes(&out.events);
        assert!(added.contains(&stick) && removed.is_empty());
        assert!(knows_recipe(&ctx, ME, "minecraft:stick"));
        assert!(highlighted_of(&ctx).contains(&"minecraft:stick".to_owned()));
        // Giving it again changes nothing.
        assert!(
            changes(&run(&ctx, "recipe give P52 minecraft:stick").events)
                .0
                .is_empty()
        );
        let out = run(&ctx, "recipe take P52 minecraft:stick");
        assert_eq!(changes(&out.events), (vec![], vec![stick]));
        assert!(!knows_recipe(&ctx, ME, "minecraft:stick"));
        assert!(!highlighted_of(&ctx).contains(&"minecraft:stick".to_owned()));
        // Everything at once.
        let out = run(&ctx, "recipe give P52 *");
        assert!(changes(&out.events).0.len() > 100);
        let all = initial_book(&ctx, ME);
        assert!(all.replace && all.entries.len() > 100);
        run(&ctx, "recipe take P52 *");
        assert!(known_recipes(&ctx, ME).is_empty());
    }

    fn highlighted_of(ctx: &ServerContext) -> Vec<String> {
        book_state(ctx, ME).highlight
    }

    #[test]
    fn picking_up_an_ingredient_unlocks_the_recipes_that_use_it() {
        let Some(ctx) = setup() else { return };
        let mut rx = ctx.events.subscribe();
        watch(&ctx);
        // The first look gives what a player gets for nothing.
        assert!(knows_recipe(&ctx, ME, "minecraft:crafting_table"));
        assert!(!knows_recipe(&ctx, ME, "minecraft:stick"));
        let planks = ctx.items.id("minecraft:oak_planks").unwrap();
        lock(&ctx.inventories).get_mut(&ME).unwrap().slots[20] = Some(ItemStack::new(planks, 3));
        watch(&ctx);
        assert!(knows_recipe(&ctx, ME, "minecraft:stick"));
        let (added, _) = changes(&drain(&mut rx));
        assert!(added.contains(&ctx.recipes.book_id("minecraft:stick").unwrap()));
        // Nothing new when nothing changed.
        watch(&ctx);
        assert!(changes(&drain(&mut rx)).0.is_empty());
        // A recipe nobody asks the player to hold for stays locked.
        assert!(!knows_recipe(
            &ctx,
            ME,
            "minecraft:iron_ingot_from_smelting_raw_iron"
        ));
    }

    #[test]
    fn looking_at_a_recipe_clears_its_highlight() {
        let Some(ctx) = setup() else { return };
        run(&ctx, "recipe give P52 minecraft:stick");
        let id = ctx.recipes.book_id("minecraft:stick").unwrap();
        assert!(highlighted_of(&ctx).contains(&"minecraft:stick".to_owned()));
        recipe_seen(&ctx, ME, id);
        assert!(!highlighted_of(&ctx).contains(&"minecraft:stick".to_owned()));
        assert!(knows_recipe(&ctx, ME, "minecraft:stick"));
    }

    #[test]
    fn the_book_and_its_settings_are_saved_and_loaded() {
        let Some(ctx) = setup() else { return };
        run(&ctx, "recipe give P52 minecraft:stick");
        run(&ctx, "recipe give P52 minecraft:furnace");
        for seen in ["minecraft:furnace", "minecraft:crafting_table"] {
            recipe_seen(&ctx, ME, ctx.recipes.book_id(seen).unwrap());
        }
        book_settings(&ctx, ME, 0, true, false);
        book_settings(&ctx, ME, 3, false, true);
        let dir = std::env::temp_dir().join(format!("mc-book-{}", std::process::id()));
        let store = Store::new(&dir, &HashMap::new());
        let saved = Saved {
            pose: Pose {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            dimension: "minecraft:overworld".into(),
            game_mode: 0,
            flying: false,
            hands: Default::default(),
            vitals: Default::default(),
            extra: crate::players::Extra {
                recipes: known_recipes(&ctx, ME),
                book: book_state(&ctx, ME),
                ..Default::default()
            },
        };
        store.save(ME, &saved, 1).unwrap();
        let back = store.load(ME).unwrap().unwrap().extra;
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(
            back.recipes,
            vec![
                "minecraft:crafting_table",
                "minecraft:furnace",
                "minecraft:stick"
            ]
        );
        assert_eq!(back.book.highlight, vec!["minecraft:stick".to_owned()]);
        assert_eq!(back.book.open, [true, false, false, false]);
        assert_eq!(back.book.filter, [false, false, false, true]);
        // A joining player gets it back, with only the unseen one new.
        let other = ME + 1;
        testkit::join(&ctx, other, 53, [0.5, -60.0, 0.5], 0);
        load_progress(&ctx, other, &back.recipes, &back.book);
        let packet = initial_book(&ctx, other);
        assert!(packet.replace);
        let new: Vec<i32> = packet
            .entries
            .iter()
            .filter(|e| e.highlight)
            .map(|e| e.id)
            .collect();
        assert_eq!(new, vec![ctx.recipes.book_id("minecraft:stick").unwrap()]);
        assert!(packet.entries.iter().all(|e| !e.notification));
        let settings = settings_packet(&ctx, other);
        assert_eq!(settings.books[0], (true, false));
        assert_eq!(settings.books[3], (false, true));
    }
}
