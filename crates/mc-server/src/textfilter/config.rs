//! The two filter services' settings, the JSON they are sent and the
//! verdicts they answer with (`text-filtering-config`, `text-filtering-version`).
//! Pure data: no sockets here.

use std::collections::HashSet;

use mc_protocol::filter::{FilterMask, FilteredText};
use serde_json::{Map, Value, json};

/// When a partly filtered text is hidden whole (`hashesToDrop`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreStrategy {
    /// Never: the text is shown with `#` where it was cut.
    Never,
    /// Only when every character was cut.
    FullyFiltered,
    /// When at least this many characters were cut.
    OverThreshold(i64),
}

impl IgnoreStrategy {
    pub fn select(hashes_to_drop: i64) -> Self {
        match hashes_to_drop {
            -1 => Self::Never,
            0 => Self::FullyFiltered,
            n => Self::OverThreshold(n),
        }
    }

    fn should_ignore(self, len: usize, removed: usize) -> bool {
        match self {
            Self::Never => false,
            Self::FullyFiltered => len == removed,
            Self::OverThreshold(n) => removed as i64 >= n,
        }
    }
}

/// The version 0 service: HTTP Basic, with join and leave notices.
#[derive(Clone, Debug)]
pub struct Legacy {
    pub join_url: String,
    pub leave_url: String,
    pub server_id: String,
    pub room_id: String,
    pub rule_id: i64,
    /// The chat endpoint is the original `v1/chat`, with its field names.
    pub legacy_chat: bool,
    /// The API key, Base64 encoded.
    pub auth_key: String,
}

/// The version 1 service (Player Safety Service): a bearer token.
#[derive(Clone, Debug)]
pub struct Safety {
    pub server_id: String,
    pub room_id: String,
    pub fully_filtered_events: HashSet<String>,
    /// From the config: where the certificate for the token request is.
    pub certificate_path: String,
}

#[derive(Clone, Debug)]
pub enum Flavor {
    Legacy(Legacy),
    Safety(Safety),
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub chat_url: String,
    pub ignore: IgnoreStrategy,
    /// Requests in flight at once.
    pub max_concurrent: usize,
    /// How long a request waits for the answer.
    pub read_timeout_ms: u64,
    pub flavor: Flavor,
}

type Parsed = Result<Settings, String>;

impl Settings {
    /// `text-filtering-version` 0.
    pub fn parse_legacy(config: &str) -> Parsed {
        let cfg = object(config)?;
        let host = Host::parse(&req_string(&cfg, "apiServer")?)?;
        let key = req_string(&cfg, "apiKey")?;
        if key.is_empty() {
            return Err("Missing API key".into());
        }
        let endpoints = match cfg.get("endpoints") {
            Some(Value::Object(o)) => Some(o),
            None => None,
            Some(_) => return Err("endpoints is not a JSON object".into()),
        };
        let endpoint = |id: &str, def: &str| -> Result<String, String> {
            match endpoints.and_then(|e| e.get(id)) {
                Some(v) => as_string(v, id),
                None => Ok(def.to_owned()),
            }
        };
        let chat = endpoint("chat", "v1/chat")?;
        let ascii: Vec<u8> = key
            .chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect();
        Ok(Settings {
            chat_url: host.absolute(&chat),
            ignore: IgnoreStrategy::select(opt_int(&cfg, "hashesToDrop", -1)?),
            max_concurrent: pool_size(opt_int(&cfg, "maxConcurrentRequests", 7)?)?,
            read_timeout_ms: 2000,
            flavor: Flavor::Legacy(Legacy {
                join_url: host.absolute(&endpoint("join", "v1/join")?),
                leave_url: host.absolute(&endpoint("leave", "v1/leave")?),
                server_id: opt_string(&cfg, "serverId", "")?,
                room_id: opt_string(&cfg, "roomId", "Java:Chat")?,
                rule_id: opt_int(&cfg, "ruleId", 1)?,
                legacy_chat: chat == "v1/chat",
                auth_key: crate::serverlist::base64(&ascii),
            }),
        })
    }

