//! Path finding over the block grid (A*).
//!
//! A path is a list of block cells a mob stands in, feet first. A cell is
//! standable when the mob's body fits in it (every cell up to its height is
//! free of collision, lava and hazards) and the block below holds it up (or
//! it wades through water, if allowed). From a cell a mob steps to the four
//! sides and four diagonals at the same height, up one block (a jump) or
//! down a few blocks (a fall that does not hurt). This follows the public
//! description of vanilla's ground navigation (minecraft.wiki, "Pathfinding").

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::physics::Collide;

/// A block cell.
pub type Cell = [i32; 3];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liquid {
    None,
    Water,
    Lava,
}

/// What the ground under a sheep offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Graze {
    None,
    /// Short grass in the cell.
    Plant,
    /// A grass block under it.
    Block,
}

/// A door in a cell (either half).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Door {
    /// Opened by hand (every door but iron): zombies break these,
    /// villagers open them.
    pub by_hand: bool,
    pub open: bool,
}

/// Something a cat sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perch {
    None,
    /// A chest nobody has open.
    Chest,
    /// A lit furnace.
    Furnace,
    /// Either half of a bed.
    Bed,
}

/// What mobs need to know about the world around them.
pub trait Terrain: Collide {
    fn liquid(&self, x: i32, y: i32, z: i32) -> Liquid;
    /// Fire, cactus, magma and the like: never walked into.
    fn hazard(&self, x: i32, y: i32, z: i32) -> bool;
    /// Sky light (15 under open sky).
    fn sky_light(&self, x: i32, y: i32, z: i32) -> u8;
    /// Block or sky light, whichever is brighter.
    fn light(&self, x: i32, y: i32, z: i32) -> u8;
    /// Whether the block can be climbed (ladders, vines).
    fn climbable(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
    /// What there is to graze at a cell.
    fn grazing(&self, _x: i32, _y: i32, _z: i32) -> Graze {
        Graze::None
    }
    /// The door in a cell, if any.
    fn door(&self, _x: i32, _y: i32, _z: i32) -> Option<Door> {
        None
    }
    /// Whether a mob with its eyes in this block suffocates (a full cube,
    /// not glass or leaves).
    fn smothers(&self, x: i32, y: i32, z: i32) -> bool {
        self.span(x, y, z) == Some((0.0, 1.0))
    }
    /// What a cat could sit on here.
    fn perch(&self, _x: i32, _y: i32, _z: i32) -> Perch {
        Perch::None
    }
    /// Whether an enderman may pick the block up (`#enderman_holdable`).
    fn holdable(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// What a walker is like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathParams {
    /// Blocks of headroom its body needs (1 for a chicken, 2 for a zombie).
    pub height: i32,
    /// Blocks it may drop in one step.
    pub max_fall: i32,
    /// Cells looked at before giving up.
    pub max_nodes: usize,
    /// Extra cost of wading (`f32::INFINITY` forbids water).
    pub water_cost: f32,
    /// How close the end must come: horizontal squared distance in cells.
    pub reach_sq: i32,
    /// Walks through closed doors opened by hand (villagers open them,
    /// zombies that break doors bash them).
    pub doors: bool,
}

impl PathParams {
    pub fn walker(height: i32) -> Self {
        Self {
            height,
            max_fall: 3,
            max_nodes: 600,
            water_cost: 8.0,
            reach_sq: 0,
            doors: false,
        }
    }
}

/// Whether the block gets in a walker's way (thin floors such as carpets do
/// not: they are stepped over).
pub fn blocks(w: &dyn Terrain, x: i32, y: i32, z: i32) -> bool {
    w.span(x, y, z).is_some_and(|(_, top)| top > 0.25)
}

/// Fences and walls: taller than a block, so never stepped onto.
pub fn tall(w: &dyn Terrain, x: i32, y: i32, z: i32) -> bool {
    w.span(x, y, z).is_some_and(|(_, top)| top > 1.0)
}

/// Height of the floor a walker in cell `c` stands on.
pub fn floor(w: &dyn Terrain, c: Cell) -> f64 {
    match w.span(c[0], c[1] - 1, c[2]) {
        Some((_, top)) if top > 0.25 => f64::from(c[1] - 1) + top,
        _ => f64::from(c[1]),
    }
}

fn clear(w: &dyn Terrain, c: Cell, p: &PathParams) -> bool {
    !tall(w, c[0], c[1] - 1, c[2])
        && (0..p.height).all(|dy| {
            let (x, y, z) = (c[0], c[1] + dy, c[2]);
            (!blocks(w, x, y, z) || p.doors && w.door(x, y, z).is_some_and(|d| d.by_hand))
                && w.liquid(x, y, z) != Liquid::Lava
                && !w.hazard(x, y, z)
        })
}

/// Whether a mob can stand in `c`.
pub fn standable(w: &dyn Terrain, c: Cell, p: &PathParams) -> bool {
    if !clear(w, c, p) {
        return false;
    }
    let below = [c[0], c[1] - 1, c[2]];
    if w.hazard(below[0], below[1], below[2]) {
        return false;
    }
    let wet = w.liquid(c[0], c[1], c[2]) == Liquid::Water;
    if wet && p.water_cost.is_infinite() {
        return false;
    }
    blocks(w, below[0], below[1], below[2])
        || (wet && w.liquid(below[0], below[1], below[2]) != Liquid::Lava)
}

fn step_cost(w: &dyn Terrain, to: Cell, p: &PathParams, base: f32) -> f32 {
    if w.liquid(to[0], to[1], to[2]) == Liquid::Water {
        base + p.water_cost
    } else {
        base
    }
}

/// The cells a mob can step to from `from`, with the cost of each step.
fn neighbors(w: &dyn Terrain, from: Cell, p: &PathParams, out: &mut Vec<(Cell, f32)>) {
    out.clear();
    const SIDES: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    const DIAGONALS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
    let [x, y, z] = from;
    // Room above to jump: one more block of headroom than the body needs.
    let can_jump = !blocks(w, x, y + p.height, z);
    for (dx, dz) in SIDES {
        let level = [x + dx, y, z + dz];
        if standable(w, level, p) {
            out.push((level, step_cost(w, level, p, 1.0)));
            continue;
        }
        if clear(w, level, p) {
            // Nothing to stand on: a drop, if it lands soon enough.
            for fall in 1..=p.max_fall {
                let down = [x + dx, y - fall, z + dz];
                if blocks(w, down[0], down[1] - 1, down[2])
                    || w.liquid(down[0], down[1], down[2]) == Liquid::Water
                {
                    if standable(w, down, p) {
                        out.push((down, step_cost(w, down, p, 1.0 + 0.2 * fall as f32)));
                    }
                    break;
                }
                if !clear(w, down, p) {
                    break;
                }
            }
        } else if can_jump {
            let up = [x + dx, y + 1, z + dz];
            // A one-block step: the block in front is the only obstacle.
            if standable(w, up, p) && !blocks(w, x + dx, y + p.height, z + dz) {
                out.push((up, step_cost(w, up, p, 2.0)));
            }
        }
    }
    for (dx, dz) in DIAGONALS {
        let to = [x + dx, y, z + dz];
        if standable(w, to, p) && clear(w, [x + dx, y, z], p) && clear(w, [x, y, z + dz], p) {
            out.push((to, step_cost(w, to, p, 1.414)));
        }
    }
}

fn heuristic(a: Cell, b: Cell) -> f32 {
    let dx = (a[0] - b[0]).abs() as f32;
    let dz = (a[2] - b[2]).abs() as f32;
    let dy = (a[1] - b[1]).abs() as f32;
    let (lo, hi) = if dx < dz { (dx, dz) } else { (dz, dx) };
    hi + 0.414 * lo + dy
}

fn key(cost: f32) -> u32 {
    (cost * 64.0).min(u32::MAX as f32) as u32
}

fn at_goal(c: Cell, goal: Cell, p: &PathParams) -> bool {
    let dx = c[0] - goal[0];
    let dz = c[2] - goal[2];
    dx * dx + dz * dz <= p.reach_sq && (c[1] - goal[1]).abs() <= 1
}

/// Finds a path from `start` towards `goal`. The path excludes `start`.
/// When the goal cannot be reached within `max_nodes` cells, the path leads
/// to the explored cell closest to it (as vanilla's does); `None` when no
/// step gets any closer.
pub fn find_path(w: &dyn Terrain, start: Cell, goal: Cell, p: &PathParams) -> Option<Vec<Cell>> {
    if at_goal(start, goal, p) {
        return Some(Vec::new());
    }
    let mut open = BinaryHeap::new();
    // cell -> (cost so far, came from)
    let mut seen: HashMap<Cell, (f32, Cell)> = HashMap::new();
    seen.insert(start, (0.0, start));
    open.push(Reverse((key(heuristic(start, goal)), start)));
    let mut best = (heuristic(start, goal), start);
    let mut steps = Vec::new();
    let mut expanded = 0;
    let mut found = None;
    while let Some(Reverse((_, cell))) = open.pop() {
        if at_goal(cell, goal, p) {
            found = Some(cell);
            break;
        }
        expanded += 1;
        if expanded > p.max_nodes {
            break;
        }
        let cost = seen[&cell].0;
        neighbors(w, cell, p, &mut steps);
        for &(next, step) in &steps {
            let g = cost + step;
            if seen.get(&next).is_some_and(|&(old, _)| old <= g) {
                continue;
            }
            seen.insert(next, (g, cell));
            let h = heuristic(next, goal);
            if h < best.0 {
                best = (h, next);
            }
            open.push(Reverse((key(g + h), next)));
        }
    }
    let end = match found {
        Some(c) => c,
        None if best.1 != start => best.1,
        None => return None,
    };
    let mut path = vec![end];
    let mut at = end;
    while let Some(&(_, from)) = seen.get(&at) {
        if from == start {
            break;
        }
        path.push(from);
        at = from;
    }
    path.reverse();
    Some(path)
}

/// A mob following a path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nav {
    pub path: Vec<Cell>,
    pub index: usize,
    /// Where the path was made to lead.
    pub goal: Option<Cell>,
    /// Ticks until a new path may be looked for.
    pub cooldown: u32,
    /// Where the mob was when progress was last checked, and when.
    checked: ([f64; 3], u32),
    /// Times in a row the mob did not get anywhere.
    pub stuck: u32,
}

impl Nav {
    pub fn is_done(&self) -> bool {
        self.index >= self.path.len()
    }

