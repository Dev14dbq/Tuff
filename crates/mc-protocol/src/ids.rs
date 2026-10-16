//! Packet ids per protocol state. They are not hardcoded: the table is filled at
//! startup from the data generator's packet report (`mc-data`), so a new
//! Minecraft version only needs new layouts where packets actually changed.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl State {
    /// Phase names as used in the packet report.
    pub fn from_report_name(name: &str) -> Option<Self> {
        Some(match name {
            "handshake" => State::Handshake,
            "status" => State::Status,
            "login" => State::Login,
            "configuration" => State::Configuration,
            "play" => State::Play,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Clientbound,
    Serverbound,
}

impl Direction {
    pub fn from_report_name(name: &str) -> Option<Self> {
        match name {
            "clientbound" => Some(Direction::Clientbound),
            "serverbound" => Some(Direction::Serverbound),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PacketIds {
    by_name: HashMap<(State, Direction), HashMap<String, i32>>,
    by_id: HashMap<(State, Direction, i32), String>,
}

impl PacketIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, state: State, direction: Direction, name: &str, id: i32) {
        self.by_name
            .entry((state, direction))
            .or_default()
            .insert(name.to_owned(), id);
        self.by_id.insert((state, direction, id), name.to_owned());
    }

    pub fn id(&self, state: State, direction: Direction, name: &str) -> Option<i32> {
        self.by_name.get(&(state, direction))?.get(name).copied()
    }

    pub fn name(&self, state: State, direction: Direction, id: i32) -> Option<&str> {
        self.by_id.get(&(state, direction, id)).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}
