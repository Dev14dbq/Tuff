//! Target selectors resolved against the players and entities, and how
//! vanilla names them in messages.

use mc_command::CommandError;
use mc_command::args::{Range, Selector};
use mc_nbt::{Compound, Tag};

use super::{Result, Src};
use crate::context::{Pose, ServerContext};
use crate::tick::lock;

/// A player or another entity a selector found.
#[derive(Clone, Debug)]
pub enum Target {
    Player {
        uuid: u128,
        name: String,
        pose: Pose,
        game_mode: i32,
    },
    Entity {
        uuid: u128,
        id: i32,
        /// `minecraft:item`.
        kind: String,
        pos: [f64; 3],
        /// The translation key of its name.
        name_key: String,
    },
}

impl Target {
    pub fn uuid(&self) -> u128 {
        match self {
            Target::Player { uuid, .. } | Target::Entity { uuid, .. } => *uuid,
        }
    }

    pub fn pos(&self) -> [f64; 3] {
        match self {
            Target::Player { pose, .. } => [pose.x, pose.y, pose.z],
            Target::Entity { pos, .. } => *pos,
        }
    }

    pub fn rot(&self) -> [f32; 2] {
        match self {
            Target::Player { pose, .. } => [pose.yaw, pose.pitch],
            Target::Entity { .. } => [0.0, 0.0],
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Target::Player { .. } => "minecraft:player",
            Target::Entity { kind, .. } => kind,
        }
    }

    pub fn is_player(&self) -> bool {
        matches!(self, Target::Player { .. })
    }

    /// The name as messages show it: clickable for players, with a hover
    /// card for everything.
    pub fn display(&self) -> Tag {
        match self {
            Target::Player { uuid, name, .. } => player_name(name, *uuid),
            Target::Entity {
                uuid,
                kind,
                name_key,
                ..
            } => {
                let mut name = Compound::new();
                name.insert("translate", name_key.as_str());
                let mut hover = Compound::new();
                hover.insert("action", "show_entity");
                hover.insert("id", kind.as_str());
                hover.insert("name", Tag::Compound(name));
                hover.insert("uuid", Tag::IntArray(uuid_ints(*uuid)));
                let mut c = Compound::new();
                c.insert("translate", name_key.as_str());
                c.insert("hover_event", Tag::Compound(hover));
                c.insert("insertion", Tag::String(crate::util::uuid_string(*uuid)));
                Tag::Compound(c)
            }
        }
    }
}

/// A UUID as the four ints of an int array.
pub fn uuid_ints(u: u128) -> Vec<i32> {
    (0..4).map(|i| (u >> (96 - 32 * i)) as u32 as i32).collect()
}

/// A player's name in messages: click to message them, hover for who.
pub fn player_name(name: &str, uuid: u128) -> Tag {
    let mut click = Compound::new();
    click.insert("action", "suggest_command");
    click.insert("command", Tag::String(format!("/tell {name} ")));
    let mut hover = Compound::new();
    hover.insert("action", "show_entity");
    hover.insert("id", "minecraft:player");
    hover.insert("name", name);
    hover.insert("uuid", Tag::IntArray(uuid_ints(uuid)));
    let mut c = Compound::new();
    c.insert("text", name);
    c.insert("click_event", Tag::Compound(click));
    c.insert("hover_event", Tag::Compound(hover));
    c.insert("insertion", name);
    Tag::Compound(c)
}

fn game_mode_name(mode: i32) -> &'static str {
    match mode {
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "survival",
    }
}

fn item_key(ctx: &ServerContext, item: i32) -> String {
    let name = ctx.items.name(item).unwrap_or("minecraft:air");
    let short = name.trim_start_matches("minecraft:");
    if ctx.data.blocks.get(name).is_some() {
        format!("block.minecraft.{short}")
    } else {
        format!("item.minecraft.{short}")
    }
}