    pub fn stop(&mut self) {
        self.path.clear();
        self.index = 0;
        self.goal = None;
    }

    /// Leads the mob towards `goal`, finding a new path if the goal moved
    /// away from where the current one ends or there is none.
    pub fn go_to(&mut self, w: &dyn Terrain, from: Cell, goal: Cell, p: &PathParams) {
        let moved = self.goal.is_none_or(|g| {
            (g[0] - goal[0]).abs() + (g[1] - goal[1]).abs() + (g[2] - goal[2]).abs() > 2
        });
        if (moved || self.is_done()) && self.cooldown == 0 {
            self.path = find_path(w, from, goal, p).unwrap_or_default();
            self.index = 0;
            self.goal = Some(goal);
            self.cooldown = 10;
        }
    }

    /// The block cell to head for next, dropping those already reached.
    pub fn waypoint(&mut self, pos: [f64; 3], reached: f64) -> Option<Cell> {
        while let Some(&c) = self.path.get(self.index) {
            let dx = f64::from(c[0]) + 0.5 - pos[0];
            let dz = f64::from(c[2]) + 0.5 - pos[2];
            let dy = f64::from(c[1]) - pos[1];
            if dx * dx + dz * dz < reached * reached && dy.abs() < 1.0 {
                self.index += 1;
            } else {
                return Some(c);
            }
        }
        None
    }

