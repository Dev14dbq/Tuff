//! Mobs: living entities that decide what to do with a set of goals.
//!
//! The goal selector follows the public description of vanilla's
//! (minecraft.wiki, "Mob AI" and the pages of each mob). Every goal has a
//! priority (lower runs first) and claims some of the mob's controls (move,
//! look, jump). A goal that is not running starts when it can be used and every
//! running goal that shares a control with it has a lower priority (a larger
//! number); those are stopped. Target goals work the same way on their own
//! list and only choose whom the mob goes after.
//!
//! - [`species`]: what each kind is (size, attributes, food, sounds);
//! - [`goals`]: the goals and the selector;
//! - [`body`]: steering, walking, swimming and flying.

mod body;
mod brains;
mod chores;
pub mod goals;
pub mod species;

pub use goals::{Goal, Slot};
pub use species::{Category, Sound, Species};

use crate::Entity;
use crate::Kind;
use crate::nav::{Cell, Nav, Terrain};

pub const MOVE: u8 = 1;
pub const LOOK: u8 = 2;
pub const JUMP: u8 = 4;
pub const TARGET: u8 = 8;

/// Equipment slots, numbered as the equipment packet numbers them.
pub mod slot {
    pub const MAINHAND: usize = 0;
    pub const OFFHAND: usize = 1;
    pub const FEET: usize = 2;
    pub const LEGS: usize = 3;
    pub const CHEST: usize = 4;
    pub const HEAD: usize = 5;
    pub const BODY: usize = 6;
    pub const SADDLE: usize = 7;
}

/// An item a mob wears or holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Gear {
    pub item: i32,
    pub count: i32,
    pub damage: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brain {
    pub goals: Vec<Slot>,
    pub targets: Vec<Slot>,
}

/// A player as mobs see them.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// Entity id.
    pub id: i32,
    pub uuid: u128,
    /// Feet.
    pub pos: [f64; 3],
    /// Whether mobs may go after them (survival or adventure, alive).
    pub targetable: bool,
    /// Alive and not a spectator: animals notice their food.
    pub present: bool,
    /// Items in the main and the off hand (-1 for none).
    pub held: [i32; 2],
    /// The helmet (-1 for none): a carved pumpkin hides the eyes from
    /// endermen.
    pub head: i32,
    /// Wears a piece of gold armor (piglins leave them be).
    pub gold: bool,
    /// Where they look (degrees).
    pub yaw: f32,
    pub pitch: f32,
    /// Who last hurt them, and whom they last hit (entity ids), for pets.
    pub hurt_by: Option<i32>,
    pub hit: Option<i32>,
}

impl Target {
    /// A player at `pos` with nothing in hand.
    pub fn at(id: i32, uuid: u128, pos: [f64; 3]) -> Self {
        Self {
            id,
            uuid,
            pos,
            targetable: true,
            present: true,
            held: [-1; 2],
            head: -1,
            gold: false,
            yaw: 0.0,
            pitch: 0.0,
            hurt_by: None,
            hit: None,
        }
    }
}

/// Another mob (or a villager, `species: None`) as mobs see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Other {
    pub id: i32,
    pub species: Option<Species>,
    pub pos: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub baby: bool,
    pub in_love: bool,
    pub owner: Option<u128>,
    pub alive: bool,
    /// On a lead.
    pub leashed: bool,
    /// A llama in a caravan, with someone behind it, and whether the line
    /// it is in is held by a leashed llama at its head.
    pub in_caravan: bool,
    pub has_tail: bool,
    pub chain_held: bool,
}

/// Item facts the mobs need, answered by the server's item tables.
pub trait Items {
    /// Whether the item is in an item tag (`minecraft:cow_food`).
    fn tagged(&self, item: i32, tag: &str) -> bool;
    /// `minecraft:bow`, or "" for an unknown id.
    fn name(&self, item: i32) -> &str;
}

/// No items at all (tests).
#[derive(Debug)]
pub struct NoItems;

impl Items for NoItems {
    fn tagged(&self, _: i32, _: &str) -> bool {
        false
    }
    fn name(&self, _: i32) -> &str {
        ""
    }
}

