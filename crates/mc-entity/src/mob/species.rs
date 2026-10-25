//! What each kind of mob is: its size, attributes, food, sounds and how much
//! experience it gives. Numbers are the public ones (minecraft.wiki, the page
//! of each mob, "Attribute").

/// Spawn and despawn categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Creature,
    Monster,
    /// Neither: golems. They do not spawn with the biomes, do not despawn
    /// and give no experience.
    Misc,
    // Aquatic (Claude): the water categories, each with its own cap.
    /// Squids, dolphins, nautiluses (`water_creature`).
    WaterCreature,
    /// Fish (`water_ambient`).
    WaterAmbient,
    /// Glow squids (`underground_water_creature`).
    UndergroundWater,
    /// Axolotls (`axolotls`).
    Axolotls,
    /// Bats (flying-mobs): they spawn in the dark to a cap of their own,
    /// despawn like monsters and give nothing.
    Ambient,
}

macro_rules! species {
    ($($name:ident => $id:literal, $display:literal;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Species {
            $($name,)*
        }

        impl Species {
            pub const ALL: &'static [Species] = &[$(Species::$name,)*];

            /// The registry name.
            pub fn id(self) -> &'static str {
                match self {
                    $(Species::$name => $id,)*
                }
            }

            /// The name death messages show.
            pub fn display_name(self) -> &'static str {
                match self {
                    $(Species::$name => $display,)*
                }
            }
        }
    };
}

species! {
    Cow => "minecraft:cow", "Cow";
    Mooshroom => "minecraft:mooshroom", "Mooshroom";
    Sheep => "minecraft:sheep", "Sheep";
    Pig => "minecraft:pig", "Pig";
    Strider => "minecraft:strider", "Strider";
    Chicken => "minecraft:chicken", "Chicken";
    Rabbit => "minecraft:rabbit", "Rabbit";
    Horse => "minecraft:horse", "Horse";
    Donkey => "minecraft:donkey", "Donkey";
    Mule => "minecraft:mule", "Mule";
    Llama => "minecraft:llama", "Llama";
    TraderLlama => "minecraft:trader_llama", "Trader Llama";
    Wolf => "minecraft:wolf", "Wolf";
    Cat => "minecraft:cat", "Cat";
    Fox => "minecraft:fox", "Fox";
    Parrot => "minecraft:parrot", "Parrot";
    Zombie => "minecraft:zombie", "Zombie";
    Husk => "minecraft:husk", "Husk";
    Drowned => "minecraft:drowned", "Drowned";
    ZombieVillager => "minecraft:zombie_villager", "Zombie Villager";
    Skeleton => "minecraft:skeleton", "Skeleton";
    Stray => "minecraft:stray", "Stray";
    WitherSkeleton => "minecraft:wither_skeleton", "Wither Skeleton";
    Creeper => "minecraft:creeper", "Creeper";
    Spider => "minecraft:spider", "Spider";
    CaveSpider => "minecraft:cave_spider", "Cave Spider";
    Enderman => "minecraft:enderman", "Enderman";
    Witch => "minecraft:witch", "Witch";
    Slime => "minecraft:slime", "Slime";
    Phantom => "minecraft:phantom", "Phantom";
    Piglin => "minecraft:piglin", "Piglin";
    ZombifiedPiglin => "minecraft:zombified_piglin", "Zombified Piglin";
    Ghast => "minecraft:ghast", "Ghast";
    Blaze => "minecraft:blaze", "Blaze";
    MagmaCube => "minecraft:magma_cube", "Magma Cube";
    EnderDragon => "minecraft:ender_dragon", "Ender Dragon";
    Wither => "minecraft:wither", "Wither";
    Silverfish => "minecraft:silverfish", "Silverfish";
    Endermite => "minecraft:endermite", "Endermite";
    IronGolem => "minecraft:iron_golem", "Iron Golem";
    SnowGolem => "minecraft:snow_golem", "Snow Golem";
    // Land animals (Cursor)
    Goat => "minecraft:goat", "Goat";
    PolarBear => "minecraft:polar_bear", "Polar Bear";
    Ocelot => "minecraft:ocelot", "Ocelot";
    SkeletonHorse => "minecraft:skeleton_horse", "Skeleton Horse";
    ZombieHorse => "minecraft:zombie_horse", "Zombie Horse";
    Hoglin => "minecraft:hoglin", "Hoglin";
    Zoglin => "minecraft:zoglin", "Zoglin";
    PiglinBrute => "minecraft:piglin_brute", "Piglin Brute";
    Armadillo => "minecraft:armadillo", "Armadillo";
    Panda => "minecraft:panda", "Panda";
    Camel => "minecraft:camel", "Camel";
    CamelHusk => "minecraft:camel_husk", "Camel Husk";
    Sniffer => "minecraft:sniffer", "Sniffer";
    // Aquatic (Claude)
    Cod => "minecraft:cod", "Cod";
    Salmon => "minecraft:salmon", "Salmon";
    Pufferfish => "minecraft:pufferfish", "Pufferfish";
    TropicalFish => "minecraft:tropical_fish", "Tropical Fish";
    Squid => "minecraft:squid", "Squid";
    GlowSquid => "minecraft:glow_squid", "Glow Squid";
    Dolphin => "minecraft:dolphin", "Dolphin";
    Turtle => "minecraft:turtle", "Turtle";
    Axolotl => "minecraft:axolotl", "Axolotl";
    Frog => "minecraft:frog", "Frog";
    Tadpole => "minecraft:tadpole", "Tadpole";
    Guardian => "minecraft:guardian", "Guardian";
    ElderGuardian => "minecraft:elder_guardian", "Elder Guardian";
    Nautilus => "minecraft:nautilus", "Nautilus";
    ZombieNautilus => "minecraft:zombie_nautilus", "Zombie Nautilus";
    // Flying mobs (flying-mobs)
    Bat => "minecraft:bat", "Bat";
    Bee => "minecraft:bee", "Bee";
    Allay => "minecraft:allay", "Allay";
    Breeze => "minecraft:breeze", "Breeze";
    HappyGhast => "minecraft:happy_ghast", "Happy Ghast";
    Bogged => "minecraft:bogged", "Bogged";
    Parched => "minecraft:parched", "Parched";
    Giant => "minecraft:giant", "Giant";
    Mannequin => "minecraft:mannequin", "Mannequin";
    Shulker => "minecraft:shulker", "Shulker";
    Creaking => "minecraft:creaking", "Creaking";
    Warden => "minecraft:warden", "Warden";
    CopperGolem => "minecraft:copper_golem", "Copper Golem";
    SulfurCube => "minecraft:sulfur_cube", "Sulfur Cube";
    // Illagers and their kin
    Pillager => "minecraft:pillager", "Pillager";
    Vindicator => "minecraft:vindicator", "Vindicator";
    Evoker => "minecraft:evoker", "Evoker";
    Illusioner => "minecraft:illusioner", "Illusioner";
    Vex => "minecraft:vex", "Vex";
    Ravager => "minecraft:ravager", "Ravager";
}

