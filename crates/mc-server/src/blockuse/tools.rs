//! Items used on blocks (minecraft.wiki, "Axe", "Hoe", "Shovel",
//! "Honeycomb", "Candle", "Sea Pickle"...): axes strip logs and scrape
//! copper, hoes till, shovels make paths and put out campfires, honeycomb
//! waxes copper, and candles, pickles, eggs, petals and snow add to their
//! block.

use super::{Click, centre, set, state_of, use_up, wear};
use crate::interact::Outcome;

/// The state of block `name` with the properties of `state`.
pub(crate) fn renamed(c: &Click, state: u32, name: &str) -> Option<u32> {
    let blocks = &c.ctx.data.blocks;
    let key = blocks.describe(state)?;
    let props: Vec<(&str, &str)> = key
        .find('[')
        .map(|i| &key[i + 1..key.len() - 1])
        .unwrap_or("")
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    let full = format!("minecraft:{}", name.trim_start_matches("minecraft:"));
    let info = blocks.get(&full)?;
    let props: Vec<(&str, &str)> = props
        .into_iter()
        .filter(|(k, _)| info.default_properties.iter().any(|(d, _)| d == k))
        .collect();
    blocks.state_id(&full, &props)
}

const STAGES: [&str; 3] = ["exposed_", "weathered_", "oxidized_"];

/// The copper block one stage less weathered than `name` (without its
/// `minecraft:`), if it is weathered.
fn less_weathered(name: &str) -> Option<String> {
    let (i, rest) = STAGES
        .iter()
        .enumerate()
        .find_map(|(i, s)| name.strip_prefix(s).map(|r| (i, r)))?;
    let previous = if i == 0 {
        if rest == "copper" {
            "copper_block".to_owned()
        } else {
            rest.to_owned()
        }
    } else {
        format!("{}{rest}", STAGES[i - 1])
    };
    Some(previous)
}

fn is_copper(name: &str) -> bool {
    name.contains("copper") || name.ends_with("lightning_rod")
}

/// Swaps the clicked block for `new`, wears the tool and plays `sound`.
fn change(c: &Click, outcome: &mut Outcome, new: u32, sound: Option<&str>) -> bool {
    set(c.ctx, outcome, c.at, new);
    if let Some(sound) = sound {
        c.sound(sound);
    }
    wear(c);
    true
}

fn axe(c: &Click, outcome: &mut Outcome, n: &str) -> bool {
    let strippable = !n.starts_with("stripped_")
        && (n.ends_with("_log")
            || n.ends_with("_wood")
            || n.ends_with("_stem")
            || n.ends_with("_hyphae")
            || n == "bamboo_block");
    if strippable && let Some(new) = renamed(c, c.state, &format!("stripped_{n}")) {
        return change(c, outcome, new, Some("item.axe.strip"));
    }
    if !is_copper(n) {
        return false;
    }
    if let Some(prev) = less_weathered(n)
        && let Some(new) = renamed(c, c.state, &prev)
    {
        c.sound("item.axe.scrape");
        crate::fx::level_event(c.ctx, 3005, centre(c.at), 0);
        return change(c, outcome, new, None);
    }
    if let Some(bare) = n.strip_prefix("waxed_")
        && let Some(new) = renamed(c, c.state, bare)
    {
        c.sound("item.axe.wax_off");
        crate::fx::level_event(c.ctx, 3004, centre(c.at), 0);
        return change(c, outcome, new, None);
    }
    false
}

/// Whether the block above the clicked one is air.
fn open_above(c: &Click) -> bool {
    let above = mc_protocol::Position::new(c.at.x, c.at.y + 1, c.at.z);
    c.ctx
        .world
        .block(above, &c.ctx.data.blocks)
        .is_some_and(|s| c.ctx.shapes.is_air(s))
}

