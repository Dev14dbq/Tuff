//! State of the thrown, hung and ridden entities: projectiles, frames,
//! armor stands, paintings, boats and minecarts. The rules that run them live
//! in `mc-server` (`projectiles/`, `decor/`, `vehicles/`).

/// An effect carried by a potion or a tipped arrow: id, amplifier, ticks.
pub type EffectSpec = (String, i32, i32);

/// Who may pick a stuck arrow up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pickup {
    No,
    Yes,
    /// Creative players only (shot with Infinity).
    Creative,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arrow {
    /// Base damage (2, 1.25 for a trident-less spectral arrow is the same 2).
    pub damage: f64,
    pub crit: bool,
    pub punch: i32,
    pub flame: bool,
    /// Entities it may still go through (Piercing level).
    pub pierce: i32,
    pub pickup: Pickup,
    /// The arrow item it gives back, if any.
    pub item: i32,
    pub spectral: bool,
    pub effects: Vec<EffectSpec>,
    /// Ticks since it stuck in a block, 0 while flying.
    pub stuck: i32,
    /// Entities it went through already.
    pub hit: Vec<i32>,
    /// A thrown trident (a drowned's) rather than an arrow.
    pub trident: bool,
}

impl Arrow {
    pub fn new(item: i32) -> Self {
        Self {
            damage: 2.0,
            crit: false,
            punch: 0,
            flame: false,
            pierce: 0,
            pickup: Pickup::Yes,
            item,
            spectral: false,
            effects: Vec::new(),
            stuck: 0,
            hit: Vec::new(),
            trident: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Firework {
    /// Flight time in ticks.
    pub life: i32,
    /// The player it is carrying (elytra boost), if any.
    pub boosts: Option<u128>,
    /// Stars: colours of each (RGB), whether the shot shows a trail.
    pub stars: Vec<Vec<i32>>,
    pub shot_at_angle: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shot {
    Arrow(Arrow),
    Snowball,
    Egg,
    Pearl,
    Potion {
        /// Item shown (splash or lingering potion).
        item: i32,
        lingering: bool,
        effects: Vec<EffectSpec>,
        color: i32,
    },
    Firework(Firework),
    /// The cloud a lingering potion leaves.
    Cloud {
        effects: Vec<EffectSpec>,
        color: i32,
        radius: f32,
        /// Ticks left.
        left: i32,
    },
    /// A ghast's fireball (explodes) or a blaze's small one (burns).
    Fireball {
        small: bool,
    },
    /// A wither's skull; a blue (`dangerous`) one is slower and breaks
    /// almost anything.
    WitherSkull {
        dangerous: bool,
    },
    /// The ender dragon's fireball: it leaves a cloud of breath.
    DragonFireball,
    /// A cloud of dragon's breath: harming for whoever stands in it.
    Breath {
        radius: f32,
        /// Radius gained each tick (it grows, or shrinks).
        grow: f32,
        /// Ticks left.
        left: i32,
        /// Harming's amplifier.
        amplifier: i32,
        /// Players it hurt and the age it may hurt them again.
        hit: Vec<(u128, u32)>,
    },
    /// A fishing bobber; `hooked` is the entity it pulls.
    Bobber {
        hooked: Option<i32>,
        /// Ticks until a fish bites, then the ticks it stays bitten.
        wait: i32,
        bite: i32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Projectile {
    pub shot: Shot,
    pub owner: Option<u128>,
    /// A mob that shot it: its entity id and name (death messages).
    pub mob: Option<(i32, &'static str)>,
    /// Facing in degrees, set from the velocity as it flies.
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub glow: bool,
    /// Item shown: id, damage.
    pub item: Option<(i32, i32)>,
    /// Quarter turns... eighths of a turn, 0..8.
    pub rotation: u8,
    pub invisible: bool,
    pub fixed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stand {
    pub small: bool,
    pub arms: bool,
    pub no_base: bool,
    pub marker: bool,
    pub invisible: bool,
    /// Head, body, left arm, right arm, left leg, right leg (degrees).
    pub pose: [[f32; 3]; 6],
    /// Main hand, off hand, feet, legs, chest, head: (item, damage).
    pub gear: [Option<(i32, i32)>; 6],
    pub health: f32,
    /// World age of the last punch, for the second one that breaks it.
    pub last_hit: i64,
}

impl Default for Stand {
    fn default() -> Self {
        Self {
            small: false,
            arms: false,
            no_base: false,
            marker: false,
            invisible: false,
            pose: [
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [-10.0, 0.0, -10.0],
                [-15.0, 0.0, 10.0],
                [-1.0, 0.0, -1.0],
                [1.0, 0.0, 1.0],
            ],
            gear: [None; 6],
            health: 20.0,
            last_hit: -100,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decor {
    Frame(Frame),
    Painting {
        /// `minecraft:painting_variant` name and size in blocks.
        variant: String,
        size: (i32, i32),
    },
    Stand(Stand),
    /// The knot a lead is tied to on a fence.
    LeashKnot,
}

/// A hanging entity: the block it hangs on is `anchor`, `facing` is the side
/// of that block it looks out of (0 down .. 5 east).
#[derive(Clone, Debug, PartialEq)]
pub struct Hung {
    pub decor: Decor,
    /// Facing of a hanging entity, the yaw of an armor stand.
    pub facing: u8,
    pub yaw: f32,
    pub anchor: [i32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cart {
    Rideable,
    Chest,
    Furnace,
    Hopper,
    Tnt,
    /// `command_block_minecart`.
    Command,
}

/// What a command block minecart runs and what came of it.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandCart {
    pub command: String,
    pub track_output: bool,
    /// The last message, as players see it (`[HH:mm:ss] ...`).
    pub last_output: Option<String>,
    pub success_count: i32,
    /// World age it last ran at (`-1`: never).
    pub last_execution: i64,
    /// Its age when an activator rail last set it off.
    pub activated_at: i64,
}

impl Default for CommandCart {
    fn default() -> Self {
        Self {
            command: String::new(),
            track_output: true,
            last_output: None,
            success_count: 0,
            last_execution: -1,
            activated_at: i64::MIN / 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Craft {
    /// `wood` is the entity name's prefix: `oak`, `bamboo`...; `chest` boats
    /// carry an inventory.
    Boat {
        wood: String,
        chest: bool,
    },
    Minecart(Cart),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vehicle {
    pub craft: Craft,
    pub yaw: f32,
    /// Entity ids of the riders.
    pub riders: Vec<i32>,
    /// Hurt wobble: ticks left and its strength.
    pub wobble: i32,
    /// Which way it shakes when hit (1 or -1, turned at each hit).
    pub hurt_dir: i32,
    pub damage: f32,
    /// A boat's left and right paddles turning (its driver says so).
    pub paddles: [bool; 2],
    /// Fuel left in a furnace minecart.
    pub fuel: i32,
    /// Fuse of a TNT minecart that was lit (-1: not lit).
    pub fuse: i32,
    /// What the minecart rolled on in the last tick.
    pub on_rail: bool,
    /// A hopper minecart takes items in (an activator rail with power turns
    /// it off, one without turns it on again).
    pub enabled: bool,
    /// A command block minecart's command and output.
    pub command: Box<CommandCart>,
}

impl Vehicle {
    pub fn new(craft: Craft, yaw: f32) -> Self {
        Self {
            craft,
            yaw,
            riders: Vec::new(),
            wobble: 0,
            hurt_dir: 1,
            damage: 0.0,
            paddles: [false; 2],
            fuel: 0,
            fuse: -1,
            on_rail: false,
            enabled: true,
            command: Box::default(),
        }
    }
}
