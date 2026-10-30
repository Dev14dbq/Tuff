//! What the fuzz targets share: the extracted game data (packet names and
//! registries), found the way the server finds it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mc_protocol::{Direction, PacketIds, State};

/// `cache/mcdata/<version>` above this crate, if extracted.
pub fn data_dir() -> Option<PathBuf> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.ancestors().find_map(|dir| {
        let cache = dir.join("cache/mcdata");
        std::fs::read_dir(cache)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.join("version.json").is_file())
    })
}

/// Serverbound packet names of each state, in id order, with the
/// registries installed for item decoding. Empty without extracted data.
pub fn serverbound() -> &'static [(State, Vec<String>)] {
    static NAMES: OnceLock<Vec<(State, Vec<String>)>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let Some(data) = data_dir().and_then(|d| mc_data::McData::load(&d).ok()) else {
            return Vec::new();
        };
        mc_protocol::components::install_registries(data.builtin.iter());
        let mut ids = PacketIds::new();
        for p in &data.packets {
            if let (Some(state), Some(direction)) = (
                State::from_report_name(&p.phase),
                Direction::from_report_name(&p.direction),
            ) {
                ids.insert(state, direction, &p.name, p.id);
            }
        }
        [
            State::Handshake,
            State::Status,
            State::Login,
            State::Configuration,
            State::Play,
        ]
        .into_iter()
        .map(|state| {
            let names = (0..)
                .map_while(|id| ids.name(state, Direction::Serverbound, id))
                .map(str::to_owned)
                .collect();
            (state, names)
        })
        .collect()
    })
}

/// Decodes one serverbound packet body (after its id) as `name` in `state`.
pub fn decode(state: State, name: &str, body: &[u8]) {
    use mc_protocol::packets::{configuration, handshake, login, play, status};
    let r = &mut mc_protocol::Reader::new(body);
    let _ = match state {
        State::Handshake => handshake::Serverbound::decode(name, r).map(drop),
        State::Status => status::Serverbound::decode(name, r).map(drop),
        State::Login => login::Serverbound::decode(name, r).map(drop),
        State::Configuration => configuration::Serverbound::decode(name, r).map(drop),
        State::Play => play::Serverbound::decode(name, r).map(drop),
    };
}
