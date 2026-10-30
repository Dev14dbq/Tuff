//! Outbound management notifications, one JSON-RPC line each.
//!
//! Formats Minecraft Server Management Protocol 3.1.0 notifications. Each
//! line is one compact JSON object and is not written anywhere by this module.

use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Player {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    ServerStarted,
    ServerStopping,
    ServerSaving,
    ServerSaved,
    PlayerJoined(Player),
    PlayerLeft(Player),
    AllowlistAdded(Player),
    AllowlistRemoved(Player),
    GameruleUpdated {
        key: String,
        boolean: Option<bool>,
        integer: Option<i32>,
    },
    /// Somebody logged in; at most one per 30 seconds (see [`Bus`]).
    Activity,
    WorldUpgradeStarted,
    /// Progress of a world upgrade, 0 to 100.
    WorldUpgradeProgress(f32),
    WorldUpgradeFinished,
    WorldUpgradeFailed(String),
    OperatorAdded(Operator),
    OperatorRemoved(Operator),
    IpBanned(Banned),
    IpUnbanned(String),
    PlayerBanned(Player, Banned),
    PlayerUnbanned(Player),
    /// The periodic `server/status` heartbeat, with the server state
    /// object the management API's `server/status` returns.
    Status(Value),
}

/// An operator entry as the management API shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operator {
    pub player: Player,
    pub level: i32,
    pub bypasses_player_limit: bool,
}

/// A ban entry; `subject` is the address for an address ban.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banned {
    pub subject: String,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub expires: Option<String>,
}

impl Banned {
    /// The notification view of an entry of `banned-players.json` or
    /// `banned-ips.json`.
    pub fn from_entry(ban: &crate::lists::Ban) -> Self {
        let some = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        Self {
            subject: ban.name.clone(),
            reason: some(&ban.reason),
            source: some(&ban.source),
            expires: some(&ban.expires).filter(|e| e != "forever"),
        }
    }
}

impl Player {
    pub fn known(uuid: u128, name: &str) -> Self {
        Self {
            id: Some(crate::util::uuid_string(uuid)),
            name: Some(name.to_owned()),
        }
    }
}

fn ban_params(player: Option<&Player>, ban: &Banned) -> Value {
    let mut object = Map::new();
    match player {
        Some(player) => {
            object.insert("player".to_owned(), player_object(player));
        }
        None => {
            object.insert("ip".to_owned(), Value::String(ban.subject.clone()));
        }
    }
    for (key, value) in [
        ("reason", &ban.reason),
        ("source", &ban.source),
        ("expires", &ban.expires),
    ] {
        if let Some(value) = value {
            object.insert(key.to_owned(), Value::String(value.clone()));
        }
    }
    Value::Array(vec![Value::Object(object)])
}

fn operator_params(op: &Operator) -> Value {
    serde_json::json!([{
        "player": player_object(&op.player),
        "permissionLevel": op.level,
        "bypassesPlayerLimit": op.bypasses_player_limit,
    }])
}

