//! Block positions and the six directions.

/// A block position in the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn offset(self, dir: Dir) -> Self {
        let (dx, dy, dz) = dir.offset();
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Dir {
    /// Protocol face order (Player Action, Use Item On).
    pub const ALL: [Dir; 6] = [
        Dir::Down,
        Dir::Up,
        Dir::North,
        Dir::South,
        Dir::West,
        Dir::East,
    ];
    /// Order of neighbour-changed updates (minecraft.wiki, "Block update").
    pub const NC_ORDER: [Dir; 6] = [
        Dir::West,
        Dir::East,
        Dir::Down,
        Dir::Up,
        Dir::North,
        Dir::South,
    ];
    /// Order of post-placement (shape) updates.
    pub const PP_ORDER: [Dir; 6] = [
        Dir::West,
        Dir::East,
        Dir::North,
        Dir::South,
        Dir::Down,
        Dir::Up,
    ];
    pub const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

    pub fn offset(self) -> (i32, i32, i32) {
        match self {
            Dir::Down => (0, -1, 0),
            Dir::Up => (0, 1, 0),
            Dir::North => (0, 0, -1),
            Dir::South => (0, 0, 1),
            Dir::West => (-1, 0, 0),
            Dir::East => (1, 0, 0),
        }
    }

    pub fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }

    /// Clockwise seen from above (north → east); vertical stays.
    pub fn cw(self) -> Dir {
        match self {
            Dir::North => Dir::East,
            Dir::East => Dir::South,
            Dir::South => Dir::West,
            Dir::West => Dir::North,
            other => other,
        }
    }

    pub fn ccw(self) -> Dir {
        self.cw().cw().cw()
    }

    pub fn axis(self) -> Axis {
        match self {
            Dir::Down | Dir::Up => Axis::Y,
            Dir::North | Dir::South => Axis::Z,
            Dir::West | Dir::East => Axis::X,
        }
    }

    pub fn is_horizontal(self) -> bool {
        self.axis() != Axis::Y
    }

    /// Name as used in block state properties.
    pub fn name(self) -> &'static str {
        match self {
            Dir::Down => "down",
            Dir::Up => "up",
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }

    pub fn from_name(name: &str) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.name() == name)
    }

    /// Index in [`Dir::ALL`] (protocol face number).
    pub fn index(self) -> usize {
        Dir::ALL.iter().position(|&d| d == self).expect("listed")
    }

    /// Bit in an observed horizontal face mask: north 1, east 2, south 4, west 8.
    pub fn face_bit(self) -> u8 {
        match self {
            Dir::North => 1,
            Dir::East => 2,
            Dir::South => 4,
            Dir::West => 8,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations() {
        assert_eq!(Dir::North.cw(), Dir::East);
        assert_eq!(Dir::East.ccw(), Dir::North);
        for d in Dir::ALL {
            assert_eq!(d.opposite().opposite(), d);
            assert_eq!(Dir::from_name(d.name()), Some(d));
            let (a, b) = (d.offset(), d.opposite().offset());
            assert_eq!((a.0 + b.0, a.1 + b.1, a.2 + b.2), (0, 0, 0));
        }
    }
}
