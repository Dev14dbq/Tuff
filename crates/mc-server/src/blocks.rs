//! What placing and breaking a block does, before block behaviours exist
//! (ROADMAP Ф3): which state an item places, where it goes and which blocks
//! can be replaced.
//!
//! The rules come from watching vanilla 26.3 (`cargo xtask blocks`) and from
//! block tags in the data pack. They are a first approximation: no neighbour
//! updates (stair shapes, fences), and block shapes are assumed to be full
//! cubes unless the block is known to have no collision.

use mc_data::McData;
use mc_protocol::Position;
pub use mc_tick::BlockRules;

/// The six faces, in Player Action / Use Item On order.
pub const FACES: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];
const FACE_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

pub fn offset(p: Position, face: usize) -> Position {
    let (dx, dy, dz) = FACES[face];
    Position::new(p.x + dx, p.y + dy, p.z + dz)
}

/// Blocks a survival player breaks with one hit (hardness 0, minecraft.wiki
/// "Breaking"): plants, torches, redstone parts, TNT, slime, scaffolding...
pub fn breaks_instantly(data: &McData, rules: &BlockRules, state: u32) -> bool {
    let name = data.blocks.block_name(state).unwrap_or("");
    let plant = !rules.blocks_motion(state)
        && !matches!(
            name,
            "minecraft:cobweb"
                | "minecraft:ladder"
                | "minecraft:vine"
                | "minecraft:snow"
                | "minecraft:lever"
                | "minecraft:water"
                | "minecraft:lava"
                | "minecraft:bubble_column"
        )
        && !name.ends_with("_carpet")
        && !name.ends_with("_rail")
        && name != "minecraft:rail"
        && !name.ends_with("_button")
        && !name.ends_with("_pressure_plate")
        && !name.ends_with("_sign")
        && !name.ends_with("_banner");
    plant
        || matches!(
            name,
            "minecraft:tnt"
                | "minecraft:slime_block"
                | "minecraft:scaffolding"
                | "minecraft:repeater"
                | "minecraft:comparator"
                | "minecraft:flower_pot"
                | "minecraft:decorated_pot"
                | "minecraft:frogspawn"
                | "minecraft:honey_block"
        )
}

/// Everything about one Use Item On that decides the placed state.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub clicked: Position,
    pub face: usize,
    pub cursor: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Sneaking players place chests single.
    pub sneaking: bool,
}

/// Horizontal direction a yaw looks at.
fn look_horizontal(yaw: f32) -> &'static str {
    let quarter = ((yaw / 90.0 + 0.5).floor() as i32).rem_euclid(4);
    ["south", "west", "north", "east"][quarter as usize]
}

fn opposite(direction: &str) -> &'static str {
    match direction {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        _ => "up",
    }
}

/// Main direction of the view, vertical included.
fn look_direction(yaw: f32, pitch: f32) -> &'static str {
    if pitch < -45.0 {
        "up"
    } else if pitch > 45.0 {
        "down"
    } else {
        look_horizontal(yaw)
    }
}

/// The blocks one placement sets: the main block and, for two-block blocks
/// (doors, tall plants, beds), the other part.
#[derive(Debug, PartialEq)]
pub struct Placed {
    pub blocks: Vec<(Position, u32)>,
}