/// Compact JSON-RPC notification. No trailing newline, and no `id` field.
pub fn line(note: &Note) -> String {
    let (method, params) = match note {
        Note::Activity => ("minecraft:notification/server/activity", None),
        Note::WorldUpgradeStarted => ("minecraft:notification/world/upgrade_started", None),
        Note::WorldUpgradeProgress(progress) => (
            "minecraft:notification/world/upgrade_progress",
            Some(serde_json::json!([progress])),
        ),
        Note::WorldUpgradeFinished => ("minecraft:notification/world/upgrade_finished", None),
        Note::WorldUpgradeFailed(reason) => (
            "minecraft:notification/world/upgrade_failed",
            Some(serde_json::json!([reason])),
        ),
        Note::OperatorAdded(op) => (
            "minecraft:notification/operators/added",
            Some(operator_params(op)),
        ),
        Note::OperatorRemoved(op) => (
            "minecraft:notification/operators/removed",
            Some(operator_params(op)),
        ),
        Note::IpBanned(ban) => (
            "minecraft:notification/ip_bans/added",
            Some(ban_params(None, ban)),
        ),
        Note::IpUnbanned(ip) => (
            "minecraft:notification/ip_bans/removed",
            Some(serde_json::json!([ip])),
        ),
        Note::PlayerBanned(player, ban) => (
            "minecraft:notification/bans/added",
            Some(ban_params(Some(player), ban)),
        ),
        Note::PlayerUnbanned(player) => (
            "minecraft:notification/bans/removed",
            Some(player_params(player)),
        ),
        Note::Status(state) => (
            "minecraft:notification/server/status",
            Some(Value::Array(vec![state.clone()])),
        ),
        Note::ServerStarted => ("minecraft:notification/server/started", None),
        Note::ServerStopping => ("minecraft:notification/server/stopping", None),
        Note::ServerSaving => ("minecraft:notification/server/saving", None),
        Note::ServerSaved => ("minecraft:notification/server/saved", None),
        Note::PlayerJoined(player) => (
            "minecraft:notification/players/joined",
            Some(player_params(player)),
        ),
        Note::PlayerLeft(player) => (
            "minecraft:notification/players/left",
            Some(player_params(player)),
        ),
        Note::AllowlistAdded(player) => (
            "minecraft:notification/allowlist/added",
            Some(player_params(player)),
        ),
        Note::AllowlistRemoved(player) => (
            "minecraft:notification/allowlist/removed",
            Some(player_params(player)),
        ),
        Note::GameruleUpdated {
            key,
            boolean,
            integer,
        } => (
            "minecraft:notification/gamerules/updated",
            Some(gamerule_params(key, *boolean, *integer)),
        ),
    };
    notification(method, params)
}

fn notification(method: &str, params: Option<Value>) -> String {
    let mut object = Map::new();
    object.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    object.insert("method".to_owned(), Value::String(method.to_owned()));
    if let Some(params) = params {
        object.insert("params".to_owned(), params);
    }
    serde_json::to_string(&Value::Object(object)).expect("notification is valid json")
}

/// Fan-out of notification lines. A send with no subscriber is dropped.
#[derive(Clone, Debug)]
pub struct Bus {
    tx: tokio::sync::broadcast::Sender<String>,
    activity: std::sync::Arc<std::sync::Mutex<ActivityMonitor>>,
}

/// Vanilla's rate limit on `server/activity`: one notification per 30 s.
pub const ACTIVITY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug)]
struct ActivityMonitor {
    interval: std::time::Duration,
    pending: bool,
    last: Option<std::time::Instant>,
}

impl Bus {
    pub fn new() -> Self {
        Self::with_activity_interval(ACTIVITY_INTERVAL)
    }

