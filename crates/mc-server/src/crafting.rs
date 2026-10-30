//! Crafting recipes from the data pack (`recipe/*.json`, minecraft.wiki
//! "Recipe" for the format): `crafting_shaped` (pattern and key, mirrored
//! patterns match too) and `crafting_shapeless`. An ingredient is an item,
//! an item tag (`#minecraft:planks`) or a list of those.

use std::collections::{HashMap, HashSet};

use mc_data::{McData, resolve_tags};
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{BookEntry, ItemStack, RecipeDisplay, SlotDisplay};
use serde_json::Value;

/// Items an ingredient accepts.
type Ingredient = HashSet<i32>;

#[derive(Debug)]
enum Shape {
    Shaped {
        width: usize,
        height: usize,
        /// Row by row; `None` is an empty cell.
        cells: Vec<Option<Ingredient>>,
    },
    Shapeless(Vec<Ingredient>),
    /// `crafting_decorated_pot`: back, left, right, front of the 3×3.
    DecoratedPot {
        back: Ingredient,
        left: Ingredient,
        right: Ingredient,
        front: Ingredient,
    },
}

#[derive(Debug)]
struct Recipe {
    shape: Shape,
    result: ItemStack,
    /// The recipe's registry name.
    name: String,
}

/// A furnace, blast furnace, smoker or campfire recipe.
#[derive(Clone, Debug)]
pub struct Cooking {
    /// `minecraft:smelting`, `minecraft:blasting`, `minecraft:smoking`,
    /// `minecraft:campfire_cooking`.
    pub kind: String,
    pub input: HashSet<i32>,
    pub result: ItemStack,
    /// Ticks at normal speed.
    pub time: i32,
    /// The recipe's registry name.
    pub name: String,
    /// Experience points one smelt is worth.
    pub xp: f32,
}

/// A stonecutter recipe.
#[derive(Clone, Debug)]
pub struct Cut {
    pub input: HashSet<i32>,
    pub result: ItemStack,
}

/// A smithing table recipe: an upgrade (`result` set) or a trim (not).
#[derive(Debug)]
pub struct Smithing {
    /// The recipe's id (`minecraft:coast_armor_trim_smithing_template_smithing_trim`).
    pub id: String,
    pub template: HashSet<i32>,
    pub base: HashSet<i32>,
    pub addition: HashSet<i32>,
    pub result: Option<ItemStack>,
    /// A trim's pattern (`minecraft:coast`).
    pub pattern: Option<String>,
}

/// A recipe the recipe book lists: its crafting, furnace, blast furnace or
/// smoker kind, what it shows and what it takes to place it.
#[derive(Clone, Debug)]
pub struct BookRecipe {
    /// The recipe's registry name.
    pub name: String,
    pub display: RecipeDisplay,
    /// `minecraft:recipe_book_category`.
    pub category: i32,
    /// Recipes of one group share a place in the book.
    pub group: Option<i32>,
    /// Which book: 0 crafting, 1 furnace, 2 blast furnace, 3 smoker (4 for
    /// the campfire, which has none).
    pub book: usize,
    pub width: usize,
    pub height: usize,
    /// Whether the ingredients sit in a `width`×`height` pattern (else any
    /// order).
    pub shaped: bool,
    /// The items each slot takes, row by row for a pattern; `None` is an
    /// empty cell.
    pub slots: Vec<Option<Vec<i32>>>,
}

impl BookRecipe {
    /// The items each ingredient takes, empty cells left out.
    pub fn ingredients(&self) -> Vec<Vec<i32>> {
        self.slots.iter().flatten().cloned().collect()
    }
}

#[derive(Debug, Default)]
pub struct Recipes {
    recipes: Vec<Recipe>,
    /// The recipe book's recipes; a recipe is known to the client by its
    /// index here.
    pub book: Vec<BookRecipe>,
    book_ids: HashMap<String, usize>,
    pub cooking: Vec<Cooking>,
    /// In the order the client is told of them.
    pub stonecutting: Vec<Cut>,
    pub smithing: Vec<Smithing>,
    /// Item tags (`minecraft:planks`) with the items in them.
    pub item_tags: std::collections::HashMap<String, Vec<i32>>,
    /// Item id to `minecraft:` name, for components crafting has to write.
    names: HashMap<i32, String>,
}