/// Every player and entity, players first in join order.
pub fn everyone(ctx: &ServerContext) -> Vec<Target> {
    let mut out: Vec<Target> = lock(&ctx.players)
        .values()
        .filter_map(|p| {
            Some(Target::Player {
                uuid: p.profile.uuid,
                name: p.profile.name.clone(),
                pose: p.pose?,
                game_mode: p.game_mode,
            })
        })
        .collect();
    for e in lock(&ctx.entities).iter().filter(|e| !e.removed) {
        let (kind, name_key) = match &e.kind {
            mc_entity::Kind::Item { item, .. } => {
                ("minecraft:item".to_owned(), item_key(ctx, *item))
            }
            mc_entity::Kind::FallingBlock { .. } => (
                "minecraft:falling_block".to_owned(),
                "entity.minecraft.falling_block".to_owned(),
            ),
        };
        out.push(Target::Entity {
            uuid: e.uuid,
            id: e.id,
            kind,
            pos: e.pos,
            name_key,
        });
    }
    out
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn matches_type(ctx: &ServerContext, t: &Target, want: &str) -> bool {
    match want.strip_prefix('#') {
        Some(tag) => ctx
            .commands
            .env
            .tag("minecraft:entity_type", tag)
            .iter()
            .any(|k| k == t.kind()),
        None => t.kind() == want,
    }
}

fn in_range(r: &Option<Range>, v: f64) -> bool {
    r.as_ref().is_none_or(|r| r.contains(v))
}

/// What a selector means for `src`.
pub fn resolve(ctx: &ServerContext, s: &Selector, src: &Src) -> Vec<Target> {
    let all = everyone(ctx);
    if let Some(name) = &s.player {
        return all
            .into_iter()
            .filter(|t| matches!(t, Target::Player { name: n, .. } if n.eq_ignore_ascii_case(name)))
            .collect();
    }
    if let Some(u) = s.uuid {
        return all.into_iter().filter(|t| t.uuid() == u).collect();
    }
    let kind = s.kind.unwrap_or('e');
    if kind == 's' {
        return match src.who {
            super::Who::Player(u) | super::Who::Entity(u) => {
                all.into_iter().filter(|t| t.uuid() == u).collect()
            }
            super::Who::Console => Vec::new(),
        };
    }
    let base = [
        s.x.unwrap_or(src.pos[0]),
        s.y.unwrap_or(src.pos[1]),
        s.z.unwrap_or(src.pos[2]),
    ];
    let players_only = matches!(kind, 'p' | 'a' | 'r');
    let mut found: Vec<Target> = all
        .into_iter()
        .filter(|t| !players_only || t.is_player())
        .filter(|t| {
            s.types
                .iter()
                .all(|(neg, want)| matches_type(ctx, t, want) != *neg)
        })
        .filter(|t| {
            s.names.iter().all(|(neg, want)| {
                let name = match t {
                    Target::Player { name, .. } => name.as_str(),
                    Target::Entity { .. } => "",
                };
                (name == want) != *neg
            })
        })
        .filter(|t| {
            s.gamemodes.iter().all(|(neg, want)| match t {
                Target::Player { game_mode, .. } => (game_mode_name(*game_mode) == want) != *neg,
                Target::Entity { .. } => false,
            })
        })
        // No entity has tags or a team yet.
        .filter(|_| s.tags.iter().all(|(neg, want)| *neg || want.is_empty()))
        .filter(|_| s.teams.iter().all(|(neg, want)| *neg != want.is_empty()))
        .filter(|t| {
            let p = t.pos();
            in_range(&s.distance, dist2(p, base).sqrt())
                && (s.dx.is_none() && s.dy.is_none() && s.dz.is_none() || {
                    let d = [
                        s.dx.unwrap_or(0.0),
                        s.dy.unwrap_or(0.0),
                        s.dz.unwrap_or(0.0),
                    ];
                    (0..3).all(|i| {
                        let (lo, hi) = (
                            base[i].min(base[i] + d[i]),
                            base[i].max(base[i] + d[i]) + 1.0,
                        );
                        p[i] >= lo && p[i] < hi
                    })
                })
                && in_range(&s.x_rotation, f64::from(t.rot()[1]))
                && in_range(&s.y_rotation, f64::from(t.rot()[0]))
        })
        .collect();
    let sort = s.sort.as_deref().unwrap_or(match kind {
        'p' | 'n' => "nearest",
        'r' => "random",
        _ => "arbitrary",
    });
    match sort {
        "nearest" => found.sort_by(|a, b| dist2(a.pos(), base).total_cmp(&dist2(b.pos(), base))),
        "furthest" => found.sort_by(|a, b| dist2(b.pos(), base).total_cmp(&dist2(a.pos(), base))),
        "random" => {
            for i in (1..found.len()).rev() {
                let j = (crate::util::random_u64() % (i as u64 + 1)) as usize;
                found.swap(i, j);
            }
        }
        _ => {}
    }
    if let Some(limit) = s.limit {
        found.truncate(limit.max(0) as usize);
    }
    found
}

/// Entities (or players) for an argument, failing as vanilla does when
/// none are found.
pub fn entities(
    ctx: &ServerContext,
    s: &Selector,
    src: &Src,
    players: bool,
) -> Result<Vec<Target>> {
    let found = resolve(ctx, s, src);
    let found: Vec<Target> = if players {
        found.into_iter().filter(Target::is_player).collect()
    } else {
        found
    };
    if found.is_empty() {
        let key = if players {
            "argument.entity.notfound.player"
        } else {
            "argument.entity.notfound.entity"
        };
        return Err(CommandError::new(key, Vec::new()));
    }
    Ok(found)
}
