//! Crafting recipes from the data pack (`recipe/*.json`, minecraft.wiki
//! "Recipe" for the format): `crafting_shaped` (pattern and key, mirrored
//! patterns match too) and `crafting_shapeless`. An ingredient is an item,
//! an item tag (`#minecraft:planks`) or a list of those.

use std::collections::HashSet;

use mc_data::{McData, resolve_tags};
use mc_protocol::packets::play::ItemStack;
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
}

#[derive(Debug)]
struct Recipe {
    shape: Shape,
    result: ItemStack,
}

/// A furnace, blast furnace, smoker or campfire recipe.
#[derive(Debug)]
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
#[derive(Debug)]
pub struct Cut {
    pub input: HashSet<i32>,
    pub result: ItemStack,
}

/// A smithing table recipe: an upgrade (`result` set) or a trim (not).
#[derive(Debug)]
pub struct Smithing {
    pub template: HashSet<i32>,
    pub base: HashSet<i32>,
    pub addition: HashSet<i32>,
    pub result: Option<ItemStack>,
}

#[derive(Debug, Default)]
pub struct Recipes {
    recipes: Vec<Recipe>,
    pub cooking: Vec<Cooking>,
    /// In the order the client is told of them.
    pub stonecutting: Vec<Cut>,
    pub smithing: Vec<Smithing>,
    /// Item tags (`minecraft:planks`) with the items in them.
    pub item_tags: std::collections::HashMap<String, Vec<i32>>,
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
                        template,
                        base,
                        addition,
                        result: None,
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
                        template,
                        base,
                        addition,
                        result: Some(result),
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
            recipes.push(Recipe { shape, result });
        }
        Self {
            recipes,
            cooking,
            stonecutting,
            smithing,
            item_tags: item_tags
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.to_vec()))
                .collect(),
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

    /// What a `size`×`size` crafting grid (row by row) makes.
    pub fn craft(&self, grid: &[Option<ItemStack>], size: usize) -> Option<ItemStack> {
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
                return Some(recipe.result.clone());
            }
        }
        None
    }
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

    fn data() -> Option<McData> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        McData::load(&dir).ok()
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