pub const PLAYER_EYE: f64 = 1.62;

/// What a mob can see and sense this tick.
#[derive(Clone, Copy)]
pub struct Senses<'a> {
    pub world: &'a dyn Terrain,
    pub players: &'a [Target],
    pub mobs: &'a [Other],
    pub items: &'a dyn Items,
    /// Game time.
    pub tick: u64,
    /// 0 peaceful to 3 hard.
    pub difficulty: i32,
    /// Day and not raining: the sun burns.
    pub sunny: bool,
    pub raining: bool,
    /// In the Nether (piglins and hoglins do not turn into zombies there).
    pub nether: bool,
    /// Bright outside: day, and no thunderstorm (rain is no matter).
    pub day: bool,
    /// The `mob_griefing` game rule: zombies break doors, endermen move
    /// blocks.
    pub griefing: bool,
}

impl std::fmt::Debug for Senses<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Senses")
            .field("players", &self.players.len())
            .field("mobs", &self.mobs.len())
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
}

/// Something a mob can go after: a player or another mob.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub id: i32,
    pub pos: [f64; 3],
    pub width: f64,
    pub eye: f64,
    pub player: bool,
}

impl Senses<'_> {
    pub fn player(&self, id: i32) -> Option<&Target> {
        self.players.iter().find(|p| p.id == id)
    }

    pub fn mob(&self, id: i32) -> Option<&Other> {
        self.mobs.iter().find(|m| m.id == id)
    }

    /// A living target by entity id: a targetable player or a live mob.
    pub fn seen(&self, id: i32) -> Option<Seen> {
        if let Some(p) = self.player(id) {
            return p.targetable.then_some(Seen {
                id,
                pos: p.pos,
                width: 0.6,
                eye: PLAYER_EYE,
                player: true,
            });
        }
        let m = self.mob(id).filter(|m| m.alive)?;
        Some(Seen {
            id,
            pos: m.pos,
            width: m.width,
            eye: m.height * 0.85,
            player: false,
        })
    }

    /// The nearest player passing `ok`, within `range`.
    pub fn nearest(
        &self,
        from: [f64; 3],
        range: f64,
        ok: impl Fn(&Target) -> bool,
    ) -> Option<&Target> {
        self.players
            .iter()
            .filter(|p| ok(p))
            .map(|p| (dist_sq(from, p.pos), p))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p)
    }

    /// The nearest other mob passing `ok`, within `range`.
    pub fn nearest_mob(
        &self,
        from: [f64; 3],
        range: f64,
        ok: impl Fn(&Other) -> bool,
    ) -> Option<&Other> {
        self.mobs
            .iter()
            .filter(|m| m.alive && ok(m))
            .map(|m| (dist_sq(from, m.pos), m))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, m)| m)
    }
}