    /// Counts ticks and notes when the mob has not moved for a second.
    pub fn tick(&mut self, pos: [f64; 3], now: u32) {
        self.cooldown = self.cooldown.saturating_sub(1);
        if self.is_done() {
            self.stuck = 0;
            self.checked = (pos, now);
            return;
        }
        if now.wrapping_sub(self.checked.1) >= 20 {
            let (dx, dz) = (pos[0] - self.checked.0[0], pos[2] - self.checked.0[2]);
            if dx * dx + dz * dz < 0.09 {
                self.stuck += 1;
                self.path.clear();
                self.index = 0;
                self.goal = None;
                self.cooldown = 0;
            } else {
                self.stuck = 0;
            }
            self.checked = (pos, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat floor at y = -1 with extra solid cells.
    struct Grid {
        walls: Vec<Cell>,
        water: Vec<Cell>,
    }

    impl Collide for Grid {
        fn solid(&self, x: i32, y: i32, z: i32) -> bool {
            y < 0 || self.walls.contains(&[x, y, z])
        }
    }

    impl Terrain for Grid {
        fn liquid(&self, x: i32, y: i32, z: i32) -> Liquid {
            if self.water.contains(&[x, y, z]) {
                Liquid::Water
            } else {
                Liquid::None
            }
        }
        fn hazard(&self, _: i32, _: i32, _: i32) -> bool {
            false
        }
        fn sky_light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
        fn light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
    }

    fn open() -> Grid {
        Grid {
            walls: Vec::new(),
            water: Vec::new(),
        }
    }

    #[test]
    fn walks_straight_on_open_ground() {
        let path = find_path(&open(), [0, 0, 0], [5, 0, 0], &PathParams::walker(2)).unwrap();
        assert_eq!(path.last(), Some(&[5, 0, 0]));
        assert_eq!(path.len(), 5);
    }

    #[test]
    fn goes_around_a_wall() {
        let mut g = open();
        for z in -3..=3 {
            for y in 0..3 {
                g.walls.push([2, y, z]);
            }
        }
        let path = find_path(&g, [0, 0, 0], [4, 0, 0], &PathParams::walker(2)).unwrap();
        assert_eq!(path.last(), Some(&[4, 0, 0]));
        assert!(path.iter().all(|c| !g.walls.contains(c)));
        assert!(path.iter().any(|c| c[2].abs() >= 4), "{path:?}");
    }

    #[test]
    fn jumps_one_block_but_not_two() {
        let mut g = open();
        g.walls.push([1, 0, 0]);
        // Walls either side so the way over is the only way.
        for z in (-8..=8).filter(|&z| z != 0) {
            g.walls.extend([[1, 0, z], [1, 1, z], [1, 2, z]]);
        }
        let p = PathParams::walker(2);
        let path = find_path(&g, [0, 0, 0], [2, 1, 0], &p);
        // Stepping onto the block and down on its far side.
        assert!(path.is_some_and(|p| p.contains(&[1, 1, 0])));
        g.walls.push([1, 1, 0]);
        let short = PathParams {
            max_nodes: 100,
            ..p
        };
        let path = find_path(&g, [0, 0, 0], [2, 0, 0], &short);
        // No way across within reach: it never gets to the far side.
        assert!(path.is_none_or(|p| p.iter().all(|c| c[0] <= 0)));
    }

    #[test]
    fn avoids_water_when_it_can() {
        let mut g = open();
        for x in 1..=2 {
            for z in -1..=1 {
                g.water.push([x, 0, z]);
            }
        }
        let path = find_path(&g, [0, 0, 0], [3, 0, 0], &PathParams::walker(2)).unwrap();
        assert!(path.iter().all(|c| !g.water.contains(c)), "{path:?}");
        let mut forbid = PathParams::walker(2);
        forbid.water_cost = f32::INFINITY;
        assert!(find_path(&g, [0, 0, 0], [3, 0, 0], &forbid).is_some());
    }

    #[test]
    fn partial_path_when_goal_is_walled_in() {
        let mut g = open();
        for x in 4..=6 {
            for z in -1..=1 {
                for y in 0..3 {
                    if (x, z) != (5, 0) {
                        g.walls.push([x, y, z]);
                    }
                }
            }
        }
        g.walls.push([5, 0, 0]);
        g.walls.push([5, 1, 0]);
        g.walls.push([5, 2, 0]);
        let path = find_path(&g, [0, 0, 0], [5, 0, 0], &PathParams::walker(2)).unwrap();
        assert!(!path.is_empty());
        assert_ne!(path.last(), Some(&[5, 0, 0]));
    }

    #[test]
    fn nav_follows_and_finishes() {
        let g = open();
        let p = PathParams::walker(2);
        let mut nav = Nav::default();
        nav.go_to(&g, [0, 0, 0], [3, 0, 0], &p);
        assert_eq!(nav.path.len(), 3);
        assert_eq!(nav.waypoint([0.5, 0.0, 0.5], 0.4), Some([1, 0, 0]));
        assert_eq!(nav.waypoint([1.5, 0.0, 0.5], 0.4), Some([2, 0, 0]));
        assert_eq!(nav.waypoint([2.5, 0.0, 0.5], 0.4), Some([3, 0, 0]));
        assert_eq!(nav.waypoint([3.5, 0.0, 0.5], 0.4), None);
        assert!(nav.is_done());
    }
}
