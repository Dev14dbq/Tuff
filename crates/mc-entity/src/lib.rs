//! Entities that are not players: storage, movement with block collisions,
//! and the simple ones (falling blocks, dropped items).
//!
//! Movement follows the public description of entity physics
//! (minecraft.wiki, "Entity" and the pages of each entity): each tick the
//! entity accelerates (gravity), moves as far as blocks allow — vertical axis
//! first, then the larger horizontal one — and its velocity is damped.
//! Blocks collide as full cubes for now; shapes come later.

pub mod extra;
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
    /// Arrows, thrown things, fireworks and bobbers.
    Projectile(extra::Projectile),
    /// Frames, paintings, armor stands and lead knots.
    Hung(extra::Hung),
    /// Boats and minecarts.
    Vehicle(extra::Vehicle),
    /// Primed TNT, exploding when the fuse runs out.
    Tnt {
        fuse: i32,
        /// The player who lit it.
        owner: Option<u128>,
    },
    /// A creature with goals: animals and monsters.
    Mob(Box<Mob>),
    /// Experience: `count` merged orbs of `value` points each.
    ExperienceOrb { value: i32, count: i32 },
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
        match &self.kind {
            Kind::FallingBlock { .. } | Kind::Tnt { .. } => (0.98, 0.98),
            Kind::Item { .. } => (0.25, 0.25),
            Kind::Mob(m) => m.species.size(),
            Kind::ExperienceOrb { .. } => (0.5, 0.5),
            Kind::Projectile(p) => match &p.shot {
                extra::Shot::Arrow(_) => (0.5, 0.5),
                extra::Shot::Cloud { radius, .. } => (f64::from(*radius) * 2.0, 0.5),
                _ => (0.25, 0.25),
            },
            Kind::Hung(h) => match &h.decor {
                extra::Decor::Stand(s) if s.marker => (0.0, 0.0),
                extra::Decor::Stand(s) if s.small => (0.25, 0.9875),
                extra::Decor::Stand(_) => (0.5, 1.975),
                extra::Decor::LeashKnot => (0.375, 0.5),
                // Thin boxes are cut to the wall in `Entity::aabb`.
                _ => (0.75, 0.75),
            },
            Kind::Vehicle(v) => match &v.craft {
                extra::Craft::Boat { .. } => (1.375, 0.5625),
                extra::Craft::Minecart(_) => (0.98, 0.7),
            },
        }
    }

    pub fn aabb(&self) -> Aabb {
        if let Kind::Hung(h) = &self.kind
            && matches!(
                h.decor,
                extra::Decor::Frame(_) | extra::Decor::Painting { .. }
            )
        {
            return self.wall_box(h);
        }
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

    /// The thin box of a frame or painting on its wall: 1/16 deep, and as
    /// wide as the picture along the wall.
    fn wall_box(&self, h: &extra::Hung) -> Aabb {
        let (w, t) = match &h.decor {
            extra::Decor::Painting { size, .. } => (f64::from(size.0), f64::from(size.1)),
            _ => (0.75, 0.75),
        };
        let depth = 0.0625 / 2.0;
        let half = match h.facing {
            2 | 3 => [w / 2.0, t / 2.0, depth],
            4 | 5 => [depth, t / 2.0, w / 2.0],
            _ => [w / 2.0, depth, w / 2.0],
        };
        Aabb {
            min: [
                self.pos[0] - half[0],
                self.pos[1] - half[1],
                self.pos[2] - half[2],
            ],
            max: [
                self.pos[0] + half[0],
                self.pos[1] + half[1],
                self.pos[2] + half[2],
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
    fall_with(entity, world, GRAVITY);
}

/// Experience orbs fall more slowly.
pub const ORB_GRAVITY: f64 = 0.03;

/// `fall` with the gravity of the entity.
pub fn fall_with(entity: &mut Entity, world: &dyn Collide, gravity: f64) {
    entity.vel[1] -= gravity;
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
