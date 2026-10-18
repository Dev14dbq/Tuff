//! Structures with their own piece types (minecraft.wiki: "Desert
//! Pyramid", "Jungle Pyramid", "Swamp Hut", "Igloo", "Shipwreck", "Ocean
//! Ruins", "Ruined Portal", "Buried Treasure", "Mineshaft", "Stronghold",
//! "Ocean Monument", "Woodland Mansion").

use serde_json::Value;

use super::template::BBox;
use super::{Ctx2, Ground, Start, Structures};
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{P, Region};

/// A piece of one of these structures.
#[derive(Debug, Clone)]
pub struct Piece {
    pub bbox: BBox,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Nothing,
}

/// The pieces of a structure of type `ty` starting in chunk `x, z`, and
/// where its biome is checked.
#[allow(clippy::too_many_arguments)]
pub fn generate(
    _s: &Structures,
    _ty: &str,
    _v: &Value,
    _r: &mut WorldgenRandom,
    _seed: i64,
    _x: i32,
    _z: i32,
    _heights: Heights,
    _ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    None
}

pub fn place(
    _cx: &Ctx2,
    _region: &mut dyn Region,
    _r: &mut WorldgenRandom,
    _p: &Piece,
    _chunk: BBox,
    _start: &Start,
) {
}