    pub fn with_activity_interval(interval: std::time::Duration) -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(64).0,
            activity: std::sync::Arc::new(std::sync::Mutex::new(ActivityMonitor {
                interval,
                pending: false,
                last: None,
            })),
        }
    }

    /// A player was verified by the session server. Marks activity and
    /// sends the notification right away unless one went out lately.
    pub fn report_login_activity(&self) {
        self.activity.lock().expect("activity").pending = true;
        self.tick_activity();
    }

    /// Once per server tick: sends the pending activity notification if
    /// the rate limit allows, and forgets the activity either way.
    pub fn tick_activity(&self) {
        let due = {
            let mut monitor = self.activity.lock().expect("activity");
            let due = monitor.pending
                && monitor
                    .last
                    .is_none_or(|last| last.elapsed() >= monitor.interval);
            if due {
                monitor.last = Some(std::time::Instant::now());
            }
            monitor.pending = false;
            due
        };
        if due {
            self.publish(&Note::Activity);
        }
    }

    /// One JSON line, including its newline.
    pub fn publish(&self, note: &Note) {
        let mut text = line(note);
        text.push('\n');
        let _ = self.tx.send(text);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

fn player_object(player: &Player) -> Value {
    let mut object = Map::new();
    if let Some(id) = &player.id {
        object.insert("id".to_owned(), Value::String(id.clone()));
    }
    if let Some(name) = &player.name {
        object.insert("name".to_owned(), Value::String(name.clone()));
    }
    Value::Object(object)
}

fn player_params(player: &Player) -> Value {
    Value::Array(vec![player_object(player)])
}

fn gamerule_params(key: &str, boolean: Option<bool>, integer: Option<i32>) -> Value {
    let key = if key.starts_with("minecraft:") {
        key.to_owned()
    } else {
        format!("minecraft:{key}")
    };
    let (type_name, value) = match boolean {
        Some(value) => ("boolean", Value::Bool(value)),
        None => ("integer", Value::from(integer.unwrap_or(0))),
    };
    serde_json::json!([{
        "key": key,
        "type": type_name,
        "value": value,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn parse(note: &Note) -> Value {
        let text = line(note);
        assert!(
            !text.contains('\n'),
            "notification line must not contain a newline"
        );
        serde_json::from_str(&text).expect("notification line is json")
    }

    #[test]
    fn server_started_has_no_params_or_id() {
        let value = parse(&Note::ServerStarted);
        let object = value.as_object().expect("notification is an object");
        assert_eq!(object.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        assert_eq!(
            object.get("method").and_then(Value::as_str),
            Some("minecraft:notification/server/started")
        );
        assert!(!object.contains_key("params"));
        assert!(!object.contains_key("id"));
    }

    #[test]
    fn player_joined_params_shape() {
        let value = parse(&Note::PlayerJoined(Player {
            id: Some("853c80ef-3c37-49fd-aa49-938b674adae6".to_owned()),
            name: Some("jeb_".to_owned()),
        }));
        assert_eq!(value["method"], "minecraft:notification/players/joined");
        assert_eq!(
            value["params"],
            json!([{
                "id": "853c80ef-3c37-49fd-aa49-938b674adae6",
                "name": "jeb_"
            }])
        );
    }

    #[test]
    fn gamerule_boolean_prefixes_bare_key() {
        let value = parse(&Note::GameruleUpdated {
            key: "locator_bar".to_owned(),
            boolean: Some(false),
            integer: None,
        });
        let rule = &value["params"][0];
        assert_eq!(rule["key"], "minecraft:locator_bar");
        assert_eq!(rule["type"], "boolean");
        assert_eq!(rule["value"], false);
    }

    #[test]
    fn player_left_with_no_fields_is_empty_object() {
        let value = parse(&Note::PlayerLeft(Player {
            id: None,
            name: None,
        }));
        assert_eq!(value["method"], "minecraft:notification/players/left");
        assert_eq!(value["params"], json!([{}]));
    }

    #[test]
    fn other_server_and_allowlist_notes() {
        let bare = Player {
            id: None,
            name: None,
        };
        let cases = [
            (
                Note::ServerStopping,
                "minecraft:notification/server/stopping",
                false,
            ),
            (
                Note::ServerSaving,
                "minecraft:notification/server/saving",
                false,
            ),
            (
                Note::ServerSaved,
                "minecraft:notification/server/saved",
                false,
            ),
            (
                Note::AllowlistAdded(bare.clone()),
                "minecraft:notification/allowlist/added",
                true,
            ),
            (
                Note::AllowlistRemoved(bare),
                "minecraft:notification/allowlist/removed",
                true,
            ),
        ];
        for (note, method, has_params) in cases {
            let value = parse(&note);
            let object = value.as_object().expect("notification is an object");
            assert_eq!(object.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
            assert_eq!(object.get("method").and_then(Value::as_str), Some(method));
            assert!(!object.contains_key("id"));
            assert_eq!(object.contains_key("params"), has_params);
        }
    }

    #[test]
    fn gamerule_integer_and_boolean_precedence() {
        let prefixed = parse(&Note::GameruleUpdated {
            key: "minecraft:random_tick_speed".to_owned(),
            boolean: None,
            integer: Some(3),
        });
        assert_eq!(prefixed["params"][0]["key"], "minecraft:random_tick_speed");
        assert_eq!(prefixed["params"][0]["type"], "integer");
        assert_eq!(prefixed["params"][0]["value"], json!(3));

        let boolean_wins = parse(&Note::GameruleUpdated {
            key: "locator_bar".to_owned(),
            boolean: Some(true),
            integer: Some(1),
        });
        assert_eq!(boolean_wins["params"][0]["type"], "boolean");
        assert_eq!(boolean_wins["params"][0]["value"], true);

        let fallback = parse(&Note::GameruleUpdated {
            key: "spawn_radius".to_owned(),
            boolean: None,
            integer: None,
        });
        assert_eq!(fallback["params"][0]["key"], "minecraft:spawn_radius");
        assert_eq!(fallback["params"][0]["type"], "integer");
        assert_eq!(fallback["params"][0]["value"], json!(0));
    }

    fn steve() -> Player {
        Player {
            id: Some("853c80ef-3c37-49fd-aa49-938b674adae6".into()),
            name: Some("Steve".into()),
        }
    }

    #[test]
    fn operator_and_ban_notes_use_the_api_shapes() {
        let op = parse(&Note::OperatorAdded(Operator {
            player: steve(),
            level: 4,
            bypasses_player_limit: false,
        }));
        assert_eq!(op["method"], "minecraft:notification/operators/added");
        assert_eq!(op["params"][0]["player"]["name"], "Steve");
        assert_eq!(op["params"][0]["permissionLevel"], 4);
        assert_eq!(op["params"][0]["bypassesPlayerLimit"], false);

        let ban = Banned {
            subject: "10.0.0.1".into(),
            reason: Some("Spam".into()),
            source: Some("Console".into()),
            expires: None,
        };
        let ip = parse(&Note::IpBanned(ban.clone()));
        assert_eq!(ip["method"], "minecraft:notification/ip_bans/added");
        assert_eq!(ip["params"][0]["ip"], "10.0.0.1");
        assert!(ip["params"][0].get("expires").is_none());
        let player = parse(&Note::PlayerBanned(steve(), ban));
        assert_eq!(player["method"], "minecraft:notification/bans/added");
        assert_eq!(
            player["params"][0]["player"]["id"],
            "853c80ef-3c37-49fd-aa49-938b674adae6"
        );
        assert_eq!(player["params"][0]["reason"], "Spam");

        assert_eq!(
            parse(&Note::IpUnbanned("10.0.0.1".into()))["params"],
            json!(["10.0.0.1"])
        );
        assert_eq!(
            parse(&Note::PlayerUnbanned(steve()))["method"],
            "minecraft:notification/bans/removed"
        );
        assert_eq!(
            parse(&Note::WorldUpgradeProgress(50.0))["method"],
            "minecraft:notification/world/upgrade_progress"
        );
        assert_eq!(
            parse(&Note::Activity)["method"],
            "minecraft:notification/server/activity"
        );
    }

    #[test]
    fn activity_is_rate_limited_and_forgotten_when_not_sent() {
        let bus = Bus::with_activity_interval(std::time::Duration::from_millis(80));
        let mut rx = bus.subscribe();
        bus.tick_activity();
        assert!(rx.try_recv().is_err(), "no activity, no notification");
        bus.report_login_activity();
        assert!(rx.try_recv().unwrap().contains("server/activity"));
        // Inside the interval the activity is dropped, not queued.
        bus.report_login_activity();
        assert!(rx.try_recv().is_err());
        std::thread::sleep(std::time::Duration::from_millis(100));
        bus.tick_activity();
        assert!(rx.try_recv().is_err());
        bus.report_login_activity();
        assert!(rx.try_recv().unwrap().contains("server/activity"));
    }
}
