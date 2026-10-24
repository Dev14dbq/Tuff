//! Armadillos, pandas, sniffers and camels (land animals).

use super::*;
use mc_entity::mob::land::{Gene, personality, set_genes};

fn set_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut Mob) -> R) -> R {
    let mut all = lock(&ctx.entities);
    let Kind::Mob(m) = &mut all.iter_mut().find(|e| e.id == id).unwrap().kind else {
        panic!()
    };
    f(m)
}

#[test]
fn land_animals_of_the_second_batch_have_their_wiki_stats() {
    for (s, hp, size) in [
        (Species::Armadillo, 12.0, (0.7, 0.65)),
        (Species::Panda, 20.0, (1.3, 1.25)),
        (Species::Camel, 32.0, (1.7, 2.375)),
        (Species::CamelHusk, 32.0, (1.7, 2.375)),
        (Species::Sniffer, 14.0, (1.9, 1.75)),
    ] {
        assert_eq!(s.max_health(), hp, "{s:?}");
        assert_eq!(s.size(), size, "{s:?}");
        assert_eq!(Species::from_id(s.id()), Some(s));
    }
    assert!(Species::CamelHusk.undead() && Species::CamelHusk.food_tag().is_none());
}

#[test]
fn a_panda_shows_a_recessive_gene_only_when_both_genes_carry_it() {
    let mut m = Mob::new(Species::Panda, 7);
    set_genes(&mut m, Gene::Weak, Gene::Lazy);
    assert_eq!(personality(&m), Gene::Normal);
    set_genes(&mut m, Gene::Weak, Gene::Weak);
    assert_eq!(personality(&m), Gene::Weak);
    assert_eq!(m.max_health, 10.0);
    set_genes(&mut m, Gene::Lazy, Gene::Brown);
    assert_eq!(personality(&m), Gene::Lazy);
    assert_eq!(m.speed_attr, 0.07);
    // The genes are saved by name.
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut c = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &m, &mut c);
    assert_eq!(c.get("MainGene"), Some(&mc_nbt::Tag::String("lazy".into())));
    let mut back = Mob::new(Species::Panda, 9);
    crate::mobs::nbt::apply(&ctx, &mut back, &c);
    assert_eq!(
        (back.quirks.land.main_gene, back.quirks.land.hidden_gene),
        (Gene::Lazy as u8, Gene::Brown as u8)
    );
}

#[test]
fn an_armadillo_curls_up_near_undead_takes_less_and_opens_again() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let id = put(&ctx, Species::Armadillo, [0.5, -60.0, 0.5]);
    let zombie = put(&ctx, Species::Zombie, [3.5, -60.0, 0.5]);
    set_mob(&ctx, zombie, |m| m.no_ai = true);
    run(&ctx, 3);
    assert!(with_mob(&ctx, id, |m| m.quirks.land.rolled), "never curled");
    // 9 damage leaves (9 - 1) / 2 = 4 off its 12.
    ctx.mobs.harm(ctx.dim, id, 9.0, "minecraft:generic", None);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 8.0);
    for e in lock(&ctx.entities).iter_mut().filter(|e| e.id == zombie) {
        e.removed = true;
    }
    run(&ctx, 80);
    assert!(
        !with_mob(&ctx, id, |m| m.quirks.land.rolled),
        "never opened"
    );
}

#[test]
fn a_brush_takes_a_scute_off_an_armadillo() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Armadillo, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:brush", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert_eq!(items_of(&ctx, "minecraft:armadillo_scute"), 1);
    let worn = lock(&ctx.inventories)[&STEVE].slots[36]
        .as_ref()
        .map(|s| s.damage);
    assert_eq!(worn, Some(16));
}

#[test]
fn a_sniffer_digs_up_a_seed_and_waits_before_the_next() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [30.5, -60.0, 30.5]);
    let id = put(&ctx, Species::Sniffer, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.land.digging = 3);
    run(&ctx, 5);
    let seeds =
        items_of(&ctx, "minecraft:torchflower_seeds") + items_of(&ctx, "minecraft:pitcher_pod");
    assert_eq!(seeds, 1);
    assert!(with_mob(&ctx, id, |m| m.quirks.land.dig_cd) > 9000);
}

#[test]
fn two_players_ride_a_saddled_camel() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::Camel, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:saddle", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 0.5]), 0);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.saddled()));
    let mut all = lock(&ctx.entities);
    let e = all.iter_mut().find(|e| e.id == id).unwrap();
    assert!(crate::mobs::ride::mount(&ctx, e, 7000));
    assert!(crate::mobs::ride::mount(&ctx, e, 7001));
    assert!(!crate::mobs::ride::mount(&ctx, e, 7002));
}
