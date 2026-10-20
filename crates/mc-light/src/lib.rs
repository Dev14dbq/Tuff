//! Block and sky light.
//!
//! The model, from the public description (minecraft.wiki, "Light"):
//! - light levels go from 0 to 15;
//! - a block emits its light level; light spreading into a neighbour loses
//!   at least 1, more through blocks that dim it (their opacity, observed per
//!   state from vanilla: `cargo xtask observe opacity`);
//! - sky light is 15 above the world and goes straight down through blocks
//!   that do not dim it without losing anything; sideways (and through
//!   dimming blocks) it spreads like block light;
//! - some blocks (slabs, stairs, farmland...) block light by their shape
//!   instead: light does not cross a full face of theirs (the bottom of a
//!   bottom slab, the back of a stair).
//!
//! Changes are incremental: when a block changes, the light it may have
//! carried is taken away in a widening wave, sources found at the edge of
//! the wave are remembered, and light then spreads again from them and from
//! the new block.

use std::collections::VecDeque;

pub mod region;

/// Which light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Sky = 0,
    Block = 1,
}

/// Per block state light facts.
#[derive(Debug, Clone)]
pub struct Tables {
    opacity: Vec<u8>,
    emission: Vec<u8>,
    /// Full faces that stop light, one bit per direction in [`FACES`] order.
    faces: Vec<u8>,
}

/// Face bits: down, up, north, south, west, east.
pub const FACES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

impl Tables {
    pub fn new(opacity: Vec<u8>, emission: Vec<u8>) -> Self {
        Self {
            opacity,
            emission,
            faces: Vec::new(),
        }
    }

    /// With the full faces of shape-occluding states.
    pub fn with_faces(mut self, faces: Vec<u8>) -> Self {
        self.faces = faces;
        self
    }

    fn faces(&self, state: u32) -> u8 {
        self.faces.get(state as usize).copied().unwrap_or(0)
    }

    /// Whether light can go from a cell holding `from` to its neighbour
    /// holding `to` in direction `NEIGHBOURS[d]`.
    fn crosses(&self, from: u32, to: u32, d: usize) -> bool {
        self.faces(from) & (1 << d) == 0 && self.faces(to) & (1 << (d ^ 1)) == 0
    }

    pub fn opacity(&self, state: u32) -> u8 {
        self.opacity.get(state as usize).copied().unwrap_or(15)
    }

    pub fn emission(&self, state: u32) -> u8 {
        self.emission.get(state as usize).copied().unwrap_or(0)
    }
}

/// A block position.
pub type Pos = (i32, i32, i32);

/// What the engine needs from the world.
pub trait LightWorld {
    /// The block state at `p` (`None` outside what is loaded or the world's
    /// height).
    fn state(&self, p: Pos) -> Option<u32>;
    fn light(&self, kind: Kind, p: Pos) -> u8;
    fn set_light(&mut self, kind: Kind, p: Pos, level: u8);
    /// Highest y of the world (exclusive): sky light is 15 at and above it.
    fn top(&self) -> i32;
}

const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

fn add(p: Pos, d: (i32, i32, i32)) -> Pos {
    (p.0 + d.0, p.1 + d.1, p.2 + d.2)
}

/// Light that reaches a neighbour (state `to`) from a cell with `level`
/// (state `from`, `None` above the world), moving in direction
/// `NEIGHBOURS[d]`.
fn passed(tables: &Tables, kind: Kind, level: u8, from: Option<u32>, to: u32, d: usize) -> u8 {
    if from.is_some_and(|from| !tables.crosses(from, to, d)) {
        return 0;
    }
    let opacity = tables.opacity(to);
    if kind == Kind::Sky && level == 15 && d == 0 && opacity == 0 {
        return 15;
    }
    level.saturating_sub(opacity.max(1))
}

