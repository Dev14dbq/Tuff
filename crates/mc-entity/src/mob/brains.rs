//! The goals of each kind of mob, by priority (minecraft.wiki, the "Behavior"
//! section of each mob's page).

use super::goals::{Avoidee, Goal, Ranged, Slot};
use super::land::Act;
use super::{Brain, JUMP, LOOK, MOVE, Species, TARGET};

fn slot(priority: u8, flags: u8, goal: Goal) -> Slot {
    Slot {
        priority,
        flags,
        goal,
        running: false,
    }
}

fn float() -> Slot {
    slot(0, JUMP, Goal::Float)
}

fn look(p: u8, range: f64) -> Slot {
    slot(p, LOOK, Goal::LookAtPlayer { range, left: 0 })
}

fn random_look(p: u8) -> Slot {
    slot(
        p,
        MOVE | LOOK,
        Goal::RandomLook {
            left: 0,
            angle: 0.0,
        },
    )
}

fn stroll(p: u8, speed: f64) -> Slot {
    slot(
        p,
        MOVE,
        Goal::Stroll {
            speed,
            interval: 120,
            avoid_water: true,
        },
    )
}

fn panic(p: u8, speed: f64) -> Slot {
    slot(p, MOVE, Goal::Panic { speed })
}

fn breed(p: u8, speed: f64) -> Slot {
    slot(
        p,
        MOVE | LOOK,
        Goal::Breed {
            speed,
            partner: None,
            timer: 0,
        },
    )
}

fn tempt(p: u8, speed: f64) -> Slot {
    slot(
        p,
        MOVE | LOOK,
        Goal::Tempt {
            speed,
            cooldown: 0,
            player: None,
        },
    )
}

fn follow_parent(p: u8, speed: f64) -> Slot {
    slot(
        p,
        MOVE,
        Goal::FollowParent {
            speed,
            parent: None,
            delay: 0,
        },
    )
}

fn melee(p: u8, speed: f64, fickle: bool) -> Slot {
    slot(p, MOVE | LOOK, Goal::Melee { speed, fickle })
}

fn hurt_by(p: u8, alert: bool) -> Slot {
    slot(p, TARGET, Goal::HurtBy { alert })
}

fn nearest(p: u8, range: f64, fickle: bool) -> Slot {
    slot(
        p,
        TARGET,
        Goal::Nearest {
            range,
            fickle,
            spare_gold: false,
            unseen: 0,
        },
    )
}

fn prey(p: u8, kinds: &'static [Species], villagers: bool, wild_only: bool) -> Slot {
    slot(
        p,
        TARGET,
        Goal::Prey {
            kinds,
            villagers,
            wild_only,
            unseen: 0,
        },
    )
}

fn avoid(p: u8, from: Avoidee, range: f64, walk: f64, sprint: f64) -> Slot {
    slot(
        p,
        MOVE,
        Goal::Avoid {
            from,
            range,
            walk,
            sprint,
            threat: None,
        },
    )
}

fn ranged(p: u8, kind: Ranged, interval: i32, range: f64) -> Slot {
    slot(
        p,
        MOVE | LOOK,
        Goal::Ranged {
            kind,
            interval,
            range,
            timer: -1,
            seen: 0,
        },
    )
}

fn sit(p: u8) -> Slot {
    slot(p, MOVE | JUMP, Goal::Sit)
}

fn follow_owner(p: u8, speed: f64, start: f64, stop: f64) -> Slot {
    slot(p, MOVE | LOOK, Goal::FollowOwner { speed, start, stop })
}

fn perch(p: u8, bed: bool, speed: f64) -> Slot {
    slot(
        p,
        MOVE | JUMP,
        Goal::Perch {
            bed,
            speed,
            spot: None,
            countdown: 0,
            tries: 0,
            stay: 0,
        },
    )
}

fn leap(p: u8, up: f64) -> Slot {
    slot(p, MOVE | JUMP, Goal::Leap { up })
}

/// The animals of a farm: float, panic, breed, tempt, follow the parents,
/// stroll and look about.
fn farm(speeds: [f64; 5]) -> Vec<Slot> {
    let [
        panic_speed,
        breed_speed,
        tempt_speed,
        parent_speed,
        stroll_speed,
    ] = speeds;
    vec![
        float(),
        panic(1, panic_speed),
        breed(2, breed_speed),
        tempt(3, tempt_speed),
        follow_parent(4, parent_speed),
        stroll(6, stroll_speed),
        look(7, 6.0),
        random_look(8),
    ]
}