/// What a projectile a mob throws or shoots is.
#[derive(Clone, Debug, PartialEq)]
pub enum Missile {
    /// An arrow; strays' slow, and `damage` is the base.
    Arrow {
        damage: f64,
        slowness: bool,
    },
    Trident,
    /// A ghast's fireball, with its explosion power.
    Fireball {
        power: i32,
    },
    /// One of a blaze's three.
    SmallFireball,
    /// A witch's splash potion (`minecraft:harming`).
    Potion(&'static str),
    /// A llama's spit.
    Spit,
}

/// What a mob did that the world must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum MobEvent {
    /// A bite or a blow at the entity with this id.
    Attack { target: i32, damage: f32 },
    /// A projectile at the entity with this id: from the mob's eyes towards
    /// `aim`.
    Shoot {
        target: i32,
        aim: [f64; 3],
        missile: Missile,
    },
    /// The creeper blows up.
    Explode { power: f32 },
    /// Drops an item it makes (a chicken's egg).
    Lay { item: &'static str },
    /// A sheep grazed: the plant, or the grass block under it, goes.
    Graze { cell: Cell, block: bool },
    /// Entity event for clients (`EntityEvent` status).
    Status(u8),
    /// The mob took damage not from an entity (burning, falling).
    Hurt { amount: f32, kind: &'static str },
    /// Two in love made a baby; this mob found `partner`.
    Breed { partner: i32 },
    /// A sound of the mob (`entity.cow.ambient`, without `minecraft:`).
    Sound {
        name: String,
        volume: f32,
        pitch: f32,
    },
    /// Swings its arm.
    Swing,
    /// Turns into another kind (a zombie under water into a drowned).
    Convert(Species),
    /// Calls the others of its kind nearby on its target.
    Alert { target: i32 },
    /// A zombie broke this door (its top half).
    BreakDoor { cell: Cell },
    /// A zombie bashes a door: the cracks (0..10, or -1 to clear them).
    DoorCrack { cell: Cell, stage: i32 },
    /// The door shakes under a zombie's blows.
    DoorBash { cell: Cell },
    /// A villager opens or closes a door.
    OpenDoor { cell: Cell, open: bool },
    /// An enderman takes the block at this cell.
    TakeBlock { cell: Cell },
    /// An enderman sets its block down here (if the spot takes it).
    PlaceBlock { cell: Cell },
    /// Moved far at once: clients see a teleport.
    Teleported,
    /// Heals itself (a witch's potion).
    Heal(f32),
    /// Wears the item in a slot (a helmet in the sun).
    Wear { slot: usize, amount: i32 },
    /// A horse throws its rider off.
    Buck,
    /// A zombie villager is cured: it becomes a villager.
    Cured,
    /// A llama falls in behind this one (a caravan), or leaves it.
    CaravanJoin { head: i32 },
    CaravanLeave { head: i32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mob {
    pub species: Species,
    pub health: f32,
    pub max_health: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub brain: Brain,
    pub nav: Nav,
    /// Whom it goes after: an entity id (player or mob).
    pub target: Option<i32>,
    /// Who hurt it last and for how many more ticks that counts.
    pub revenge: Option<(i32, i32)>,
    /// Where goals want it to go this tick: the point and speed multiplier.
    pub goto: Option<([f64; 3], f64)>,
    /// Where goals want it to look this tick.
    pub look: Option<[f64; 3]>,
    pub jump: bool,
    /// Ticks until it may be hurt again, and the damage of the hit that did.
    pub invulnerable: i32,
    pub last_damage: f32,
    pub burning: i32,
    pub attack_cooldown: i32,
    pub fall_distance: f64,
    /// Remaining ticks of the death animation; `Some` once health is gone.
    pub dying: Option<i32>,
    /// Never despawns (named, tamed, or it picked something up).
    pub persistent: bool,
    /// `NoAI`: stands still, no goals.
    pub no_ai: bool,
    pub custom_name: Option<String>,
    pub left_handed: bool,
    // Species state.
    /// Sheep: the wool colour (0..16) and whether it is shorn.
    pub color: u8,
    pub sheared: bool,
    /// Creeper: fuse ticks, their direction, and whether it was lit by flint.
    pub swell: i32,
    pub swell_dir: i32,
    pub ignited: bool,
    pub powered: bool,
    /// Chicken: ticks until an egg.
    pub egg_time: i32,
    pub age: u32,
    /// Spider: clinging to a wall.
    pub climbing: bool,
    /// Vanilla's `Age`: below zero a baby growing up, above zero the
    /// ticks before it may breed again.
    pub baby_age: i32,
    /// Ticks of love left, and the player who fed it.
    pub love: i32,
    pub love_by: Option<u128>,
    /// The owner of a tamed animal.
    pub owner: Option<u128>,
    /// Told to sit (or a fox asleep).
    pub sitting: bool,
    /// A horse let a rider stay on (it may also have no owner).
    pub tame: bool,
    /// Llamas: strength 1 to 5, three chest slots per point.
    pub strength: i32,
    /// Horses: how close it is to letting the rider stay.
    pub temper: i32,
    /// A variant number as clients know it (cat, parrot, horse, rabbit,
    /// fox, wolf, cow, pig, chicken, mooshroom).
    pub variant: i32,
    /// Collar colour of a tamed wolf or cat.
    pub collar: u8,
    /// Equipment by slot (see [`slot`]).
    pub gear: [Option<Gear>; 8],
    /// Chance of each slot dropping on death (2 or more: always, undamaged).
    pub drop_chance: [f32; 8],
    pub can_pick_up: bool,
    /// Neutral mobs: ticks of anger left.
    pub anger: i32,
    /// Ticks left of a conversion (zombie to drowned, zombie villager
    /// cured, skeleton to stray), or -1.
    pub conversion: i32,
    /// Ticks spent with the head under water (zombies), or in powder snow.
    pub soaked: i32,
    /// Slimes, magma cubes and phantoms grow in size.
    pub size: i32,
    /// Enderman: the block in its hands.
    pub carried: Option<u32>,
    /// Witch: ticks left of drinking.
    pub drinking: i32,
    /// Pig: ticks of a carrot-on-a-stick boost, and how long the boost
    /// lasts in all (clients speed the pig up along it).
    pub boost: i32,
    pub boost_total: i32,
    /// Strider: out of lava, cold and slow (shaking).
    pub cold: bool,
    /// Zombies: may break doors (`CanBreakDoors`), and walk to them.
    pub breaks_doors: bool,
    /// Where the lead it is on is held (set by the world every tick).
    pub leash: Option<[f64; 3]>,
    /// Donkeys and mules: a chest on the back (its items are a container
    /// of the server's, keyed by the entity).
    pub chested: bool,
    /// The chest's `Items` as saved (slots counted from the chest's first),
    /// kept up to date from the window's container; set from NBT before it
    /// is first loaded into one.
    pub chest_items: Option<mc_nbt::Tag>,
    pub chest_loaded: bool,
    /// Enderman: a player looks it in the eyes.
    pub stared: bool,
    /// Cat: lying down (on a bed), or sitting on a block it chose.
    pub lying: bool,
    pub perched: bool,
    /// When (its age) it last changed target.
    pub target_since: u32,
    /// Walked into something sideways last tick.
    pub bumped: bool,
    /// Llamas in a caravan: the one it follows, and whether another follows
    /// it.
    pub caravan_head: Option<i32>,
    pub caravan_tail: bool,
    /// A zombie villager's former self: type, profession and level.
    pub villager: Option<(String, Option<String>, i32)>,
    /// Players riding it (entity ids).
    pub riders: Vec<i32>,
    /// Horses roll their own speed and jump.
    pub speed_attr: f64,
    pub jump_strength: f64,
    /// Ticks towards the next ambient sound.
    pub ambient_timer: i32,
    /// Distance walked towards the next step sound.
    pub walked: f64,
    /// The player who last hurt it, and how many more ticks that counts
    /// (loot and experience).
    pub hurt_by_player: Option<(u128, i32)>,
    /// Ghasts charge and blazes burn before shooting.
    pub charge: i32,
    /// Zombies raise their arms while chasing.
    pub arms_up: bool,
    /// Whether clients must be told about changed fields.
    pub dirty: bool,
    aggressive: bool,
    rng: u64,
}

impl Mob {
    pub fn new(species: Species, seed: u64) -> Self {
        let mut mob = Self {
            species,
            health: species.max_health(),
            max_health: species.max_health(),
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: 0.0,
            brain: brains::brain(species),
            nav: Nav::default(),
            target: None,
            revenge: None,
            goto: None,
            look: None,
            jump: false,
            invulnerable: 0,
            last_damage: 0.0,
            burning: 0,
            attack_cooldown: 0,
            fall_distance: 0.0,
            dying: None,
            persistent: false,
            no_ai: false,
            custom_name: None,
            left_handed: false,
            color: 0,
            sheared: false,
            swell: 0,
            swell_dir: -1,
            ignited: false,
            powered: false,
            egg_time: 0,
            age: 0,
            climbing: false,
            baby_age: 0,
            love: 0,
            love_by: None,
            owner: None,
            sitting: false,
            tame: false,
            temper: 0,
            strength: 0,
            variant: 0,
            collar: 14,
            gear: Default::default(),
            drop_chance: [0.085; 8],
            can_pick_up: false,
            anger: 0,
            conversion: -1,
            soaked: 0,
            size: 1,
            carried: None,
            drinking: 0,
            boost: 0,
            boost_total: 0,
            cold: false,
            breaks_doors: false,
            leash: None,
            chested: false,
            chest_items: None,
            chest_loaded: false,
            stared: false,
            lying: false,
            perched: false,
            target_since: 0,
            bumped: false,
            caravan_head: None,
            caravan_tail: false,
            villager: None,
            riders: Vec::new(),
            speed_attr: species.speed(),
            jump_strength: 0.7,
            ambient_timer: 0,
            walked: 0.0,
            hurt_by_player: None,
            charge: 0,
            arms_up: false,
            dirty: false,
            aggressive: false,
            rng: seed | 1,
        };
        mob.yaw = (mob.next_f64() * 360.0) as f32;
        mob.head_yaw = mob.yaw;
        mob.left_handed = mob.next_f64() < 0.05;
        mob.drop_chance[slot::SADDLE] = 2.0;
        mob.drop_chance[slot::BODY] = 2.0;
        match species {
            Species::Chicken => mob.egg_time = mob.rand_range(6000, 12000),
            Species::Horse | Species::Donkey | Species::Mule => {
                // Each horse rolls its health, speed and jump.
                if species == Species::Horse {
                    let hp = 15.0 + mob.below(8) as f32 + mob.below(9) as f32;
                    mob.set_max_health(hp);
                    mob.speed_attr =
                        (0.45 + mob.next_f64() * 0.3 + mob.next_f64() * 0.3 + mob.next_f64() * 0.3)
                            * 0.25;
                    mob.jump_strength =
                        0.4 + mob.next_f64() * 0.2 + mob.next_f64() * 0.2 + mob.next_f64() * 0.2;
                } else {
                    let hp = 15.0 + mob.below(8) as f32 + mob.below(9) as f32;
                    mob.set_max_health(hp);
                    mob.jump_strength = 0.5;
                }
            }
            Species::Llama | Species::TraderLlama => {
                let hp = 15.0 + mob.below(8) as f32 + mob.below(9) as f32;
                mob.set_max_health(hp);
                mob.strength = 1 + mob.below(5) as i32;
                mob.variant = mob.below(4) as i32;
            }
            Species::Slime | Species::MagmaCube => {
                let size = 1 << mob.below(3);
                mob.set_size(size);
            }
            _ => {}
        }
        mob
    }

    // -- random numbers (xorshift64*)

    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A whole number in `0..n`.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.next_u64() >> 33) as u32 % n
        }
    }

    pub fn rand_range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1).max(1) as u32) as i32
    }

    /// A pitch for its sounds: around 1, higher for babies.
    pub fn voice_pitch(&mut self) -> f32 {
        let base = (self.next_f64() - self.next_f64()) as f32 * 0.2 + 1.0;
        if self.is_baby() { base + 0.5 } else { base }
    }

    pub fn is_dead(&self) -> bool {
        self.health <= 0.0
    }

    pub fn is_baby(&self) -> bool {
        self.baby_age < 0
    }

    pub fn set_baby(&mut self, baby: bool) {
        self.baby_age = if baby { -24000 } else { 0 };
        self.dirty = true;
    }

    pub fn is_tame(&self) -> bool {
        self.owner.is_some() || self.tame
    }

    pub fn in_love(&self) -> bool {
        self.love > 0
    }

    pub fn set_max_health(&mut self, hp: f32) {
        self.max_health = hp;
        self.health = hp;
    }

    /// Slimes and magma cubes: size sets health, speed and damage.
    pub fn set_size(&mut self, size: i32) {
        self.size = size.clamp(1, 127);
        if matches!(self.species, Species::Slime | Species::MagmaCube) {
            let s = self.size as f32;
            self.set_max_health(s * s);
            self.speed_attr = 0.2 + 0.1 * f64::from(self.size);
        }
        self.dirty = true;
    }

    /// Width and height of the collision box now (babies are half size).
    pub fn size(&self) -> (f64, f64) {
        let (w, h) = self.species.size();
        match self.species {
            Species::Slime | Species::MagmaCube => {
                let s = f64::from(self.size);
                (w * s, h * s)
            }
            Species::Phantom => {
                let k = 1.0 + 0.15 * f64::from(self.size);
                (w * k, h * k)
            }
            _ if self.is_baby() => (w / 2.0, h / 2.0),
            _ => (w, h),
        }
    }

    pub fn eye_height(&self) -> f64 {
        self.size().1 * self.species.eye_ratio()
    }

    /// Hurts the mob. Returns whether the hit counted. Within the
    /// invulnerability window only a stronger hit goes through, for the
    /// difference.
    pub fn hurt(&mut self, amount: f32, attacker: Option<i32>) -> bool {
        if self.is_dead() || amount <= 0.0 {
            return false;
        }
        let amount = if self.invulnerable > 10 {
            if amount <= self.last_damage {
                return false;
            }
            let more = amount - self.last_damage;
            self.last_damage = amount;
            more
        } else {
            self.invulnerable = 20;
            self.last_damage = amount;
            amount
        };
        self.health = (self.health - amount).max(0.0);
        if let Some(by) = attacker {
            self.revenge = Some((by, 100));
        } else if self.species.category() == Category::Creature {
            // Hurt by something else: animals run all the same.
            self.revenge = Some((-1, 100));
        }
        // Hurt animals stop loving and stop sitting.
        self.love = 0;
        if self.species != Species::Wolf || attacker.is_some() {
            self.sitting = false;
        }
        self.dirty = true;
        true
    }

    /// Whether `Sheep::shear` etc. apply: a shorn or baby sheep gives
    /// nothing.
    pub fn can_shear(&self) -> bool {
        self.species == Species::Sheep && !self.sheared && !self.is_dead() && !self.is_baby()
    }

    pub fn shear(&mut self) {
        self.sheared = true;
        self.dirty = true;
    }

    pub fn set_color(&mut self, color: u8) {
        self.color = color & 15;
        self.dirty = true;
    }

    fn set_aggressive(&mut self, on: bool) {
        if self.aggressive != on {
            self.aggressive = on;
            self.dirty = true;
        }
    }

    pub fn aggressive(&self) -> bool {
        self.aggressive
    }

    /// The item in a slot, if any.
    pub fn gear_item(&self, slot: usize) -> Option<i32> {
        self.gear[slot].as_ref().map(|g| g.item)
    }

    /// Whether it carries a saddle.
    pub fn saddled(&self) -> bool {
        self.gear[slot::SADDLE].is_some()
    }

    /// Starts love mode (fed its food).
    pub fn fall_in_love(&mut self, by: Option<u128>) {
        self.love = 600;
        self.love_by = by;
    }

    /// One of its sounds by kind, with the slime's size.
    pub fn sound_name(&self, which: Sound) -> Option<String> {
        if matches!(self.species, Species::Slime | Species::MagmaCube) && self.size <= 1 {
            let name = self.species.sound(which, false)?;
            return Some(format!("{name}_small"));
        }
        let mut name = self.species.sound(which, self.is_baby())?;
        // An angry wolf growls, a drowned in water gurgles.
        if self.species == Species::Wolf && self.anger > 0 && which != Sound::Step {
            name = name.replace("entity.wolf.", "entity.wolf_angry.");
        }
        Some(name)
    }

    /// The sound event for `which`, ready to play.
    pub fn sound(&mut self, which: Sound) -> Option<MobEvent> {
        let name = self.sound_name(which)?;
        let volume = if which == Sound::Step {
            0.15
        } else {
            self.species.volume()
        };
        let pitch = self.voice_pitch();
        Some(MobEvent::Sound {
            name,
            volume,
            pitch,
        })
    }
}

