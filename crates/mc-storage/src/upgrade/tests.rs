//! Whole documents saved by old versions, built by hand, against the 26.3
//! format (as `encode_chunk` and the 26.3 server write it).

use std::path::PathBuf;

use mc_nbt::{Compound, Tag};

use super::nbt::{compound, list, string};
use super::*;

#[test]
fn the_target_is_the_extracted_version() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../cache/mcdata/26.3/version.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["world_version"].as_i64(), Some(i64::from(TARGET)));
}

fn c(pairs: Vec<(&str, Tag)>) -> Compound {
    let mut out = Compound::new();
    for (k, v) in pairs {
        out.insert(k, v);
    }
    out
}

fn s(text: &str) -> Tag {
    Tag::String(text.into())
}

fn old_item(id: &str, count: i8, slot: i8) -> Tag {
    Tag::Compound(c(vec![
        ("Slot", Tag::Byte(slot)),
        ("id", s(id)),
        ("Count", Tag::Byte(count)),
    ]))
}

/// A 1.20.1 (3465) chunk: grass, a chest with stone, a sign, a jukebox.
fn chunk_1_20_1() -> Compound {
    let palette = Tag::List(vec![
        Tag::Compound(c(vec![("Name", s("minecraft:air"))])),
        Tag::Compound(c(vec![("Name", s("minecraft:grass"))])),
        Tag::Compound(c(vec![
            ("Name", s("minecraft:chain")),
            ("Properties", Tag::Compound(c(vec![("axis", s("y"))]))),
        ])),
    ]);
    let section = c(vec![
        ("Y", Tag::Byte(4)),
        (
            "block_states",
            Tag::Compound(c(vec![
                ("palette", palette),
                ("data", Tag::LongArray(vec![0; 256])),
            ])),
        ),
        ("SkyLight", Tag::ByteArray(vec![0; 2048])),
    ]);
    let chest = c(vec![
        ("id", s("minecraft:chest")),
        ("x", Tag::Int(1)),
        ("y", Tag::Int(64)),
        ("z", Tag::Int(2)),
        ("Items", Tag::List(vec![old_item("minecraft:grass", 3, 0)])),
        ("Lock", s("key")),
    ]);
    let sign = c(vec![
        ("id", s("minecraft:sign")),
        ("x", Tag::Int(2)),
        ("y", Tag::Int(64)),
        ("z", Tag::Int(2)),
        (
            "front_text",
            Tag::Compound(c(vec![(
                "messages",
                Tag::List(vec![
                    s(r#"{"text":"Hi","bold":"true"}"#),
                    s(r#"{"text":""}"#),
                    s(r#"{"text":""}"#),
                    s(r#"{"text":""}"#),
                ]),
            )])),
        ),
        ("_filtered_correct", Tag::Byte(1)),
    ]);
    let furnace = c(vec![
        ("id", s("minecraft:furnace")),
        ("x", Tag::Int(3)),
        ("y", Tag::Int(64)),
        ("z", Tag::Int(2)),
        ("BurnTime", Tag::Short(20)),
    ]);
    c(vec![
        ("DataVersion", Tag::Int(3465)),
        ("xPos", Tag::Int(0)),
        ("zPos", Tag::Int(0)),
        ("Status", s("minecraft:full")),
        ("isLightOn", Tag::Byte(1)),
        ("sections", Tag::List(vec![Tag::Compound(section)])),
        (
            "block_entities",
            Tag::List(vec![
                Tag::Compound(chest),
                Tag::Compound(sign),
                Tag::Compound(furnace),
            ]),
        ),
    ])
}

#[test]
fn a_1_20_1_chunk_reads_as_26_3() {
    let mut chunk = chunk_1_20_1();
    assert_eq!(
        upgrade(&mut chunk, Kind::Chunk, &Context::default(), TARGET),
        Ok(true)
    );
    assert_eq!(chunk.get("DataVersion"), Some(&Tag::Int(TARGET)));
    // 4537 dropped stored light; the server relights.
    assert!(chunk.get("isLightOn").is_none());
    let [Tag::Compound(section)] = list(&chunk, "sections") else {
        panic!("no section");
    };
    assert!(section.get("SkyLight").is_none());
    let palette = list(compound(section, "block_states").unwrap(), "palette");
    let names: Vec<_> = palette
        .iter()
        .map(|t| match t {
            Tag::Compound(e) => string(e, "id").unwrap_or("?").to_owned(),
            _ => "?".into(),
        })
        .collect();
    assert_eq!(
        names,
        [
            "minecraft:air",
            "minecraft:short_grass",
            "minecraft:iron_chain"
        ]
    );
    let Tag::Compound(chain) = &palette[2] else {
        panic!("chain is not a compound");
    };
    assert!(compound(chain, "properties").is_some());

    let bes = list(&chunk, "block_entities");
    let Tag::Compound(chest) = &bes[0] else {
        panic!("no chest");
    };
    let [Tag::Compound(item)] = list(chest, "Items") else {
        panic!("no item");
    };
    assert_eq!(item.get("id"), Some(&s("minecraft:short_grass")));
    assert_eq!(item.get("count"), Some(&Tag::Int(3)));
    assert_eq!(item.get("Slot"), Some(&Tag::Byte(0)));
    // The lock is an item predicate: items named "key", the name as NBT text.
    let lock = compound(chest, "lock").expect("lock");
    let components = compound(lock, "components").expect("lock components");
    assert_eq!(components.get("minecraft:custom_name"), Some(&s("key")));

    let Tag::Compound(sign) = &bes[1] else {
        panic!("no sign");
    };
    let front = compound(sign, "front_text").unwrap();
    let Tag::Compound(first) = &list(front, "messages")[0] else {
        panic!("sign text is not NBT");
    };
    assert_eq!(first.get("text"), Some(&s("Hi")));
    assert_eq!(first.get("bold"), Some(&Tag::Byte(1)));
    assert_eq!(sign.get("allow_op_features"), Some(&Tag::Byte(1)));
    assert!(sign.get("_filtered_correct").is_none());

    let Tag::Compound(furnace) = &bes[2] else {
        panic!("no furnace");
    };
    assert_eq!(furnace.get("lit_time_remaining"), Some(&Tag::Short(20)));
    assert_eq!(furnace.get("lit_total_time"), Some(&Tag::Short(20)));
    // Blending from 4997 marks it as an old Overworld chunk.
    assert!(chunk.get("blending_data").is_some());

    // A second pass changes nothing.
    let before = chunk.clone();
    assert_eq!(
        upgrade(&mut chunk, Kind::Chunk, &Context::default(), TARGET),
        Ok(false)
    );
    assert_eq!(chunk, before);
}

#[test]
fn old_entities_get_26_3_fields() {
    let zombie = c(vec![
        ("id", s("minecraft:zombie")),
        (
            "Pos",
            Tag::List(vec![Tag::Double(0.5), Tag::Double(64.0), Tag::Double(0.5)]),
        ),
        ("FallDistance", Tag::Float(1.5)),
        (
            "HandItems",
            Tag::List(vec![
                Tag::Compound(c(vec![
                    ("id", s("minecraft:iron_sword")),
                    ("Count", Tag::Byte(1)),
                ])),
                Tag::Compound(Compound::new()),
            ]),
        ),
        (
            "ArmorItems",
            Tag::List(vec![Tag::Compound(Compound::new()); 4]),
        ),
        (
            "Attributes",
            Tag::List(vec![Tag::Compound(c(vec![
                ("Name", s("minecraft:generic.max_health")),
                ("Base", Tag::Double(20.0)),
            ]))]),
        ),
        ("CustomName", s(r#"{"text":"Bob"}"#)),
        (
            "ActiveEffects",
            Tag::List(vec![Tag::Compound(c(vec![
                ("Id", Tag::Int(12)),
                ("Duration", Tag::Int(40)),
            ]))]),
        ),
    ]);
    let boat = c(vec![
        ("id", s("minecraft:boat")),
        ("Pos", Tag::List(vec![Tag::Double(0.0); 3])),
        ("Type", s("birch")),
        (
            "Passengers",
            Tag::List(vec![Tag::Compound(c(vec![
                ("id", s("minecraft:pig")),
                ("Saddle", Tag::Byte(1)),
            ]))]),
        ),
    ]);
    let mut root = c(vec![
        ("DataVersion", Tag::Int(2975)),
        ("Position", Tag::IntArray(vec![0, 0])),
        (
            "Entities",
            Tag::List(vec![Tag::Compound(zombie), Tag::Compound(boat)]),
        ),
    ]);
    assert_eq!(
        upgrade(&mut root, Kind::EntityChunk, &Context::default(), TARGET),
        Ok(true)
    );
    let ents = list(&root, "Entities");
    let Tag::Compound(z) = &ents[0] else {
        panic!("no zombie");
    };
    assert_eq!(z.get("fall_distance"), Some(&Tag::Double(1.5)));
    let eq = compound(z, "equipment").expect("equipment");
    let sword = compound(eq, "mainhand").expect("mainhand");
    assert_eq!(sword.get("count"), Some(&Tag::Int(1)));
    let [Tag::Compound(attr)] = list(z, "attributes") else {
        panic!("no attributes");
    };
    assert_eq!(attr.get("id"), Some(&s("minecraft:max_health")));
    assert_eq!(attr.get("base"), Some(&Tag::Double(20.0)));
    let Some(Tag::Compound(name)) = z.get("CustomName") else {
        panic!("name is not NBT");
    };
    assert_eq!(name.get("text"), Some(&s("Bob")));
    let [Tag::Compound(eff)] = list(z, "active_effects") else {
        panic!("no effects");
    };
    assert_eq!(eff.get("id"), Some(&s("minecraft:fire_resistance")));

    let Tag::Compound(boat) = &ents[1] else {
        panic!("no boat");
    };
    assert_eq!(boat.get("id"), Some(&s("minecraft:birch_boat")));
    let [Tag::Compound(pig)] = list(boat, "Passengers") else {
        panic!("no pig");
    };
    let eq = compound(pig, "equipment").expect("pig equipment");
    assert_eq!(
        compound(eq, "saddle").and_then(|s| string(s, "id")),
        Some("minecraft:saddle")
    );
}

#[test]
fn too_old_and_too_new_are_refused() {
    let mut old = c(vec![("DataVersion", Tag::Int(1343))]);
    assert_eq!(
        upgrade(&mut old, Kind::Chunk, &Context::default(), TARGET),
        Err(Unsupported::TooOld(Some(1343)))
    );
    let mut none = Compound::new();
    assert_eq!(
        upgrade(&mut none, Kind::Chunk, &Context::default(), TARGET),
        Err(Unsupported::TooOld(None))
    );
    let mut new = c(vec![("DataVersion", Tag::Int(TARGET + 1))]);
    assert_eq!(
        upgrade(&mut new, Kind::Chunk, &Context::default(), TARGET),
        Err(Unsupported::TooNew(TARGET + 1))
    );
}

#[test]
fn an_old_player_gets_equipment_and_respawn() {
    let mut p = c(vec![
        ("DataVersion", Tag::Int(3465)),
        ("Pos", Tag::List(vec![Tag::Double(0.0); 3])),
        (
            "Inventory",
            Tag::List(vec![
                old_item("minecraft:scute", 2, 0),
                old_item("minecraft:diamond_helmet", 1, 103),
            ]),
        ),
        ("SpawnX", Tag::Int(5)),
        ("SpawnY", Tag::Int(70)),
        ("SpawnZ", Tag::Int(5)),
        ("SpawnDimension", s("minecraft:overworld")),
    ]);
    assert_eq!(files::upgrade_player(&mut p, TARGET), Ok(true));
    let [Tag::Compound(scute)] = list(&p, "Inventory") else {
        panic!("inventory");
    };
    assert_eq!(scute.get("id"), Some(&s("minecraft:turtle_scute")));
    let eq = compound(&p, "equipment").unwrap();
    assert_eq!(
        compound(eq, "head").and_then(|h| h.get("count")),
        Some(&Tag::Int(1))
    );
    let respawn = compound(&p, "respawn").unwrap();
    assert_eq!(respawn.get("pos"), Some(&Tag::IntArray(vec![5, 70, 5])));
    assert_eq!(respawn.get("yaw"), Some(&Tag::Float(0.0)));
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("mc-files-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_region(dir: &std::path::Path, root: &Compound) {
    std::fs::create_dir_all(dir).unwrap();
    let mut bytes = Vec::new();
    mc_nbt::write_named("", &Tag::Compound(root.clone()), &mut bytes).unwrap();
    crate::RegionStorage::new(dir)
        .write_chunk(0, 0, &bytes, 1)
        .unwrap();
}

fn read_region(dir: &std::path::Path) -> Compound {
    match crate::RegionStorage::new(dir).read_chunk(0, 0).unwrap() {
        Some(Tag::Compound(root)) => root,
        other => panic!("no chunk in {}: {other:?}", dir.display()),
    }
}

/// A Paper 1.20.1 server folder: `world` with the Overworld and players,
/// `world_nether/DIM-1`, `world_the_end/DIM1`, settings in level.dat.
#[test]
fn a_paper_1_20_world_moves_to_the_26_3_layout() {
    let scratch = Scratch::new("paper");
    let world = scratch.0.join("world");
    let level = c(vec![(
        "Data",
        Tag::Compound(c(vec![
            ("DataVersion", Tag::Int(3465)),
            ("SpawnX", Tag::Int(8)),
            ("SpawnY", Tag::Int(65)),
            ("SpawnZ", Tag::Int(-8)),
            ("Difficulty", Tag::Byte(1)),
            ("Time", Tag::Long(1000)),
            ("DayTime", Tag::Long(13000)),
            ("raining", Tag::Byte(1)),
            ("BorderSize", Tag::Double(500.0)),
            ("BorderCenterX", Tag::Double(80.0)),
            (
                "GameRules",
                Tag::Compound(c(vec![
                    ("keepInventory", s("true")),
                    ("doDaylightCycle", s("false")),
                ])),
            ),
            (
                "WorldGenSettings",
                Tag::Compound(c(vec![
                    ("seed", Tag::Long(42)),
                    ("generate_features", Tag::Byte(1)),
                ])),
            ),
            (
                "DragonFight",
                Tag::Compound(c(vec![("DragonKilled", Tag::Byte(1))])),
            ),
        ])),
    )]);
    files::write_gz(&world.join("level.dat"), &level).unwrap();
    let chunk = chunk_1_20_1();
    write_region(&world.join("region"), &chunk);
    let mut nether = chunk.clone();
    nether.insert("DataVersion", Tag::Int(3465));
    write_region(&scratch.0.join("world_nether/DIM-1/region"), &nether);
    write_region(&scratch.0.join("world_the_end/DIM1/region"), &nether);
    std::fs::write(scratch.0.join("world_the_end/uid.dat"), [7u8; 16]).unwrap();
    let player = c(vec![
        ("DataVersion", Tag::Int(3465)),
        ("Pos", Tag::List(vec![Tag::Double(0.0); 3])),
        (
            "Inventory",
            Tag::List(vec![old_item("minecraft:grass", 1, 0)]),
        ),
    ]);
    let uuid = "0a0b0c0d-0000-0000-0000-000000000001.dat";
    files::write_gz(&world.join("playerdata").join(uuid), &player).unwrap();
    files::write_gz(
        &world.join("data/scoreboard.dat"),
        &c(vec![
            ("DataVersion", Tag::Int(3465)),
            (
                "data",
                Tag::Compound(c(vec![(
                    "DisplaySlots",
                    Tag::Compound(c(vec![("slot_1", s("kills"))])),
                )])),
            ),
        ]),
    )
    .unwrap();
    files::write_gz(
        &world.join("data/map_3.dat"),
        &c(vec![
            ("DataVersion", Tag::Int(3465)),
            ("data", Tag::Compound(Compound::new())),
        ]),
    )
    .unwrap();

    let report = files::fix_world(&world, TARGET, "26.3").unwrap();
    assert_eq!(report.from, Some(3465));
    assert_eq!(report.paper.len(), 2);
    let dims = world.join("dimensions/minecraft");
    for d in ["overworld", "the_nether", "the_end"] {
        assert!(
            dims.join(d).join("region/r.0.0.mca").is_file(),
            "{d} region"
        );
    }
    assert!(!world.join("region").exists() && !world.join("DIM-1").exists());
    assert!(scratch.0.join("world_the_end/uid.dat").is_file());
    assert!(world.join("players/data").join(uuid).is_file());
    assert!(world.join("data/minecraft/maps/3.dat").is_file());

    let level = files::read_gz(&world.join("level.dat")).unwrap();
    let data = compound(&level, "Data").unwrap();
    assert_eq!(data.get("DataVersion"), Some(&Tag::Int(TARGET)));
    assert_eq!(
        compound(data, "spawn").and_then(|s| s.get("pos")),
        Some(&Tag::IntArray(vec![8, 65, -8]))
    );
    assert_eq!(
        compound(data, "difficulty_settings").and_then(|d| d.get("difficulty")),
        Some(&s("easy"))
    );
    for gone in [
        "GameRules",
        "game_rules",
        "world_border",
        "DragonFight",
        "raining",
        "WorldGenSettings",
    ] {
        assert!(data.get(gone).is_none(), "{gone} still in level.dat");
    }
    assert!(world.join("level.dat_old").is_file());

    let saved = |p: &str| -> Compound {
        let root = files::read_gz(&world.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        compound(&root, "data").cloned().unwrap()
    };
    let rules = saved("data/minecraft/game_rules.dat");
    assert_eq!(rules.get("minecraft:keep_inventory"), Some(&Tag::Byte(1)));
    assert_eq!(rules.get("minecraft:advance_time"), Some(&Tag::Byte(0)));
    let gen_settings = saved("data/minecraft/world_gen_settings.dat");
    assert_eq!(gen_settings.get("seed"), Some(&Tag::Long(42)));
    assert_eq!(gen_settings.get("generate_structures"), Some(&Tag::Byte(1)));
    let clocks = saved("data/minecraft/world_clocks.dat");
    let overworld = compound(&clocks, "minecraft:overworld").unwrap();
    assert_eq!(overworld.get("total_ticks"), Some(&Tag::Long(13000)));
    assert_eq!(
        saved("data/minecraft/weather.dat").get("raining"),
        Some(&Tag::Byte(1))
    );
    let nether_border = saved("dimensions/minecraft/the_nether/data/minecraft/world_border.dat");
    assert_eq!(nether_border.get("center_x"), Some(&Tag::Double(10.0)));
    let fight = saved("dimensions/minecraft/the_end/data/minecraft/ender_dragon_fight.dat");
    assert_eq!(fight.get("dragon_killed"), Some(&Tag::Byte(1)));
    let board = saved("data/minecraft/scoreboard.dat");
    assert!(
        compound(&board, "DisplaySlots")
            .unwrap()
            .get("sidebar")
            .is_some()
    );

    let mut notes = Vec::new();
    let r = world::upgrade_world(&world, TARGET, false, &mut |n| notes.push(n)).unwrap();
    assert_eq!(r.chunks_left, 0, "{notes:?}");
    assert_eq!(r.chunks_rewritten, 3);
    assert_eq!(r.players, 1);
    let nether = read_region(&dims.join("the_nether/region"));
    assert_eq!(nether.get("DataVersion"), Some(&Tag::Int(TARGET)));
    // Blending is only for the Overworld.
    assert!(nether.get("blending_data").is_none());
    let player = files::read_gz(&world.join("players/data").join(uuid)).unwrap();
    let [Tag::Compound(item)] = list(&player, "Inventory") else {
        panic!("inventory");
    };
    assert_eq!(item.get("id"), Some(&s("minecraft:short_grass")));

    // Nothing left to do the second time.
    let again = files::fix_world(&world, TARGET, "26.3").unwrap();
    assert!(again.moved.is_empty());
    let r = world::upgrade_world(&world, TARGET, true, &mut |_| {}).unwrap();
    assert_eq!(r.chunks_current, 3, "nothing cached is left to erase");
    let r = world::upgrade_world(&world, TARGET, false, &mut |_| {}).unwrap();
    assert_eq!((r.chunks_rewritten, r.players), (0, 0));
}

#[test]
fn a_pre_1_18_world_is_refused() {
    let scratch = Scratch::new("old");
    let world = scratch.0.join("world");
    let level = c(vec![(
        "Data",
        Tag::Compound(c(vec![("DataVersion", Tag::Int(2730))])),
    )]);
    files::write_gz(&world.join("level.dat"), &level).unwrap();
    let err = files::fix_world(&world, TARGET, "26.3").unwrap_err();
    assert!(err.to_string().contains("1.18"), "{err}");
}
