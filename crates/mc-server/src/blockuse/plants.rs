//! Plants a hand picks from, and bone meal from the player's hand
//! (minecraft.wiki, "Sweet Berries", "Glow Berries", "Bone Meal").

use super::{Click, set, use_up};
use crate::interact::Outcome;
use crate::level::WorldLevel;

/// Berries drop at the click.
fn berries(c: &Click, outcome: &mut Outcome, item: &str, count: i32) {
    if let Some(item) = c.ctx.items.id(item) {
        outcome.drops.push((
            c.at,
            crate::loot::Drop {
                item,
                count,
                components: Default::default(),
            },
        ));
    }
}

pub fn use_on(c: &Click, outcome: &mut Outcome) -> bool {
    match c.name {
        "minecraft:sweet_berry_bush" => {
            let age = c.int("age");
            // Bone meal on an unripe bush grows it instead.
            if c.held_name() == "minecraft:bone_meal" && age < 3 {
                return false;
            }
            if age <= 1 {
                return false;
            }
            let r = (crate::util::random_u64() % 2) as i32;
            berries(
                c,
                outcome,
                "minecraft:sweet_berries",
                1 + r + i32::from(age == 3),
            );
            c.sound("block.sweet_berry_bush.pick_berries");
            set(c.ctx, outcome, c.at, c.with("age", "1"));
            true
        }
        "minecraft:cave_vines" | "minecraft:cave_vines_plant" => {
            if c.prop("berries") != Some("true") {
                return false;
            }
            berries(c, outcome, "minecraft:glow_berries", 1);
            c.sound("block.cave_vines.pick_berries");
            set(c.ctx, outcome, c.at, c.with("berries", "false"));
            true
        }
        _ => false,
    }
}

/// Bone meal from the hand: the plant grows (and green sparkles show
/// where).
pub fn item_on(c: &Click, outcome: &mut Outcome) -> bool {
    if c.held_name() != "minecraft:bone_meal" {
        return false;
    }
    let mut level = WorldLevel::new(c.ctx);
    if !crate::growth::bone_meal(&mut level, c.at) {
        // Else on a solid face under water, the water plants.
        let face = mc_tick::Dir::ALL.get(c.face).copied();
        let state = mc_tick::Level::block(&level, crate::level::to_pos(c.at));
        let solid = face.is_some_and(|f| match f {
            mc_tick::Dir::Up => c.ctx.shapes.support(state) & mc_tick::support::TOP_FULL != 0,
            mc_tick::Dir::Down => c.ctx.shapes.support(state) & mc_tick::support::BOTTOM_FULL != 0,
            _ => c.ctx.shapes.face_attaches(state, f.opposite()),
        });
        if !solid || !crate::growth::bone_meal_water(&mut level, c.at, face) {
            return false;
        }
    }
    let changes = level.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome
        .events
        .push(crate::context::Event::BlocksChanged { by: 0, changes });
    crate::fx::level_event(c.ctx, 1505, super::centre(c.at), 15);
    use_up(c, 1);
    true
}