/// Where a block item goes and as which state. `block_at` reads the world.
/// `None`: the placement is refused.
pub fn place(
    data: &McData,
    rules: &BlockRules,
    block: &str,
    p: Placement,
    block_at: &dyn Fn(Position) -> Option<u32>,
) -> Option<Placed> {
    let blocks = &data.blocks;
    let default = blocks.default_state(block)?;
    let clicked_state = block_at(p.clicked)?;

    // A single slab clicked on its open side, or reached from a neighbour,
    // becomes a double slab.
    if blocks.property(default, "type").is_some() && is_slab(blocks, default) {
        let same = |s: u32| blocks.block_name(s) == Some(block);
        let single = |s: u32| matches!(blocks.property(s, "type"), Some("top" | "bottom"));
        if same(clicked_state) && single(clicked_state) {
            let open = match blocks.property(clicked_state, "type") {
                Some("bottom") => p.face == 1,
                _ => p.face == 0,
            };
            if open {
                let double = blocks.with_property(clicked_state, "type", "double")?;
                return Some(Placed {
                    blocks: vec![(p.clicked, double)],
                });
            }
        }
        let target = offset(p.clicked, p.face);
        if let Some(existing) = block_at(target)
            && same(existing)
            && single(existing)
        {
            let double = blocks.with_property(existing, "type", "double")?;
            return Some(Placed {
                blocks: vec![(target, double)],
            });
        }
    }

    let (target, replacing_clicked) = if rules.is_replaceable(clicked_state) {
        (p.clicked, true)
    } else {
        (offset(p.clicked, p.face), false)
    };
    let replaced = block_at(target)?;
    if !rules.is_replaceable(replaced) {
        return None;
    }
    // Clicking a replaceable block acts as clicking the top of the one below.
    let face = if replacing_clicked { 1 } else { p.face };

    // Torches, signs, banners, coral fans and heads go on a wall as their
    // wall block (minecraft.wiki: "Torch", "Sign"...).
    let wall = wall_variant(blocks, block);
    let on_wall = (2..6).contains(&face);
    let default = match &wall {
        Some(w) if on_wall => blocks.default_state(w)?,
        _ => default,
    };
    let mut state = default;
    let set = |state: &mut u32, property: &str, value: &str| {
        if let Some(changed) = blocks.with_property(*state, property, value) {
            *state = changed;
        }
    };
    if blocks.property(state, "axis").is_some() {
        let axis = match face {
            0 | 1 => "y",
            2 | 3 => "z",
            _ => "x",
        };
        set(&mut state, "axis", axis);
    }
    if blocks.property(state, "facing").is_some() {
        let horizontal_only = blocks.with_property(state, "facing", "up").is_none();
        let look = if horizontal_only {
            look_horizontal(p.yaw)
        } else {
            look_direction(p.yaw, p.pitch)
        };
        let facing = if rules.faces_look_direction(state) {
            look
        } else {
            opposite(look)
        };
        set(&mut state, "facing", facing);
    }
    let name = blocks.block_name(default).unwrap_or(block);
    if wall.is_some() && on_wall || matches!(name, "minecraft:ladder" | "minecraft:tripwire_hook") {
        // Facing away from the wall it is on.
        if on_wall {
            set(&mut state, "facing", FACE_NAMES[face]);
        }
    }
    if blocks.property(state, "face").is_some() {
        // Levers and buttons: on a floor, a ceiling or a wall.
        match face {
            0 => set(&mut state, "face", "ceiling"),
            1 => set(&mut state, "face", "floor"),
            _ => {
                set(&mut state, "face", "wall");
                set(&mut state, "facing", FACE_NAMES[face]);
            }
        }
        if face < 2 {
            set(&mut state, "facing", look_horizontal(p.yaw));
        }
    }
    if blocks.property(state, "rotation").is_some() {
        // Signs, banners and heads standing: one of 16 turns towards the
        // player.
        let segment = (((p.yaw + 180.0) / 22.5 + 0.5).floor() as i32).rem_euclid(16);
        set(&mut state, "rotation", &segment.to_string());
    }
    if blocks.property(state, "hanging").is_some() {
        set(
            &mut state,
            "hanging",
            if face == 0 { "true" } else { "false" },
        );
    }
    if (matches!(name, "minecraft:chest" | "minecraft:trapped_chest")
        || name.ends_with("copper_chest"))
        && !p.sneaking
    {
        // Joins a single chest beside it facing the same way: as the left
        // half if that chest is clockwise of its facing, else the right.
        let facing = blocks.property(state, "facing").unwrap_or("north");
        let side = |clockwise: bool| {
            let turns = ["north", "east", "south", "west"];
            let i = turns.iter().position(|d| *d == facing).unwrap_or(0);
            let d = turns[if clockwise { (i + 1) % 4 } else { (i + 3) % 4 }];
            FACE_NAMES.iter().position(|f| *f == d).unwrap_or(2)
        };
        let joins = |clockwise: bool| {
            block_at(offset(target, side(clockwise))).is_some_and(|s| {
                blocks.block_name(s) == Some(name)
                    && blocks.property(s, "type") == Some("single")
                    && blocks.property(s, "facing") == Some(facing)
            })
        };
        if joins(true) {
            set(&mut state, "type", "left");
        } else if joins(false) {
            set(&mut state, "type", "right");
        }
    }
    let upper_half = match face {
        0 => true,
        1 => false,
        _ => p.cursor[1] > 0.5,
    };
    if is_slab(blocks, state) {
        set(
            &mut state,
            "type",
            if upper_half { "top" } else { "bottom" },
        );
    }
    if matches!(blocks.property(state, "half"), Some("top" | "bottom")) {
        set(
            &mut state,
            "half",
            if upper_half { "top" } else { "bottom" },
        );
    }
    if blocks.property(state, "waterlogged").is_some() && replaced == rules.water {
        set(&mut state, "waterlogged", "true");
    }

    let mut placed = vec![(target, state)];
    // Two-block blocks: doors and tall plants (`half` lower/upper), beds (`part`).
    if blocks.property(state, "half") == Some("lower") {
        let above = offset(target, 1);
        if !block_at(above).is_some_and(|s| rules.is_replaceable(s)) {
            return None;
        }
        placed.push((above, blocks.with_property(state, "half", "upper")?));
    } else if blocks.property(state, "part") == Some("foot") {
        let facing = blocks.property(state, "facing")?;
        let face = FACE_NAMES.iter().position(|f| *f == facing)?;
        let head = offset(target, face);
        if !block_at(head).is_some_and(|s| rules.is_replaceable(s)) {
            return None;
        }
        placed.push((head, blocks.with_property(state, "part", "head")?));
    }
    Some(Placed { blocks: placed })
}

