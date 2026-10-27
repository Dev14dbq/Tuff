//! `cargo xtask mcc`: oversized chunks (external `c.<x>.<z>.mcc` files)
//! both ways between vanilla and us, on a generated world (fixed seed;
//! everything stands in the air at y = 200, above the terrain):
//!
//! 1. vanilla fills chunk (0, 0) with 64 chests of items with long random
//!    names, more than 1 MB compressed, and saves: the chunk goes external;
//! 2. we read that chunk from vanilla's files and find every name;
//! 3. our server places a gold block in the chunk and saves: the chunk is
//!    still external, now written by us;
//! 4. vanilla opens the world again and finds the gold block and the names.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};

use crate::parity::Server;

const PORT: u16 = 25603;
const Y: i32 = 200;
const ITEMS: usize = 27;
const NAME: usize = 1100;
const GOLD: [i32; 3] = [1, Y, 1];

pub fn run(_args: &[String]) -> Result<()> {
    let root = crate::workspace_root();
    let vanilla = root.join("cache/vanilla-server/26.3-mcc");
    let ours = root.join("cache/ours-mcc");
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-type=minecraft\\:normal\nlevel-seed=12345\ngenerate-structures=false\nspawn-protection=0\n\
         pause-when-empty-seconds=0\nsync-chunk-writes=true\nenable-status=true\n"
    );
    let region = |dir: &Path| dir.join("world/dimensions/minecraft/overworld/region");

    // 1. Vanilla writes the oversized chunk.
    let _ = std::fs::remove_dir_all(vanilla.join("world"));
    Server::prepare(&root, &vanilla, &properties)?;
    let mut server = Server::start_java(&root, &vanilla)?;
    let filled = fill(&mut server);
    server.stop();
    filled?;
    let stored = external(&region(&vanilla))?;
    ensure!(stored.0, "vanilla kept chunk (0, 0) inside the region file");
    println!(
        "1. vanilla: chunk (0, 0) external, c.0.0.mcc {} KB",
        stored.1 / 1024
    );

    // 2. We read it.
    let found = names(&region(&vanilla))?;
    ensure!(
        found == expected(),
        "read {} names from vanilla's .mcc, {} of them right",
        found.len(),
        found
            .iter()
            .zip(expected())
            .filter(|(a, b)| **a == *b)
            .count()
    );
    println!(
        "2. ours reads vanilla's .mcc: {} item names match",
        found.len()
    );

    // 3. Our server changes the chunk and saves it.
    let world = vanilla.join("world");
    let mut server = Server::ours_in_with(&root, &ours, &properties, &|dir| {
        copy_dir(&world, &dir.join("world"))
    })?;
    let placed = (|| -> Result<()> {
        server.command("forceload add 0 0", false)?;
        std::thread::sleep(Duration::from_secs(1));
        let [x, y, z] = GOLD;
        server.wait_for(
            &format!("setblock {x} {y} {z} minecraft:gold_block"),
            "Changed the block",
            Duration::from_secs(30),
        )?;
        Ok(())
    })();
    server.stop();
    placed?;
    let stored = external(&region(&ours))?;
    ensure!(stored.0, "our save put chunk (0, 0) back inside the region");
    let found = names(&region(&ours))?;
    ensure!(found == expected(), "our save lost item names");
    println!(
        "3. ours writes the chunk: still external, c.0.0.mcc {} KB, names kept",
        stored.1 / 1024
    );

    // 4. Vanilla opens what we wrote.
    let _ = std::fs::remove_dir_all(vanilla.join("world"));
    copy_dir(&ours.join("world"), &vanilla.join("world"))?;
    let mut server = Server::start_java(&root, &vanilla)?;
    let checked = check(&mut server);
    server.stop();
    checked?;
    println!("4. vanilla reads our .mcc: gold block and names in place");
    Ok(())
}

/// The name of item `slot` in chest `chest`: random letters and digits, so
/// it compresses badly.
fn name(chest: usize, slot: usize) -> String {
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ ((chest * ITEMS + slot) as u64 + 1);
    (0..NAME)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[(state % 62) as usize]
                as char
        })
        .collect()
}

/// Chest `i` stands at (2·(i % 8), Y, 2·(i / 8)): apart, so none join.
fn chest_pos(i: usize) -> [i32; 3] {
    [2 * (i % 8) as i32, Y, 2 * (i / 8) as i32]
}

fn expected() -> Vec<String> {
    (0..64)
        .flat_map(|c| (0..ITEMS).map(move |s| name(c, s)))
        .collect()
}

