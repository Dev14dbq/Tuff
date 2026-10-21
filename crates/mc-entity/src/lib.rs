//! Entities that are not players: storage, movement with block collisions,
//! and the simple ones (falling blocks, dropped items).
//!
//! Movement follows the public description of entity physics
//! (minecraft.wiki, "Entity" and the pages of each entity): each tick the
//! entity accelerates (gravity), moves as far as blocks allow — vertical axis
//! first, then the larger horizontal one — and its velocity is damped.
//! Blocks collide as full cubes for now; shapes come later.

pub mod mob;
pub mod nav;
pub mod physics;

pub use mob::{Mob, Species};
pub use physics::{Aabb, Collide, move_entity};

/// What kind of entity, with its own state.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A block falling under gravity (sand, gravel, ...).
    FallingBlock { state: u32 },
    /// A dropped item stack.
    Item {
        item: i32,
        count: i32,
        /// The stack's `minecraft:damage`.
        damage: i32,
        /// Ticks before a player may pick it up.
        pickup_delay: i32,
        /// Hit points (5); explosions and fire take them.
        health: f32,
    },
    /// Primed TNT, exploding when the fuse runs out.
    Tnt {
        fuse: i32,
        /// The player who lit it.
        owner: Option<u128>,
    },
    /// A creature with goals: animals and monsters.
    Mob(Box<Mob>),
}

impl Kind {
    /// A dropped stack as a block drops it.
    pub fn item(item: i32, count: i32, damage: i32, pickup_delay: i32) -> Self {
        Kind::Item {
            item,
            count,
            damage,
            pickup_delay,
            health: 5.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub id: i32,
    pub uuid: u128,
    pub kind: Kind,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    /// Ticks lived.
    pub age: u32,
    pub removed: bool,
    /// `/tag` (`Tags`).
    pub tags: Vec<String>,
}

impl Entity {
    /// Width and height of the collision box.
    pub fn size(&self) -> (f64, f64) {
        match self.kind {
            Kind::FallingBlock { .. } | Kind::Tnt { .. } => (0.98, 0.98),
            Kind::Item { .. } => (0.25, 0.25),
            Kind::Mob(ref m) => m.species.size(),
        }
    }

    pub fn aabb(&self) -> Aabb {
        let (w, h) = self.size();
        Aabb {
            min: [self.pos[0] - w / 2.0, self.pos[1], self.pos[2] - w / 2.0],
            max: [
                self.pos[0] + w / 2.0,
                self.pos[1] + h,
                self.pos[2] + w / 2.0,
            ],
        }
    }

    /// Block coordinates of the entity's position.
    pub fn block_pos(&self) -> (i32, i32, i32) {
        (
            self.pos[0].floor() as i32,
            self.pos[1].floor() as i32,
            self.pos[2].floor() as i32,
        )
    }
}

/// Gravity and drag per tick for falling blocks and items.
pub const GRAVITY: f64 = 0.04;
pub const DRAG: f64 = 0.98;

/// One tick of falling: accelerate, move, damp.
pub fn fall(entity: &mut Entity, world: &dyn Collide) {
    entity.vel[1] -= GRAVITY;
    let (moved, on_ground) = move_entity(entity.aabb(), entity.vel, world);
    for ((pos, vel), moved) in entity.pos.iter_mut().zip(&mut entity.vel).zip(moved) {
        *pos += moved;
        // Blocked axes stop.
        if (moved - *vel).abs() > 1e-9 {
            *vel = 0.0;
        }
    }
    entity.on_ground = on_ground;
    for v in &mut entity.vel {
        *v *= DRAG;
    }
    entity.age += 1;
}