pub(crate) fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

pub(crate) fn cell_of(pos: [f64; 3]) -> Cell {
    [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ]
}

/// Whether nothing solid lies between two points (stepped along the line).
pub fn line_clear(w: &dyn Terrain, a: [f64; 3], b: [f64; 3]) -> bool {
    let d = dist_sq(a, b).sqrt();
    let steps = (d * 2.0).ceil() as i32;
    (1..steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let p = [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ];
        !w.solid(
            p[0].floor() as i32,
            p[1].floor() as i32,
            p[2].floor() as i32,
        )
    })
}

/// What a mob moves with.
#[derive(Debug)]
pub struct Body {
    /// The mob's entity id.
    pub id: i32,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
}

/// Advances a mob one tick. Anything the world must act on goes to `out`.
pub fn tick(entity: &mut Entity, s: &Senses, out: &mut Vec<MobEvent>) {
    let Kind::Mob(m) = &mut entity.kind else {
        return;
    };
    let mut b = Body {
        id: entity.id,
        pos: entity.pos,
        vel: entity.vel,
        on_ground: entity.on_ground,
    };
    m.age += 1;
    m.invulnerable = (m.invulnerable - 1).max(0);
    m.attack_cooldown = (m.attack_cooldown - 1).max(0);
    if let Some((_, ticks)) = &mut m.revenge {
        *ticks -= 1;
        if *ticks <= 0 {
            m.revenge = None;
        }
    }
    if let Some((_, ticks)) = &mut m.hurt_by_player {
        *ticks -= 1;
        if *ticks <= 0 {
            m.hurt_by_player = None;
        }
    }
    if let Some(left) = m.dying {
        // Dead: the body lies there for a second.
        m.dying = Some(left - 1);
        b.vel[0] *= 0.5;
        b.vel[2] *= 0.5;
        body::physics(m, &mut b, s, None, &mut Vec::new());
        entity.pos = b.pos;
        entity.vel = b.vel;
        entity.on_ground = b.on_ground;
        if left <= 1 {
            entity.removed = true;
        }
        return;
    }
    if m.is_dead() {
        return;
    }
    if b.pos[1] < -128.0 {
        out.push(MobEvent::Hurt {
            amount: 4.0,
            kind: "minecraft:out_of_world",
        });
    }
    body::life(m, &mut b, s, out);
    if m.is_dead() || entity.removed {
        return;
    }
    if m.no_ai {
        body::physics(m, &mut b, s, None, out);
    } else {
        // Goals.
        m.look = None;
        let target = m.target;
        let mut brain = std::mem::take(&mut m.brain);
        goals::run(&mut brain.targets, m, &mut b, s, out);
        goals::run(&mut brain.goals, m, &mut b, s, out);
        m.brain = brain;
        if m.target != target {
            m.target_since = m.age;
            if m.target.is_none() && m.stared {
                m.stared = false;
                m.dirty = true;
            }
        }
        if let Some(holder) = m.leash {
            chores::follow_lead(m, &mut b, holder);
        }
        let aggressive = m.target.is_some();
        m.set_aggressive(aggressive);
        let input = body::drive(m, &mut b, s);
        body::physics(m, &mut b, s, input, out);
        body::turn_head(m, &b);
    }
    entity.pos = b.pos;
    entity.vel = b.vel;
    entity.on_ground = b.on_ground;
}

