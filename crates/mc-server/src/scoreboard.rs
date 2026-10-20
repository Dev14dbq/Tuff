//! The scoreboard: objectives, scores, display slots and teams, kept as
//! vanilla keeps them in the world's `data/minecraft/scoreboard.dat`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{
    NumberFormat, ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore,
    TeamParams,
};

use crate::fastmap::{JavaSet, Order};

/// Chat colours by id.
pub const COLORS: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

/// Visibility rules (name tags, death messages) by id.
pub const VISIBILITY: [&str; 4] = ["always", "never", "hideForOtherTeams", "hideForOwnTeam"];
/// Collision rules by id.
pub const COLLISION: [&str; 4] = ["always", "never", "pushOtherTeams", "pushOwnTeam"];

/// Display slot names by id: list, sidebar, below_name, then a sidebar per
/// team colour.
pub fn slot_name(slot: usize) -> String {
    match slot {
        0 => "list".into(),
        1 => "sidebar".into(),
        2 => "below_name".into(),
        n => format!("sidebar.team.{}", COLORS[n - 3]),
    }
}

pub fn slot_id(name: &str) -> Option<usize> {
    match name {
        "list" => Some(0),
        "sidebar" => Some(1),
        "below_name" => Some(2),
        _ => {
            let color = name.strip_prefix("sidebar.team.")?;
            COLORS.iter().position(|c| *c == color).map(|i| i + 3)
        }
    }
}

/// Criteria whose scores the game sets and commands may not.
pub fn read_only(criteria: &str) -> bool {
    matches!(
        criteria,
        "health" | "xp" | "level" | "food" | "air" | "armor"
    )
}

#[derive(Clone, Debug)]
pub struct Objective {
    pub name: String,
    pub criteria: String,
    pub display: Tag,
    pub hearts: bool,
    pub auto_update: bool,
    pub format: Option<NumberFormat>,
}

#[derive(Clone, Debug)]
pub struct Score {
    pub value: i32,
    pub locked: bool,
    pub display: Option<Tag>,
    pub format: Option<NumberFormat>,
}