fn fill(server: &mut Server) -> Result<()> {
    server.command("forceload add 0 0", false)?;
    std::thread::sleep(Duration::from_secs(2));
    for chest in 0..64 {
        let items: Vec<String> = (0..ITEMS)
            .map(|slot| {
                format!(
                    "{{Slot:{slot}b,id:\"minecraft:stone\",count:1,\
                     components:{{\"minecraft:custom_name\":\"{}\"}}}}",
                    name(chest, slot)
                )
            })
            .collect();
        let [x, y, z] = chest_pos(chest);
        server.wait_for(
            &format!(
                "setblock {x} {y} {z} minecraft:chest{{Items:[{}]}}",
                items.join(",")
            ),
            "Changed the block",
            Duration::from_secs(30),
        )?;
    }
    server.wait_for("save-all flush", "Saved the game", Duration::from_secs(60))?;
    Ok(())
}

fn check(server: &mut Server) -> Result<()> {
    server.command("forceload add 0 0", false)?;
    std::thread::sleep(Duration::from_secs(2));
    let [x, y, z] = GOLD;
    let line = server.wait_for(
        &format!("execute if block {x} {y} {z} minecraft:gold_block"),
        "Test",
        Duration::from_secs(30),
    )?;
    ensure!(line.contains("passed"), "no gold block: {line}");
    for chest in [0, 37, 63] {
        let [x, y, z] = chest_pos(chest);
        let slot = ITEMS - 1;
        let line = server.wait_for(
            &format!(
                "data get block {x} {y} {z} Items[{slot}].components.\"minecraft:custom_name\""
            ),
            "has the following",
            Duration::from_secs(30),
        )?;
        ensure!(
            line.contains(&name(chest, slot)),
            "chest {chest}: wrong name: {}",
            &line[..line.len().min(200)]
        );
    }
    Ok(())
}

/// Whether chunk (0, 0) of r.0.0.mca is stored externally, and the size of
/// its `.mcc`.
fn external(region: &Path) -> Result<(bool, u64)> {
    let file = std::fs::read(region.join("r.0.0.mca")).context("r.0.0.mca")?;
    let location = u32::from_be_bytes([file[0], file[1], file[2], file[3]]);
    let offset = (location >> 8) as usize * 4096;
    ensure!(offset > 0, "chunk (0, 0) not in the region");
    let compression = *file.get(offset + 4).context("short region")?;
    let mcc = std::fs::metadata(region.join("c.0.0.mcc")).map_or(0, |m| m.len());
    Ok((compression & 0x80 != 0 && mcc > 0, mcc))
}

/// Every item name in the chests of chunk (0, 0), chest by chest.
fn names(region: &Path) -> Result<Vec<String>> {
    let mut storage = mc_storage::region::RegionStorage::new(region);
    let chunk = storage.read_chunk(0, 0)?.context("no chunk (0, 0)")?;
    let mut by_pos = Vec::new();
    for be in list(compound(&chunk)?.get("block_entities"))? {
        let be = compound(be)?;
        let int = |k: &str| match be.get(k) {
            Some(mc_nbt::Tag::Int(v)) => Ok(*v),
            _ => bail!("block entity without {k}"),
        };
        let pos = [int("x")?, int("y")?, int("z")?];
        let Some(index) = (0..64).find(|&i| chest_pos(i) == pos) else {
            continue;
        };
        let mut items = Vec::new();
        for item in list(be.get("Items"))? {
            let name = compound(item)?
                .get("components")
                .map(compound)
                .transpose()?
                .and_then(|c| c.get("minecraft:custom_name"));
            match name {
                Some(mc_nbt::Tag::String(s)) => items.push(s.clone()),
                other => bail!("chest {index}: name {other:?}"),
            }
        }
        by_pos.push((index, items));
    }
    by_pos.sort_by_key(|(i, _)| *i);
    Ok(by_pos.into_iter().flat_map(|(_, items)| items).collect())
}

fn compound(tag: &mc_nbt::Tag) -> Result<&mc_nbt::Compound> {
    match tag {
        mc_nbt::Tag::Compound(c) => Ok(c),
        _ => bail!("not a compound"),
    }
}

fn list(tag: Option<&mc_nbt::Tag>) -> Result<&[mc_nbt::Tag]> {
    match tag {
        Some(mc_nbt::Tag::List(l)) => Ok(l),
        None => Ok(&[]),
        _ => bail!("not a list"),
    }
}

/// Copies a world folder (without its `session.lock`).
pub(crate) fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if entry.file_name() != "session.lock" {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
