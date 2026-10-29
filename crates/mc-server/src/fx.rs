//! What clients are told about the thrown, hung and ridden entities
//! (`mc_entity::extra`): how they are spawned, their metadata, and the sounds
//! and effects around them.

use mc_entity::extra::{Cart, Craft, Decor, Shot};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::{self, ItemStack, MetadataValue};

use crate::context::{Event, ServerContext};

/// Where an entity is now, for clients to place and move it by.
#[derive(Clone, Debug, PartialEq)]
pub struct Sync {
    pub id: i32,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

/// Packets everyone near (or everyone who has the entity) gets.
#[derive(Clone, Debug)]
pub enum Fx {
    Sound(play::Sound),
    Level(play::LevelEvent),
    Particles(play::LevelParticles),
    Status(play::EntityEvent),
    Equip(play::SetEquipment),
    Link(play::SetEntityLink),
    Passengers(play::SetPassengers),
    Motion(play::SetEntityMotion),
    Damage(play::DamageEvent),
    Swing(play::SwingAnimation),
    Data(play::SetEntityData),
    Cracks(play::BlockDestruction),
}

impl Fx {
    /// The entity the packet is about, if it is about one.
    pub fn entity(&self) -> Option<i32> {
        match self {
            Fx::Status(p) => Some(p.entity_id),
            Fx::Equip(p) => Some(p.entity_id),
            Fx::Link(p) => Some(p.entity_id),
            Fx::Passengers(p) => Some(p.vehicle),
            Fx::Motion(p) => Some(p.entity_id),
            Fx::Damage(p) => Some(p.entity_id),
            Fx::Swing(p) => Some(p.entity_id),
            Fx::Data(p) => Some(p.entity_id),
            _ => None,
        }
    }