/// Spreads light from the queued cells (each already holding its level).
fn spread(world: &mut dyn LightWorld, tables: &Tables, kind: Kind, queue: &mut VecDeque<Pos>) {
    while let Some(p) = queue.pop_front() {
        let level = world.light(kind, p);
        if level <= 1 {
            continue;
        }
        let from = world.state(p);
        for (d, step) in NEIGHBOURS.into_iter().enumerate() {
            let n = add(p, step);
            let Some(to) = world.state(n) else { continue };
            let value = passed(tables, kind, level, from, to, d);
            if value > world.light(kind, n) {
                world.set_light(kind, n, value);
                queue.push_back(n);
            }
        }
    }
}

/// The light a cell should have from its own block and sky alone.
fn own_light(world: &dyn LightWorld, tables: &Tables, kind: Kind, p: Pos, state: u32) -> u8 {
    match kind {
        Kind::Block => tables.emission(state),
        Kind::Sky => {
            if p.1 >= world.top() {
                15
            } else {
                0
            }
        }
    }
}

/// The block at `p` changed (it now is `state`): fix both lights around it.
pub fn block_changed(world: &mut dyn LightWorld, tables: &Tables, p: Pos) {
    for kind in [Kind::Block, Kind::Sky] {
        relight(world, tables, kind, p);
    }
}

fn relight(world: &mut dyn LightWorld, tables: &Tables, kind: Kind, p: Pos) {
    let Some(state) = world.state(p) else { return };
    // Take away what may have come through or from this cell.
    let mut removal: VecDeque<(Pos, u8)> = VecDeque::new();
    let mut again: VecDeque<Pos> = VecDeque::new();
    let old = world.light(kind, p);
    if old > 0 {
        world.set_light(kind, p, 0);
        removal.push_back((p, old));
    }
    while let Some((q, level)) = removal.pop_front() {
        for d in NEIGHBOURS {
            let n = add(q, d);
            let Some(_) = world.state(n) else { continue };
            let have = world.light(kind, n);
            if have == 0 {
                continue;
            }
            // Light that may have come from q: smaller, or sky light carried
            // straight down at full strength.
            let from_q =
                have < level || (kind == Kind::Sky && d == (0, -1, 0) && have == 15 && level == 15);
            if from_q {
                world.set_light(kind, n, 0);
                removal.push_back((n, have));
            } else {
                again.push_back(n);
            }
        }
    }
    // The cell's own light (an emitting block).
    let own = own_light(world, tables, kind, p, state);
    if own > world.light(kind, p) {
        world.set_light(kind, p, own);
        again.push_back(p);
    }
    // Light flows back in from the neighbours, including into p.
    for d in NEIGHBOURS {
        let n = add(p, d);
        if world.state(n).is_some() && world.light(kind, n) > 0 {
            again.push_back(n);
        }
    }
    // Sky light from above the world falls into the column.
    if kind == Kind::Sky {
        let above = (p.0, world.top(), p.2);
        again.push_back(above);
    }
    spread(world, tables, kind, &mut again);
}

/// Lights a column region from scratch: `min..max` blocks in x and z,
/// `bottom..top` in y. Sky light falls straight down from the top, block
/// light starts at emitting blocks, then both spread (also into what is
/// around the region, and from it).
pub fn light_region(
    world: &mut dyn LightWorld,
    tables: &Tables,
    min: (i32, i32),
    max: (i32, i32),
    bottom: i32,
) {
    let top = world.top();
    let mut sky = VecDeque::new();
    let mut block = VecDeque::new();
    for x in min.0..max.0 {
        for z in min.1..max.1 {
            let mut level = 15u8;
            let mut above: Option<u32> = None;
            for y in (bottom..top).rev() {
                let p = (x, y, z);
                let Some(state) = world.state(p) else {
                    continue;
                };
                let open = above.is_none_or(|a| tables.crosses(a, state, 0));
                if !(level == 15 && open && tables.opacity(state) == 0) {
                    level = 0;
                }
                above = Some(state);
                world.set_light(Kind::Sky, p, level);
                if level == 15 {
                    sky.push_back(p);
                }
                let emission = tables.emission(state);
                world.set_light(Kind::Block, p, emission);
                if emission > 0 {
                    block.push_back(p);
                }
            }
        }
    }
    // Light from around the region comes in too.
    for x in (min.0 - 1)..=max.0 {
        for z in (min.1 - 1)..=max.1 {
            let inside = x >= min.0 && x < max.0 && z >= min.1 && z < max.1;
            if inside {
                continue;
            }
            for y in bottom..top {
                let p = (x, y, z);
                if world.state(p).is_none() {
                    continue;
                }
                if world.light(Kind::Sky, p) > 1 {
                    sky.push_back(p);
                }
                if world.light(Kind::Block, p) > 1 {
                    block.push_back(p);
                }
            }
        }
    }
    spread(world, tables, Kind::Sky, &mut sky);
    spread(world, tables, Kind::Block, &mut block);
}