impl Default for Score {
    fn default() -> Self {
        Score {
            value: 0,
            locked: true,
            display: None,
            format: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Team {
    pub name: String,
    pub display: Tag,
    pub color: Option<usize>,
    pub prefix: Tag,
    pub suffix: Tag,
    pub friendly_fire: bool,
    pub see_invisible: bool,
    pub nametag: usize,
    pub death_messages: usize,
    pub collision: usize,
    pub members: JavaSet,
}

impl Team {
    pub fn new(name: &str) -> Self {
        Team {
            name: name.to_owned(),
            display: Tag::from(name),
            color: None,
            prefix: Tag::from(""),
            suffix: Tag::from(""),
            friendly_fire: true,
            see_invisible: true,
            nametag: 0,
            death_messages: 0,
            collision: 0,
            members: JavaSet::default(),
        }
    }

    pub fn params(&self) -> TeamParams {
        TeamParams {
            display: self.display.clone(),
            prefix: self.prefix.clone(),
            suffix: self.suffix.clone(),
            nametag: self.nametag as i32,
            collision: self.collision as i32,
            color: self.color.map(|c| c as i32),
            flags: u8::from(self.friendly_fire) | u8::from(self.see_invisible) << 1,
        }
    }

    pub fn packet(&self, method: u8, players: Vec<String>) -> SetPlayerTeam {
        SetPlayerTeam {
            name: self.name.clone(),
            method,
            params: matches!(method, 0 | 2).then(|| self.params()),
            players,
        }
    }

    /// The team as commands show it: its name in brackets, in its colour,
    /// with the id on hover.
    pub fn formatted(&self) -> Tag {
        let mut inner = with_hover(&self.display, &self.name);
        if let Tag::Compound(c) = &mut inner {
            c.insert("insertion", self.name.as_str());
        }
        let mut c = Compound::new();
        c.insert("translate", "chat.square_brackets");
        c.insert("with", Tag::List(vec![inner]));
        if let Some(color) = self.color {
            c.insert("color", COLORS[color]);
        }
        Tag::Compound(c)
    }

    /// A member's name as the team shows it: prefix, name in the team's
    /// colour, suffix.
    pub fn decorate(&self, name: Tag) -> Compound {
        let mut c = Compound::new();
        c.insert("text", "");
        if let Some(color) = self.color {
            c.insert("color", COLORS[color]);
        }
        c.insert(
            "extra",
            mc_protocol::text::list(vec![self.prefix.clone(), name, self.suffix.clone()]),
        );
        c
    }
}

/// A component with a hover showing `text` (a plain string becomes
/// `{text: ...}`).
pub fn with_hover(component: &Tag, text: &str) -> Tag {
    let mut c = match component {
        Tag::Compound(c) => c.clone(),
        Tag::String(s) => {
            let mut c = Compound::new();
            c.insert("text", s.as_str());
            c
        }
        other => {
            let mut c = Compound::new();
            c.insert("text", "");
            c.insert("extra", Tag::List(vec![other.clone()]));
            c
        }
    };
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", text);
    c.insert("hover_event", Tag::Compound(hover));
    Tag::Compound(c)
}

impl Objective {
    /// The objective as commands show it: its display name in brackets,
    /// with the name on hover.
    pub fn formatted(&self) -> Tag {
        let mut c = Compound::new();
        c.insert("translate", "chat.square_brackets");
        c.insert(
            "with",
            Tag::List(vec![with_hover(&self.display, &self.name)]),
        );
        Tag::Compound(c)
    }

    pub fn packet(&self, mode: u8) -> SetObjective {
        SetObjective {
            name: self.name.clone(),
            mode,
            display: self.display.clone(),
            hearts: self.hearts,
            format: self.format.clone(),
        }
    }
}

/// What changed, for the players to see.
#[derive(Clone, Debug)]
pub enum Packet {
    Objective(SetObjective),
    Display(SetDisplayObjective),
    Score(SetScore),
    Reset(ResetScore),
    Team(SetPlayerTeam),
}

#[derive(Debug)]
pub struct Scoreboard {
    pub objectives: HashMap<String, Objective>,
    objective_order: Order,
    /// Score holder → (objective, score), in the order they were set.
    scores: HashMap<String, Vec<(String, Score)>>,
    holder_order: Order,
    pub slots: [Option<String>; 19],
    pub teams: HashMap<String, Team>,
    team_order: Order,
    /// Holder → team.
    team_of: HashMap<String, String>,
    file: Option<PathBuf>,
    data_version: i32,
}

fn format_from_nbt(t: &Tag) -> Option<NumberFormat> {
    let Tag::Compound(c) = t else { return None };
    match c.get("type") {
        Some(Tag::String(s)) if s == "minecraft:blank" => Some(NumberFormat::Blank),
        Some(Tag::String(s)) if s == "minecraft:fixed" => {
            Some(NumberFormat::Fixed(c.get("value").cloned()?))
        }
        Some(Tag::String(s)) if s == "minecraft:styled" => {
            let mut style = c.clone();
            style.remove("type");
            Some(NumberFormat::Styled(Tag::Compound(style)))
        }
        _ => None,
    }
}

fn format_to_nbt(f: &NumberFormat) -> Tag {
    let mut c = Compound::new();
    match f {
        NumberFormat::Blank => {
            c.insert("type", "minecraft:blank");
        }
        NumberFormat::Fixed(v) => {
            c.insert("type", "minecraft:fixed");
            c.insert("value", v.clone());
        }
        NumberFormat::Styled(style) => {
            if let Tag::Compound(s) = style {
                for (k, v) in s.iter() {
                    c.insert(k, v.clone());
                }
            }
            c.insert("type", "minecraft:styled");
        }
    }
    Tag::Compound(c)
}

impl Scoreboard {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let file = world.map(|w| w.join("data/minecraft/scoreboard.dat"));
        let mut s = Scoreboard {
            objectives: HashMap::new(),
            objective_order: Order::new(0.5, false),
            scores: HashMap::new(),
            holder_order: Order::new(0.75, true),
            slots: Default::default(),
            teams: HashMap::new(),
            team_order: Order::new(0.75, false),
            team_of: HashMap::new(),
            file,
            data_version,
        };
        let data = s
            .file
            .as_ref()
            .and_then(|f| crate::world::read_nbt_gz_compound(f).ok())
            .and_then(|root| match root.get("data") {
                Some(Tag::Compound(d)) => Some(d.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let str_of = |c: &Compound, k: &str| match c.get(k) {
            Some(Tag::String(v)) => v.clone(),
            _ => String::new(),
        };
        let flag = |c: &Compound, k: &str, d: bool| match c.get(k) {
            Some(Tag::Byte(b)) => *b != 0,
            _ => d,
        };
        if let Some(Tag::List(list)) = data.get("Objectives") {
            for o in list {
                let Tag::Compound(c) = o else { continue };
                let name = str_of(c, "Name");
                s.add_objective(Objective {
                    criteria: str_of(c, "CriteriaName"),
                    display: c
                        .get("DisplayName")
                        .cloned()
                        .unwrap_or_else(|| Tag::from(name.as_str())),
                    hearts: str_of(c, "RenderType") == "hearts",
                    auto_update: flag(c, "display_auto_update", false),
                    format: c.get("format").and_then(format_from_nbt),
                    name,
                });
            }
        }
        if let Some(Tag::List(list)) = data.get("PlayerScores") {
            for p in list {
                let Tag::Compound(c) = p else { continue };
                let score = Score {
                    value: match c.get("Score") {
                        Some(Tag::Int(v)) => *v,
                        _ => 0,
                    },
                    locked: flag(c, "Locked", false),
                    display: c.get("display").cloned(),
                    format: c.get("format").and_then(format_from_nbt),
                };
                *s.score_mut(&str_of(c, "Name"), &str_of(c, "Objective")) = score;
            }
        }
        if let Some(Tag::Compound(slots)) = data.get("DisplaySlots") {
            for (k, v) in slots.iter() {
                if let (Some(i), Tag::String(o)) = (slot_id(k), v) {
                    s.slots[i] = Some(o.clone());
                }
            }
        }
        if let Some(Tag::List(list)) = data.get("Teams") {
            for t in list {
                let Tag::Compound(c) = t else { continue };
                let name = str_of(c, "Name");
                let mut team = Team::new(&name);
                if let Some(d) = c.get("DisplayName") {
                    team.display = d.clone();
                }
                team.color = COLORS.iter().position(|n| *n == str_of(c, "TeamColor"));
                if let Some(p) = c.get("MemberNamePrefix") {
                    team.prefix = p.clone();
                }
                if let Some(p) = c.get("MemberNameSuffix") {
                    team.suffix = p.clone();
                }
                team.friendly_fire = flag(c, "AllowFriendlyFire", true);
                team.see_invisible = flag(c, "SeeFriendlyInvisibles", true);
                let rule = |k: &str, list: &[&str]| {
                    list.iter().position(|n| *n == str_of(c, k)).unwrap_or(0)
                };
                team.nametag = rule("NameTagVisibility", &VISIBILITY);
                team.death_messages = rule("DeathMessageVisibility", &VISIBILITY);
                team.collision = rule("CollisionRule", &COLLISION);
                let players: Vec<String> = match c.get("Players") {
                    Some(Tag::List(l)) => l
                        .iter()
                        .filter_map(|p| match p {
                            Tag::String(p) => Some(p.clone()),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                s.add_team(team);
                for p in players {
                    s.join(&name, &p);
                }
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(file) = &self.file else { return };
        let mut objectives = Vec::new();
        for name in self.objective_order.keys() {
            let o = &self.objectives[name];
            let mut c = Compound::new();
            c.insert("Name", o.name.as_str());
            c.insert("CriteriaName", o.criteria.as_str());
            c.insert("DisplayName", o.display.clone());
            if o.hearts {
                c.insert("RenderType", "hearts");
            }
            if o.auto_update {
                c.insert("display_auto_update", Tag::Byte(1));
            }
            if let Some(f) = &o.format {
                c.insert("format", format_to_nbt(f));
            }
            objectives.push(Tag::Compound(c));
        }
        let mut scores = Vec::new();
        for holder in self.holder_order.keys() {
            for (objective, s) in &self.scores[holder] {
                let mut c = Compound::new();
                c.insert("Name", holder);
                c.insert("Objective", objective.as_str());
                if s.value != 0 {
                    c.insert("Score", Tag::Int(s.value));
                }
                if s.locked {
                    c.insert("Locked", Tag::Byte(1));
                }
                if let Some(d) = &s.display {
                    c.insert("display", d.clone());
                }
                if let Some(f) = &s.format {
                    c.insert("format", format_to_nbt(f));
                }
                scores.push(Tag::Compound(c));
            }
        }
        let mut slots = Compound::new();
        for (i, o) in self.slots.iter().enumerate() {
            if let Some(o) = o {
                slots.insert(slot_name(i).as_str(), o.as_str());
            }
        }
        let mut teams = Vec::new();
        for name in self.team_order.keys() {
            let t = &self.teams[name];
            let mut c = Compound::new();
            c.insert("Name", t.name.as_str());
            c.insert("DisplayName", t.display.clone());
            if let Some(color) = t.color {
                c.insert("TeamColor", COLORS[color]);
            }
            if t.prefix != Tag::from("") {
                c.insert("MemberNamePrefix", t.prefix.clone());
            }
            if t.suffix != Tag::from("") {
                c.insert("MemberNameSuffix", t.suffix.clone());
            }
            if !t.friendly_fire {
                c.insert("AllowFriendlyFire", Tag::Byte(0));
            }
            if !t.see_invisible {
                c.insert("SeeFriendlyInvisibles", Tag::Byte(0));
            }
            if t.nametag != 0 {
                c.insert("NameTagVisibility", VISIBILITY[t.nametag]);
            }
            if t.death_messages != 0 {
                c.insert("DeathMessageVisibility", VISIBILITY[t.death_messages]);
            }
            if t.collision != 0 {
                c.insert("CollisionRule", COLLISION[t.collision]);
            }
            let members: Vec<Tag> = t.members.keys().into_iter().map(Tag::from).collect();
            if !members.is_empty() {
                c.insert("Players", Tag::List(members));
            }
            teams.push(Tag::Compound(c));
        }
        let mut data = Compound::new();
        data.insert("PlayerScores", Tag::List(scores));
        data.insert("DisplaySlots", Tag::Compound(slots));
        data.insert("Objectives", Tag::List(objectives));
        data.insert("Teams", Tag::List(teams));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving the scoreboard: {e:#}");
        }
    }

    // Objectives.

    pub fn add_objective(&mut self, o: Objective) {
        self.objective_order.insert(&o.name);
        self.objectives.insert(o.name.clone(), o);
    }

    /// Removes an objective with its scores; the packets for that.
    pub fn remove_objective(&mut self, name: &str) -> Vec<Packet> {
        let mut packets = Vec::new();
        let shown = self.displayed(name);
        for i in 0..self.slots.len() {
            if self.slots[i].as_deref() == Some(name) {
                self.slots[i] = None;
                packets.push(Packet::Display(SetDisplayObjective {
                    slot: i as i32,
                    name: String::new(),
                }));
            }
        }
        if let Some(o) = self.objectives.remove(name)
            && shown
        {
            packets.push(Packet::Objective(o.packet(1)));
        }
        self.objective_order.remove(name);
        let holders: Vec<String> = self.scores.keys().cloned().collect();
        for h in holders {
            self.remove_score(&h, name);
        }
        packets
    }

    /// The objective names in the order vanilla lists them.
    pub fn objective_names(&self) -> Vec<String> {
        self.objective_order
            .keys()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Whether the objective shows in some display slot (players then get
    /// its scores).
    pub fn displayed(&self, name: &str) -> bool {
        self.slots.iter().any(|s| s.as_deref() == Some(name))
    }

    /// Shows `objective` (or nothing) in a slot; the packets for that.
    pub fn set_slot(&mut self, slot: usize, objective: Option<String>) -> Vec<Packet> {
        let mut packets = Vec::new();
        let old = self.slots[slot].take();
        if let Some(name) = &objective
            && !self.displayed(name)
        {
            packets.extend(self.objective_packets(name));
        }
        self.slots[slot] = objective.clone();
        packets.push(Packet::Display(SetDisplayObjective {
            slot: slot as i32,
            name: objective.unwrap_or_default(),
        }));
        if let Some(old) = old
            && !self.displayed(&old)
            && let Some(o) = self.objectives.get(&old)
        {
            packets.push(Packet::Objective(o.packet(1)));
        }
        packets
    }

    /// An objective and all its scores, for players to start showing it.
    pub fn objective_packets(&self, name: &str) -> Vec<Packet> {
        let Some(o) = self.objectives.get(name) else {
            return Vec::new();
        };
        let mut packets = vec![Packet::Objective(o.packet(0))];
        for holder in self.holder_order.keys() {
            if let Some(p) = self.score_packet(holder, name) {
                packets.push(p);
            }
        }
        packets
    }

    /// What a player joining needs: teams, shown objectives and their
    /// scores, and the slots.
    pub fn join_packets(&self) -> Vec<Packet> {
        let mut packets: Vec<Packet> = self
            .team_order
            .keys()
            .into_iter()
            .map(|t| {
                let team = &self.teams[t];
                let members = team.members.keys().into_iter().map(str::to_owned).collect();
                Packet::Team(team.packet(0, members))
            })
            .collect();
        let mut sent: Vec<&str> = Vec::new();
        for (i, slot) in self.slots.iter().enumerate() {
            let Some(name) = slot else { continue };
            if !sent.contains(&name.as_str()) {
                sent.push(name);
                packets.extend(self.objective_packets(name));
            }
            packets.push(Packet::Display(SetDisplayObjective {
                slot: i as i32,
                name: name.clone(),
            }));
        }
        packets
    }

    // Scores.

    pub fn score(&self, holder: &str, objective: &str) -> Option<&Score> {
        self.scores
            .get(holder)?
            .iter()
            .find(|(o, _)| o == objective)
            .map(|(_, s)| s)
    }

    /// The score, made (0, locked) if there was none.
    pub fn score_mut(&mut self, holder: &str, objective: &str) -> &mut Score {
        self.holder_order.insert(holder);
        let list = self.scores.entry(holder.to_owned()).or_default();
        let i = match list.iter().position(|(o, _)| o == objective) {
            Some(i) => i,
            None => {
                list.push((objective.to_owned(), Score::default()));
                list.len() - 1
            }
        };
        &mut list[i].1
    }

    /// Removes a score; whether there was one.
    pub fn remove_score(&mut self, holder: &str, objective: &str) -> bool {
        let Some(list) = self.scores.get_mut(holder) else {
            return false;
        };
        let before = list.len();
        list.retain(|(o, _)| o != objective);
        let removed = list.len() != before;
        if list.is_empty() {
            self.scores.remove(holder);
            self.holder_order.remove(holder);
        }
        removed
    }

    /// Removes all of a holder's scores.
    pub fn reset(&mut self, holder: &str) {
        self.scores.remove(holder);
        self.holder_order.remove(holder);
    }

    /// Forgets a removed entity: its scores and its team; the packets.
    pub fn forget(&mut self, holder: &str) -> Vec<Packet> {
        let mut packets = Vec::new();
        if self.scores.contains_key(holder) {
            self.reset(holder);
            packets.push(Packet::Reset(ResetScore {
                holder: holder.to_owned(),
                objective: None,
            }));
        }
        packets.extend(self.leave(holder));
        packets
    }

    /// The holders with scores, in the order vanilla lists them.
    pub fn holders(&self) -> Vec<String> {
        self.holder_order
            .keys()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// A holder's scores (objective, score).
    pub fn scores_of(&self, holder: &str) -> Vec<(String, Score)> {
        self.scores.get(holder).cloned().unwrap_or_default()
    }

    /// The packet showing a score, when its objective is shown.
    pub fn score_packet(&self, holder: &str, objective: &str) -> Option<Packet> {
        if !self.displayed(objective) {
            return None;
        }
        let s = self.score(holder, objective)?;
        Some(Packet::Score(SetScore {
            holder: holder.to_owned(),
            objective: objective.to_owned(),
            value: s.value,
            display: s.display.clone(),
            format: s.format.clone(),
        }))
    }

    // Teams.

    pub fn add_team(&mut self, team: Team) {
        self.team_order.insert(&team.name);
        self.teams.insert(team.name.clone(), team);
    }

    pub fn remove_team(&mut self, name: &str) -> Option<Team> {
        let team = self.teams.remove(name)?;
        self.team_order.remove(name);
        self.team_of.retain(|_, t| t != name);
        Some(team)
    }

    pub fn team_names(&self) -> Vec<String> {
        self.team_order
            .keys()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    pub fn team_of(&self, holder: &str) -> Option<&Team> {
        self.teams.get(self.team_of.get(holder)?)
    }

    /// Puts a holder in a team (leaving any other); the packets for that.
    pub fn join(&mut self, team: &str, holder: &str) -> Vec<Packet> {
        let mut packets = self.leave(holder).into_iter().collect::<Vec<_>>();
        if let Some(t) = self.teams.get_mut(team) {
            t.members.insert(holder);
            self.team_of.insert(holder.to_owned(), team.to_owned());
            packets.push(Packet::Team(t.packet(3, vec![holder.to_owned()])));
        }
        packets
    }

    /// Takes a holder out of their team; the packet, if they had one.
    pub fn leave(&mut self, holder: &str) -> Option<Packet> {
        let team = self.team_of.remove(holder)?;
        let t = self.teams.get_mut(&team)?;
        t.members.remove(holder);
        Some(Packet::Team(t.packet(4, vec![holder.to_owned()])))
    }
}