    /// Where the packet happens, if it happens somewhere.
    pub fn at(&self) -> Option<[f64; 3]> {
        match self {
            Fx::Sound(p) => Some([p.x, p.y, p.z]),
            Fx::Level(p) if !p.global => Some([
                f64::from(p.position.x),
                f64::from(p.position.y),
                f64::from(p.position.z),
            ]),
            Fx::Particles(p) => Some(p.pos),
            Fx::Cracks(p) => Some([
                f64::from(p.position.x),
                f64::from(p.position.y),
                f64::from(p.position.z),
            ]),
            _ => None,
        }
    }
}

/// Sends `fx` to whoever should get it.
pub fn emit(ctx: &ServerContext, fx: Fx) {
    let _ = ctx.events.send(Event::Fx(fx));
}

/// A sound by name (`entity.arrow.hit`) at a place.
pub fn sound(
    ctx: &ServerContext,
    name: &str,
    category: i32,
    at: [f64; 3],
    volume: f32,
    pitch: f32,
) {
    emit(
        ctx,
        Fx::Sound(play::Sound {
            id: None,
            name: format!("minecraft:{name}"),
            category,
            x: at[0],
            y: at[1],
            z: at[2],
            volume,
            pitch,
            seed: crate::util::random_u64() as i64,
        }),
    );
}

/// Sound categories.
pub mod category {
    pub const NEUTRAL: i32 = 6;
    pub const PLAYERS: i32 = 7;
    pub const AMBIENT: i32 = 8;
    pub const BLOCKS: i32 = 4;
    pub const HOSTILE: i32 = 5;
}

/// Particles that need no data (`crit`, `poof`, ...), by name.
pub fn particles(
    ctx: &ServerContext,
    name: &str,
    at: [f64; 3],
    spread: f32,
    speed: f32,
    count: i32,
) {
    let Some(id) = ctx.data.builtin.id("minecraft:particle_type", name) else {
        return;
    };
    let mut particle = Vec::new();
    mc_protocol::Write::put_varint(&mut particle, id);
    emit(
        ctx,
        Fx::Particles(play::LevelParticles {
            particle,
            force: false,
            always_show: false,
            pos: at,
            delta: [spread; 3],
            speed,
            count,
        }),
    );
}

/// Wither spawn (1023) and the dragon's death (1028). With
/// `global_sound_events` every client plays them; otherwise only those
/// close enough to `at`.
pub fn heard(ctx: &ServerContext, event: i32, at: [f64; 3]) {
    emit(
        ctx,
        Fx::Level(play::LevelEvent {
            event,
            position: mc_protocol::Position::new(
                at[0].floor() as i32,
                at[1].floor() as i32,
                at[2].floor() as i32,
            ),
            data: 0,
            global: ctx.game_rule_bool("global_sound_events"),
        }),
    );
}

/// A world event (2002 potion shatter: data is the colour).
pub fn level_event(ctx: &ServerContext, event: i32, at: [f64; 3], data: i32) {
    emit(
        ctx,
        Fx::Level(play::LevelEvent {
            event,
            position: mc_protocol::Position::new(
                at[0].floor() as i32,
                at[1].floor() as i32,
                at[2].floor() as i32,
            ),
            data,
            global: false,
        }),
    );
}

/// An entity status (17: a firework bursts, 9: ...).
pub fn status(ctx: &ServerContext, entity_id: i32, status: i8) {
    emit(ctx, Fx::Status(play::EntityEvent { entity_id, status }));
}

/// Yaw and pitch (degrees) of a velocity.
pub fn facing_of(v: [f64; 3]) -> (f32, f32) {
    let flat = v[0].hypot(v[2]);
    (
        v[0].atan2(v[2]).to_degrees() as f32,
        v[1].atan2(flat).to_degrees() as f32,
    )
}

/// How a spawned entity is described to clients.
pub struct Spawn {
    pub kind: String,
    /// Where clients are told it is, when not the entity's own position
    /// (a painting's block).
    pub at: Option<[f64; 3]>,
    pub data: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub vel: [f64; 3],
    pub meta: Vec<(u8, MetadataValue)>,
    pub gear: Vec<(u8, Option<ItemStack>)>,
}

/// Serializer type of a painting variant holder.
const PAINTING_VARIANT: i32 = 34;
/// Serializer type of a dye color (a cushion's), as a vanilla server sends it.
const DYE_COLOR: i32 = 43;

/// Yaw a hanging entity looks out with, from its facing (north 2, south 3,
/// west 4, east 5).
pub fn hung_yaw(facing: u8) -> f32 {
    match facing {
        4 => 90.0,
        2 => 180.0,
        5 => 270.0,
        _ => 0.0,
    }
}

/// The entity's type name and fields for clients, for the kinds in
/// `mc_entity::extra`; `None` for the others.
pub fn describe(ctx: &ServerContext, e: &Entity) -> Option<Spawn> {
    let owner_id = |owner: Option<u128>| {
        owner.map_or(0, |u| {
            crate::tick::lock(&ctx.players)
                .get(&u)
                .map_or(0, |p| p.entity_id)
        })
    };
    let mut s = Spawn {
        kind: type_name(&e.kind)?,
        at: None,
        data: 0,
        yaw: 0.0,
        pitch: 0.0,
        vel: e.vel,
        meta: Vec::new(),
        gear: Vec::new(),
    };
    match &e.kind {
        Kind::Projectile(p) => {
            s.yaw = p.yaw;
            s.pitch = p.pitch;
            s.data = match p.mob {
                Some((id, _)) => id,
                None => owner_id(p.owner),
            };
            let item = |name: &str| {
                ctx.items.id(name).map(|i| {
                    (
                        play::metadata::ITEM,
                        MetadataValue::Item(Some(ItemStack::new(i, 1))),
                    )
                })
            };
            match &p.shot {
                Shot::Arrow(a) => s.meta.extend(arrow_meta(a)),
                Shot::Snowball => {
                    s.meta.extend(item("minecraft:snowball"));
                }
                Shot::Egg => {
                    s.meta.extend(item("minecraft:egg"));
                }
                Shot::Pearl => {
                    s.meta.extend(item("minecraft:ender_pearl"));
                }
                Shot::Eye { .. } => {
                    s.meta.extend(item("minecraft:ender_eye"));
                }
                Shot::Bottle => {
                    s.meta.extend(item("minecraft:experience_bottle"));
                }
                Shot::Potion { item, color, .. } => {
                    s.meta.push((
                        play::metadata::ITEM,
                        MetadataValue::Item(Some(potion_stack(*item, *color))),
                    ));
                }
                Shot::Firework(f) => {
                    match &f.stack {
                        Some(st) => s
                            .meta
                            .push((play::metadata::ITEM, MetadataValue::Item(Some(st.clone())))),
                        None => s.meta.extend(item("minecraft:firework_rocket")),
                    }
                    if let Some(rider) = f.boosts {
                        let id = owner_id(Some(rider));
                        s.meta.push((9, MetadataValue::OptVarInt(Some(id))));
                    }
                    if f.shot_at_angle {
                        s.meta.push((10, MetadataValue::Boolean(true)));
                    }
                }
                Shot::Fireball { .. } | Shot::DragonFireball => {}
                Shot::WitherSkull { dangerous } => {
                    s.meta.push((8, MetadataValue::Boolean(*dangerous)));
                }
                Shot::Breath { radius, .. } => {
                    s.meta.push((8, MetadataValue::Float(*radius)));
                    if let Some(id) = ctx
                        .data
                        .builtin
                        .id("minecraft:particle_type", "minecraft:dragon_breath")
                    {
                        let mut bytes = Vec::new();
                        mc_protocol::Write::put_varint(&mut bytes, id);
                        mc_protocol::Write::put_f32(&mut bytes, 1.0);
                        s.meta.push((10, MetadataValue::Particle(bytes)));
                    }
                }
                Shot::Spit => {}
                Shot::Cloud { radius, .. } => {
                    s.meta.push((8, MetadataValue::Float(*radius)));
                }
                Shot::Bobber { .. } => {}
                Shot::Lightning { .. } => s.data = 0,
                Shot::ItemSpawner { item, .. } => {
                    s.meta.push((
                        play::metadata::ITEM,
                        MetadataValue::Item(Some(item.clone())),
                    ));
                }
                Shot::WindCharge { .. } | Shot::ShulkerBullet { .. } | Shot::Fangs { .. } => {}
            }
        }
        Kind::Crystal(c) => {
            s.meta = crate::bosses::crystal::metadata(c);
        }
        Kind::Hung(h) => {
            s.yaw = h.yaw;
            match &h.decor {
                Decor::Frame(f) => {
                    s.data = i32::from(h.facing);
                    s.yaw = hung_yaw(h.facing);
                    s.pitch = match h.facing {
                        0 => 90.0,
                        1 => -90.0,
                        _ => 0.0,
                    };
                    s.meta
                        .push((8, MetadataValue::Direction(i32::from(h.facing))));
                    if let Some(item) = &f.item {
                        s.meta.push((9, MetadataValue::Item(Some(item.clone()))));
                    }
                    if f.rotation != 0 {
                        s.meta
                            .push((10, MetadataValue::VarInt(i32::from(f.rotation))));
                    }
                    if f.invisible {
                        s.meta
                            .push((play::metadata::FLAGS, MetadataValue::Byte(0x20)));
                    }
                }
                Decor::Painting { variant, size } => {
                    s.data = i32::from(h.facing);
                    s.yaw = hung_yaw(h.facing);
                    // Clients work its box out from its block, size and facing.
                    let cell = crate::decor::painting::cell_of(e.pos, h.facing, *size);
                    s.at = Some(cell.map(f64::from));
                    s.meta
                        .push((8, MetadataValue::Direction(i32::from(h.facing))));
                    if let Some(id) = crate::decor::painting::network_id(ctx, variant) {
                        // A holder: the registry id plus one (0 is inline).
                        s.meta
                            .push((9, MetadataValue::Holder(PAINTING_VARIANT, id + 1)));
                    }
                }
                Decor::LeashKnot => {
                    s.data = i32::from(h.facing);
                }
                Decor::Cushion { color, .. } => {
                    s.meta
                        .push((8, MetadataValue::Holder(DYE_COLOR, i32::from(*color))));
                }
                Decor::Stand(a) => {
                    if a.invisible {
                        s.meta
                            .push((play::metadata::FLAGS, MetadataValue::Byte(0x20)));
                    }
                    let flags = i8::from(a.small)
                        | if a.arms { 4 } else { 0 }
                        | if a.no_base { 8 } else { 0 }
                        | if a.marker { 16 } else { 0 };
                    if flags != 0 {
                        s.meta.push((15, MetadataValue::Byte(flags)));
                    }
                    let defaults = mc_entity::extra::Stand::default().pose;
                    for (i, angles) in a.pose.iter().enumerate() {
                        if *angles != defaults[i] {
                            s.meta
                                .push((16 + i as u8, MetadataValue::Rotations(*angles)));
                        }
                    }
                    for (slot, gear) in a.gear.iter().enumerate() {
                        if let Some(item) = gear {
                            s.gear.push((slot as u8, Some(item.clone())));
                        }
                    }
                }
            }
        }
        Kind::Vehicle(v) => {
            s.yaw = v.yaw;
            s.meta.extend(crate::vehicles::meta(v));
            if v.craft == mc_entity::extra::Craft::Minecart(Cart::Command) {
                s.meta.extend(command_cart_meta(&v.command));
            }
        }
        _ => return None,
    }
    Some(s)
}

/// An arrow's or trident's metadata: flags (critical, flying back
/// through blocks), Piercing, in the ground, the tipped arrow's colour,
/// the trident's Loyalty and glint.
pub fn arrow_meta(a: &mc_entity::extra::Arrow) -> Vec<(u8, MetadataValue)> {
    let mut meta = Vec::new();
    let flags = i8::from(a.crit) | if a.returning { 2 } else { 0 };
    if flags != 0 {
        meta.push((8, MetadataValue::Byte(flags)));
    }
    if a.pierce > 0 {
        meta.push((9, MetadataValue::Byte(a.pierce.min(127) as i8)));
    }
    if a.stuck > 0 {
        meta.push((10, MetadataValue::Boolean(true)));
    }
    if a.trident {
        let stack = a.stack.as_ref();
        let loyalty = stack.map_or(0, |s| {
            crate::enchants::levels(s)
                .iter()
                .find(|(n, _)| n == "minecraft:loyalty")
                .map_or(0, |l| l.1)
        });
        if loyalty > 0 {
            meta.push((11, MetadataValue::Byte(loyalty.min(127) as i8)));
        }
        if stack.is_some_and(shines) {
            meta.push((12, MetadataValue::Boolean(true)));
        }
    } else if !a.spectral && !a.effects.is_empty() {
        let color = crate::projectiles::potions::color(&a.effects);
        meta.push((11, MetadataValue::VarInt(color)));
    }
    meta
}

/// Whether a stack has the enchantment glint: enchanted, or its
/// `enchantment_glint_override` says so.
pub fn shines(stack: &ItemStack) -> bool {
    match stack.components.get("minecraft:enchantment_glint_override") {
        Some(mc_nbt::Tag::Byte(b)) => *b != 0,
        _ => !crate::enchants::levels(stack).is_empty(),
    }
}

/// A thrown potion's item as clients draw it: tinted its colour.
fn potion_stack(item: i32, color: i32) -> ItemStack {
    let mut s = ItemStack::new(item, 1);
    let mut c = mc_nbt::Compound::new();
    c.insert("custom_color", mc_nbt::Tag::Int(color));
    s.components
        .set("minecraft:potion_contents", mc_nbt::Tag::Compound(c));
    s
}

/// A command block minecart's command and last output, as its window
/// shows them.
pub fn command_cart_meta(c: &mc_entity::extra::CommandCart) -> Vec<(u8, MetadataValue)> {
    let mut output = mc_nbt::Compound::new();
    output.insert(
        "text",
        mc_nbt::Tag::String(c.last_output.clone().unwrap_or_default()),
    );
    vec![
        (13, MetadataValue::String(c.command.clone())),
        (14, MetadataValue::Component(mc_nbt::Tag::Compound(output))),
    ]
}

/// `minecraft:entity_type` id of a name from [`Spawn::kind`].
pub fn type_id(ctx: &ServerContext, name: &str) -> Option<i32> {
    ctx.data
        .builtin
        .id("minecraft:entity_type", &format!("minecraft:{name}"))
}

/// The `minecraft:entity_type` name (without the namespace) of the kinds in
/// `mc_entity::extra`.
pub fn type_name(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Projectile(p) => match &p.shot {
            Shot::Arrow(a) if a.trident => "trident",
            Shot::Arrow(a) if a.spectral => "spectral_arrow",
            Shot::Fireball { small: true } => "small_fireball",
            Shot::Fireball { small: false } => "fireball",
            Shot::WitherSkull { .. } => "wither_skull",
            Shot::DragonFireball => "dragon_fireball",
            Shot::Breath { .. } => "area_effect_cloud",
            Shot::Arrow(_) => "arrow",
            Shot::Snowball => "snowball",
            Shot::Spit => "llama_spit",
            Shot::Egg => "egg",
            Shot::Pearl => "ender_pearl",
            Shot::Eye { .. } => "eye_of_ender",
            Shot::Bottle => "experience_bottle",
            Shot::Potion {
                lingering: true, ..
            } => "lingering_potion",
            Shot::Potion { .. } => "splash_potion",
            Shot::Firework(_) => "firework_rocket",
            Shot::Cloud { .. } => "area_effect_cloud",
            Shot::Bobber { .. } => "fishing_bobber",
            Shot::Lightning { .. } => "lightning_bolt",
            Shot::WindCharge { breeze: true } => "breeze_wind_charge",
            Shot::WindCharge { breeze: false } => "wind_charge",
            Shot::ShulkerBullet { .. } => "shulker_bullet",
            Shot::Fangs { .. } => "evoker_fangs",
            Shot::ItemSpawner { .. } => "ominous_item_spawner",
        }
        .into(),
        Kind::Hung(h) => match &h.decor {
            Decor::Frame(f) if f.glow => "glow_item_frame",
            Decor::Frame(_) => "item_frame",
            Decor::Painting { .. } => "painting",
            Decor::LeashKnot => "leash_knot",
            Decor::Cushion { .. } => "cushion",
            Decor::Stand(_) => "armor_stand",
        }
        .into(),
        Kind::Crystal(_) => "end_crystal".into(),
        Kind::Vehicle(v) => match &v.craft {
            Craft::Boat { wood, chest } => match (wood.as_str(), chest) {
                ("bamboo", false) => "bamboo_raft".into(),
                ("bamboo", true) => "bamboo_chest_raft".into(),
                (w, false) => format!("{w}_boat"),
                (w, true) => format!("{w}_chest_boat"),
            },
            Craft::Minecart(c) => match c {
                Cart::Rideable => "minecart",
                Cart::Chest => "chest_minecart",
                Cart::Furnace => "furnace_minecart",
                Cart::Hopper => "hopper_minecart",
                Cart::Tnt => "tnt_minecart",
                Cart::Command => "command_block_minecart",
            }
            .into(),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod component_tests {
    use super::*;
    use crate::testkit::{flat, stack};
    use mc_entity::extra::{Arrow, Frame, Hung, Projectile, Stand};

    fn entity(kind: Kind) -> Entity {
        Entity {
            id: 5,
            uuid: 5,
            kind,
            pos: [0.0; 3],
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    fn shot(s: Shot) -> Kind {
        Kind::Projectile(Projectile {
            shot: s,
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        })
    }

    #[test]
    fn items_keep_their_components_for_clients() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // An enchanted sword in a frame shines.
        let sword = stack(&ctx, "diamond_sword", &[("sharpness", 5)]);
        assert!(shines(&sword));
        let frame = entity(Kind::Hung(Hung {
            decor: Decor::Frame(Frame {
                glow: false,
                item: Some(sword.clone()),
                rotation: 0,
                invisible: false,
                fixed: false,
            }),
            facing: 2,
            yaw: 0.0,
            anchor: [0; 3],
        }));
        let d = describe(&ctx, &frame).unwrap();
        assert!(
            d.meta
                .contains(&(9, MetadataValue::Item(Some(sword.clone()))))
        );
        // A shield with a banner's patterns on an armor stand.
        let mut shield = stack(&ctx, "shield", &[]);
        shield
            .components
            .set("minecraft:base_color", mc_nbt::Tag::String("red".into()));
        let mut gear: [Option<ItemStack>; 6] = Default::default();
        gear[1] = Some(shield.clone());
        let stand = entity(Kind::Hung(Hung {
            decor: Decor::Stand(Box::new(Stand {
                gear,
                ..Stand::default()
            })),
            facing: 0,
            yaw: 0.0,
            anchor: [0; 3],
        }));
        let d = describe(&ctx, &stand).unwrap();
        assert_eq!(d.gear, vec![(1, Some(shield))]);
        // A tipped arrow's colour, a trident's Loyalty and glint.
        let mut a = Arrow::new(1);
        a.effects = vec![("minecraft:poison".into(), 0, 100)];
        let d = describe(&ctx, &entity(shot(Shot::Arrow(a.clone())))).unwrap();
        let color = crate::projectiles::potions::color(&a.effects);
        assert!(d.meta.contains(&(11, MetadataValue::VarInt(color))));
        let mut t = Arrow::new(1);
        t.trident = true;
        t.stack = Some(stack(&ctx, "trident", &[("loyalty", 2)]));
        let d = describe(&ctx, &entity(shot(Shot::Arrow(t)))).unwrap();
        assert_eq!(d.kind, "trident");
        assert!(d.meta.contains(&(11, MetadataValue::Byte(2))));
        assert!(d.meta.contains(&(12, MetadataValue::Boolean(true))));
        // A thrown potion is tinted its colour.
        let splash = ctx.items.id("minecraft:splash_potion").unwrap();
        let d = describe(
            &ctx,
            &entity(shot(Shot::Potion {
                item: splash,
                lingering: false,
                effects: Vec::new(),
                color: 0x123456,
            })),
        )
        .unwrap();
        let Some((_, MetadataValue::Item(Some(s)))) = d.meta.iter().find(|m| m.0 == 8) else {
            panic!()
        };
        let Some(mc_nbt::Tag::Compound(c)) = s.components.get("minecraft:potion_contents") else {
            panic!()
        };
        assert_eq!(c.get("custom_color"), Some(&mc_nbt::Tag::Int(0x123456)));
        // The rocket's own item, for its stars.
        let rocket = crate::projectiles::crossbow::rocket_stack(&ctx, 1, 2);
        let f = crate::projectiles::firework::rocket(&rocket, None, true);
        let d = describe(&ctx, &entity(shot(Shot::Firework(f)))).unwrap();
        assert!(d.meta.contains(&(8, MetadataValue::Item(Some(rocket)))));
        assert!(d.meta.contains(&(10, MetadataValue::Boolean(true))));
    }
}
