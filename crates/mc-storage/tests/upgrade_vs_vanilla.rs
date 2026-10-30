//! Upgrades a hand-built 1.20.1 world and checks the result against a world a
//! vanilla 26.3 server saved (cache/vanilla-server/26.3-bench-world): the same
//! files in the same places, no field vanilla does not write, and chunks that
//! decode with the 26.3 block registry. Skipped when the vanilla world or the
//! extracted data is missing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};
use mc_storage::upgrade::{self, Context, Kind, files, world};
use mc_storage::{Decoded, Palettes, RegionStorage, decode_chunk};

const TARGET: i32 = 5023;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vanilla() -> Option<PathBuf> {
    let dir = root().join("cache/vanilla-server/26.3-bench-world/world");
    dir.join("level.dat").is_file().then_some(dir)
}

fn c(pairs: Vec<(&str, Tag)>) -> Compound {
    let mut out = Compound::new();
    for (k, v) in pairs {
        out.insert(k, v);
    }
    out
}

fn s(t: &str) -> Tag {
    Tag::String(t.into())
}

fn keys(c: &Compound) -> BTreeSet<String> {
    c.iter().map(|(k, _)| k.to_owned()).collect()
}

fn sub<'a>(c: &'a Compound, k: &str) -> &'a Compound {
    match c.get(k) {
        Some(Tag::Compound(x)) => x,
        _ => panic!("no {k}"),
    }
}

fn chunks(dir: &Path) -> Vec<Compound> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut storage = RegionStorage::new(dir);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("r.").and_then(|r| r.strip_suffix(".mca")) else {
            continue;
        };
        let mut parts = rest.split('.').map(|p| p.parse::<i32>().unwrap());
        let (rx, rz) = (parts.next().unwrap(), parts.next().unwrap());
        for x in 0..32 {
            for z in 0..32 {
                if let Ok(Some(Tag::Compound(chunk))) = storage.read_chunk(rx * 32 + x, rz * 32 + z)
                {
                    out.push(chunk);
                }
            }
        }
    }
    out
}

#[test]
fn vanilla_26_3_data_is_left_alone() {
    let Some(dir) = vanilla() else {
        eprintln!("skipped: no vanilla 26.3 world");
        return;
    };
    let dims = dir.join("dimensions/minecraft/overworld");
    for (sub, kind) in [
        ("region", Kind::Chunk),
        ("entities", Kind::EntityChunk),
        ("poi", Kind::Poi),
    ] {
        for mut chunk in chunks(&dims.join(sub)) {
            let before = chunk.clone();
            assert_eq!(
                upgrade::upgrade(&mut chunk, kind, &Context::default(), TARGET),
                Ok(false)
            );
            assert_eq!(chunk, before);
        }
    }
    let mut level = files::read_gz(&dir.join("level.dat")).unwrap();
    assert_eq!(
        upgrade::upgrade(&mut level, Kind::Level, &Context::default(), TARGET),
        Ok(false)
    );
}

