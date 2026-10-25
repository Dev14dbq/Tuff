//! Clicking a recipe in the recipe book (minecraft.wiki, "Recipe book" →
//! "Usage"; protocol, "Place Recipe"): the grid or the furnace's input is
//! emptied into the inventory and filled with the recipe's ingredients
//! taken from it, one set, or as many as fit with shift. When the
//! inventory lacks something, the client is shown the recipe as a ghost.
//!
//! With the `limited_crafting` game rule the crafting grids only make
//! recipes the player's book holds, and making one adds it to the book.

use std::collections::HashMap;

use mc_protocol::packets::play::ItemStack;

use crate::context::ServerContext;
use crate::crafting::BookRecipe;
use crate::interact::Hands;

/// What clicking a recipe did.
#[derive(Debug, PartialEq)]
pub struct Placed {
    /// The inventory lacked an ingredient: the grid was only emptied.
    pub missing: bool,
    /// What the emptied grid held and the inventory had no room for.
    pub thrown: Vec<ItemStack>,
}

/// The inventory slots ingredients are taken from: the main inventory and
/// the hotbar.
const POOL: std::ops::Range<usize> = 9..45;

/// Empties `grid` into the inventory and fills it with `recipe` from there.
/// `grid` is `width`×`height` cells row by row. `None` when the recipe does
/// not fit the grid at all.
pub fn place(
    hands: &mut Hands,
    grid: &mut [Option<ItemStack>],
    (width, height): (usize, usize),
    recipe: &BookRecipe,
    all: bool,
    max_stack: &dyn Fn(i32) -> i32,
) -> Option<Placed> {
    let fits = if recipe.shaped {
        recipe.width <= width && recipe.height <= height
    } else {
        recipe.slots.len() <= width * height
    };
    if !fits {
        return None;
    }
    let mut thrown = Vec::new();
    for cell in grid.iter_mut() {
        if let Some(stack) = cell.take() {
            let (taken, _) = hands.insert_stack(&stack, max_stack(stack.item));
            if taken < stack.count {
                thrown.push(ItemStack {
                    count: stack.count - taken,
                    ..stack
                });
            }
        }
    }
    // Items count by kind; ones with changes (damaged, enchanted, named)
    // are not used as ingredients.
    let mut have: HashMap<i32, i32> = HashMap::new();
    for stack in hands.slots[POOL]
        .iter()
        .flatten()
        .filter(|s| !s.has_changes())
    {
        *have.entry(stack.item).or_default() += stack.count;
    }
    // Which grid cell takes which item, favouring one already in use.
    let mut chosen: Vec<(usize, i32)> = Vec::new();
    let mut uses: HashMap<i32, i32> = HashMap::new();
    let mut lacking = false;
    for (i, slot) in recipe.slots.iter().enumerate() {
        let Some(accepted) = slot else { continue };
        let cell = if recipe.shaped {
            (i / recipe.width) * width + i % recipe.width
        } else {
            i
        };
        let spare = |item: &i32| {
            have.get(item).copied().unwrap_or(0) - uses.get(item).copied().unwrap_or(0)
        };
        let item = accepted
            .iter()
            .filter(|item| uses.contains_key(item) && spare(item) > 0)
            .chain(accepted.iter().filter(|item| spare(item) > 0))
            .max_by_key(|item| (uses.contains_key(item), spare(item)))
            .copied();
        match item {
            Some(item) => {
                *uses.entry(item).or_default() += 1;
                chosen.push((cell, item));
            }
            None => lacking = true,
        }
    }
    if lacking || chosen.is_empty() {
        return Some(Placed {
            missing: true,
            thrown,
        });
    }
    // How many of each ingredient go into a cell.
    let each = if all {
        uses.iter()
            .map(|(&item, &n)| (have[&item] / n).min(max_stack(item)))
            .min()
            .unwrap_or(1)
            .max(1)
    } else {
        1
    };
    for (&item, &n) in &uses {
        let mut left = n * each;
        for stack in hands.slots[POOL].iter_mut() {
            if left == 0 {
                break;
            }
            if let Some(s) = stack
                && s.item == item
                && !s.has_changes()
            {
                let n = s.count.min(left);
                s.count -= n;
                left -= n;
                if s.count == 0 {
                    *stack = None;
                }
            }
        }
    }
    for (cell, item) in chosen {
        grid[cell] = Some(ItemStack::new(item, each));
    }
    Some(Placed {
        missing: false,
        thrown,
    })
}

