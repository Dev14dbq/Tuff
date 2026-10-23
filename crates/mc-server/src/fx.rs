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
    pub data: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub vel: [f64; 3],
    pub meta: Vec<(u8, MetadataValue)>,
    pub gear: Vec<(u8, Option<ItemStack>)>,
}

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

fn stack(item: (i32, i32)) -> ItemStack {
    ItemStack {
        damage: item.1,
        ..ItemStack::new(item.0, 1)
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
                Shot::Arrow(a) => {
                    if a.crit {
                        s.meta.push((8, MetadataValue::Byte(1)));
                    }
                }
                Shot::Snowball => {
                    s.meta.extend(item("minecraft:snowball"));
                }
                Shot::Egg => {
                    s.meta.extend(item("minecraft:egg"));
                }
                Shot::Pearl => {
                    s.meta.extend(item("minecraft:ender_pearl"));
                }
                Shot::Potion { item, .. } => {
                    s.meta.push((
                        play::metadata::ITEM,
                        MetadataValue::Item(Some(ItemStack::new(*item, 1))),
                    ));
                }
                Shot::Firework(_) => {
                    s.meta.extend(item("minecraft:firework_rocket"));
                }
                Shot::Fireball { .. } => {}
                Shot::Cloud { radius, .. } => {
                    s.meta.push((8, MetadataValue::Float(*radius)));
                }
                Shot::Bobber { .. } => {}
            }
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
                    if let Some(item) = f.item {
                        s.meta.push((8, MetadataValue::Item(Some(stack(item)))));
                    }
                    if f.rotation != 0 {
                        s.meta
                            .push((9, MetadataValue::VarInt(i32::from(f.rotation))));
                    }
                    if f.invisible {
                        s.meta
                            .push((play::metadata::FLAGS, MetadataValue::Byte(0x20)));
                    }
                }
                Decor::Painting { .. } => {
                    s.data = i32::from(h.facing);
                    s.yaw = hung_yaw(h.facing);
                }
                Decor::LeashKnot => {
                    s.data = i32::from(h.facing);
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
                            s.gear.push((slot as u8, Some(stack(*item))));
                        }
                    }
                }
            }
        }
        Kind::Vehicle(v) => {
            s.yaw = v.yaw;
            if v.craft == mc_entity::extra::Craft::Minecart(Cart::Command) {
                s.meta.extend(command_cart_meta(&v.command));
            }
        }
        _ => return None,
    }
    Some(s)
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
            Shot::Arrow(_) => "arrow",
            Shot::Snowball => "snowball",
            Shot::Egg => "egg",
            Shot::Pearl => "ender_pearl",
            Shot::Potion {
                lingering: true, ..
            } => "lingering_potion",
            Shot::Potion { .. } => "splash_potion",
            Shot::Firework(_) => "firework_rocket",
            Shot::Cloud { .. } => "area_effect_cloud",
            Shot::Bobber { .. } => "fishing_bobber",
        }
        .into(),
        Kind::Hung(h) => match &h.decor {
            Decor::Frame(f) if f.glow => "glow_item_frame",
            Decor::Frame(_) => "item_frame",
            Decor::Painting { .. } => "painting",
            Decor::LeashKnot => "leash_knot",
            Decor::Stand(_) => "armor_stand",
        }
        .into(),
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
