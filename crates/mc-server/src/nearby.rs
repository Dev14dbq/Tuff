//! Item entities and item-holding vehicles by the block cells they are in,
//! so a hopper looking at the space above it (vanilla
//! `HopperBlockEntity.getItemsAtAndAbove`, `getContainerAt`) does not walk
//! every entity of the dimension. Built while hoppers take their turns,
//! when entities stand still; an answer is only trusted while the entity
//! list is the one it was built from, else the caller walks the list.

use mc_entity::Entity;
use mc_tick::fast::FastMap;

type Cell = (i32, i32, i32);

/// The index (see the module docs). Empty until [`Nearby::build`].
#[derive(Debug, Default)]
pub struct Nearby {
    /// The entity list's length when built; `None`: not built.
    len: Option<usize>,
    /// Item entities by the cell of their position: place in the list, id.
    items: FastMap<Cell, Vec<(u32, i32)>>,
    /// Vehicles by every cell their box reaches into.
    vehicles: FastMap<Cell, Vec<(u32, i32)>>,
}

fn cell(p: [f64; 3]) -> Cell {
    (
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    )
}

impl Nearby {
    pub fn build(&mut self, entities: &[Entity]) {
        self.clear();
        for (i, e) in entities.iter().enumerate() {
            if e.removed {
                continue;
            }
            let at = (i as u32, e.id);
            match &e.kind {
                mc_entity::Kind::Item { .. } => self.items.entry(cell(e.pos)).or_default().push(at),
                mc_entity::Kind::Vehicle(_) => {
                    let b = e.aabb();
                    let (lo, hi) = (cell(b.min), cell(b.max));
                    for x in lo.0..=hi.0 {
                        for y in lo.1..=hi.1 {
                            for z in lo.2..=hi.2 {
                                self.vehicles.entry((x, y, z)).or_default().push(at);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.len = Some(entities.len());
    }

    pub fn clear(&mut self) {
        self.len = None;
        self.items.clear();
        self.vehicles.clear();
    }

    /// The item entities that may have their position in the box from
    /// `min` (inclusive) to `max` (exclusive), into `out` (a superset: the
    /// caller checks each, after [`still`]); the length of the list it was
    /// built from. `None` if not built.
    pub fn items(&self, min: [f64; 3], max: [f64; 3], out: &mut Vec<(u32, i32)>) -> Option<usize> {
        let below = |v: f64| (v - 1.0).ceil() as i32;
        let hi = (below(max[0]), below(max[1]), below(max[2]));
        self.gather(&self.items, cell(min), hi, out)
    }

    /// The vehicles whose box may reach into the block `p`.
    pub fn vehicles(&self, p: Cell, out: &mut Vec<(u32, i32)>) -> Option<usize> {
        self.gather(&self.vehicles, p, p, out)
    }

    fn gather(
        &self,
        map: &FastMap<Cell, Vec<(u32, i32)>>,
        lo: Cell,
        hi: Cell,
        out: &mut Vec<(u32, i32)>,
    ) -> Option<usize> {
        let len = self.len?;
        out.clear();
        if map.is_empty() {
            return Some(len);
        }
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                for z in lo.2..=hi.2 {
                    out.extend(map.get(&(x, y, z)).into_iter().flatten());
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(len)
    }
}

/// Whether what [`Nearby::items`] or [`Nearby::vehicles`] found from a list
/// of `len` still holds for `entities`: the same list, everyone found where
/// they were.
pub fn still(len: usize, entities: &[Entity], found: &[(u32, i32)]) -> bool {
    len == entities.len()
        && found
            .iter()
            .all(|&(i, id)| entities.get(i as usize).is_some_and(|e| e.id == id))
}
