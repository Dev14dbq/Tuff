//! `cargo xtask chunk-info <region dir> <chunk x> <chunk z> [--depth n]`:
//! prints the NBT structure of one stored chunk (keys, types, sizes, short
//! values). For checking the on-disk format against the documentation.

use anyhow::{Context, Result, bail};
use mc_nbt::Tag;
use mc_storage::RegionStorage;

pub fn run(args: &[String]) -> Result<()> {
    let mut positional = Vec::new();
    let mut depth = 6usize;
    let mut all = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--depth" => depth = args.next().context("--depth needs a value")?.parse()?,
            "--all" => all = true,
            other if other.starts_with("--") => bail!("unknown option `{other}` for chunk-info"),
            other => positional.push(other.to_owned()),
        }
    }
    // A single NBT file (e.g. level.dat, gzip-compressed or not).
    if let [file] = positional.as_slice() {
        let raw = std::fs::read(file).with_context(|| format!("cannot read {file}"))?;
        let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut out)?;
            out
        } else {
            raw
        };
        let (_, tag, _) = mc_nbt::read_named(&bytes)?;
        print_tag("", &tag, 0, depth, if all { usize::MAX } else { 2 });
        return Ok(());
    }
    let [dir, x, z] = positional.as_slice() else {
        bail!(
            "usage: chunk-info <region dir> <chunk x> <chunk z> | <nbt file> [--depth n] [--all]"
        );
    };
    let (x, z): (i32, i32) = (x.parse()?, z.parse()?);
    let mut storage = RegionStorage::new(dir);
    let Some(tag) = storage.read_chunk(x, z)? else {
        bail!("chunk {x},{z} is not stored in {dir}");
    };
    print_tag("", &tag, 0, depth, if all { usize::MAX } else { 2 });
    Ok(())
}

fn print_tag(name: &str, tag: &Tag, indent: usize, depth: usize, shown: usize) {
    let pad = "  ".repeat(indent);
    let label = if name.is_empty() {
        String::new()
    } else {
        format!("{name}: ")
    };
    match tag {
        Tag::Compound(c) => {
            println!("{pad}{label}compound ({} keys)", c.len());
            if indent < depth {
                for (key, value) in c.iter() {
                    print_tag(key, value, indent + 1, depth, shown);
                }
            }
        }
        Tag::List(items) => {
            let kind = items
                .first()
                .map_or("empty".to_owned(), |t| type_name(t).to_owned());
            println!("{pad}{label}list of {} {kind}", items.len());
            if indent < depth {
                // By default only the first two elements; the rest usually look alike.
                for (i, item) in items.iter().take(shown).enumerate() {
                    print_tag(&format!("[{i}]"), item, indent + 1, depth, shown);
                }
            }
        }
        Tag::ByteArray(v) => println!("{pad}{label}byte array [{}]", v.len()),
        Tag::IntArray(v) => println!("{pad}{label}int array [{}]", v.len()),
        Tag::LongArray(v) => println!("{pad}{label}long array [{}]", v.len()),
        Tag::String(s) => println!("{pad}{label}string {s:?}"),
        other => println!("{pad}{label}{} {:?}", type_name(other), other),
    }
}

fn type_name(tag: &Tag) -> &'static str {
    match tag {
        Tag::Byte(_) => "byte",
        Tag::Short(_) => "short",
        Tag::Int(_) => "int",
        Tag::Long(_) => "long",
        Tag::Float(_) => "float",
        Tag::Double(_) => "double",
        Tag::ByteArray(_) => "byte array",
        Tag::String(_) => "string",
        Tag::List(_) => "list",
        Tag::Compound(_) => "compound",
        Tag::IntArray(_) => "int array",
        Tag::LongArray(_) => "long array",
    }
}