    /// `text-filtering-version` 1. The Azure application and tenant ids and
    /// the scope are required as in the game, though only the token request
    /// (not done here) uses them.
    pub fn parse_safety(config: &str) -> Parsed {
        let cfg = object(config)?;
        let host = Host::parse(&req_string(&cfg, "apiServer")?)?;
        let api_path = req_string(&cfg, "apiPath")?;
        for key in ["scope", "applicationId", "tenantId"] {
            req_string(&cfg, key)?;
        }
        let certificate_path = req_string(&cfg, "certificatePath")?;
        let events = match cfg.get("fullyFilteredEvents") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|e| as_string(e, "filteredEvent"))
                .collect::<Result<HashSet<_>, _>>()?,
            _ => return Err("Missing fullyFilteredEvents, expected a JSON array".into()),
        };
        Ok(Settings {
            chat_url: host.resolve(&api_path),
            ignore: IgnoreStrategy::select(opt_int(&cfg, "hashesToDrop", -1)?),
            max_concurrent: pool_size(opt_int(&cfg, "maxConcurrentRequests", 7)?)?,
            read_timeout_ms: u64::try_from(opt_int(&cfg, "connectionReadTimeoutMs", 2000)?)
                .map_err(|_| "connectionReadTimeoutMs is negative".to_owned())?,
            flavor: Flavor::Safety(Safety {
                server_id: opt_string(&cfg, "serverId", "")?,
                room_id: opt_string(&cfg, "roomId", "Java:Chat")?,
                fully_filtered_events: events,
                certificate_path,
            }),
        })
    }

    /// The JSON body asking whether `message` from the player may be said.
    pub fn chat_request(&self, id: u128, name: &str, message: &str) -> Value {
        let uuid = crate::util::uuid_string(id);
        match &self.flavor {
            Flavor::Legacy(l) if l.legacy_chat => json!({
                "rule": l.rule_id,
                "server": l.server_id,
                "room": l.room_id,
                "player": uuid,
                "player_display_name": name,
                "text": message,
                "language": "*",
            }),
            Flavor::Legacy(l) => json!({
                "rule_id": l.rule_id.to_string(),
                "category": l.server_id,
                "subcategory": l.room_id,
                "user_id": uuid,
                "user_display_name": name,
                "text": message,
                "language": "*",
            }),
            Flavor::Safety(s) => json!({
                "userId": uuid,
                "userDisplayName": name,
                "server": s.server_id,
                "room": s.room_id,
                "area": "JavaChatRealms",
                "data": message,
                "language": "*",
            }),
        }
    }

    /// The body of a join or leave notice (version 0 only).
    pub fn presence_request(&self, id: u128, name: &str) -> Option<Value> {
        let Flavor::Legacy(l) = &self.flavor else {
            return None;
        };
        Some(json!({
            "server": l.server_id,
            "room": l.room_id,
            "user_id": crate::util::uuid_string(id),
            "user_display_name": name,
        }))
    }

    /// What the service's answer says about `message`. An answer that does
    /// not make sense is an error; the caller hides the whole text.
    pub fn verdict(&self, message: &str, answer: &Value) -> Result<FilteredText, String> {
        match &self.flavor {
            Flavor::Legacy(_) => self.legacy_verdict(message, answer),
            Flavor::Safety(s) => self.safety_verdict(s, message, answer),
        }
    }

    fn legacy_verdict(&self, message: &str, answer: &Value) -> Result<FilteredText, String> {
        let answer = answer.as_object().ok_or("answer is not a JSON object")?;
        if opt_bool(answer, "response", false)? {
            return Ok(FilteredText::pass_through(message));
        }
        match answer.get("hashed") {
            None => return Ok(FilteredText::fully_filtered(message)),
            Some(hashed) => {
                as_string(hashed, "hashed")?;
            }
        }
        let removed = array(answer, "hashes")?;
        Ok(FilteredText {
            raw: message.to_owned(),
            mask: self.mask(message, removed)?,
        })
    }

    fn safety_verdict(
        &self,
        s: &Safety,
        message: &str,
        answer: &Value,
    ) -> Result<FilteredText, String> {
        let answer = answer.as_object().ok_or("answer is not a JSON object")?;
        let result = match answer.get("result") {
            None => return Ok(FilteredText::fully_filtered(message)),
            Some(Value::Object(o)) => o,
            Some(_) => return Err("result is not a JSON object".into()),
        };
        if !opt_bool(result, "filtered", true)? {
            return Ok(FilteredText::pass_through(message));
        }
        if let Some(events) = result.get("events") {
            for event in events.as_array().ok_or("events is not a JSON array")? {
                let event = event.as_object().ok_or("an event is not a JSON object")?;
                if s.fully_filtered_events
                    .contains(&opt_string(event, "id", "")?)
                {
                    return Ok(FilteredText::fully_filtered(message));
                }
            }
        }
        let removed = match result.get("redactedTextIndex") {
            None => &[][..],
            Some(v) => v
                .as_array()
                .ok_or("redactedTextIndex is not a JSON array")?,
        };
        Ok(FilteredText {
            raw: message.to_owned(),
            mask: self.mask(message, removed)?,
        })
    }

    fn mask(&self, message: &str, removed: &[Value]) -> Result<FilterMask, String> {
        if removed.is_empty() {
            return Ok(FilterMask::PassThrough);
        }
        if self
            .ignore
            .should_ignore(message.encode_utf16().count(), removed.len())
        {
            return Ok(FilterMask::FullyFiltered);
        }
        let mut mask = FilterMask::partial();
        for index in removed {
            mask.set_filtered(as_int(index, "hashes")?)?;
        }
        Ok(mask)
    }
}

