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
        tags: Vec<String>,
        team: Option<Box<Decor>>,
    },
    Entity {
        uuid: u128,
        id: i32,
        /// `minecraft:item`.
        kind: String,
        pos: [f64; 3],
        /// The translation key of its name.
        name_key: String,
        tags: Vec<String>,
        team: Option<Box<Decor>>,
    },
}

/// The team a target is in, and how it decorates their name.
#[derive(Clone, Debug)]
pub struct Decor {
    pub team: String,
    pub name: Compound,
}

fn decor(board: &crate::scoreboard::Scoreboard, holder: &str, name: Tag) -> Option<Box<Decor>> {
    let team = board.team_of(holder)?;
    Some(Box::new(Decor {
        team: team.name.clone(),
        name: team.decorate(name),
    }))
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

    pub fn tags(&self) -> &[String] {
        match self {
            Target::Player { tags, .. } | Target::Entity { tags, .. } => tags,
        }
    }

    /// The name the scoreboard knows them by: a player's name, or an
    /// entity's UUID.
    pub fn holder(&self) -> String {
        match self {
            Target::Player { name, .. } => name.clone(),
            Target::Entity { uuid, .. } => crate::util::uuid_string(*uuid),
        }
    }

    /// The team they are in.
    pub fn team(&self) -> Option<&str> {
        match self {
            Target::Player { team, .. } | Target::Entity { team, .. } => {
                team.as_ref().map(|d| d.team.as_str())
            }
        }
    }

    pub fn is_player(&self) -> bool {
        matches!(self, Target::Player { .. })
    }

    /// The name as messages show it: clickable for players, with a hover
    /// card for everything.
    /// The name as scoreboard and team commands show it: as `display`,
    /// but hovering shows the name.
    pub fn feedback(&self) -> Tag {
        let mut shown = self.display();
        if let Tag::Compound(c) = &mut shown {
            let text = match self {
                Target::Player { name, .. } => name.clone(),
                Target::Entity { uuid, .. } => crate::util::uuid_string(*uuid),
            };
            c.insert("hover_event", show_text(&text));
        }
        shown
    }

    pub fn display(&self) -> Tag {
        match self {
            Target::Player {
                name,
                uuid,
                team: Some(d),
                ..
            } => {
                let mut c = d.name.clone();
                if let Tag::Compound(plain) = player_name(name, *uuid) {
                    for (k, v) in plain.iter() {
                        if k != "text" {
                            c.insert(k, v.clone());
                        }
                    }
                }
                Tag::Compound(c)
            }
            Target::Entity { team: Some(d), .. } => {
                let mut plain = self.clone();
                if let Target::Entity { team, .. } = &mut plain {
                    *team = None;
                }
                let mut c = d.name.clone();
                if let Tag::Compound(p) = plain.display() {
                    for (k, v) in p.iter() {
                        if k != "translate" {
                            c.insert(k, v.clone());
                        }
                    }
                }
                Tag::Compound(c)
            }
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
fn show_text(text: &str) -> Tag {
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", text);
    Tag::Compound(hover)
}

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
    let board = lock(&ctx.scoreboard);
    let mut out: Vec<Target> = lock(&ctx.players)
        .values()
        .filter_map(|p| {
            Some(Target::Player {
                uuid: p.profile.uuid,
                name: p.profile.name.clone(),
                pose: p.pose?,
                game_mode: p.game_mode,
                tags: p.extra.tags.clone(),
                team: decor(&board, &p.profile.name, Tag::from(p.profile.name.as_str())),
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
            mc_entity::Kind::Tnt { .. } => (
                "minecraft:tnt".to_owned(),
                "entity.minecraft.tnt".to_owned(),
            ),
            mc_entity::Kind::Merchant(m) if m.wandering => (
                "minecraft:wandering_trader".to_owned(),
                "entity.minecraft.wandering_trader".to_owned(),
            ),
            mc_entity::Kind::Merchant(_) => (
                "minecraft:villager".to_owned(),
                "entity.minecraft.villager".to_owned(),
            ),
            mc_entity::Kind::Mob(m) => (
                m.species.id().to_owned(),
                format!(
                    "entity.minecraft.{}",
                    m.species.id().trim_start_matches("minecraft:")
                ),
            ),
            mc_entity::Kind::ExperienceOrb { .. } => (
                "minecraft:experience_orb".to_owned(),
                "entity.minecraft.experience_orb".to_owned(),
            ),
            other => {
                let name = crate::fx::type_name(other).unwrap_or_default();
                (
                    format!("minecraft:{name}"),
                    format!("entity.minecraft.{name}"),
                )
            }
        };
        let mut key = Compound::new();
        key.insert("translate", name_key.as_str());
        let team = decor(
            &board,
            &crate::util::uuid_string(e.uuid),
            Tag::Compound(key),
        );
        out.push(Target::Entity {
            team,
            uuid: e.uuid,
            id: e.id,
            kind,
            pos: e.pos,
            name_key,
            tags: e.tags.clone(),
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
    // `@s` is the source, if the rest of the selector fits it.
    let all: Vec<Target> = if kind == 's' {
        match src.who {
            super::Who::Player(u) | super::Who::Entity(u) => {
                all.into_iter().filter(|t| t.uuid() == u).collect()
            }
            super::Who::Console => Vec::new(),
        }
    } else {
        all
    };
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
                // An entity is named by its type until it has a name.
                let name = match t {
                    Target::Player { name, .. } => name.as_str(),
                    Target::Entity { name_key, .. } => ctx.commands.lang.get(name_key),
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
        // `tag=x` has x, `tag=` has none; `!` turns either round.
        .filter(|t| {
            s.tags.iter().all(|(neg, want)| {
                let has = if want.is_empty() {
                    t.tags().is_empty()
                } else {
                    t.tags().iter().any(|g| g == want)
                };
                has != *neg
            })
        })
        .filter(|t| {
            s.teams.iter().all(|(neg, want)| match t.team() {
                _ if want.is_empty() => t.team().is_some() == *neg,
                team => (team == Some(want.as_str())) != *neg,
            })
        })
        .filter(|t| {
            if s.scores.is_empty() {
                return true;
            }
            let board = lock(&ctx.scoreboard);
            let holder = t.holder();
            s.scores.iter().all(|(objective, range)| {
                board
                    .score(&holder, objective)
                    .is_some_and(|sc| range.contains(f64::from(sc.value)))
            })
        })
        .filter(|t| {
            let p = t.pos();
            in_range(&s.distance, dist2(p, base).sqrt())
                && (s.dx.is_none() && s.dy.is_none() && s.dz.is_none() || {
                    // The volume touches the target's box, as in vanilla.
                    let d = [
                        s.dx.unwrap_or(0.0),
                        s.dy.unwrap_or(0.0),
                        s.dz.unwrap_or(0.0),
                    ];
                    let (lo, hi) = bounds(ctx, t);
                    (0..3).all(|i| {
                        let (min, max) = (base[i] + d[i].min(0.0), base[i] + d[i].max(0.0) + 1.0);
                        lo[i] < max && hi[i] > min
                    })
                })
                && rotation_in(&s.x_rotation, rotation(ctx, t)[1])
                && rotation_in(&s.y_rotation, rotation(ctx, t)[0])
        })
        .filter(|t| {
            let Some(range) = &s.level else {
                return true;
            };
            t.is_player()
                && lock(&ctx.vitals)
                    .get(&t.uuid())
                    .is_some_and(|v| range.contains(f64::from(v.xp_level)))
        })
        .filter(|t| {
            s.nbt.is_empty() || {
                let data = Tag::Compound(super::data::entity_nbt(ctx, t));
                s.nbt
                    .iter()
                    .all(|(neg, want)| mc_command::nbtpath::matches(want, &data) != *neg)
            }
        })
        .filter(|t| {
            let Some(text) = &s.advancements else {
                return true;
            };
            t.is_player()
                && advancement_tests(text).iter().all(|(id, criterion, want)| {
                    super::progress::has_done(ctx, t.uuid(), id, criterion.as_deref()) == *want
                })
        })
        .filter(|_| {
            s.predicates
                .iter()
                .all(|(neg, id)| super::execute::predicate_holds(ctx, id) != *neg)
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

/// The corners of a target's collision box.
fn bounds(ctx: &ServerContext, t: &Target) -> ([f64; 3], [f64; 3]) {
    let p = t.pos();
    if let Target::Entity { uuid, .. } = t
        && let Some(e) = lock(&ctx.entities).iter().find(|e| e.uuid == *uuid)
    {
        let b = e.aabb();
        return (b.min, b.max);
    }
    ([p[0] - 0.3, p[1], p[2] - 0.3], [p[0] + 0.3, p[1] + 1.8, p[2] + 0.3])
}

/// A target's [yaw, pitch].
fn rotation(ctx: &ServerContext, t: &Target) -> [f32; 2] {
    if let Target::Entity { uuid, .. } = t
        && let Some(e) = lock(&ctx.entities).iter().find(|e| e.uuid == *uuid)
        && let mc_entity::Kind::Mob(m) = &e.kind
    {
        return [m.yaw, m.pitch];
    }
    t.rot()
}

fn wrap_degrees(v: f64) -> f64 {
    let v = v % 360.0;
    if v >= 180.0 {
        v - 360.0
    } else if v < -180.0 {
        v + 360.0
    } else {
        v
    }
}

/// A rotation range test: both ends and the angle are wrapped to
/// [-180, 180), and a range whose start is past its end goes round.
fn rotation_in(r: &Option<Range>, angle: f32) -> bool {
    let Some(r) = r else {
        return true;
    };
    let min = wrap_degrees(r.min.unwrap_or(0.0));
    let max = wrap_degrees(r.max.unwrap_or(359.0));
    let a = wrap_degrees(f64::from(angle));
    if min > max {
        a >= min || a <= max
    } else {
        a >= min && a <= max
    }
}

/// The tests of an `advancements={...}` option: (advancement, criterion,
/// whether it must be done).
fn advancement_tests(text: &str) -> Vec<(String, Option<String>, bool)> {
    let mut out = Vec::new();
    let inner = text.trim().trim_start_matches('{');
    let inner = inner.strip_suffix('}').unwrap_or(inner);
    let mut rest = inner;
    while let Some(eq) = rest.find('=') {
        let id = rest[..eq].trim().trim_start_matches(',').trim().to_owned();
        let after = rest[eq + 1..].trim_start();
        if let Some(body) = after.strip_prefix('{') {
            let end = body.find('}').unwrap_or(body.len());
            for part in body[..end].split(',') {
                if let Some((c, v)) = part.split_once('=') {
                    out.push((id.clone(), Some(c.trim().to_owned()), v.trim() == "true"));
                }
            }
            rest = body.get(end + 1..).unwrap_or("");
        } else {
            let end = after.find(',').unwrap_or(after.len());
            out.push((id, None, after[..end].trim() == "true"));
            rest = &after[end..];
        }
    }
    out
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