/// Light a block state gives off, from the public table (minecraft.wiki,
/// "Light", light-emitting blocks). `property` reads the state's properties.
pub fn emission(name: &str, property: &dyn Fn(&str) -> Option<String>) -> u8 {
    let name = name.trim_start_matches("minecraft:");
    let is = |p: &str| property(p).as_deref() == Some("true");
    let number = |p: &str| property(p).and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
    let lit = is("lit");
    let plain = name.trim_start_matches("waxed_");
    match plain {
        "beacon"
        | "conduit"
        | "end_gateway"
        | "end_portal"
        | "fire"
        | "ochre_froglight"
        | "verdant_froglight"
        | "pearlescent_froglight"
        | "glowstone"
        | "jack_o_lantern"
        | "lantern"
        | "lava"
        | "lava_cauldron"
        | "sea_lantern"
        | "shroomlight" => 15,
        n if n.ends_with("copper_lantern") => 15,
        "campfire" if lit => 15,
        "redstone_lamp" if lit => 15,
        "copper_bulb" if lit => 15,
        "exposed_copper_bulb" if lit => 12,
        "weathered_copper_bulb" if lit => 8,
        "oxidized_copper_bulb" if lit => 4,
        "respawn_anchor" => match number("charges") {
            0 => 0,
            c => (c * 4 - 1).min(15),
        },
        "sea_pickle" if is("waterlogged") => 3 + 3 * number("pickles"),
        "cave_vines" | "cave_vines_plant" if is("berries") => 14,
        "torch" | "wall_torch" | "end_rod" => 14,
        n if n.starts_with("copper_") && n.ends_with("torch") => 14,
        "furnace" | "blast_furnace" | "smoker" if lit => 13,
        "vault" => match property("vault_state").as_deref() {
            Some("active") | Some("unlocking") | Some("ejecting") => 12,
            _ => 6,
        },
        "crying_obsidian" | "soul_fire" | "soul_lantern" | "soul_torch" | "soul_wall_torch" => 10,
        "soul_campfire" if lit => 10,
        "nether_portal" => 11,
        "redstone_ore" | "deepslate_redstone_ore" if lit => 9,
        "trial_spawner" => match property("trial_spawner_state").as_deref() {
            Some("inactive") | None => 0,
            Some("waiting_for_players") | Some("cooldown") => 4,
            Some(_) => 8,
        },
        "enchanting_table" | "ender_chest" | "glow_lichen" => 7,
        "redstone_torch" | "redstone_wall_torch" if lit => 7,
        "sculk_catalyst" => 6,
        "amethyst_cluster" => 5,
        "large_amethyst_bud" => 4,
        "magma_block" => 3,
        "candle_cake" if lit => 3,
        n if n.ends_with("candle_cake") && lit => 3,
        n if (n == "candle" || n.ends_with("_candle")) && lit => 3 * number("candles"),
        "medium_amethyst_bud" | "firefly_bush" => 2,
        "brewing_stand"
        | "brown_mushroom"
        | "calibrated_sculk_sensor"
        | "dragon_egg"
        | "end_portal_frame"
        | "sculk_sensor"
        | "small_amethyst_bud" => 1,
        "light" => number("level"),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A small world: 0 air, 1 stone (opaque), 2 glowstone (emits 15), 3
    /// leaves (dims 1); 16×16 columns from y 0 to 32.
    struct Test {
        blocks: HashMap<Pos, u32>,
        light: [HashMap<Pos, u8>; 2],
        /// Loaded 16×16 chunks; all when empty.
        loaded: Vec<(i32, i32)>,
    }

    impl LightWorld for Test {
        fn state(&self, p: Pos) -> Option<u32> {
            let loaded = self.loaded.is_empty() || self.loaded.contains(&(p.0 >> 4, p.2 >> 4));
            (loaded && (0..48).contains(&p.0) && (0..48).contains(&p.2) && (0..32).contains(&p.1))
                .then(|| self.blocks.get(&p).copied().unwrap_or(0))
        }
        fn light(&self, kind: Kind, p: Pos) -> u8 {
            if kind == Kind::Sky && p.1 >= 32 {
                return 15;
            }
            self.light[kind as usize].get(&p).copied().unwrap_or(0)
        }
        fn set_light(&mut self, kind: Kind, p: Pos, level: u8) {
            self.light[kind as usize].insert(p, level);
        }
        fn top(&self) -> i32 {
            32
        }
    }

    fn tables() -> Tables {
        Tables::new(vec![0, 15, 15, 1], vec![0, 0, 15, 0])
    }

    fn world() -> Test {
        let mut t = Test {
            blocks: HashMap::new(),
            light: [HashMap::new(), HashMap::new()],
            loaded: Vec::new(),
        };
        for x in 0..48 {
            for z in 0..48 {
                t.blocks.insert((x, 0, z), 1);
            }
        }
        light_region(&mut t, &tables(), (0, 0), (48, 48), 0);
        t
    }

    #[test]
    fn open_sky_is_full_and_a_roof_shades() {
        let tables = tables();
        let mut t = world();
        assert_eq!(t.light(Kind::Sky, (5, 1, 5)), 15);
        assert_eq!(t.light(Kind::Sky, (5, 0, 5)), 0, "inside stone");
        // A 5×5 roof at y 10: right below its middle the sky comes in sideways.
        for x in 3..8 {
            for z in 3..8 {
                t.blocks.insert((x, 10, z), 1);
                block_changed(&mut t, &tables, (x, 10, z));
            }
        }
        assert_eq!(
            t.light(Kind::Sky, (5, 9, 5)),
            12,
            "three steps from the edge"
        );
        assert_eq!(t.light(Kind::Sky, (5, 11, 5)), 15);
        // Taking it away lets the sky back in.
        for x in 3..8 {
            for z in 3..8 {
                t.blocks.insert((x, 10, z), 0);
                block_changed(&mut t, &tables, (x, 10, z));
            }
        }
        assert_eq!(t.light(Kind::Sky, (5, 9, 5)), 15);
    }

    #[test]
    fn a_light_source_lights_and_unlights() {
        let tables = tables();
        let mut t = world();
        t.blocks.insert((20, 5, 20), 2);
        block_changed(&mut t, &tables, (20, 5, 20));
        assert_eq!(t.light(Kind::Block, (20, 5, 20)), 15);
        assert_eq!(t.light(Kind::Block, (23, 5, 20)), 12);
        assert_eq!(t.light(Kind::Block, (20, 5, 34)), 1);
        assert_eq!(t.light(Kind::Block, (20, 5, 35)), 0);
        // Leaves dim by 1 more... still at least 1 per step.
        t.blocks.insert((20, 5, 20), 0);
        block_changed(&mut t, &tables, (20, 5, 20));
        assert_eq!(t.light(Kind::Block, (23, 5, 20)), 0, "gone with the source");
        assert_eq!(t.light(Kind::Block, (20, 5, 34)), 0);
    }

    #[test]
    fn two_sources_one_removed() {
        let tables = tables();
        let mut t = world();
        for p in [(10, 5, 10), (16, 5, 10)] {
            t.blocks.insert(p, 2);
            block_changed(&mut t, &tables, p);
        }
        t.blocks.insert((10, 5, 10), 0);
        block_changed(&mut t, &tables, (10, 5, 10));
        assert_eq!(t.light(Kind::Block, (13, 5, 10)), 12, "from the one left");
        assert_eq!(t.light(Kind::Block, (10, 5, 10)), 9);
    }

    /// Random changes, each followed by the incremental fix, end where
    /// lighting from scratch does.
    #[test]
    fn incremental_matches_from_scratch() {
        let tables = tables();
        let mut seed: u64 = 0x1234_5678;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..20 {
            let mut t = Test {
                blocks: HashMap::new(),
                light: [HashMap::new(), HashMap::new()],
                loaded: Vec::new(),
            };
            // Hilly stone with caves and some leaves (opacity 1).
            for x in 0..24 {
                for z in 0..24 {
                    let h = 8 + (x * 7 + z * 3) % 9;
                    for y in 0..h {
                        let b = if next(10) == 0 {
                            0
                        } else if next(15) == 0 {
                            3
                        } else {
                            1
                        };
                        t.blocks.insert((x, y, z), b);
                    }
                }
            }
            light_region(&mut t, &tables, (0, 0), (24, 24), 0);
            for _ in 0..60 {
                let p = (next(24) as i32, next(24) as i32, next(24) as i32);
                let b = [0, 1, 2, 3][next(4) as usize];
                t.blocks.insert(p, b);
                block_changed(&mut t, &tables, p);
            }
            let mut fresh = Test {
                blocks: t.blocks.clone(),
                light: [HashMap::new(), HashMap::new()],
                loaded: Vec::new(),
            };
            light_region(&mut fresh, &tables, (0, 0), (24, 24), 0);
            for x in 0..24 {
                for z in 0..24 {
                    for y in 0..32 {
                        for kind in [Kind::Sky, Kind::Block] {
                            let (a, b) = (t.light(kind, (x, y, z)), fresh.light(kind, (x, y, z)));
                            assert_eq!(a, b, "round {round}: {kind:?} at {x} {y} {z}");
                        }
                    }
                }
            }
        }
    }

    /// Chunks lit one by one as they load, in any order, end where lighting
    /// them all at once does.
    #[test]
    fn chunks_lit_in_turn_match_all_at_once() {
        let tables = tables();
        let mut seed: u64 = 0x9e37_79b9;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..10 {
            let mut blocks = HashMap::new();
            for x in 0..48 {
                for z in 0..48 {
                    let h = 6 + (x * 5 + z * 11) % 13;
                    for y in 0..h {
                        let b = if next(6) == 0 {
                            0
                        } else if next(20) == 0 {
                            2
                        } else if next(9) == 0 {
                            3
                        } else {
                            1
                        };
                        blocks.insert((x, y, z), b);
                    }
                    // Overhangs.
                    if next(5) == 0 {
                        blocks.insert((x, h + 3, z), 1);
                    }
                }
            }
            let mut order: Vec<(i32, i32)> =
                (0..3).flat_map(|x| (0..3).map(move |z| (x, z))).collect();
            for i in (1..order.len()).rev() {
                order.swap(i, next(i as u64 + 1) as usize);
            }
            let mut t = Test {
                blocks: blocks.clone(),
                light: [HashMap::new(), HashMap::new()],
                loaded: Vec::new(),
            };
            t.loaded.push((99, 99));
            for &(cx, cz) in &order {
                t.loaded.push((cx, cz));
                light_region(
                    &mut t,
                    &tables,
                    (cx * 16, cz * 16),
                    (cx * 16 + 16, cz * 16 + 16),
                    0,
                );
            }
            let mut fresh = Test {
                blocks,
                light: [HashMap::new(), HashMap::new()],
                loaded: Vec::new(),
            };
            light_region(&mut fresh, &tables, (0, 0), (48, 48), 0);
            for x in 0..48 {
                for z in 0..48 {
                    for y in 0..32 {
                        for kind in [Kind::Sky, Kind::Block] {
                            let (a, b) = (t.light(kind, (x, y, z)), fresh.light(kind, (x, y, z)));
                            assert_eq!(
                                a, b,
                                "round {round} order {order:?}: {kind:?} at {x} {y} {z}"
                            );
                        }
                    }
                }
            }
        }
    }
}