/// One tick of a mob its rider moves: it lives (grows up, burns, makes
/// sounds) but neither thinks nor moves itself.
pub fn tick_ridden(entity: &mut Entity, s: &Senses, out: &mut Vec<MobEvent>) {
    let Kind::Mob(m) = &mut entity.kind else {
        return;
    };
    let mut b = Body {
        id: entity.id,
        pos: entity.pos,
        vel: [0.0; 3],
        on_ground: true,
    };
    m.age += 1;
    m.invulnerable = (m.invulnerable - 1).max(0);
    if m.is_dead() || m.dying.is_some() {
        return;
    }
    body::life(m, &mut b, s, out);
}

/// Moves a mob to a random standable spot within `range` (an enderman
/// dodging); whether it found one.
pub fn random_teleport(entity: &mut Entity, world: &dyn Terrain, range: i32) -> bool {
    let Kind::Mob(m) = &mut entity.kind else {
        return false;
    };
    let mut b = Body {
        id: entity.id,
        pos: entity.pos,
        vel: entity.vel,
        on_ground: entity.on_ground,
    };
    let s = Senses {
        world,
        players: &[],
        mobs: &[],
        items: &NoItems,
        tick: 0,
        difficulty: 0,
        sunny: false,
        raining: false,
        nether: false,
        day: false,
        griefing: false,
    };
    let at = b.pos;
    if goals::teleport_near(m, &mut b, &s, at, range) {
        entity.pos = b.pos;
        entity.vel = b.vel;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod chore_tests;