/// What a crafting grid makes for a player: with `limited_crafting` only a
/// recipe they know.
pub fn craft(
    ctx: &ServerContext,
    uuid: u128,
    grid: &[Option<ItemStack>],
    size: usize,
) -> Option<ItemStack> {
    let (made, name) = ctx.recipes.craft_named(grid, size)?;
    if ctx.game_rule_bool("limited_crafting") && !crate::commands::knows_recipe(ctx, uuid, name) {
        return None;
    }
    Some(made)
}

/// A player took the result out of a crafting grid that held `grid`: the
/// recipe it made is added to their book.
pub fn used(ctx: &ServerContext, uuid: u128, grid: &[Option<ItemStack>], size: usize) {
    if let Some((_, name)) = ctx.recipes.craft_named(grid, size) {
        crate::commands::unlock(ctx, uuid, name);
        let used: Vec<ItemStack> = grid.iter().flatten().cloned().collect();
        crate::criteria::crafted(ctx, uuid, "minecraft:recipe_crafted", name, &used);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{book_settings, book_state, knows_recipe};
    use crate::testkit;
    use mc_protocol::packets::play::{RecipeDisplay, SlotDisplay};

    fn recipe(
        shaped: bool,
        (width, height): (usize, usize),
        slots: &[Option<Vec<i32>>],
    ) -> BookRecipe {
        BookRecipe {
            name: "minecraft:test".into(),
            display: RecipeDisplay::Stonecutter {
                ingredient: SlotDisplay::Empty,
                result: SlotDisplay::Empty,
                station: SlotDisplay::Empty,
            },
            category: 0,
            group: None,
            book: 0,
            width,
            height,
            shaped,
            slots: slots.to_vec(),
        }
    }

    fn hands_with(stacks: &[(usize, i32, i32)]) -> Hands {
        let mut hands = Hands::default();
        for &(slot, item, count) in stacks {
            hands.slots[slot] = Some(ItemStack::new(item, count));
        }
        hands
    }

    const MAX: &dyn Fn(i32) -> i32 = &|_| 64;

    fn count(hands: &Hands, item: i32) -> i32 {
        hands
            .slots
            .iter()
            .flatten()
            .filter(|s| s.item == item)
            .map(|s| s.count)
            .sum()
    }

    #[test]
    fn a_recipe_takes_one_of_each_ingredient_or_a_full_set_with_shift() {
        // Four planks of either kind make a table: 2×2.
        let r = recipe(true, (2, 2), &vec![Some(vec![1, 2]); 4]);
        let mut hands = hands_with(&[(10, 1, 10), (11, 2, 30)]);
        let mut grid = vec![None; 4];
        let done = place(&mut hands, &mut grid, (2, 2), &r, false, MAX).unwrap();
        assert!(!done.missing);
        // The kind there is most of is used for every cell.
        assert!(grid.iter().all(|c| c == &Some(ItemStack::new(2, 1))));
        assert_eq!(count(&hands, 2), 26);
        // Shift fills the cells as far as the stock goes.
        let mut grid = grid.to_vec();
        place(&mut hands, &mut grid, (2, 2), &r, true, MAX).unwrap();
        assert!(grid.iter().all(|c| c == &Some(ItemStack::new(2, 7))));
        assert_eq!(count(&hands, 2), 2);
        assert_eq!(count(&hands, 1), 10);
    }

    #[test]
    fn a_pattern_keeps_its_shape_inside_a_bigger_grid() {
        // A 2×1 pattern of sticks over planks, in a 3×3 grid.
        let r = recipe(true, (1, 2), &[Some(vec![5]), Some(vec![6])]);
        let mut hands = hands_with(&[(36, 5, 2), (37, 6, 3)]);
        let mut grid = vec![None; 9];
        place(&mut hands, &mut grid, (3, 3), &r, false, MAX).unwrap();
        assert_eq!(grid[0], Some(ItemStack::new(5, 1)));
        assert_eq!(grid[3], Some(ItemStack::new(6, 1)));
        assert_eq!(grid.iter().flatten().count(), 2);
    }

    #[test]
    fn missing_items_empty_the_grid_and_ask_for_a_ghost() {
        let r = recipe(
            false,
            (0, 0),
            &[Some(vec![1]), Some(vec![1]), Some(vec![2])],
        );
        let mut hands = hands_with(&[(10, 1, 1), (11, 2, 4)]);
        let mut grid = vec![Some(ItemStack::new(7, 3)), None, None, None];
        let done = place(&mut hands, &mut grid, (2, 2), &r, false, MAX).unwrap();
        assert!(done.missing);
        assert!(grid.iter().all(Option::is_none));
        // What was in the grid is back in the inventory, nothing was taken.
        assert_eq!(
            (count(&hands, 7), count(&hands, 1), count(&hands, 2)),
            (3, 1, 4)
        );
    }

    #[test]
    fn a_recipe_that_does_not_fit_the_grid_is_ignored() {
        let r = recipe(true, (3, 3), &vec![Some(vec![1]); 9]);
        let mut hands = hands_with(&[(10, 1, 64)]);
        let mut grid = vec![None; 4];
        assert_eq!(place(&mut hands, &mut grid, (2, 2), &r, false, MAX), None);
    }

    #[test]
    fn what_the_inventory_cannot_take_back_is_thrown() {
        let r = recipe(true, (1, 1), &[Some(vec![1])]);
        let mut hands = Hands::default();
        for slot in 9..45 {
            hands.slots[slot] = Some(ItemStack::new(3, 64));
        }
        let mut grid = vec![Some(ItemStack::new(7, 5))];
        let done = place(&mut hands, &mut grid, (1, 1), &r, false, MAX).unwrap();
        assert!(done.missing);
        assert_eq!(done.thrown, vec![ItemStack::new(7, 5)]);
    }

    const ME: u128 = 0xb00c;

    fn id(ctx: &ServerContext, name: &str) -> i32 {
        ctx.items.id(name).unwrap()
    }

    #[test]
    fn real_recipes_are_placed_from_the_book_ids() {
        let Some(ctx) = testkit::flat() else { return };
        let planks = id(&ctx, "minecraft:oak_planks");
        let r = ctx
            .recipes
            .book_recipe(ctx.recipes.book_id("minecraft:crafting_table").unwrap())
            .unwrap();
        let mut hands = hands_with(&[(20, planks, 9)]);
        let mut grid = vec![None; 4];
        let done = place(&mut hands, &mut grid, (2, 2), r, true, MAX).unwrap();
        assert!(!done.missing);
        let (made, name) = ctx.recipes.craft_named(&grid, 2).unwrap();
        assert_eq!(
            (made.item, name),
            (
                id(&ctx, "minecraft:crafting_table"),
                "minecraft:crafting_table"
            )
        );
        assert_eq!(count(&hands, planks), 1);
    }

    #[test]
    fn the_furnace_book_places_one_input() {
        let Some(ctx) = testkit::flat() else { return };
        let iron = id(&ctx, "minecraft:raw_iron");
        let r = ctx
            .recipes
            .book_recipe(
                ctx.recipes
                    .book_id("minecraft:iron_ingot_from_smelting_raw_iron")
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(r.book, 1);
        let mut hands = hands_with(&[(36, iron, 12)]);
        let mut grid = vec![None];
        place(&mut hands, &mut grid, (1, 1), r, false, MAX).unwrap();
        assert_eq!(grid[0], Some(ItemStack::new(iron, 1)));
        place(&mut hands, &mut grid, (1, 1), r, true, MAX).unwrap();
        assert_eq!(grid[0], Some(ItemStack::new(iron, 12)));
    }

    #[test]
    fn limited_crafting_only_makes_known_recipes_and_making_one_teaches_it() {
        let Some(ctx) = testkit::flat() else { return };
        testkit::join(&ctx, ME, 71, [0.5, -60.0, 0.5], 0);
        crate::commands::load_progress(&ctx, ME, &[], &crate::players::BookState::default());
        let planks = id(&ctx, "minecraft:oak_planks");
        let grid = vec![Some(ItemStack::new(planks, 1)), None, None, None];
        let name = ctx.recipes.craft_named(&grid, 2).unwrap().1.to_owned();
        assert!(craft(&ctx, ME, &grid, 2).is_some());
        crate::tick::lock(&ctx.game_rules).set("limited_crafting", crate::rules::Value::Bool(true));
        assert!(!knows_recipe(&ctx, ME, &name));
        assert!(craft(&ctx, ME, &grid, 2).is_none());
        used(&ctx, ME, &grid, 2);
        assert!(knows_recipe(&ctx, ME, &name));
        assert!(craft(&ctx, ME, &grid, 2).is_some());
    }

    #[test]
    fn book_settings_are_kept_per_player_and_book() {
        let Some(ctx) = testkit::flat() else { return };
        testkit::join(&ctx, ME, 71, [0.5, -60.0, 0.5], 0);
        crate::commands::load_progress(&ctx, ME, &[], &crate::players::BookState::default());
        book_settings(&ctx, ME, 2, true, true);
        book_settings(&ctx, ME, 9, true, true);
        let state = book_state(&ctx, ME);
        assert_eq!(
            (state.open, state.filter),
            ([false, false, true, false], [false, false, true, false])
        );
    }
}