fn old_world(world: &Path) {
    let level = c(vec![(
        "Data",
        Tag::Compound(c(vec![
            ("DataVersion", Tag::Int(3465)),
            ("version", Tag::Int(19133)),
            (
                "Version",
                Tag::Compound(c(vec![
                    ("Id", Tag::Int(3465)),
                    ("Name", s("1.20.1")),
                    ("Series", s("main")),
                    ("Snapshot", Tag::Byte(0)),
                ])),
            ),
            ("LevelName", s("world")),
            ("GameType", Tag::Int(0)),
            ("allowCommands", Tag::Byte(0)),
            ("initialized", Tag::Byte(1)),
            ("WasModded", Tag::Byte(0)),
            ("ServerBrands", Tag::List(vec![s("vanilla")])),
            ("LastPlayed", Tag::Long(1)),
            ("Time", Tag::Long(500)),
            ("DayTime", Tag::Long(500)),
            ("SpawnX", Tag::Int(0)),
            ("SpawnY", Tag::Int(64)),
            ("SpawnZ", Tag::Int(0)),
            ("SpawnAngle", Tag::Float(0.0)),
            ("Difficulty", Tag::Byte(2)),
            ("DifficultyLocked", Tag::Byte(0)),
            ("hardcore", Tag::Byte(0)),
            ("raining", Tag::Byte(0)),
            ("rainTime", Tag::Int(100)),
            ("thundering", Tag::Byte(0)),
            ("thunderTime", Tag::Int(100)),
            ("clearWeatherTime", Tag::Int(0)),
            ("BorderCenterX", Tag::Double(0.0)),
            ("BorderCenterZ", Tag::Double(0.0)),
            ("BorderSize", Tag::Double(5.9999968e7)),
            ("BorderSafeZone", Tag::Double(5.0)),
            ("BorderWarningBlocks", Tag::Double(5.0)),
            ("BorderWarningTime", Tag::Double(15.0)),
            ("BorderSizeLerpTarget", Tag::Double(5.9999968e7)),
            ("BorderSizeLerpTime", Tag::Long(0)),
            ("BorderDamagePerBlock", Tag::Double(0.2)),
            ("WanderingTraderSpawnChance", Tag::Int(25)),
            ("WanderingTraderSpawnDelay", Tag::Int(24000)),
            ("CustomBossEvents", Tag::Compound(Compound::new())),
            ("ScheduledEvents", Tag::List(Vec::new())),
            (
                "DragonFight",
                Tag::Compound(c(vec![
                    ("DragonKilled", Tag::Byte(0)),
                    ("NeedsStateScanning", Tag::Byte(1)),
                ])),
            ),
            ("enabled_features", Tag::List(vec![s("minecraft:vanilla")])),
            (
                "DataPacks",
                Tag::Compound(c(vec![
                    ("Enabled", Tag::List(vec![s("vanilla")])),
                    ("Disabled", Tag::List(Vec::new())),
                ])),
            ),
            (
                "GameRules",
                Tag::Compound(c(vec![
                    ("keepInventory", s("false")),
                    ("doDaylightCycle", s("true")),
                    ("randomTickSpeed", s("3")),
                    ("doFireTick", s("true")),
                    ("spawnRadius", s("10")),
                    ("disableRaids", s("false")),
                ])),
            ),
            (
                "WorldGenSettings",
                Tag::Compound(c(vec![
                    ("seed", Tag::Long(1)),
                    ("generate_features", Tag::Byte(1)),
                    ("bonus_chest", Tag::Byte(0)),
                    ("dimensions", Tag::Compound(Compound::new())),
                ])),
            ),
        ])),
    )]);
    files::write_gz(&world.join("level.dat"), &level).unwrap();
    let palette = Tag::List(vec![
        Tag::Compound(c(vec![("Name", s("minecraft:grass"))])),
        Tag::Compound(c(vec![
            ("Name", s("minecraft:chain")),
            (
                "Properties",
                Tag::Compound(c(vec![("axis", s("x")), ("waterlogged", s("false"))])),
            ),
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
        (
            "biomes",
            Tag::Compound(c(vec![("palette", Tag::List(vec![s("minecraft:plains")]))])),
        ),
        ("BlockLight", Tag::ByteArray(vec![0; 2048])),
    ]);
    let chunk = c(vec![
        ("DataVersion", Tag::Int(3465)),
        ("xPos", Tag::Int(0)),
        ("zPos", Tag::Int(0)),
        ("yPos", Tag::Int(-4)),
        ("Status", s("minecraft:full")),
        ("LastUpdate", Tag::Long(1)),
        ("InhabitedTime", Tag::Long(0)),
        ("isLightOn", Tag::Byte(1)),
        ("sections", Tag::List(vec![Tag::Compound(section)])),
        ("block_entities", Tag::List(Vec::new())),
        ("block_ticks", Tag::List(Vec::new())),
        ("fluid_ticks", Tag::List(Vec::new())),
        ("PostProcessing", Tag::List(Vec::new())),
        (
            "structures",
            Tag::Compound(c(vec![
                ("starts", Tag::Compound(Compound::new())),
                ("References", Tag::Compound(Compound::new())),
            ])),
        ),
    ]);
    let dir = world.join("region");
    std::fs::create_dir_all(&dir).unwrap();
    let mut bytes = Vec::new();
    mc_nbt::write_named("", &Tag::Compound(chunk), &mut bytes).unwrap();
    RegionStorage::new(&dir)
        .write_chunk(0, 0, &bytes, 1)
        .unwrap();
}

#[test]
fn an_upgraded_1_20_1_world_looks_like_a_26_3_one() {
    let Some(vanilla_dir) = vanilla() else {
        eprintln!("skipped: no vanilla 26.3 world");
        return;
    };
    let data_dir = root().join("cache/mcdata/26.3");
    if !data_dir.is_dir() {
        eprintln!("skipped: no extracted data");
        return;
    }
    let tmp = std::env::temp_dir().join(format!("mc-upgrade-vs-vanilla-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let world_dir = tmp.join("world");
    old_world(&world_dir);
    files::fix_world(&world_dir, TARGET, "26.3").unwrap();
    world::upgrade_world(&world_dir, TARGET, false, &mut |n| panic!("{n}")).unwrap();

    // Files where vanilla has them.
    for f in [
        "data/minecraft/game_rules.dat",
        "data/minecraft/world_gen_settings.dat",
        "data/minecraft/world_clocks.dat",
        "data/minecraft/weather.dat",
        "data/minecraft/scheduled_events.dat",
        "data/minecraft/custom_boss_events.dat",
    ] {
        assert!(vanilla_dir.join(f).is_file(), "vanilla has no {f}");
        assert!(world_dir.join(f).is_file(), "we wrote no {f}");
    }
    assert!(
        world_dir
            .join("dimensions/minecraft/overworld/region/r.0.0.mca")
            .is_file()
    );

    // level.dat: nothing vanilla 26.3 would not write.
    let ours = files::read_gz(&world_dir.join("level.dat")).unwrap();
    let theirs = files::read_gz(&vanilla_dir.join("level.dat")).unwrap();
    // Optional fields vanilla reads and leaves out when they hold the default.
    let mut allowed = keys(sub(&theirs, "Data"));
    allowed.extend(["enabled_features".to_owned(), "removed_features".to_owned()]);
    let extra: Vec<_> = keys(sub(&ours, "Data"))
        .difference(&allowed)
        .cloned()
        .collect();
    assert_eq!(
        sub(&ours, "Data").get("version_history"),
        Some(&Tag::List(vec![Tag::Int(TARGET)]))
    );
    assert!(
        extra.is_empty(),
        "level.dat fields vanilla does not write: {extra:?} (vanilla: {:?})",
        keys(sub(&theirs, "Data"))
    );

    // Game rules: every converted rule is one vanilla knows.
    let ours = files::read_gz(&world_dir.join("data/minecraft/game_rules.dat")).unwrap();
    let theirs = files::read_gz(&vanilla_dir.join("data/minecraft/game_rules.dat")).unwrap();
    let extra: Vec<_> = keys(sub(&ours, "data"))
        .difference(&keys(sub(&theirs, "data")))
        .cloned()
        .collect();
    assert!(
        extra.is_empty(),
        "game rules vanilla does not have: {extra:?}"
    );
    for (k, v) in sub(&ours, "data").iter() {
        let theirs = sub(&theirs, "data").get(k).unwrap();
        assert_eq!(
            v.type_id(),
            theirs.type_id(),
            "{k} has another type than vanilla's"
        );
    }

    // The chunk: root and section fields vanilla writes, blocks the 26.3
    // registry knows.
    let ours = chunks(&world_dir.join("dimensions/minecraft/overworld/region"));
    let theirs = chunks(&vanilla_dir.join("dimensions/minecraft/overworld/region"));
    let mut root_keys = BTreeSet::new();
    let mut section_keys = BTreeSet::new();
    for chunk in &theirs {
        root_keys.extend(keys(chunk));
        if let Some(Tag::List(sections)) = chunk.get("sections") {
            for s in sections {
                if let Tag::Compound(s) = s {
                    section_keys.extend(keys(s));
                }
            }
        }
    }
    let [chunk] = &ours[..] else {
        panic!("expected one chunk");
    };
    if let Some(Tag::List(sections)) = chunk.get("sections") {
        for s in sections {
            if let Tag::Compound(s) = s {
                let extra: Vec<_> = keys(s).difference(&section_keys).cloned().collect();
                assert!(
                    extra.is_empty(),
                    "section fields vanilla does not write: {extra:?}"
                );
            }
        }
    }
    // Old Overworld chunks blend into new terrain; fresh ones have nothing to
    // blend.
    root_keys.insert("blending_data".to_owned());
    let extra: Vec<_> = keys(chunk).difference(&root_keys).cloned().collect();
    assert!(
        extra.is_empty(),
        "chunk fields vanilla does not write: {extra:?}"
    );
    let data = mc_data::McData::load(&data_dir).unwrap();
    let biomes = data
        .data_pack()
        .registry_entries("minecraft:worldgen/biome")
        .unwrap();
    let biome_id = |name: &str| biomes.iter().position(|b| b == name).map(|i| i as u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &biome_id,
        air: data.blocks.default_state("minecraft:air").unwrap(),
        fallback_biome: biome_id("minecraft:plains").unwrap(),
        unresolved: Default::default(),
    };
    match decode_chunk(&Tag::Compound(chunk.clone()), -64, 384, &palettes).unwrap() {
        Decoded::Chunk(_) => {}
        Decoded::NotFull(_) => panic!("the chunk is not full any more"),
    }
    assert_eq!(palettes.unresolved.get(), 0, "blocks 26.3 does not know");
    let _ = std::fs::remove_dir_all(&tmp);
}