const WOLF_PREY: &[Species] = &[Species::Sheep, Species::Rabbit, Species::Fox];
const SKELETONS: &[Species] = &[Species::Skeleton, Species::Stray, Species::WitherSkeleton];
const CATS: &[Species] = &[Species::Cat];
const WOLVES: &[Species] = &[Species::Wolf];
const LLAMAS: &[Species] = &[Species::Llama, Species::TraderLlama];
const FOX_PREY: &[Species] = &[Species::Chicken, Species::Rabbit];
const RABBITS: &[Species] = &[Species::Rabbit];
const ENDERMITES: &[Species] = &[Species::Endermite];
const IRON_GOLEMS: &[Species] = &[Species::IronGolem];
// Land animals (Cursor)
const FOXES: &[Species] = &[Species::Fox];
const CHICKENS: &[Species] = &[Species::Chicken];
const WITHER_SKELETONS: &[Species] = &[Species::WitherSkeleton, Species::Wither];
/// What a zoglin goes for: every mob but creepers, ghasts and zoglins.
const ZOGLIN_PREY: &[Species] = &[
    Species::Cow,
    Species::Mooshroom,
    Species::Sheep,
    Species::Pig,
    Species::Chicken,
    Species::Rabbit,
    Species::Horse,
    Species::Donkey,
    Species::Mule,
    Species::Llama,
    Species::TraderLlama,
    Species::Wolf,
    Species::Cat,
    Species::Fox,
    Species::Goat,
    Species::PolarBear,
    Species::Ocelot,
    Species::Zombie,
    Species::Husk,
    Species::Skeleton,
    Species::Stray,
    Species::Spider,
    Species::CaveSpider,
    Species::Enderman,
    Species::Piglin,
    Species::PiglinBrute,
    Species::Hoglin,
    Species::Witch,
    Species::IronGolem,
    Species::SnowGolem,
];
/// What golems hunt: the monsters that walk (not creepers for the iron
/// golem, flyers and bosses for neither).
const GOLEM_PREY: &[Species] = &[
    Species::Zombie,
    Species::Husk,
    Species::Drowned,
    Species::ZombieVillager,
    Species::ZombifiedPiglin,
    Species::Skeleton,
    Species::Stray,
    Species::WitherSkeleton,
    Species::Spider,
    Species::CaveSpider,
    Species::Enderman,
    Species::Witch,
    Species::Slime,
    Species::MagmaCube,
    Species::Piglin,
    Species::Silverfish,
    Species::Endermite,
];
const SNOW_GOLEM_PREY: &[Species] = &[
    Species::Zombie,
    Species::Husk,
    Species::Drowned,
    Species::ZombieVillager,
    Species::ZombifiedPiglin,
    Species::Skeleton,
    Species::Stray,
    Species::WitherSkeleton,
    Species::Spider,
    Species::CaveSpider,
    Species::Enderman,
    Species::Witch,
    Species::Slime,
    Species::MagmaCube,
    Species::Piglin,
    Species::Silverfish,
    Species::Endermite,
    Species::Creeper,
    Species::Blaze,
];