/// The wall form of a block, if it has one: `torch` → `wall_torch`,
/// `oak_sign` → `oak_wall_sign`, `oak_hanging_sign` → `oak_wall_hanging_sign`.
fn wall_variant(blocks: &mc_data::Blocks, block: &str) -> Option<String> {
    let short = block.trim_start_matches("minecraft:");
    let candidate = if let Some(wood) = short.strip_suffix("_hanging_sign") {
        format!("{wood}_wall_hanging_sign")
    } else if let Some((head, last)) = short.rsplit_once('_') {
        format!("{head}_wall_{last}")
    } else {
        format!("wall_{short}")
    };
    let full = format!("minecraft:{candidate}");
    blocks.get(&full).is_some().then_some(full)
}

fn is_slab(blocks: &mc_data::Blocks, state: u32) -> bool {
    blocks
        .block_name(state)
        .is_some_and(|n| n.ends_with("_slab"))
        && blocks.property(state, "type").is_some()
}

/// What breaking the block at `position` changes: the block itself (to air,
/// or water if it was waterlogged) and the other part of a two-block block.
pub fn break_block(
    data: &McData,
    rules: &BlockRules,
    position: Position,
    state: u32,
    block_at: &dyn Fn(Position) -> Option<u32>,
) -> Vec<(Position, u32)> {
    let blocks = &data.blocks;
    let left = |s: u32| {
        if blocks.property(s, "waterlogged") == Some("true") {
            rules.water
        } else {
            rules.air
        }
    };
    let mut changes = vec![(position, left(state))];
    let same_block = |other: u32| blocks.block_name(other) == blocks.block_name(state);
    let partner = match (
        blocks.property(state, "half"),
        blocks.property(state, "part"),
    ) {
        (Some("lower"), _) => Some(offset(position, 1)),
        (Some("upper"), _) => Some(offset(position, 0)),
        (_, Some(part)) => blocks
            .property(state, "facing")
            .and_then(|f| FACE_NAMES.iter().position(|n| *n == f))
            .map(|face| {
                if part == "foot" {
                    offset(position, face)
                } else {
                    offset(position, face ^ 1)
                }
            }),
        _ => None,
    };
    if let Some(partner) = partner
        && let Some(other) = block_at(partner)
        && same_block(other)
    {
        changes.push((partner, left(other)));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_to_direction() {
        assert_eq!(look_horizontal(0.0), "south");
        assert_eq!(look_horizontal(90.0), "west");
        assert_eq!(look_horizontal(-90.0), "east");
        assert_eq!(look_horizontal(179.0), "north");
        assert_eq!(look_horizontal(-170.0), "north");
        assert_eq!(look_direction(0.0, 80.0), "down");
    }

    #[test]
    fn torches_signs_and_buttons_go_on_walls() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Ok(data) = McData::load(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let rules = BlockRules::new(&data).unwrap();
        let stone = data.blocks.default_state("minecraft:stone").unwrap();
        let air = data.blocks.default_state("minecraft:air").unwrap();
        let block_at = |p: Position| {
            Some(if p == Position::new(0, 0, 0) {
                stone
            } else {
                air
            })
        };
        let place = |block: &str, face: usize, yaw: f32| {
            let p = Placement {
                clicked: Position::new(0, 0, 0),
                face,
                cursor: [0.5; 3],
                yaw,
                pitch: 0.0,
                sneaking: false,
            };
            let placed = super::place(&data, &rules, block, p, &block_at).unwrap();
            data.blocks.describe(placed.blocks[0].1).unwrap().to_owned()
        };
        assert!(place("minecraft:torch", 3, 0.0).starts_with("minecraft:wall_torch[facing=south"));
        assert_eq!(place("minecraft:torch", 1, 0.0), "minecraft:torch[]");
        assert!(
            place("minecraft:oak_sign", 4, 0.0).starts_with("minecraft:oak_wall_sign[facing=west")
        );
        // Standing, turned to face the player looking north (yaw 180).
        assert!(place("minecraft:oak_sign", 1, 180.0).contains("rotation=0"));
        let button = place("minecraft:stone_button", 5, 0.0);
        assert!(
            button.contains("face=wall") && button.contains("facing=east"),
            "{button}"
        );
        assert!(place("minecraft:lever", 0, 0.0).contains("face=ceiling"));
        assert!(place("minecraft:lantern", 0, 0.0).contains("hanging=true"));
        assert!(place("minecraft:ladder", 2, 0.0).contains("facing=north"));
    }

    #[test]
    fn faces_pair_with_their_opposites() {
        for face in 0..6 {
            let (a, b) = (FACES[face], FACES[face ^ 1]);
            assert_eq!((a.0 + b.0, a.1 + b.1, a.2 + b.2), (0, 0, 0));
            assert_eq!(opposite(FACE_NAMES[face]), FACE_NAMES[face ^ 1]);
        }
    }
}
