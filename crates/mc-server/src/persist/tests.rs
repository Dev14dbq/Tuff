use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mc_entity::extra::{Decor, Hung, Pickup, Shot};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use super::*;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3")
}

/// A world in a fresh folder, opened as a server would.
fn open(folder: &Path) -> Arc<ServerContext> {
    let dir = data_dir();
    let mut config = crate::Config::new(dir.clone());
    config.world = Some(folder.to_path_buf());
    config.seed = Some(12345);
    ServerContext::new(config, mc_data::McData::load(&dir).unwrap()).unwrap()
}

fn folder(name: &str) -> PathBuf {
    let f = std::env::temp_dir().join(format!("mc-persist-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&f);
    f
}

fn nbt(snbt: &str) -> Compound {
    match mc_nbt::parse_snbt(snbt).unwrap() {
        Tag::Compound(c) => c,
        other => panic!("{other:?}"),
    }
}

fn entity(ctx: &ServerContext, kind: Kind, pos: [f64; 3]) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind,
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

fn summoned(ctx: &ServerContext, name: &str, snbt: &str, pos: [f64; 3]) -> Entity {
    let (kind, at) =
        crate::summon::build(ctx, name, &nbt(snbt), pos, 30.0).unwrap_or_else(|| panic!("{name}"));
    entity(ctx, kind, at.unwrap_or(pos))
}

fn mob(ctx: &ServerContext, species: Species, pos: [f64; 3]) -> Entity {
    crate::mobs::new_mob(ctx, species, pos)
}

/// One of every kind of entity there is, all in chunk 2, 2.
fn everything(ctx: &ServerContext) -> Vec<Entity> {
    let item_id = |n: &str| ctx.items.id(n).unwrap();
    let mut all = Vec::new();

    let mut item = entity(
        ctx,
        Kind::stack(
            &ItemStack {
                damage: 12,
                ..ItemStack::new(item_id("minecraft:iron_sword"), 1)
            },
            7,
        ),
        [33.5, 100.0, 33.5],
    );
    if let Kind::Item {
        owner,
        thrower,
        ageless,
        ..
    } = &mut item.kind
    {
        *owner = Some(11);
        *thrower = Some(12);
        *ageless = true;
    }
    item.age = 0;
    item.tags = vec!["kept".into()];
    all.push(item);
    let mut plain = entity(
        ctx,
        Kind::item(item_id("minecraft:diamond"), 5, 0, 0),
        [33.5, 101.0, 34.5],
    );
    plain.age = 4321;
    plain.vel = [0.1, -0.2, 0.05];
    all.push(plain);
    let mut orb = entity(
        ctx,
        Kind::ExperienceOrb { value: 7, count: 3 },
        [34.5, 100.0, 33.5],
    );
    orb.age = 99;
    all.push(orb);
    all.push(entity(
        ctx,
        Kind::Tnt {
            fuse: 42,
            owner: Some(77),
        },
        [35.5, 100.0, 33.5],
    ));
    let mut falling = entity(
        ctx,
        Kind::FallingBlock {
            state: ctx
                .data
                .blocks
                .state_id("minecraft:anvil", &[("facing", "east")])
                .unwrap(),
        },
        [36.5, 100.0, 33.5],
    );
    falling.age = 5;
    all.push(falling);

    // Villagers.
    let mut villager =
        crate::trading::new_merchant(ctx, "minecraft:villager", [37.5, 100.0, 33.5]).unwrap();
    if let Kind::Merchant(m) = &mut villager.kind {
        m.variant = "minecraft:taiga".into();
        m.profession = Some("farmer".into());
        m.level = 2;
        m.xp = 15;
        m.offers = ctx.trades.level_offers("farmer", 1, "minecraft:taiga");
        assert!(!m.offers.is_empty());
        m.offers[0].uses = 3;
        m.offers[0].demand = 2;
        m.job_site = Some((37, 99, 34));
        m.last_restock = 12000;
        m.restocks_today = 1;
    }
    ctx.mobs.set_villager_health(villager.id, Some(13.5));
    all.push(villager);
    let mut trader =
        crate::trading::new_merchant(ctx, "minecraft:wandering_trader", [38.5, 100.0, 33.5])
            .unwrap();
    trader.age = 1000;
    all.push(trader);

    // Mobs.
    let mut zombie = mob(ctx, Species::Zombie, [39.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut zombie.kind {
        m.custom_name = Some("Bob".into());
        m.persistent = true;
        m.health = 11.0;
        m.burning = 40;
        m.fall_distance = 2.5;
        m.gear[mc_entity::mob::slot::HEAD] = Some(mc_entity::mob::Gear {
            item: item_id("minecraft:iron_helmet"),
            count: 1,
            damage: 30,
        });
        m.drop_chance[mc_entity::mob::slot::HEAD] = 1.0;
        m.yaw = 45.0;
        m.pitch = -10.0;
        m.head_yaw = 45.0;
    }
    all.push(zombie);
    let mut wolf = mob(ctx, Species::Wolf, [40.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut wolf.kind {
        m.owner = Some(0xabc);
        m.persistent = true;
        m.set_max_health(40.0);
        m.sitting = true;
        m.collar = 3;
    }
    all.push(wolf);
    let mut sheep = mob(ctx, Species::Sheep, [41.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut sheep.kind {
        m.set_color(5);
        m.sheared = true;
        m.baby_age = -1200;
        m.love = 100;
        m.love_by = Some(0x55);
    }
    all.push(sheep);
    let mut horse = mob(ctx, Species::Horse, [42.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut horse.kind {
        m.tame = true;
        m.temper = 40;
        m.gear[mc_entity::mob::slot::SADDLE] = Some(mc_entity::mob::Gear {
            item: item_id("minecraft:saddle"),
            count: 1,
            damage: 0,
        });
    }
    all.push(horse);
    let mut enderman = mob(ctx, Species::Enderman, [43.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut enderman.kind {
        m.carried = ctx.data.blocks.default_state("minecraft:grass_block");
    }
    all.push(enderman);
    let mut zv = mob(ctx, Species::ZombieVillager, [44.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut zv.kind {
        m.villager = Some(("minecraft:desert".into(), Some("mason".into()), 3));
        m.conversion = 200;
    }
    all.push(zv);
    let mut slime = mob(ctx, Species::Slime, [45.5, 100.0, 33.5]);
    if let Kind::Mob(m) = &mut slime.kind {
        m.set_size(4);
    }
    all.push(slime);

    // Vehicles, one of each, a cow on the boat.
    for (i, name) in [
        "minecraft:minecart",
        "minecraft:chest_minecart",
        "minecraft:furnace_minecart",
        "minecraft:hopper_minecart",
        "minecraft:tnt_minecart",
        "minecraft:command_block_minecart",
        "minecraft:oak_boat",
        "minecraft:spruce_chest_boat",
        "minecraft:bamboo_raft",
    ]
    .iter()
    .enumerate()
    {
        let mut v = summoned(
            ctx,
            name,
            r#"{Command:"say hi",TrackOutput:1b}"#,
            [33.5 + i as f64, 100.0, 36.5],
        );
        if let Kind::Vehicle(veh) = &mut v.kind {
            veh.yaw = 90.0;
            match name.trim_start_matches("minecraft:") {
                "furnace_minecart" => veh.fuel = 300,
                "tnt_minecart" => veh.fuse = 30,
                "hopper_minecart" => {
                    veh.enabled = false;
                    lock(&ctx.cart_rests).insert(v.id, 3);
                }
                "command_block_minecart" => {
                    veh.command.success_count = 2;
                    veh.command.last_output = Some("[12:00:00] hi".into());
                }
                _ => {}
            }
        }
        if name.contains("chest") || name.contains("hopper") {
            let mut items = Compound::new();
            items.insert(
                "Items",
                crate::containers::write_items(
                    ctx,
                    &[None, Some(ItemStack::new(item_id("minecraft:apple"), 9))],
                ),
            );
            lock(&ctx.entity_items).insert(v.id, items);
        }
        if *name == "minecraft:oak_boat" {
            let cow = mob(ctx, Species::Cow, v.pos);
            if let Kind::Vehicle(veh) = &mut v.kind {
                veh.riders.push(cow.id);
            }
            all.push(cow);
        }
        all.push(v);
    }

    // Things that hang, and a stand.
    all.push(summoned(
        ctx,
        "minecraft:item_frame",
        r#"{Facing:3b,Item:{id:"minecraft:map",count:1},ItemRotation:3b}"#,
        [33.5, 101.0, 38.5],
    ));
    all.push(summoned(
        ctx,
        "minecraft:glow_item_frame",
        r#"{Facing:1b,Fixed:1b,Invisible:1b}"#,
        [34.5, 101.0, 38.5],
    ));
    let mut painting = summoned(
        ctx,
        "minecraft:painting",
        r#"{Facing:4b,variant:"minecraft:kebab"}"#,
        [35.5, 101.0, 38.5],
    );
    if let Kind::Hung(h) = &mut painting.kind {
        h.facing = 4;
    }
    all.push(painting);
    let mut stand = summoned(
        ctx,
        "minecraft:armor_stand",
        r#"{Small:1b,ShowArms:1b,Pose:{Head:[10f,20f,30f]}}"#,
        [36.5, 100.0, 38.5],
    );
    if let Kind::Hung(h) = &mut stand.kind
        && let Decor::Stand(s) = &mut h.decor
    {
        s.gear[5] = Some((item_id("minecraft:golden_helmet"), 3));
        s.gear[0] = Some((item_id("minecraft:stick"), 0));
    }
    all.push(stand);
    let knot = entity(
        ctx,
        Kind::Hung(Hung {
            decor: Decor::LeashKnot,
            facing: 0,
            yaw: 0.0,
            anchor: [37, 100, 39],
        }),
        [37.5, 100.5, 39.5],
    );
    let pig = mob(ctx, Species::Pig, [38.5, 100.0, 39.5]);
    lock(&ctx.leashes)
        .links
        .insert(pig.id, crate::decor::leash::Holder::Knot(knot.id));
    let chicken = mob(ctx, Species::Chicken, [39.5, 100.0, 39.5]);
    lock(&ctx.leashes)
        .links
        .insert(chicken.id, crate::decor::leash::Holder::Player(0x99));
    all.extend([knot, pig, chicken]);

    // Projectiles.
    let mut arrow = summoned(
        ctx,
        "minecraft:arrow",
        "{crit:1b,pickup:2b}",
        [40.5, 100.0, 40.5],
    );
    if let Kind::Projectile(p) = &mut arrow.kind {
        p.owner = Some(0x42);
        p.yaw = 12.0;
        p.pitch = 34.0;
        if let Shot::Arrow(a) = &mut p.shot {
            a.stuck = 25;
            a.damage = 3.5;
            a.punch = 2;
            a.flame = true;
            a.pierce = 1;
            a.item = item_id("minecraft:tipped_arrow");
            a.effects = vec![("minecraft:poison".into(), 1, 100)];
            assert_eq!(a.pickup, Pickup::Creative);
        }
    }
    arrow.age = 30;
    all.push(arrow);
    let mut trident = summoned(ctx, "minecraft:arrow", "{}", [41.5, 100.0, 40.5]);
    if let Kind::Projectile(p) = &mut trident.kind
        && let Shot::Arrow(a) = &mut p.shot
    {
        a.trident = true;
        a.item = item_id("minecraft:trident");
    }
    all.push(trident);
    all.push(summoned(
        ctx,
        "minecraft:spectral_arrow",
        "{}",
        [42.5, 100.0, 40.5],
    ));
    for (i, n) in [
        "snowball",
        "egg",
        "ender_pearl",
        "splash_potion",
        "lingering_potion",
    ]
    .iter()
    .enumerate()
    {
        let mut e = summoned(
            ctx,
            &format!("minecraft:{n}"),
            "{}",
            [33.5 + i as f64, 102.0, 42.5],
        );
        e.vel = [0.0, 0.5, 0.25];
        if let Kind::Projectile(p) = &mut e.kind
            && let Shot::Potion { effects, color, .. } = &mut p.shot
        {
            *effects = vec![("minecraft:speed".into(), 0, 3600)];
            *color = 0x123456;
        }
        all.push(e);
    }
    let mut rocket = summoned(
        ctx,
        "minecraft:firework_rocket",
        "{LifeTime:40}",
        [39.5, 102.0, 42.5],
    );
    if let Kind::Projectile(p) = &mut rocket.kind
        && let Shot::Firework(f) = &mut p.shot
    {
        f.stars = vec![vec![0xff0000, 0x00ff00]];
        f.life = 25;
    }
    rocket.age = 15;
    all.push(rocket);
    let mut cloud = entity(
        ctx,
        Kind::Projectile(mc_entity::extra::Projectile {
            shot: Shot::Cloud {
                effects: vec![("minecraft:regeneration".into(), 0, 100)],
                color: 0xcd5cab,
                radius: 2.5,
                left: 300,
            },
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        }),
        [40.5, 100.0, 42.5],
    );
    cloud.age = 20;
    all.push(cloud);
    for (i, small) in [false, true].into_iter().enumerate() {
        all.push(entity(
            ctx,
            Kind::Projectile(mc_entity::extra::Projectile {
                shot: Shot::Fireball { small },
                owner: None,
                mob: None,
                yaw: 0.0,
                pitch: 0.0,
            }),
            [41.5 + i as f64, 103.0, 42.5],
        ));
    }
    all
}

/// Every entity of the world as it would be stored, by uuid.
fn stored(ctx: &ServerContext) -> HashMap<u128, Compound> {
    let all = lock(&ctx.entities).clone();
    all.iter()
        .filter_map(|e| Some((e.uuid, write(ctx, e, &all)?)))
        .collect()
}

#[test]
fn every_kind_of_entity_comes_back_after_a_restart() {
    if mc_data::McData::load(&data_dir()).is_err() {
        eprintln!("skipped: no game data");
        return;
    }
    let dir = folder("all");
    let at = Position::new(40, 100, 40);
    let before = {
        let ctx = open(&dir);
        let _ = ctx.world.block(at, &ctx.data.blocks);
        load_stored(&ctx);
        let all = everything(&ctx);
        lock(&ctx.entities).extend(all);
        let before = stored(&ctx);
        crate::save(&ctx);
        before
    };
    let ctx = open(&dir);
    let _ = ctx.world.block(at, &ctx.data.blocks);
    load_stored(&ctx);
    let after = stored(&ctx);
    let kinds: Vec<String> = before
        .values()
        .filter_map(|c| match c.get("id") {
            Some(Tag::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    for (uuid, was) in &before {
        let now = after
            .get(uuid)
            .unwrap_or_else(|| panic!("lost: {:?}", was.get("id")));
        assert_eq!(was, now, "{:?} changed", was.get("id"));
    }
    assert_eq!(before.len(), after.len(), "{kinds:?}");
    // State outside the entities came back too.
    let entities = lock(&ctx.entities).clone();
    let find = |id: &str| {
        entities
            .iter()
            .find(|e| crate::fx::type_name(&e.kind).as_deref() == Some(id))
            .unwrap()
    };
    let chest = find("chest_minecart");
    assert!(lock(&ctx.entity_items).contains_key(&chest.id));
    let hopper = find("hopper_minecart");
    assert_eq!(lock(&ctx.cart_rests).get(&hopper.id), Some(&3));
    let villager = entities
        .iter()
        .find(|e| matches!(&e.kind, Kind::Merchant(m) if !m.wandering))
        .unwrap();
    assert_eq!(ctx.mobs.villager_health(villager.id), 13.5);
    let pig = entities
        .iter()
        .find(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Pig))
        .unwrap();
    let knot = find("leash_knot");
    assert_eq!(
        lock(&ctx.leashes).links.get(&pig.id),
        Some(&crate::decor::leash::Holder::Knot(knot.id))
    );
    let boat = find("oak_boat");
    let Kind::Vehicle(v) = &boat.kind else {
        panic!()
    };
    assert_eq!(v.riders.len(), 1);
    assert!(entities.iter().any(|e| e.id == v.riders[0]));
    // Loading the same chunk again brings nobody twice.
    drop(entities);
    let count = lock(&ctx.entities).len();
    crate::save(&ctx);
    let dropped = ctx.world.unload_unused();
    unloaded(&ctx, &dropped);
    assert!(lock(&ctx.entities).is_empty(), "all left with their chunk");
    assert!(lock(&ctx.entity_items).is_empty());
    let _ = ctx.world.block(at, &ctx.data.blocks);
    load_stored(&ctx);
    assert_eq!(lock(&ctx.entities).len(), count);
    assert_eq!(stored(&ctx), before, "unloading and loading keeps all");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_follow_the_entity_chunk_format() {
    if mc_data::McData::load(&data_dir()).is_err() {
        eprintln!("skipped: no game data");
        return;
    }
    let dir = folder("layout");
    let ctx = open(&dir);
    let at = Position::new(40, 100, 40);
    let _ = ctx.world.block(at, &ctx.data.blocks);
    load_stored(&ctx);
    let all = everything(&ctx);
    let roots = all.len() - 1;
    lock(&ctx.entities).extend(all);
    crate::save(&ctx);
    let Some(Tag::Compound(root)) =
        mc_storage::RegionStorage::new(dir.join("dimensions/minecraft/overworld/entities"))
            .read_chunk(2, 2)
            .unwrap()
    else {
        panic!("no entity chunk");
    };
    let keys: Vec<&str> = root.iter().map(|(k, _)| k).collect();
    assert_eq!(keys.len(), 3, "{keys:?}");
    assert_eq!(
        root.get("DataVersion"),
        Some(&Tag::Int(ctx.data.version.world_version))
    );
    assert_eq!(root.get("Position"), Some(&Tag::IntArray(vec![2, 2])));
    let Some(Tag::List(list)) = root.get("Entities") else {
        panic!()
    };
    // The cow is inside its boat.
    assert_eq!(list.len(), roots);
    for e in list {
        let Tag::Compound(e) = e else { panic!() };
        assert!(matches!(e.get("id"), Some(Tag::String(s)) if s.starts_with("minecraft:")));
        assert!(matches!(e.get("UUID"), Some(Tag::IntArray(v)) if v.len() == 4));
        assert!(matches!(e.get("Pos"), Some(Tag::List(v)) if v.len() == 3
            && v.iter().all(|d| matches!(d, Tag::Double(_)))));
        assert!(matches!(e.get("Motion"), Some(Tag::List(v)) if v.len() == 3));
        assert!(
            matches!(e.get("Rotation"), Some(Tag::List(v)) if v.len() == 2
            && v.iter().all(|d| matches!(d, Tag::Float(_))))
        );
        assert!(matches!(e.get("Air"), Some(Tag::Short(_))));
        assert!(matches!(e.get("Fire"), Some(Tag::Short(_))));
        assert!(matches!(e.get("OnGround"), Some(Tag::Byte(_))));
        if matches!(e.get("id"), Some(Tag::String(s)) if s == "minecraft:oak_boat") {
            let Some(Tag::List(p)) = e.get("Passengers") else {
                panic!("boat without its cow")
            };
            assert!(matches!(&p[0], Tag::Compound(c)
                if c.get("id") == Some(&Tag::String("minecraft:cow".into()))));
        }
    }
    let item = list
        .iter()
        .find_map(|e| match e {
            Tag::Compound(c) if c.get("Thrower").is_some() => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(item.get("Age"), Some(&Tag::Short(-32768)));
    assert!(matches!(item.get("Owner"), Some(Tag::IntArray(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn other_dimensions_keep_their_entities() {
    if mc_data::McData::load(&data_dir()).is_err() {
        eprintln!("skipped: no game data");
        return;
    }
    let dir = folder("nether");
    let at = Position::new(8, 64, 8);
    let uuid = {
        let ctx = open(&dir);
        let nether = ctx.in_dimension("minecraft:the_nether").unwrap();
        let _ = nether.world.block(at, &nether.data.blocks);
        load_stored(&nether);
        let piglin = mob(&nether, Species::ZombifiedPiglin, [8.5, 64.0, 8.5]);
        let uuid = piglin.uuid;
        lock(&nether.entities).push(piglin);
        crate::save(&ctx);
        uuid
    };
    let ctx = open(&dir);
    assert!(lock(&ctx.entities).iter().all(|e| e.uuid != uuid));
    let nether = ctx.in_dimension("minecraft:the_nether").unwrap();
    let _ = nether.world.block(at, &nether.data.blocks);
    load_stored(&nether);
    assert!(lock(&nether.entities).iter().any(|e| e.uuid == uuid));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_entities_and_keys_are_written_back() {
    if mc_data::McData::load(&data_dir()).is_err() {
        eprintln!("skipped: no game data");
        return;
    }
    let dir = folder("unknown");
    let ctx = open(&dir);
    let at = Position::new(40, 100, 40);
    let _ = ctx.world.block(at, &ctx.data.blocks);
    load_stored(&ctx);
    let marker =
        nbt(r#"{id:"minecraft:marker",Pos:[40.5d,100d,40.5d],UUID:[I;1,2,3,4],data:{a:1b}}"#);
    let cow =
        nbt(r#"{id:"minecraft:cow",Pos:[41.5d,100d,40.5d],UUID:[I;5,6,7,8],Silent:1b,Glowing:1b}"#);
    ctx.world
        .loaded_entities_for_test(vec![Tag::Compound(marker.clone()), Tag::Compound(cow)]);
    load_stored(&ctx);
    crate::save(&ctx);
    let Some(Tag::Compound(root)) =
        mc_storage::RegionStorage::new(dir.join("dimensions/minecraft/overworld/entities"))
            .read_chunk(2, 2)
            .unwrap()
    else {
        panic!("no entity chunk");
    };
    let Some(Tag::List(list)) = root.get("Entities") else {
        panic!()
    };
    assert!(list.contains(&Tag::Compound(marker)));
    let cow = list
        .iter()
        .find_map(|e| match e {
            Tag::Compound(c) if c.get("id") == Some(&Tag::String("minecraft:cow".into())) => {
                Some(c)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(cow.get("Silent"), Some(&Tag::Byte(1)));
    assert_eq!(cow.get("Glowing"), Some(&Tag::Byte(1)));
    let _ = std::fs::remove_dir_all(&dir);
}