/// Which sound of a mob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    Ambient,
    Hurt,
    Death,
    Step,
}

impl Category {
    /// The water categories.
    pub fn water(self) -> bool {
        matches!(
            self,
            Category::WaterCreature
                | Category::WaterAmbient
                | Category::UndergroundWater
                | Category::Axolotls
        )
    }

    /// Animals: everything that is not a monster or a golem.
    pub fn animal(self) -> bool {
        !matches!(self, Category::Monster | Category::Misc)
    }
}

impl Species {
    pub fn from_id(id: &str) -> Option<Self> {
        let id = if id.contains(':') {
            id.to_owned()
        } else {
            format!("minecraft:{id}")
        };
        Self::ALL.iter().copied().find(|s| s.id() == id)
    }

    /// The name without `minecraft:`.
    pub fn path(self) -> &'static str {
        self.id().trim_start_matches("minecraft:")
    }

    pub fn category(self) -> Category {
        use Species::*;
        match self {
            Cow | Mooshroom | Sheep | Pig | Chicken | Rabbit | Horse | Donkey | Mule | Llama
            | TraderLlama | Wolf | Cat | Fox | Parrot | Strider => Category::Creature,
            // Land animals (Cursor)
            Goat | PolarBear | Ocelot | SkeletonHorse | ZombieHorse | Armadillo | Panda | Camel
            | Sniffer => Category::Creature,
            IronGolem | SnowGolem | Mannequin => Category::Misc,
            // Aquatic (Claude)
            Turtle | Frog | Tadpole => Category::Creature,
            Squid | Dolphin | Nautilus => Category::WaterCreature,
            Cod | Salmon | Pufferfish | TropicalFish => Category::WaterAmbient,
            GlowSquid => Category::UndergroundWater,
            Axolotl => Category::Axolotls,
            // Flying mobs (flying-mobs)
            Bee | HappyGhast => Category::Creature,
            Allay | CopperGolem => Category::Misc,
            Bat => Category::Ambient,
            _ => Category::Monster,
        }
    }

    /// Width and height of an adult's collision box (slimes and phantoms
    /// grow with their size, see [`crate::Mob::size`]).
    pub fn size(self) -> (f64, f64) {
        use Species::*;
        match self {
            Cow | Mooshroom => (0.9, 1.4),
            Sheep => (0.9, 1.3),
            Pig => (0.9, 0.9),
            Strider => (0.9, 1.7),
            Chicken => (0.4, 0.7),
            Rabbit => (0.49, 0.6),
            Horse | Mule => (1.3964844, 1.6),
            Llama | TraderLlama => (0.9, 1.87),
            Donkey => (1.3964844, 1.5),
            Wolf => (0.6, 0.85),
            Cat => (0.6, 0.7),
            Fox => (0.6, 0.7),
            Parrot => (0.5, 0.9),
            Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin | Piglin | Witch => {
                (0.6, 1.95)
            }
            Skeleton | Stray => (0.6, 1.99),
            WitherSkeleton => (0.7, 2.4),
            Creeper => (0.6, 1.7),
            Spider => (1.4, 0.9),
            CaveSpider => (0.7, 0.5),
            Enderman => (0.6, 2.9),
            Slime | MagmaCube => (0.52, 0.52),
            Phantom => (0.9, 0.5),
            Silverfish | Endermite => (0.4, 0.3),
            IronGolem => (1.4, 2.7),
            SnowGolem => (0.7, 1.9),
            Ghast => (4.0, 4.0),
            Blaze => (0.6, 1.8),
            EnderDragon => (16.0, 8.0),
            Wither => (0.9, 3.5),
            // Land animals (Cursor)
            Goat => (0.9, 1.3),
            PolarBear => (1.4, 1.4),
            Ocelot => (0.6, 0.7),
            SkeletonHorse | ZombieHorse => (1.3964844, 1.6),
            Hoglin | Zoglin => (1.3964844, 1.4),
            PiglinBrute => (0.6, 1.95),
            Armadillo => (0.7, 0.65),
            Panda => (1.3, 1.25),
            Camel | CamelHusk => (1.7, 2.375),
            Sniffer => (1.9, 1.75),
            // Aquatic (Claude)
            Cod => (0.5, 0.3),
            // The medium one; see `Mob::size` for the others.
            Salmon => (0.7, 0.4),
            // Small; the puffed ones are in `Mob::size`.
            Pufferfish => (0.35, 0.35),
            TropicalFish => (0.5, 0.4),
            Squid | GlowSquid => (0.8, 0.8),
            Dolphin => (0.9, 0.6),
            Turtle => (1.2, 0.4),
            Axolotl => (0.75, 0.42),
            Frog => (0.5, 0.5),
            Tadpole => (0.4, 0.3),
            Guardian => (0.85, 0.85),
            ElderGuardian => (1.9975, 1.9975),
            Nautilus | ZombieNautilus => (0.875, 0.95),
            // Flying mobs (flying-mobs)
            Bat => (0.5, 0.9),
            Bee => (0.55, 0.5),
            Allay => (0.35, 0.6),
            Breeze => (0.6, 1.77),
            HappyGhast => (4.0, 4.0),
            Bogged | Parched => (0.6, 1.99),
            Giant => (3.6, 12.0),
            Mannequin => (0.6, 1.8),
            Shulker => (1.0, 1.0),
            Creaking => (0.9, 2.7),
            Warden => (0.9, 2.9),
            CopperGolem => (0.49, 0.98),
            SulfurCube => (0.98, 0.98),
            // Illagers and their kin
            Pillager | Vindicator | Evoker | Illusioner => (0.6, 1.95),
            Vex => (0.4, 0.8),
            Ravager => (1.95, 2.2),
        }
    }

    /// Eye height as a share of the height, for those that differ from 85%.
    pub fn eye_ratio(self) -> f64 {
        use Species::*;
        match self {
            Cow | Mooshroom => 1.3 / 1.4,
            Sheep => 0.95,
            Chicken => 0.92,
            Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin | Skeleton | Stray => {
                1.74 / 1.95
            }
            Piglin => 1.79 / 1.95,
            Witch => 1.62 / 1.95,
            WitherSkeleton => 2.1 / 2.4,
            Spider => 0.65 / 0.9,
            CaveSpider => 0.45 / 0.5,
            Enderman => 2.55 / 2.9,
            Ghast => 2.6 / 4.0,
            Wolf => 0.8,
            Cat | Fox => 0.5,
            Parrot => 0.6,
            Slime | MagmaCube => 0.625,
            Phantom => 0.35,
            Silverfish | Endermite => 0.13 / 0.3,
            IronGolem => 2.5 / 2.7,
            SnowGolem => 1.7 / 1.9,
            // Land animals (Cursor)
            Goat => 0.95,
            Ocelot => 0.5,
            PiglinBrute => 1.79 / 1.95,
            // Aquatic (Claude)
            Squid | GlowSquid | Dolphin | Cod | Salmon | Pufferfish | TropicalFish => 0.5,
            Guardian | ElderGuardian => 0.5,
            // Flying mobs (flying-mobs)
            Bat | Bee | Shulker => 0.5,
            Allay => 0.6,
            Breeze => 1.3452 / 1.77,
            HappyGhast => 0.65,
            Bogged | Parched => 1.74 / 1.99,
            Giant => 0.87,
            Creaking => 2.3 / 2.7,
            Warden => 2.465 / 2.9,
            CopperGolem => 0.8125 / 0.98,
            SulfurCube => 0.625,
            // Illagers and their kin
            Pillager | Vindicator | Evoker | Illusioner => 1.62 / 1.95,
            Vex => 0.51875 / 0.8,
            Ravager => 2.1 / 2.2,
            _ => 0.85,
        }
    }

    pub fn max_health(self) -> f32 {
        use Species::*;
        match self {
            Cow | Mooshroom | Pig | Cat | Fox | Ghast => 10.0,
            Sheep | Wolf => 8.0,
            Strider => 20.0,
            Chicken => 4.0,
            Rabbit => 3.0,
            Parrot => 6.0,
            Horse | Donkey | Mule | Llama | TraderLlama => 22.0,
            Spider | Piglin => 16.0,
            Silverfish | Endermite => 8.0,
            IronGolem => 100.0,
            SnowGolem => 4.0,
            CaveSpider => 12.0,
            Enderman => 40.0,
            Witch => 26.0,
            Slime | MagmaCube => 1.0,
            EnderDragon => 200.0,
            Wither => 300.0,
            // Land animals (Cursor)
            Goat | Ocelot => 10.0,
            PolarBear => 30.0,
            Armadillo => 12.0,
            Camel | CamelHusk => 32.0,
            Sniffer => 14.0,
            SkeletonHorse | ZombieHorse => 15.0,
            Hoglin | Zoglin => 40.0,
            PiglinBrute => 50.0,
            // Aquatic (Claude)
            Cod | Salmon | Pufferfish | TropicalFish => 3.0,
            Squid | GlowSquid | Dolphin | Frog => 10.0,
            Turtle | Guardian => 30.0,
            Axolotl => 14.0,
            Tadpole => 6.0,
            ElderGuardian => 80.0,
            Nautilus | ZombieNautilus => 15.0,
            // Flying mobs (flying-mobs)
            Bat => 6.0,
            Bee => 10.0,
            Allay => 20.0,
            Breeze | Shulker => 30.0,
            Bogged | Parched => 16.0,
            Giant => 100.0,
            Creaking => 1.0,
            Warden => 500.0,
            CopperGolem => 12.0,
            SulfurCube => 8.0,
            // Illagers and their kin
            Pillager | Vindicator | Evoker => 24.0,
            Illusioner => 32.0,
            Vex => 14.0,
            Ravager => 100.0,
            _ => 20.0,
        }
    }

    /// The `movement_speed` attribute.
    pub fn speed(self) -> f64 {
        use Species::*;
        match self {
            Cow | Mooshroom => 0.2,
            Sheep | Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin | Blaze => 0.23,
            Rabbit | Wolf | Cat | Fox | Spider | CaveSpider | Enderman => 0.3,
            Horse => 0.225,
            Donkey | Mule | Llama | TraderLlama | Strider => 0.175,
            Parrot => 0.2,
            Piglin => 0.35,
            SnowGolem => 0.2,
            Phantom | Ghast => 0.7,
            Wither => 0.6,
            Slime | MagmaCube => 0.3,
            // Land animals (Cursor)
            Goat | SkeletonHorse | ZombieHorse => 0.2,
            Ocelot | Hoglin | Zoglin => 0.3,
            PiglinBrute => 0.35,
            Armadillo => 0.14,
            Panda => 0.15,
            Camel | CamelHusk => 0.09,
            Sniffer => 0.1,
            // Aquatic (Claude)
            Cod | Salmon | TropicalFish | Axolotl | Frog | Tadpole | Nautilus => 1.0,
            Pufferfish => 0.7,
            Squid | GlowSquid | Dolphin => 1.2,
            ZombieNautilus => 1.1,
            Turtle => 0.25,
            Guardian => 0.5,
            ElderGuardian => 0.3,
            // Flying mobs (flying-mobs)
            Bee => 0.3,
            Allay => 0.1,
            Breeze => 0.63,
            HappyGhast => 0.05,
            Giant => 0.5,
            Shulker => 0.0,
            Creaking => 0.3,
            Warden => 0.3,
            CopperGolem => 0.2,
            SulfurCube => 0.4,
            // Illagers and their kin
            Pillager | Vindicator => 0.35,
            Evoker | Illusioner => 0.5,
            Vex => 0.6,
            Ravager => 0.3,
            _ => 0.25,
        }
    }

    /// The `follow_range` attribute.
    pub fn follow_range(self) -> f64 {
        use Species::*;
        match self {
            Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin => 35.0,
            Enderman => 64.0,
            Ghast => 100.0,
            Blaze => 48.0,
            Fox => 32.0,
            Wither => 40.0,
            PolarBear => 20.0,
            PiglinBrute => 12.0,
            // Flying mobs (flying-mobs)
            Bee | Allay => 48.0,
            Breeze => 24.0,
            Creaking => 32.0,
            Warden => 24.0,
            // Illagers and their kin
            Pillager | Ravager => 32.0,
            Vindicator | Evoker => 12.0,
            Illusioner => 18.0,
            _ => 16.0,
        }
    }

    /// The `attack_damage` attribute at normal difficulty (0 for those that
    /// do not bite).
    pub fn attack_damage(self) -> f32 {
        use Species::*;
        match self {
            Zombie | Husk | Drowned | ZombieVillager => 3.0,
            Spider | CaveSpider | Fox => 2.0,
            Wolf => 4.0,
            Cat => 3.0,
            WitherSkeleton => 4.0,
            Enderman => 7.0,
            Piglin | ZombifiedPiglin => 5.0,
            Blaze | Phantom => 6.0,
            Skeleton | Stray => 2.0,
            Silverfish => 1.0,
            Endermite => 2.0,
            // Each blow rolls 7.5 to 21.5 around this (see the melee goal).
            IronGolem => 15.0,
            PolarBear | Hoglin | Zoglin => 6.0,
            Ocelot => 3.0,
            Panda => 2.0,
            PiglinBrute => 7.0,
            // Aquatic (Claude)
            Dolphin | Nautilus | ZombieNautilus => 3.0,
            Axolotl => 2.0,
            Frog => 10.0,
            Guardian => 6.0,
            ElderGuardian => 8.0,
            // Flying mobs (flying-mobs)
            Bee | Allay | Bogged | Parched => 2.0,
            Giant => 50.0,
            Creaking => 3.0,
            Warden => 30.0,
            // Illagers and their kin
            Pillager | Vindicator => 5.0,
            Vex => 4.0,
            Ravager => 12.0,
            _ => 0.0,
        }
    }

    /// The `armor` attribute.
    pub fn armor(self) -> f32 {
        use Species::*;
        match self {
            Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin => 2.0,
            Wither => 4.0,
            _ => 0.0,
        }
    }

    /// The `knockback_resistance` attribute.
    pub fn knockback_resistance(self) -> f64 {
        match self {
            Species::IronGolem => 1.0,
            Species::Hoglin | Species::Zoglin => 0.6,
            Species::Squid | Species::GlowSquid => 0.85,
            // Flying mobs (flying-mobs): a shulker never budges.
            Species::Shulker => 1.0,
            Species::Ravager => 0.75,
            Species::Warden => 1.0,
            _ => 0.0,
        }
    }

    /// Undead: hurt by healing, healed by harming, smite applies.
    pub fn undead(self) -> bool {
        use Species::*;
        matches!(
            self,
            Zombie
                | Husk
                | Drowned
                | ZombieVillager
                | Skeleton
                | Stray
                | WitherSkeleton
                | Phantom
                | ZombifiedPiglin
                | Wither
                | SkeletonHorse
                | ZombieHorse
                | Zoglin
                | CamelHusk
                | ZombieNautilus
                // Flying mobs (flying-mobs)
                | Bogged
                | Parched
                | Giant
        )
    }

    /// Whether sunlight sets it on fire.
    pub fn burns_in_sun(self) -> bool {
        use Species::*;
        matches!(
            self,
            Zombie
                | Drowned
                | ZombieVillager
                | Skeleton
                | Stray
                | Phantom
                | Bogged
                | ZombieNautilus
        )
    }

    /// Fire and lava do not hurt it.
    pub fn fire_immune(self) -> bool {
        use Species::*;
        matches!(
            self,
            WitherSkeleton
                | Ghast
                | Blaze
                | MagmaCube
                | ZombifiedPiglin
                | Strider
                | EnderDragon
                | Wither
                | Warden
        )
    }

    /// Flies instead of walking.
    pub fn flies(self) -> bool {
        matches!(
            self,
            Species::Ghast
                | Species::Blaze
                | Species::Phantom
                | Species::Parrot
                | Species::EnderDragon
                | Species::Wither
                // Flying mobs (flying-mobs)
                | Species::Bat
                | Species::Bee
                | Species::Allay
                | Species::HappyGhast
                | Species::Vex
        )
    }

    /// Whether falls hurt it.
    pub fn takes_fall_damage(self) -> bool {
        !matches!(
            self,
            Species::Chicken
                | Species::Parrot
                | Species::Ghast
                | Species::Blaze
                | Species::Phantom
                | Species::Cat
                | Species::MagmaCube
                | Species::SnowGolem
                | Species::EnderDragon
                | Species::Wither
                // Flying mobs (flying-mobs)
                | Species::Bat
                | Species::Bee
                | Species::Allay
                | Species::HappyGhast
                | Species::Breeze
                | Species::Shulker
                | Species::CopperGolem
                | Species::Vex
        )
    }

    /// Blocks of fall it shrugs off.
    pub fn safe_fall(self) -> f64 {
        match self {
            Species::Rabbit | Species::Fox => 5.0,
            Species::Horse | Species::Donkey | Species::Mule => 4.0,
            // Land animals (Cursor): 10 HP less fall damage than the usual 3.
            Species::Goat => 13.0,
            // Aquatic (Claude): frogs take 5 less.
            Species::Frog => 8.0,
            Species::SkeletonHorse | Species::ZombieHorse => 4.0,
            _ => 3.0,
        }
    }

    /// Can have babies (`AgeableMob`).
    pub fn ageable(self) -> bool {
        self.category() == Category::Creature && !matches!(self, Species::Parrot | Species::Tadpole)
            || matches!(
                self,
                Species::Hoglin
                    | Species::Zoglin
                    | Species::CamelHusk
                    // Aquatic (Claude)
                    | Species::Squid
                    | Species::GlowSquid
                    | Species::Dolphin
                    | Species::Axolotl
                    | Species::Nautilus
            )
            // Flying mobs (flying-mobs): small sulfur cubes grow up.
            || self == Species::SulfurCube
    }

    /// Babies of these grow up; zombies and piglins stay babies.
    pub fn has_babies(self) -> bool {
        use Species::*;
        self.ageable()
            || matches!(
                self,
                Zombie | Husk | Drowned | ZombieVillager | Piglin | ZombifiedPiglin
            )
    }

    /// The item tag that makes it come along and breed (`cow_food`).
    pub fn food_tag(self) -> Option<&'static str> {
        use Species::*;
        Some(match self {
            Cow | Mooshroom => "minecraft:cow_food",
            Sheep => "minecraft:sheep_food",
            Pig => "minecraft:pig_food",
            Strider => "minecraft:strider_food",
            Chicken => "minecraft:chicken_food",
            Rabbit => "minecraft:rabbit_food",
            Horse | Donkey | Mule => "minecraft:horse_food",
            Llama | TraderLlama => "minecraft:llama_food",
            Wolf => "minecraft:wolf_food",
            Cat => "minecraft:cat_food",
            Fox => "minecraft:fox_food",
            Parrot => "minecraft:parrot_food",
            // Land animals (Cursor)
            Goat => "minecraft:goat_food",
            Ocelot => "minecraft:ocelot_food",
            Hoglin => "minecraft:hoglin_food",
            Armadillo => "minecraft:armadillo_food",
            Panda => "minecraft:panda_food",
            Camel => "minecraft:camel_food",
            Sniffer => "minecraft:sniffer_food",
            // Aquatic (Claude)
            Turtle => "minecraft:turtle_food",
            Axolotl => "minecraft:axolotl_food",
            Frog => "minecraft:frog_food",
            Nautilus | ZombieNautilus => "minecraft:nautilus_food",
            Dolphin => "minecraft:fishes",
            // Flying mobs (flying-mobs)
            Bee => "minecraft:bee_food",
            SulfurCube => "minecraft:sulfur_cube_food",
            _ => return None,
        })
    }

    /// The item tag it follows a player for, if not its food.
    pub fn tempt_tag(self) -> Option<&'static str> {
        use Species::*;
        match self {
            Horse | Donkey | Mule => Some("minecraft:horse_tempt_items"),
            Llama | TraderLlama => Some("minecraft:llama_tempt_items"),
            Strider => Some("minecraft:strider_tempt_items"),
            // Land animals (Cursor)
            CamelHusk => Some("minecraft:camel_husk_food"),
            // Flying mobs (flying-mobs)
            HappyGhast => Some("minecraft:happy_ghast_tempt_items"),
            Wolf | Parrot => None,
            _ => self.food_tag(),
        }
    }

    /// The ender dragon and the wither: they have their own tick, a boss
    /// bar, and no effects take hold of them.
    pub fn boss(self) -> bool {
        matches!(self, Species::EnderDragon | Species::Wither)
    }

    /// Whether it is a horse of some kind (saddle, temper, chest).
    pub fn horse(self) -> bool {
        matches!(self, Species::Horse | Species::Donkey | Species::Mule)
    }

    /// Llamas: chests, carpets and a strength, but nobody rides them.
    pub fn llama(self) -> bool {
        matches!(self, Species::Llama | Species::TraderLlama)
    }

    /// Donkeys, mules and llamas carry a chest.
    pub fn chested(self) -> bool {
        matches!(self, Species::Donkey | Species::Mule) || self.llama()
    }

    /// Tamed by its owner and follows them.
    pub fn tameable(self) -> bool {
        matches!(
            self,
            Species::Wolf
                | Species::Cat
                | Species::Parrot
                | Species::Horse
                | Species::Donkey
                | Species::Mule
                | Species::Llama
                | Species::TraderLlama
        )
    }

    /// The zombie kind (doors, villagers, babies, conversions).
    pub fn zombie(self) -> bool {
        matches!(
            self,
            Species::Zombie | Species::Husk | Species::Drowned | Species::ZombieVillager
        )
    }

    pub fn skeleton(self) -> bool {
        matches!(
            self,
            Species::Skeleton
                | Species::Stray
                | Species::WitherSkeleton
                // Flying mobs (flying-mobs)
                | Species::Bogged
                | Species::Parched
        )
    }

    /// Illagers: pillagers, vindicators, evokers, illusioners.
    pub fn illager(self) -> bool {
        matches!(
            self,
            Species::Pillager | Species::Vindicator | Species::Evoker | Species::Illusioner
        )
    }

    /// Those that fight in raids: illagers, witches and ravagers.
    pub fn raider(self) -> bool {
        self.illager() || matches!(self, Species::Witch | Species::Ravager)
    }

    /// Arthropods: Bane of Arthropods hurts them more and slows them.
    pub fn arthropod(self) -> bool {
        matches!(
            self,
            Species::Spider
                | Species::CaveSpider
                | Species::Silverfish
                | Species::Endermite
                | Species::Bee
        )
    }

    /// Experience a player gets for killing an adult (equipment adds more,
    /// see the server).
    pub fn xp_reward(self) -> (i32, i32) {
        use Species::*;
        match self.category() {
            Category::Misc | Category::Ambient => (0, 0),
            Category::Creature
            | Category::WaterCreature
            | Category::WaterAmbient
            | Category::UndergroundWater
            | Category::Axolotls => (1, 3),
            Category::Monster => match self {
                Blaze => (10, 10),
                Endermite => (3, 3),

                // Flying mobs (flying-mobs)
                Breeze => (10, 10),
                Creaking => (0, 0),
                Warden => (5, 5),
                SulfurCube => (1, 2),
                PiglinBrute | Ravager => (20, 20),
                Evoker => (10, 10),
                // Aquatic (Claude)
                Guardian | ElderGuardian => (10, 10),
                Wither => (50, 50),
                // The dragon's experience comes out as it dies.
                Slime | MagmaCube | EnderDragon => (0, 0),
                _ => (5, 5),
            },
        }
    }

    /// A sound event name without `minecraft:`, for a baby when it has its
    /// own.
    pub fn sound(self, which: Sound, baby: bool) -> Option<String> {
        use Species::*;
        // Aquatic (Claude): their voices have names of their own.
        if let Some(voice) = super::aqua::voice(self, which, baby) {
            return voice;
        }
        let base = match self {
            Mooshroom => "cow",
            CaveSpider => "spider",
            Mannequin => "generic",
            _ => self.path(),
        };
        let ok = match which {
            Sound::Ambient => !matches!(
                self,
                Creeper
                    | Slime
                    | MagmaCube
                    | IronGolem
                    | Bee
                    | CopperGolem
                    | SulfurCube
                    | Warden
                    | Mannequin
            ),
            Sound::Step => matches!(
                self,
                Cow | Mooshroom
                    | Sheep
                    | Pig
                    | Chicken
                    | Wolf
                    | Zombie
                    | Husk
                    | Skeleton
                    | Stray
                    | WitherSkeleton
                    | Spider
                    | CaveSpider
                    | ZombieVillager
                    | Piglin
                    | Drowned
                    | Horse
                    | Silverfish
                    | Endermite
                    | IronGolem
                    // Land animals (Cursor)
                    | Goat
                    | PolarBear
                    | Hoglin
                    | Zoglin
                    | PiglinBrute
                    | SkeletonHorse
                    | ZombieHorse
                    | Armadillo
                    | Panda
                    | Camel
                    | CamelHusk
                    | Sniffer
                    // Flying mobs (flying-mobs)
                    | Bogged
                    | Parched
                    | Creaking
                    | CopperGolem
                    | Ravager
                    | Warden
            ),
            Sound::Hurt | Sound::Death => true,
        };
        if !ok {
            return None;
        }
        let suffix = match which {
            Sound::Ambient => "ambient",
            Sound::Hurt => "hurt",
            Sound::Death => "death",
            Sound::Step => "step",
        };
        // Babies of some animals have their own voices.
        if baby && matches!(base, "cat" | "chicken" | "horse" | "pig" | "wolf") {
            return Some(format!("entity.baby_{base}.{suffix}"));
        }
        Some(format!("entity.{base}.{suffix}"))
    }

    /// Ticks between ambient sounds on average.
    pub fn ambient_interval(self) -> i32 {
        match self {
            Species::Ghast => 80,
            _ => 80,
        }
    }

    /// How loud its sounds are.
    pub fn volume(self) -> f32 {
        match self {
            Species::Ghast | Species::EnderDragon => 5.0,
            Species::Wither => 2.0,
            Species::Wolf | Species::Cat | Species::Rabbit => 0.4,
            Species::Horse
            | Species::Donkey
            | Species::Mule
            | Species::Llama
            | Species::TraderLlama => 0.8,
            Species::Slime | Species::MagmaCube => 0.4,
            // Flying mobs (flying-mobs)
            Species::Bat => 0.1,
            _ => 1.0,
        }
    }
}