pub(super) fn brain(species: Species) -> Brain {
    use Species::*;
    let (goals, targets) = match species {
        Cow | Mooshroom => (farm([2.0, 1.0, 1.25, 1.25, 1.0]), vec![]),
        Pig => (farm([1.25, 1.0, 1.2, 1.1, 1.0]), vec![]),
        Strider => (
            vec![
                panic(1, 1.65),
                breed(2, 1.0),
                slot(
                    4,
                    MOVE,
                    Goal::SeekLava {
                        speed: 1.0,
                        interval: 0,
                    },
                ),
                tempt(3, 1.4),
                follow_parent(4, 1.1),
                stroll(5, 1.0),
                look(7, 8.0),
                random_look(8),
            ],
            vec![],
        ),
        Chicken => (farm([1.4, 1.0, 1.0, 1.1, 1.0]), vec![]),
        Sheep => {
            let mut g = farm([1.25, 1.0, 1.1, 1.1, 1.0]);
            g.push(slot(5, MOVE | LOOK | JUMP, Goal::EatGrass { left: 0 }));
            (g, vec![])
        }
        Rabbit => (
            vec![
                float(),
                panic(1, 2.2),
                breed(2, 0.8),
                tempt(3, 1.0),
                avoid(4, Avoidee::Players, 8.0, 2.2, 2.2),
                avoid(4, Avoidee::Kinds(WOLVES), 10.0, 2.2, 2.2),
                stroll(6, 0.6),
                look(11, 10.0),
            ],
            vec![],
        ),
        Llama | TraderLlama => (
            vec![
                float(),
                slot(2, MOVE, Goal::Caravan { speed: 2.1 }),
                ranged(3, Ranged::Spit, 40, 20.0),
                panic(4, 1.2),
                breed(5, 1.0),
                tempt(6, 1.25),
                follow_parent(6, 1.0),
                stroll(7, 0.7),
                look(8, 6.0),
                random_look(9),
            ],
            vec![hurt_by(1, false), prey(2, WOLVES, false, true)],
        ),
        Horse | Donkey | Mule => (
            vec![
                float(),
                panic(1, 1.2),
                breed(2, 1.0),
                tempt(3, 1.25),
                follow_parent(4, 1.0),
                stroll(6, 0.7),
                look(7, 6.0),
                random_look(8),
            ],
            vec![],
        ),
        Wolf => (
            vec![
                float(),
                sit(2),
                leap(4, 0.4),
                avoid(4, Avoidee::Kinds(LLAMAS), 24.0, 1.5, 1.5),
                melee(5, 1.0, false),
                follow_owner(6, 1.0, 10.0, 2.0),
                breed(7, 1.0),
                stroll(8, 1.0),
                look(10, 8.0),
                random_look(10),
            ],
            vec![
                slot(1, TARGET, Goal::OwnerHurtBy),
                slot(2, TARGET, Goal::OwnerHurt),
                hurt_by(3, true),
                prey(5, WOLF_PREY, false, true),
                prey(7, SKELETONS, false, false),
            ],
        ),
        Cat => (
            vec![
                float(),
                panic(1, 1.5),
                sit(2),
                tempt(4, 0.6),
                avoid(4, Avoidee::Players, 16.0, 0.8, 1.33),
                perch(5, true, 1.1),
                follow_owner(6, 1.0, 10.0, 5.0),
                perch(7, false, 0.8),
                leap(8, 0.3),
                melee(9, 1.0, false),
                breed(10, 0.8),
                stroll(11, 0.8),
                look(12, 10.0),
            ],
            vec![prey(1, RABBITS, false, true)],
        ),
        Fox => (
            vec![
                float(),
                panic(2, 2.2),
                breed(3, 1.0),
                avoid(4, Avoidee::Players, 16.0, 1.6, 1.4),
                avoid(4, Avoidee::Kinds(WOLVES), 8.0, 1.6, 1.4),
                slot(
                    6,
                    MOVE,
                    Goal::Shelter {
                        speed: 1.25,
                        interval: 100,
                    },
                ),
                melee(7, 1.2, false),
                slot(7, MOVE | LOOK | JUMP, Goal::Sleep),
                follow_parent(8, 1.25),
                leap(10, 0.4),
                stroll(11, 1.0),
                look(12, 24.0),
            ],
            vec![prey(4, FOX_PREY, false, false)],
        ),
        Parrot => (
            vec![
                panic(0, 1.25),
                float(),
                look(1, 8.0),
                sit(2),
                follow_owner(2, 1.0, 5.0, 1.0),
                slot(
                    3,
                    MOVE,
                    Goal::Hover {
                        range: 8.0,
                        rise: 0.0,
                    },
                ),
            ],
            vec![],
        ),
        Zombie | Husk | ZombieVillager => (
            vec![
                float(),
                slot(
                    1,
                    0,
                    Goal::BreakDoor {
                        door: None,
                        time: 0,
                        stage: -1,
                    },
                ),
                melee(2, 1.0, false),
                stroll(7, 1.0),
                look(8, 8.0),
                random_look(8),
            ],
            vec![
                hurt_by(1, true),
                nearest(2, species.follow_range(), false),
                prey(3, IRON_GOLEMS, true, false),
            ],
        ),
        Drowned => (
            vec![
                float(),
                ranged(2, Ranged::Trident, 40, 10.0),
                melee(2, 1.0, false),
                stroll(7, 1.0),
                look(8, 8.0),
                random_look(8),
            ],
            vec![
                hurt_by(1, true),
                nearest(2, species.follow_range(), false),
                prey(3, IRON_GOLEMS, true, false),
            ],
        ),
        Skeleton | Stray | WitherSkeleton => (
            vec![
                float(),
                slot(2, MOVE, Goal::FleeSun { speed: 1.0 }),
                avoid(3, Avoidee::Kinds(WOLVES), 6.0, 1.0, 1.2),
                ranged(4, Ranged::Bow, 20, 15.0),
                melee(4, 1.2, false),
                stroll(5, 1.0),
                look(6, 8.0),
                random_look(6),
            ],
            vec![
                hurt_by(1, false),
                nearest(2, 16.0, false),
                prey(3, IRON_GOLEMS, false, false),
            ],
        ),
        Creeper => (
            vec![
                float(),
                slot(2, MOVE, Goal::Swell),
                avoid(3, Avoidee::Kinds(CATS), 6.0, 1.0, 1.2),
                melee(4, 1.0, false),
                stroll(5, 0.8),
                look(6, 8.0),
                random_look(6),
            ],
            vec![nearest(1, 16.0, false), hurt_by(2, false)],
        ),
        Spider | CaveSpider => (
            vec![
                float(),
                leap(3, 0.4),
                melee(4, 1.0, true),
                stroll(5, 0.8),
                look(6, 8.0),
                random_look(6),
            ],
            vec![
                hurt_by(1, false),
                nearest(2, 16.0, true),
                prey(3, IRON_GOLEMS, false, false),
            ],
        ),
        Enderman => (
            vec![
                float(),
                slot(1, MOVE | JUMP, Goal::Freeze),
                melee(2, 1.0, false),
                stroll(7, 1.0),
                look(8, 8.0),
                random_look(8),
                slot(10, 0, Goal::LeaveBlock),
                slot(11, 0, Goal::TakeBlock),
            ],
            vec![
                slot(
                    1,
                    TARGET,
                    Goal::Stare {
                        pending: None,
                        aggro: 0,
                        far: 0,
                    },
                ),
                hurt_by(2, false),
                // Whatever the distance, in sight: endermites.
                prey(3, ENDERMITES, false, false),
            ],
        ),
        Witch => (
            vec![
                float(),
                ranged(2, Ranged::Potion, 60, 10.0),
                stroll(2, 1.0),
                look(3, 8.0),
                random_look(3),
            ],
            vec![hurt_by(1, false), nearest(2, 16.0, false)],
        ),
        Slime | MagmaCube => (
            vec![slot(1, MOVE | LOOK | JUMP, Goal::Hop { delay: 0 })],
            vec![nearest(1, 16.0, false)],
        ),
        Phantom => (
            vec![slot(
                1,
                MOVE | LOOK,
                Goal::Swoop {
                    timer: 100,
                    angle: 0.0,
                },
            )],
            vec![nearest(1, 64.0, false)],
        ),
        Ghast => (
            vec![
                slot(
                    5,
                    MOVE,
                    Goal::Hover {
                        range: 16.0,
                        rise: 6.0,
                    },
                ),
                ranged(7, Ranged::Fireball, 0, 64.0),
                look(7, 16.0),
            ],
            vec![nearest(1, 64.0, false)],
        ),
        Blaze => (
            vec![
                ranged(4, Ranged::Blaze, 0, 48.0),
                slot(
                    5,
                    MOVE,
                    Goal::Hover {
                        range: 6.0,
                        rise: 2.0,
                    },
                ),
                look(8, 8.0),
                random_look(8),
            ],
            vec![hurt_by(1, true), nearest(2, 48.0, false)],
        ),
        Piglin => (
            vec![
                float(),
                slot(1, MOVE | LOOK, Goal::Admire),
                melee(2, 1.0, false),
                stroll(7, 0.6),
                look(8, 8.0),
                random_look(8),
            ],
            vec![
                hurt_by(1, true),
                slot(
                    2,
                    TARGET,
                    Goal::Nearest {
                        range: 16.0,
                        fickle: false,
                        spare_gold: true,
                        unseen: 0,
                    },
                ),
            ],
        ),
        ZombifiedPiglin => (
            vec![
                float(),
                melee(2, 1.0, false),
                stroll(7, 1.0),
                look(8, 8.0),
                random_look(8),
            ],
            vec![hurt_by(1, true)],
        ),
        Silverfish => (
            vec![
                float(),
                slot(1, 0, Goal::WakeFriends),
                melee(4, 1.0, false),
                slot(
                    5,
                    MOVE,
                    Goal::Hide {
                        merge: None,
                        walk: None,
                    },
                ),
            ],
            vec![hurt_by(1, true), nearest(2, species.follow_range(), false)],
        ),
        Endermite => (
            vec![
                float(),
                melee(2, 1.0, false),
                stroll(3, 1.0),
                look(7, 8.0),
                random_look(8),
            ],
            vec![hurt_by(1, true), nearest(2, species.follow_range(), false)],
        ),
        IronGolem => (
            vec![
                melee(1, 1.0, false),
                slot(2, MOVE, Goal::Patrol { speed: 0.6 }),
                slot(4, MOVE | LOOK, Goal::OfferFlower { left: 0, to: None }),
                stroll(5, 0.6),
                look(7, 6.0),
                random_look(8),
            ],
            vec![
                hurt_by(2, false),
                slot(3, TARGET, Goal::Notorious { unseen: 0 }),
                prey(4, GOLEM_PREY, false, false),
            ],
        ),
        SnowGolem => (
            vec![
                ranged(1, Ranged::Snowball, 20, 10.0),
                stroll(2, 1.0),
                look(3, 6.0),
                random_look(4),
            ],
            vec![prey(1, SNOW_GOLEM_PREY, false, false)],
        ),
        // The bosses think in the server's own tick (`bosses`).
        EnderDragon | Wither => (vec![], vec![]),
        // Land animals (Cursor)
        Goat => (
            vec![
                float(),
                panic(1, 1.4),
                breed(2, 1.0),
                tempt(3, 1.25),
                slot(
                    4,
                    MOVE | LOOK,
                    Goal::Land(Act::Ram {
                        phase: 0,
                        at: None,
                        who: None,
                    }),
                ),
                slot(
                    5,
                    MOVE | JUMP,
                    Goal::Land(Act::LongJump {
                        left: 0,
                        spot: None,
                    }),
                ),
                follow_parent(6, 1.1),
                stroll(7, 1.0),
                look(8, 6.0),
                random_look(9),
            ],
            vec![],
        ),
        // Neutral: it fights back, and goes for foxes.
        PolarBear => (
            vec![
                float(),
                melee(1, 1.25, false),
                follow_parent(4, 1.25),
                stroll(5, 1.0),
                look(6, 6.0),
                random_look(7),
            ],
            vec![hurt_by(1, true), prey(3, FOXES, false, true)],
        ),
        // Shy of players, it stalks chickens.
        Ocelot => (
            vec![
                float(),
                tempt(3, 0.6),
                avoid(4, Avoidee::Players, 16.0, 0.8, 1.33),
                leap(7, 0.3),
                melee(8, 1.0, false),
                breed(9, 0.8),
                stroll(10, 0.8),
                look(11, 10.0),
            ],
            vec![prey(1, CHICKENS, false, false)],
        ),
        SkeletonHorse | ZombieHorse => (
            vec![
                float(),
                panic(1, 1.2),
                stroll(6, 0.7),
                look(7, 6.0),
                random_look(8),
            ],
            vec![],
        ),
        Hoglin => (
            vec![
                float(),
                melee(2, 1.0, false),
                breed(3, 1.0),
                follow_parent(4, 1.0),
                stroll(7, 0.4),
                look(8, 8.0),
                random_look(8),
            ],
            vec![hurt_by(1, true), nearest(2, 16.0, false)],
        ),
        Zoglin => (
            vec![
                float(),
                melee(2, 1.0, false),
                stroll(7, 0.4),
                look(8, 8.0),
                random_look(8),
            ],
            vec![
                hurt_by(1, false),
                nearest(2, 16.0, false),
                prey(3, ZOGLIN_PREY, true, false),
            ],
        ),
        // Gold means nothing to a brute.
        PiglinBrute => (
            vec![
                float(),
                melee(2, 1.0, false),
                stroll(7, 0.6),
                look(8, 8.0),
                random_look(8),
            ],
            vec![
                hurt_by(1, true),
                nearest(2, 12.0, false),
                prey(3, WITHER_SKELETONS, false, false),
            ],
        ),
    };
    Brain { goals, targets }
}