/// `apiServer`: the scheme and authority every endpoint is resolved against.
struct Host {
    origin: String,
    path: String,
}

impl Host {
    fn parse(uri: &str) -> Result<Self, String> {
        let (scheme, rest) = uri
            .split_once("://")
            .filter(|(s, _)| matches!(*s, "http" | "https"))
            .ok_or_else(|| format!("apiServer {uri:?} is not an http(s) URL"))?;
        let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        if authority.is_empty() {
            return Err(format!("apiServer {uri:?} has no host"));
        }
        Ok(Self {
            origin: format!("{scheme}://{authority}"),
            path: path.to_owned(),
        })
    }

    /// The endpoint as an absolute path on the host (`"/" + endpoint`).
    fn absolute(&self, endpoint: &str) -> String {
        format!("{}/{endpoint}", self.origin)
    }

    /// A reference resolved against the host's own path, as a URI would.
    fn resolve(&self, reference: &str) -> String {
        if let Some(abs) = reference.strip_prefix('/') {
            return self.absolute(abs);
        }
        let dir = self.path.rfind('/').map_or("/", |i| &self.path[..=i]);
        format!("{}{dir}{reference}", self.origin)
    }
}

fn object(config: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str::<Value>(config) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err("the config is not a JSON object".into()),
        Err(e) => Err(format!("the config is not JSON: {e}")),
    }
}

fn as_string(v: &Value, name: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!("expected {name} to be a string")),
    }
}

fn as_int(v: &Value, name: &str) -> Result<i64, String> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or_else(|| format!("{name} is out of range")),
        Value::String(s) => s
            .trim()
            .parse()
            .map_err(|_| format!("expected {name} to be a number")),
        _ => Err(format!("expected {name} to be a number")),
    }
}

fn req_string(o: &Map<String, Value>, name: &str) -> Result<String, String> {
    o.get(name)
        .ok_or_else(|| format!("Missing {name}, expected to find a string"))
        .and_then(|v| as_string(v, name))
}

fn opt_string(o: &Map<String, Value>, name: &str, def: &str) -> Result<String, String> {
    o.get(name)
        .map_or_else(|| Ok(def.to_owned()), |v| as_string(v, name))
}

fn opt_int(o: &Map<String, Value>, name: &str, def: i64) -> Result<i64, String> {
    o.get(name).map_or(Ok(def), |v| as_int(v, name))
}

fn opt_bool(o: &Map<String, Value>, name: &str, def: bool) -> Result<bool, String> {
    match o.get(name) {
        None => Ok(def),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Ok(false),
        Some(_) => Err(format!("expected {name} to be a boolean")),
    }
}