fn hoe(c: &Click, outcome: &mut Outcome, n: &str) -> bool {
    if n == "rooted_dirt" {
        let Some(dirt) = state_of(c.ctx, "minecraft:dirt", &[]) else {
            return false;
        };
        if let Some(item) = c.ctx.items.id("minecraft:hanging_roots") {
            let face = crate::blocks::offset(c.at, c.face);
            outcome.drops.push((
                face,
                crate::loot::Drop {
                    item,
                    count: 1,
                    components: Default::default(),
                },
            ));
        }
        return change(c, outcome, dirt, Some("item.hoe.till"));
    }
    if c.face == 0 || !open_above(c) {
        return false;
    }
    let to = match n {
        "grass_block" | "dirt_path" | "dirt" => "minecraft:farmland",
        "coarse_dirt" => "minecraft:dirt",
        _ => return false,
    };
    let Some(new) = state_of(c.ctx, to, &[]) else {
        return false;
    };
    change(c, outcome, new, Some("item.hoe.till"))
}

fn shovel(c: &Click, outcome: &mut Outcome, n: &str) -> bool {
    if n.ends_with("campfire") && c.prop("lit") == Some("true") {
        crate::fx::level_event(c.ctx, 1009, centre(c.at), 0);
        return change(c, outcome, c.with("lit", "false"), None);
    }
    let flattens = matches!(
        n,
        "grass_block" | "dirt" | "podzol" | "coarse_dirt" | "mycelium" | "rooted_dirt"
    );
    if !flattens || c.face == 0 || !open_above(c) {
        return false;
    }
    let Some(path) = state_of(c.ctx, "minecraft:dirt_path", &[]) else {
        return false;
    };
    change(c, outcome, path, Some("item.shovel.flatten"))
}

/// Another candle, pickle, egg, petal or snow layer on the clicked block.
fn add_one(c: &Click, outcome: &mut Outcome, n: &str) -> bool {
    let held = c.held_name().trim_start_matches("minecraft:");
    if held != n {
        return false;
    }
    let (key, max) = match n {
        "sea_pickle" => ("pickles", 4),
        "turtle_egg" => ("eggs", 4),
        "pink_petals" | "wildflowers" => ("flower_amount", 4),
        "leaf_litter" => ("segment_amount", 4),
        // Snow only stacks when clicked from above.
        "snow" if c.face == 1 => ("layers", 8),
        _ if n.ends_with("candle") => ("candles", 4),
        _ => return false,
    };
    let count = c.int(key);
    if count >= max {
        return false;
    }
    let new = c.with(key, &(count + 1).to_string());
    set(c.ctx, outcome, c.at, new);
    let sound = match n {
        "sea_pickle" => "block.slime_block.place",
        "turtle_egg" => "block.metal.place",
        "snow" => "block.snow.place",
        "pink_petals" | "wildflowers" | "leaf_litter" => "block.pink_petals.place",
        _ => "block.candle.place",
    };
    c.sound(sound);
    use_up(c, 1);
    true
}

pub fn use_on(c: &Click, outcome: &mut Outcome) -> bool {
    let n = c.name.trim_start_matches("minecraft:");
    let held = c.held_name();
    if held.ends_with("_axe") {
        return axe(c, outcome, n);
    }
    if held.ends_with("_hoe") {
        return hoe(c, outcome, n);
    }
    if held.ends_with("_shovel") {
        return shovel(c, outcome, n);
    }
    if held == "minecraft:honeycomb" && is_copper(n) && !n.starts_with("waxed_") {
        let waxed = if n == "copper_block" {
            "waxed_copper_block".to_owned()
        } else {
            format!("waxed_{n}")
        };
        if let Some(new) = renamed(c, c.state, &waxed) {
            set(c.ctx, outcome, c.at, new);
            crate::fx::level_event(c.ctx, 3003, centre(c.at), 0);
            use_up(c, 1);
            return true;
        }
    }
    add_one(c, outcome, n)
}
