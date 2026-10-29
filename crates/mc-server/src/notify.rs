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

#[derive(Clone, Debug, PartialEq, Eq)]
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
}

/// Compact JSON-RPC notification. No trailing newline, and no `id` field.
pub fn line(note: &Note) -> String {
    let (method, params) = match note {
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
}

impl Bus {
    pub fn new() -> Self {
        Self {
            tx: tokio::sync::broadcast::channel(64).0,
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

fn player_params(player: &Player) -> Value {
    let mut object = Map::new();
    if let Some(id) = &player.id {
        object.insert("id".to_owned(), Value::String(id.clone()));
    }
    if let Some(name) = &player.name {
        object.insert("name".to_owned(), Value::String(name.clone()));
    }
    Value::Array(vec![Value::Object(object)])
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
}