fn array<'a>(o: &'a Map<String, Value>, name: &str) -> Result<&'a [Value], String> {
    match o.get(name) {
        Some(Value::Array(a)) => Ok(a),
        _ => Err(format!("Missing {name}, expected to find a JSON array")),
    }
}

/// A fixed pool needs at least one thread.
fn pool_size(n: i64) -> Result<usize, String> {
    usize::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| "maxConcurrentRequests must be at least 1".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"{"apiServer":"https://filter.example:8443","apiKey":"secret","serverId":"srv","ruleId":3}"#;

    #[test]
    fn legacy_defaults_match_the_game() {
        let s = Settings::parse_legacy(LEGACY).unwrap();
        assert_eq!(s.chat_url, "https://filter.example:8443/v1/chat");
        assert_eq!(s.max_concurrent, 7);
        assert_eq!(s.ignore, IgnoreStrategy::Never);
        assert_eq!(s.read_timeout_ms, 2000);
        let Flavor::Legacy(l) = &s.flavor else {
            panic!("version 0")
        };
        assert_eq!(l.join_url, "https://filter.example:8443/v1/join");
        assert_eq!(l.leave_url, "https://filter.example:8443/v1/leave");
        assert_eq!(l.room_id, "Java:Chat");
        assert_eq!(l.auth_key, "c2VjcmV0");
    }

    #[test]
    fn legacy_needs_a_server_and_a_key() {
        assert!(Settings::parse_legacy(r#"{"apiKey":"k"}"#).is_err());
        assert!(Settings::parse_legacy(r#"{"apiServer":"http://h"}"#).is_err());
        assert!(Settings::parse_legacy(r#"{"apiServer":"http://h","apiKey":""}"#).is_err());
        assert!(Settings::parse_legacy("not json").is_err());
        assert!(
            Settings::parse_legacy(
                r#"{"apiServer":"http://h","apiKey":"k","maxConcurrentRequests":0}"#
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_chat_body_names_the_original_fields() {
        let s = Settings::parse_legacy(LEGACY).unwrap();
        let body = s.chat_request(1, "Steve", "hi");
        assert_eq!(
            body,
            json!({
                "rule": 3,
                "server": "srv",
                "room": "Java:Chat",
                "player": "00000000-0000-0000-0000-000000000001",
                "player_display_name": "Steve",
                "text": "hi",
                "language": "*",
            })
        );
        let join = s.presence_request(1, "Steve").unwrap();
        assert_eq!(join["user_id"], "00000000-0000-0000-0000-000000000001");
        assert_eq!(join["user_display_name"], "Steve");
    }

    #[test]
    fn a_custom_chat_endpoint_switches_the_field_names() {
        let cfg = r#"{"apiServer":"http://h","apiKey":"k","ruleId":5,
            "endpoints":{"chat":"v2/text","join":"in"}}"#;
        let s = Settings::parse_legacy(cfg).unwrap();
        assert_eq!(s.chat_url, "http://h/v2/text");
        let body = s.chat_request(1, "Steve", "hi");
        assert_eq!(body["rule_id"], "5");
        assert_eq!(body["user_id"], "00000000-0000-0000-0000-000000000001");
        assert!(body.get("rule").is_none());
        let Flavor::Legacy(l) = &s.flavor else {
            panic!("version 0")
        };
        assert_eq!(l.join_url, "http://h/in");
    }

    #[test]
    fn legacy_verdicts() {
        let s = Settings::parse_legacy(LEGACY).unwrap();
        let pass = s.verdict("hi", &json!({"response": true})).unwrap();
        assert!(!pass.is_filtered());
        // No `hashed` at all: the service refused the text.
        let none = s.verdict("hi", &json!({})).unwrap();
        assert!(none.mask.is_fully_filtered());
        let part = s
            .verdict(
                "badword",
                &json!({"hashed": "#######", "hashes": [0, 1, 2]}),
            )
            .unwrap();
        assert_eq!(part.filtered().as_deref(), Some("###word"));
        assert_eq!(part.raw, "badword");
        // `hashed` without a list of positions is not an answer.
        assert!(s.verdict("hi", &json!({"hashed": "x"})).is_err());
        // Nothing removed.
        let clean = s
            .verdict("hi", &json!({"hashed": "hi", "hashes": []}))
            .unwrap();
        assert!(!clean.is_filtered());
    }

    #[test]
    fn hashes_to_drop_hides_heavily_filtered_text() {
        let cfg = r#"{"apiServer":"http://h","apiKey":"k","hashesToDrop":2}"#;
        let s = Settings::parse_legacy(cfg).unwrap();
        let one = s
            .verdict("abc", &json!({"hashed": "#bc", "hashes": [0]}))
            .unwrap();
        assert_eq!(one.filtered().as_deref(), Some("#bc"));
        let two = s
            .verdict("abc", &json!({"hashed": "##c", "hashes": [0, 1]}))
            .unwrap();
        assert!(two.mask.is_fully_filtered());
        // 0 hides only when every character went.
        let cfg = r#"{"apiServer":"http://h","apiKey":"k","hashesToDrop":0}"#;
        let s = Settings::parse_legacy(cfg).unwrap();
        let most = s
            .verdict("abc", &json!({"hashed": "##c", "hashes": [0, 1]}))
            .unwrap();
        assert_eq!(most.filtered().as_deref(), Some("##c"));
        let all = s
            .verdict("ab", &json!({"hashed": "##", "hashes": [0, 1]}))
            .unwrap();
        assert!(all.mask.is_fully_filtered());
    }

    const SAFETY: &str = r#"{"apiServer":"https://psvc.example","apiPath":"/api/check",
        "scope":"s","applicationId":"a","tenantId":"t","certificatePath":"cert.pfx",
        "fullyFilteredEvents":["hate"],"serverId":"srv","connectionReadTimeoutMs":500}"#;

    #[test]
    fn safety_config_and_request() {
        let s = Settings::parse_safety(SAFETY).unwrap();
        assert_eq!(s.chat_url, "https://psvc.example/api/check");
        assert_eq!(s.read_timeout_ms, 500);
        assert_eq!(
            s.chat_request(1, "Steve", "hi"),
            json!({
                "userId": "00000000-0000-0000-0000-000000000001",
                "userDisplayName": "Steve",
                "server": "srv",
                "room": "Java:Chat",
                "area": "JavaChatRealms",
                "data": "hi",
                "language": "*",
            })
        );
        assert!(s.presence_request(1, "Steve").is_none());
        assert!(Settings::parse_safety(r#"{"apiServer":"http://h"}"#).is_err());
    }

    #[test]
    fn safety_verdicts() {
        let s = Settings::parse_safety(SAFETY).unwrap();
        // No result: refused.
        assert!(s.verdict("x", &json!({})).unwrap().mask.is_fully_filtered());
        let ok = s
            .verdict("x", &json!({"result": {"filtered": false}}))
            .unwrap();
        assert!(!ok.is_filtered());
        let hate = s
            .verdict(
                "x",
                &json!({"result": {"filtered": true, "events": [{"id": "hate"}]}}),
            )
            .unwrap();
        assert!(hate.mask.is_fully_filtered());
        let part = s
            .verdict(
                "abcd",
                &json!({"result": {"filtered": true, "events": [{"id": "other"}],
                    "redactedTextIndex": [1, 2]}}),
            )
            .unwrap();
        assert_eq!(part.filtered().as_deref(), Some("a##d"));
        // `filtered` defaults to true; with nothing redacted the text passes.
        let none = s.verdict("abcd", &json!({"result": {}})).unwrap();
        assert!(!none.is_filtered());
    }

    #[test]
    fn safety_path_resolves_like_a_uri() {
        let host = Host::parse("https://h:1/base/dir").unwrap();
        assert_eq!(host.resolve("/x/y"), "https://h:1/x/y");
        assert_eq!(host.resolve("x"), "https://h:1/base/x");
        assert!(Host::parse("ftp://h").is_err());
        assert!(Host::parse("http://").is_err());
    }
}