impl Recipes {
    pub fn load(data: &McData) -> Self {
        let pack = data.data_pack();
        let Some(items) = data.builtin.get("minecraft:item") else {
            return Self::default();
        };
        let item_tags = pack
            .tags("minecraft:item")
            .map(|t| resolve_tags(&t, |name| items.get(name).copied()).0)
            .unwrap_or_default();
        let ingredient = |v: &Value| -> Option<Ingredient> {
            let one = |s: &str| -> Vec<i32> {
                match s.strip_prefix('#') {
                    Some(tag) => item_tags.get(tag).cloned().unwrap_or_default(),
                    None => items.get(s).copied().into_iter().collect(),
                }
            };
            let set: Ingredient = match v {
                Value::String(s) => one(s).into_iter().collect(),
                Value::Array(list) => list
                    .iter()
                    .filter_map(Value::as_str)
                    .flat_map(one)
                    .collect(),
                _ => return None,
            };
            (!set.is_empty()).then_some(set)
        };
        let mut recipes = Vec::new();
        let mut cooking = Vec::new();
        let mut stonecutting = Vec::new();
        let mut smithing = Vec::new();
        for name in pack
            .registry_entries("minecraft:recipe")
            .unwrap_or_default()
        {
            let Ok(json) = pack.read_entry("minecraft:recipe", &name) else {
                continue;
            };
            if json["type"].as_str() == Some("minecraft:smithing_trim") {
                if let (Some(template), Some(base), Some(addition)) = (
                    ingredient(&json["template"]),
                    ingredient(&json["base"]),
                    ingredient(&json["addition"]),
                ) {
                    smithing.push(Smithing {
                        id: name.clone(),
                        template,
                        base,
                        addition,
                        result: None,
                        pattern: json["pattern"].as_str().map(str::to_owned),
                    });
                }
                continue;
            }
            let Some(result) = json["result"]["id"].as_str().and_then(|id| items.get(id)) else {
                continue;
            };
            let result = ItemStack {
                count: json["result"]["count"].as_i64().unwrap_or(1) as i32,
                item: *result,
                damage: 0,
                components: Default::default(),
            };
            let kind = json["type"].as_str().unwrap_or("");
            if kind == "minecraft:stonecutting" {
                if let Some(input) = ingredient(&json["ingredient"]) {
                    stonecutting.push(Cut { input, result });
                }
                continue;
            }
            if kind == "minecraft:smithing_transform" {
                if let (Some(template), Some(base), Some(addition)) = (
                    ingredient(&json["template"]),
                    ingredient(&json["base"]),
                    ingredient(&json["addition"]),
                ) {
                    smithing.push(Smithing {
                        id: name.clone(),
                        template,
                        base,
                        addition,
                        result: Some(result),
                        pattern: None,
                    });
                }
                continue;
            }
            if matches!(
                kind,
                "minecraft:smelting"
                    | "minecraft:blasting"
                    | "minecraft:smoking"
                    | "minecraft:campfire_cooking"
            ) {
                if let Some(input) = ingredient(&json["ingredient"]) {
                    cooking.push(Cooking {
                        kind: kind.to_owned(),
                        input,
                        result,
                        time: json["cookingtime"].as_i64().unwrap_or(200) as i32,
                        name: name.to_string(),
                        xp: json["experience"].as_f64().unwrap_or(0.0) as f32,
                    });
                }
                continue;
            }
            let shape = match json["type"].as_str() {
                Some("minecraft:crafting_shaped") => {
                    let rows: Vec<&str> = json["pattern"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
                    let mut cells = Vec::new();
                    let mut ok = true;
                    for row in &rows {
                        for i in 0..width {
                            match row.chars().nth(i) {
                                Some(' ') | None => cells.push(None),
                                Some(c) => match ingredient(&json["key"][c.to_string()]) {
                                    Some(ing) => cells.push(Some(ing)),
                                    None => ok = false,
                                },
                            }
                        }
                    }
                    if !ok || width == 0 {
                        continue;
                    }
                    Shape::Shaped {
                        width,
                        height: rows.len(),
                        cells,
                    }
                }
                Some("minecraft:crafting_decorated_pot") => {
                    match (
                        ingredient(&json["back"]),
                        ingredient(&json["left"]),
                        ingredient(&json["right"]),
                        ingredient(&json["front"]),
                    ) {
                        (Some(back), Some(left), Some(right), Some(front)) => Shape::DecoratedPot {
                            back,
                            left,
                            right,
                            front,
                        },
                        _ => continue,
                    }
                }
                Some("minecraft:crafting_shapeless") => {
                    let list: Option<Vec<Ingredient>> = json["ingredients"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(ingredient)
                        .collect();
                    match list {
                        Some(list) if !list.is_empty() => Shape::Shapeless(list),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            recipes.push(Recipe {
                shape,
                result,
                name: name.to_string(),
            });
        }
        let book = book_recipes(data, &pack, items, &item_tags);
        let book_ids = book
            .iter()
            .enumerate()
            .map(|(i, r)| (r.name.clone(), i))
            .collect();
        let names = items.iter().map(|(n, &id)| (id, n.clone())).collect();
        Self {
            recipes,
            book,
            book_ids,
            cooking,
            stonecutting,
            smithing,
            item_tags: item_tags
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.to_vec()))
                .collect(),
            names,
        }
    }

    /// What clients are told: which items their furnace and smithing
    /// slots accept, and the stonecutter's recipes.
    pub fn update_packet(&self) -> mc_protocol::packets::play::UpdateRecipes {
        let sorted = |set: HashSet<i32>| {
            let mut v: Vec<i32> = set.into_iter().collect();
            v.sort_unstable();
            v
        };
        let cooked = |kind: &str| {
            sorted(
                self.cooking
                    .iter()
                    .filter(|c| c.kind == kind)
                    .flat_map(|c| c.input.iter().copied())
                    .collect(),
            )
        };
        let smith = |part: fn(&Smithing) -> &HashSet<i32>| {
            sorted(
                self.smithing
                    .iter()
                    .flat_map(|s| part(s).iter().copied())
                    .collect(),
            )
        };
        mc_protocol::packets::play::UpdateRecipes {
            property_sets: vec![
                (
                    "minecraft:furnace_input".into(),
                    cooked("minecraft:smelting"),
                ),
                (
                    "minecraft:blast_furnace_input".into(),
                    cooked("minecraft:blasting"),
                ),
                ("minecraft:smoker_input".into(), cooked("minecraft:smoking")),
                (
                    "minecraft:campfire_input".into(),
                    cooked("minecraft:campfire_cooking"),
                ),
                ("minecraft:smithing_base".into(), smith(|s| &s.base)),
                ("minecraft:smithing_template".into(), smith(|s| &s.template)),
                ("minecraft:smithing_addition".into(), smith(|s| &s.addition)),
            ],
            stonecutter: self
                .stonecutting
                .iter()
                .map(|c| (sorted(c.input.clone()), c.result.clone()))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    /// The number the client knows the book recipe `name` by.
    pub fn book_id(&self, name: &str) -> Option<i32> {
        self.book_ids.get(name).map(|&i| i as i32)
    }

    /// The book recipe the client calls `id`.
    pub fn book_recipe(&self, id: i32) -> Option<&BookRecipe> {
        self.book.get(usize::try_from(id).ok()?)
    }

    /// What Recipe Book Add tells of book recipe `id`.
    pub fn book_entry(&self, id: i32, notification: bool, highlight: bool) -> Option<BookEntry> {
        let r = self.book_recipe(id)?;
        Some(BookEntry {
            id,
            display: r.display.clone(),
            group: r.group,
            category: r.category,
            ingredients: Some(r.ingredients()),
            notification,
            highlight,
        })
    }

    /// What a `size`×`size` crafting grid (row by row) makes.
    pub fn craft(&self, grid: &[Option<ItemStack>], size: usize) -> Option<ItemStack> {
        self.craft_named(grid, size).map(|(made, _)| made)
    }

    /// [`Recipes::craft`] with the name of the recipe that made it.
    pub fn craft_named(
        &self,
        grid: &[Option<ItemStack>],
        size: usize,
    ) -> Option<(ItemStack, &str)> {
        let items: Vec<Option<i32>> = grid.iter().map(|s| s.as_ref().map(|s| s.item)).collect();
        // The smallest box around what is in the grid.
        let filled: Vec<(usize, usize)> = (0..size * size)
            .filter(|&i| items[i].is_some())
            .map(|i| (i % size, i / size))
            .collect();
        if filled.is_empty() {
            return None;
        }
        let (x0, x1) = (
            filled.iter().map(|c| c.0).min()?,
            filled.iter().map(|c| c.0).max()?,
        );
        let (y0, y1) = (
            filled.iter().map(|c| c.1).min()?,
            filled.iter().map(|c| c.1).max()?,
        );
        let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
        let cell = |x: usize, y: usize| items[(y0 + y) * size + x0 + x];
        for recipe in &self.recipes {
            let fits = match &recipe.shape {
                Shape::DecoratedPot {
                    back,
                    left,
                    right,
                    front,
                } => {
                    if let Some(made) = self.decorated_pot(
                        grid,
                        size,
                        (x0, y0, w, h),
                        recipe.result.item,
                        (back, left, right, front),
                    ) {
                        return Some((made, recipe.name.as_str()));
                    }
                    false
                }
                Shape::Shaped {
                    width,
                    height,
                    cells,
                } => {
                    *width == w
                        && *height == h
                        && [false, true].into_iter().any(|mirror| {
                            (0..h).all(|y| {
                                (0..w).all(|x| {
                                    let px = if mirror { w - 1 - x } else { x };
                                    match (&cells[y * w + px], cell(x, y)) {
                                        (None, None) => true,
                                        (Some(ing), Some(item)) => ing.contains(&item),
                                        _ => false,
                                    }
                                })
                            })
                        })
                }
                Shape::Shapeless(ingredients) => {
                    let present: Vec<i32> = items.iter().flatten().copied().collect();
                    present.len() == ingredients.len()
                        && assign(ingredients, &present, &mut vec![false; present.len()], 0)
                }
            };
            if fits {
                return Some((recipe.result.clone(), recipe.name.as_str()));
            }
        }
        None
    }

    /// The decorated-pot pattern: sherds or bricks on the four sides, corners
    /// and the centre empty. The result carries `pot_decorations` unless every
    /// side is a plain brick (the item's default).
    fn decorated_pot(
        &self,
        grid: &[Option<ItemStack>],
        size: usize,
        origin: (usize, usize, usize, usize),
        result: i32,
        sides: (&Ingredient, &Ingredient, &Ingredient, &Ingredient),
    ) -> Option<ItemStack> {
        let (x0, y0, w, h) = origin;
        let (back, left, right, front) = sides;
        if w != 3 || h != 3 {
            return None;
        }
        let at = |x: usize, y: usize| grid.get((y0 + y) * size + x0 + x)?.as_ref();
        let fits = |x, y, ing: &Ingredient| at(x, y).is_some_and(|s| ing.contains(&s.item));
        let empty = |x, y| at(x, y).is_none();
        if !fits(1, 0, back) || !fits(0, 1, left) || !fits(2, 1, right) || !fits(1, 2, front) {
            return None;
        }
        if [(0, 0), (2, 0), (1, 1), (0, 2), (2, 2)]
            .into_iter()
            .any(|(x, y)| !empty(x, y))
        {
            return None;
        }
        let mut stack = ItemStack::new(result, 1);
        let mut dec = Compound::new();
        let mut plain = true;
        for (key, x, y) in [
            ("back", 1, 0),
            ("left", 0, 1),
            ("right", 2, 1),
            ("front", 1, 2),
        ] {
            let side = at(x, y)?;
            let name = self.names.get(&side.item)?;
            if name != "minecraft:brick" || side.damage != 0 || !side.components.is_empty() {
                plain = false;
            }
            let mut saved = Compound::new();
            saved.insert("id", Tag::String(name.clone()));
            saved.insert("count", Tag::Int(1));
            if let Some(comp) = side.components.to_nbt(side.damage)
                && !comp.is_empty()
            {
                saved.insert("components", Tag::Compound(comp));
            }
            dec.insert(key, Tag::Compound(saved));
        }
        if !plain {
            stack.components.set("pot_decorations", Tag::Compound(dec));
        }
        Some(stack)
    }
}

/// The recipes the recipe book lists, in the order of their registry names.
fn book_recipes(
    data: &McData,
    pack: &mc_data::DataPack,
    items: &HashMap<String, i32>,
    item_tags: &std::collections::BTreeMap<String, Vec<i32>>,
) -> Vec<BookRecipe> {
    let item = |name: &str| items.get(name).copied();
    let one = |s: &str| -> Vec<i32> {
        match s.strip_prefix('#') {
            Some(tag) => item_tags.get(tag).cloned().unwrap_or_default(),
            None => item(s).into_iter().collect(),
        }
    };
    let accepted = |v: &Value| -> Option<Vec<i32>> {
        let mut set: Vec<i32> = match v {
            Value::String(s) => one(s),
            Value::Array(list) => list
                .iter()
                .filter_map(Value::as_str)
                .flat_map(one)
                .collect(),
            _ => return None,
        };
        set.sort_unstable();
        set.dedup();
        (!set.is_empty()).then_some(set)
    };
    let shown = |v: &Value| -> SlotDisplay {
        let one = |s: &str| match s.strip_prefix('#') {
            Some(tag) => SlotDisplay::Tag(tag.to_owned()),
            None => item(s).map_or(SlotDisplay::Empty, SlotDisplay::Item),
        };
        match v {
            Value::String(s) => one(s),
            Value::Array(list) => {
                SlotDisplay::Composite(list.iter().filter_map(Value::as_str).map(one).collect())
            }
            _ => SlotDisplay::Empty,
        }
    };
    let category = |name: &str| {
        data.builtin
            .id(
                "minecraft:recipe_book_category",
                &format!("minecraft:{name}"),
            )
            .unwrap_or(0)
    };
    let station = |name: &str| item(name).map_or(SlotDisplay::Empty, SlotDisplay::Item);
    let mut groups: HashMap<String, i32> = HashMap::new();
    let mut out = Vec::new();
    for name in pack
        .registry_entries("minecraft:recipe")
        .unwrap_or_default()
    {
        let Ok(json) = pack.read_entry("minecraft:recipe", &name) else {
            continue;
        };
        let kind = json["type"].as_str().unwrap_or("");
        let Some(result) = json["result"]["id"].as_str().and_then(item) else {
            continue;
        };
        let result_stack =
            ItemStack::new(result, json["result"]["count"].as_i64().unwrap_or(1) as i32);
        let result = SlotDisplay::ItemStack(result_stack);
        let wanted = json["category"].as_str().unwrap_or("");
        let (display, category, book, width, height, shaped, slots) = match kind {
            "minecraft:crafting_shaped" => {
                let rows: Vec<&str> = json["pattern"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
                let mut shows = Vec::new();
                let mut slots = Vec::new();
                let mut ok = width > 0;
                for row in &rows {
                    for i in 0..width {
                        match row.chars().nth(i) {
                            Some(' ') | None => {
                                shows.push(SlotDisplay::Empty);
                                slots.push(None);
                            }
                            Some(c) => {
                                let key = &json["key"][c.to_string()];
                                match accepted(key) {
                                    Some(set) => {
                                        shows.push(shown(key));
                                        slots.push(Some(set));
                                    }
                                    None => ok = false,
                                }
                            }
                        }
                    }
                }
                if !ok {
                    continue;
                }
                (
                    RecipeDisplay::Shaped {
                        width: width as i32,
                        height: rows.len() as i32,
                        ingredients: shows,
                        result,
                        station: station("minecraft:crafting_table"),
                    },
                    category(match wanted {
                        "building" => "crafting_building_blocks",
                        "redstone" => "crafting_redstone",
                        "equipment" => "crafting_equipment",
                        _ => "crafting_misc",
                    }),
                    0,
                    width,
                    rows.len(),
                    true,
                    slots,
                )
            }
            "minecraft:crafting_shapeless" => {
                let list = json["ingredients"].as_array().cloned().unwrap_or_default();
                let slots: Option<Vec<Option<Vec<i32>>>> =
                    list.iter().map(|v| accepted(v).map(Some)).collect();
                let Some(slots) = slots.filter(|s| !s.is_empty()) else {
                    continue;
                };
                (
                    RecipeDisplay::Shapeless {
                        ingredients: list.iter().map(shown).collect(),
                        result,
                        station: station("minecraft:crafting_table"),
                    },
                    category(match wanted {
                        "building" => "crafting_building_blocks",
                        "redstone" => "crafting_redstone",
                        "equipment" => "crafting_equipment",
                        _ => "crafting_misc",
                    }),
                    0,
                    0,
                    0,
                    false,
                    slots,
                )
            }
            "minecraft:smelting"
            | "minecraft:blasting"
            | "minecraft:smoking"
            | "minecraft:campfire_cooking" => {
                let Some(set) = accepted(&json["ingredient"]) else {
                    continue;
                };
                let (book, cat, block) = match kind {
                    "minecraft:smelting" => (
                        1,
                        match wanted {
                            "food" => "furnace_food",
                            "blocks" => "furnace_blocks",
                            _ => "furnace_misc",
                        },
                        "minecraft:furnace",
                    ),
                    "minecraft:blasting" => (
                        2,
                        match wanted {
                            "blocks" => "blast_furnace_blocks",
                            _ => "blast_furnace_misc",
                        },
                        "minecraft:blast_furnace",
                    ),
                    "minecraft:smoking" => (3, "smoker_food", "minecraft:smoker"),
                    _ => (4, "campfire", "minecraft:campfire"),
                };
                (
                    RecipeDisplay::Furnace {
                        ingredient: shown(&json["ingredient"]),
                        fuel: SlotDisplay::AnyFuel,
                        result,
                        station: station(block),
                        time: json["cookingtime"].as_i64().unwrap_or(200) as i32,
                        experience: json["experience"].as_f64().unwrap_or(0.0) as f32,
                    },
                    category(cat),
                    book,
                    1,
                    1,
                    true,
                    vec![Some(set)],
                )
            }
            _ => continue,
        };
        let group = json["group"].as_str().filter(|g| !g.is_empty()).map(|g| {
            let next = groups.len() as i32;
            *groups.entry(g.to_owned()).or_insert(next)
        });
        out.push(BookRecipe {
            name: name.to_string(),
            display,
            category,
            group,
            book,
            width,
            height,
            shaped,
            slots,
        });
    }
    out
}

/// Whether each ingredient from `i` on can take a different unused item.
fn assign(ingredients: &[Ingredient], items: &[i32], used: &mut Vec<bool>, i: usize) -> bool {
    if i == ingredients.len() {
        return true;
    }
    for (j, item) in items.iter().enumerate() {
        if !used[j] && ingredients[i].contains(item) {
            used[j] = true;
            if assign(ingredients, items, used, i + 1) {
                return true;
            }
            used[j] = false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_nbt::Tag;

    fn data() -> Option<McData> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        McData::load(&dir).ok()
    }

    #[test]
    fn a_datapack_adds_a_recipe_and_a_loot_table() {
        let Some(data) = data() else {
            eprintln!("skipped: no game data");
            return;
        };
        let tmp = std::env::temp_dir().join(format!("mc-pack-recipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let recipe = tmp.join("data/pack/recipe");
        std::fs::create_dir_all(&recipe).unwrap();
        std::fs::write(
            recipe.join("from_stick.json"),
            r#"{"type":"minecraft:crafting_shapeless","ingredients":["minecraft:stick"],"result":{"id":"minecraft:diamond","count":1}}"#,
        )
        .unwrap();
        let loot = tmp.join("data/pack/loot_table/gameplay");
        std::fs::create_dir_all(&loot).unwrap();
        std::fs::write(loot.join("custom.json"), r#"{"pools":[]}"#).unwrap();
        data.set_pack_layers(vec![tmp.join("data")]);
        let recipes = Recipes::load(&data);
        let id = |n: &str| data.builtin.id("minecraft:item", n).unwrap();
        let stick = Some(ItemStack {
            count: 1,
            item: id("minecraft:stick"),
            damage: 0,
            components: Default::default(),
        });
        let (made, name) = recipes.craft_named(&[stick, None, None, None], 2).unwrap();
        assert_eq!(name, "pack:from_stick");
        assert_eq!(made.item, id("minecraft:diamond"));
        let tables = crate::loot::BlockLoot::load(&data);
        assert!(tables.has("pack:gameplay/custom"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn reload_picks_up_a_recipe_that_was_not_there() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let stick = ctx.items.id("minecraft:stick").unwrap();
        let grid = [Some(ItemStack::new(stick, 1)), None, None, None];
        assert!(ctx.recipes.get().craft_named(&grid, 2).is_none());
        let tmp = std::env::temp_dir().join(format!("mc-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let recipe = tmp.join("data/pack/recipe");
        std::fs::create_dir_all(&recipe).unwrap();
        std::fs::write(
            recipe.join("from_stick.json"),
            r#"{"type":"minecraft:crafting_shapeless","ingredients":["minecraft:stick"],"result":{"id":"minecraft:diamond","count":1}}"#,
        )
        .unwrap();
        {
            let mut packs = ctx.packs.lock().expect("packs");
            packs.all.push(crate::packs::Pack {
                id: "file/extra".into(),
                title: "extra".into(),
                description: Tag::String(String::new()),
                dir: Some(tmp.clone()),
                features: Vec::new(),
                overlays: Vec::new(),
            });
            packs.enabled.push("file/extra".into());
        }
        ctx.reload_content();
        let recipes = ctx.recipes.get();
        let (made, name) = recipes.craft_named(&grid, 2).unwrap();
        assert_eq!(name, "pack:from_stick");
        assert_eq!(made.item, ctx.items.id("minecraft:diamond").unwrap());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn planks_sticks_table_and_torches() {
        let Some(data) = data() else {
            eprintln!("skipped: no game data");
            return;
        };
        let recipes = Recipes::load(&data);
        assert!(recipes.len() > 1000, "{} recipes", recipes.len());
        let id = |n: &str| data.builtin.id("minecraft:item", n).unwrap();
        let stack = |n: &str| {
            Some(ItemStack {
                count: 1,
                item: id(n),
                damage: 0,
                components: Default::default(),
            })
        };
        // A log anywhere in the 2×2 grid: four planks.
        let made = recipes
            .craft(&[None, None, None, stack("minecraft:oak_log")], 2)
            .unwrap();
        assert_eq!((made.item, made.count), (id("minecraft:oak_planks"), 4));
        // Planks over planks: sticks, in either column.
        let p = stack("minecraft:birch_planks");
        let made = recipes
            .craft(&[None, p.clone(), None, p.clone()], 2)
            .unwrap();
        assert_eq!((made.item, made.count), (id("minecraft:stick"), 4));
        // Four planks: a crafting table.
        let made = recipes
            .craft(&[p.clone(), p.clone(), p.clone(), p.clone()], 2)
            .unwrap();
        assert_eq!(made.item, id("minecraft:crafting_table"));
        // Coal over a stick in a 3×3 grid: four torches.
        let mut grid = vec![None; 9];
        grid[4] = stack("minecraft:coal");
        grid[7] = stack("minecraft:stick");
        let made = recipes.craft(&grid, 3).unwrap();
        assert_eq!((made.item, made.count), (id("minecraft:torch"), 4));
        // Sherds around a pot: the sides become pot_decorations.
        let mut grid = vec![None; 9];
        grid[1] = stack("minecraft:angler_pottery_sherd");
        grid[3] = stack("minecraft:brick");
        grid[5] = stack("minecraft:arms_up_pottery_sherd");
        grid[7] = stack("minecraft:brick");
        let made = recipes.craft(&grid, 3).unwrap();
        assert_eq!(made.item, id("minecraft:decorated_pot"));
        let Tag::Compound(dec) = made.components.get("pot_decorations").unwrap() else {
            panic!("decorations");
        };
        let Tag::Compound(back) = dec.get("back").unwrap() else {
            panic!("back");
        };
        assert_eq!(
            back.get("id"),
            Some(&Tag::String("minecraft:angler_pottery_sherd".into()))
        );
        let Tag::Compound(right) = dec.get("right").unwrap() else {
            panic!("right");
        };
        assert_eq!(
            right.get("id"),
            Some(&Tag::String("minecraft:arms_up_pottery_sherd".into()))
        );
        // A 2×2 cannot hold the pattern.
        assert!(recipes.craft(&grid[..4], 2).is_none());
        // Nonsense makes nothing.
        assert!(
            recipes
                .craft(
                    &[
                        stack("minecraft:dirt"),
                        None,
                        None,
                        stack("minecraft:torch")
                    ],
                    2
                )
                .is_none()
        );
    }
}
