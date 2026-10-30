//! The client against a mock HTTP service on a local port.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// What the mock saw: request line, headers (lower-cased names) and body.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

/// Answers every POST with `reply(request)` as `(status, body)`.
struct Mock {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn mock(reply: impl Fn(&Seen) -> (u16, String) + Send + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let (head_end, length) = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break (0, 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (i + 4, length);
                }
            };
            if head_end == 0 {
                continue;
            }
            while buf.len() < head_end + length {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let mut lines = head.lines();
            let line = lines.next().unwrap_or_default().to_owned();
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(n, v)| (n.trim().to_lowercase(), v.trim().to_owned()))
                .collect();
            let body = String::from_utf8_lossy(&buf[head_end..]).into_owned();
            let request = Seen {
                line,
                headers,
                body,
            };
            let (status, answer) = reply(&request);
            log.lock().unwrap().push(request);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Mock { port, seen }
}

fn legacy(port: u16, extra: &str) -> TextFilter {
    let config = format!(
        r#"{{"apiServer":"http://127.0.0.1:{port}","apiKey":"key","serverId":"s"{extra}}}"#
    );
    TextFilter::from_properties(0, &config).expect("a filter")
}

async fn verdicts(player: &PlayerFilter, texts: &[&str]) -> Vec<FilteredText> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    player.texts(texts.iter().map(|t| (*t).to_owned()).collect(), move |v| {
        let _ = tx.send(v);
    });
    tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .expect("an answer in time")
        .unwrap()
}

#[test]
fn unconfigured_is_no_filter() {
    assert!(TextFilter::from_properties(0, "").is_none());
    assert!(TextFilter::from_properties(0, "   ").is_none());
    assert!(TextFilter::from_properties(7, r#"{"apiServer":"http://h","apiKey":"k"}"#).is_none());
    assert!(TextFilter::from_properties(0, "{broken").is_none());
}

#[tokio::test]
async fn a_message_goes_to_the_service_and_the_mask_comes_back() {
    let service = mock(|req| {
        let text = req.json()["text"].as_str().unwrap().to_owned();
        if text.contains("bad") {
            (200, r####"{"hashed":"###","hashes":[0,1,2]}"####.to_owned())
        } else {
            (200, r#"{"response":true}"#.to_owned())
        }
    });
    let player = legacy(service.port, "").context(0xabc, "Steve");
    let out = verdicts(&player, &["hello", "bad word", ""]).await;
    assert_eq!(out[0], FilteredText::pass_through("hello"));
    assert_eq!(out[1].filtered().as_deref(), Some("### word"));
    assert_eq!(out[1].raw, "bad word");
    // An empty text never reaches the service.
    assert_eq!(out[2], FilteredText::pass_through(""));
    let seen = service.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].line, "POST /v1/chat HTTP/1.1");
    assert_eq!(seen[0].header("authorization"), Some("Basic a2V5"));
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(seen[0].header("accept"), Some("application/json"));
    assert_eq!(seen[0].header("user-agent"), Some("Minecraft server26.3"));
    assert_eq!(seen[0].json()["text"], "hello");
    assert_eq!(seen[0].json()["player_display_name"], "Steve");
    // Judged in the order they were written.
    assert_eq!(seen[1].json()["text"], "bad word");
}

#[tokio::test]
async fn a_failing_service_hides_the_text() {
    let service = mock(|_| (500, "{}".to_owned()));
    let player = legacy(service.port, "").context(1, "Steve");
    let out = verdicts(&player, &["hello"]).await;
    assert!(out[0].mask.is_fully_filtered());
    assert_eq!(out[0].raw, "hello");

    // So does an answer that is not JSON, and an empty (204) one.
    let garbled = mock(|_| (200, "<html>".to_owned()));
    let player = legacy(garbled.port, "").context(1, "Steve");
    assert!(
        verdicts(&player, &["hello"]).await[0]
            .mask
            .is_fully_filtered()
    );
    let empty = mock(|_| (204, String::new()));
    let player = legacy(empty.port, "").context(1, "Steve");
    assert!(
        verdicts(&player, &["hello"]).await[0]
            .mask
            .is_fully_filtered()
    );

    // And nobody listening at all.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let player = legacy(port, "").context(1, "Steve");
    assert!(
        verdicts(&player, &["hello"]).await[0]
            .mask
            .is_fully_filtered()
    );
}

#[tokio::test]
async fn join_and_leave_are_announced_in_order() {
    let service = mock(|_| (200, "{}".to_owned()));
    let player = legacy(service.port, "").context(0x1234, "Alex");
    player.join();
    let _ = verdicts(&player, &["x"]).await;
    player.leave();
    drop(player);
    for _ in 0..100 {
        if service.seen.lock().unwrap().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen = service.seen.lock().unwrap().clone();
    let lines: Vec<&str> = seen.iter().map(|s| s.line.as_str()).collect();
    assert_eq!(
        lines,
        [
            "POST /v1/join HTTP/1.1",
            "POST /v1/chat HTTP/1.1",
            "POST /v1/leave HTTP/1.1"
        ]
    );
    assert_eq!(
        seen[0].json(),
        serde_json::json!({
            "server": "s",
            "room": "Java:Chat",
            "user_id": "00000000-0000-0000-0000-000000001234",
            "user_display_name": "Alex",
        })
    );
}

#[tokio::test]
async fn requests_are_limited_to_max_concurrent() {
    let now = Arc::new(Mutex::new((0usize, 0usize)));
    let probe = now.clone();
    let service = mock(move |_| {
        {
            let mut p = probe.lock().unwrap();
            p.0 += 1;
            p.1 = p.1.max(p.0);
        }
        std::thread::sleep(Duration::from_millis(50));
        probe.lock().unwrap().0 -= 1;
        (200, r#"{"response":true}"#.to_owned())
    });
    // The mock serves one connection at a time, so what is measured is that
    // the client never has more than one request in flight either way; the
    // permit count itself is checked on the semaphore.
    let filter = legacy(service.port, r#","maxConcurrentRequests":2"#);
    assert_eq!(filter.shared.permits.available_permits(), 2);
    let a = filter.context(1, "A");
    let b = filter.context(2, "B");
    let (x, y) = tokio::join!(verdicts(&a, &["1", "2"]), verdicts(&b, &["3", "4"]));
    assert!(x.iter().chain(&y).all(|f| !f.is_filtered()));
    assert_eq!(filter.shared.permits.available_permits(), 2);
    assert!(now.lock().unwrap().1 <= 2);
}

#[tokio::test]
async fn version_one_sends_a_bearer_token() {
    let service = mock(|_| {
        (
            200,
            r#"{"result":{"filtered":true,"redactedTextIndex":[0]}}"#.to_owned(),
        )
    });
    let config = format!(
        r#"{{"apiServer":"http://127.0.0.1:{}","apiPath":"/check","scope":"s","applicationId":"a",
        "tenantId":"t","certificatePath":"c","fullyFilteredEvents":[]}}"#,
        service.port
    );
    let settings = Settings::parse_safety(&config).unwrap();
    let filter = TextFilter::new(settings, Some(Arc::new(|| Ok("tok".to_owned()))));
    let player = filter.context(1, "Steve");
    let out = verdicts(&player, &["abc"]).await;
    assert_eq!(out[0].filtered().as_deref(), Some("#bc"));
    let seen = service.seen.lock().unwrap().clone();
    assert_eq!(seen[0].line, "POST /check HTTP/1.1");
    assert_eq!(seen[0].header("authorization"), Some("Bearer tok"));
    assert_eq!(seen[0].json()["data"], "abc");
    // Join and leave mean nothing to this service.
    player.join();
    player.leave();
    drop(player);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(service.seen.lock().unwrap().len(), 1);
}

#[test]
fn version_one_without_azure_login_leaves_chat_alone() {
    let dir = std::env::temp_dir().join(format!("tuff-filter-cert-{}", std::process::id()));
    std::fs::write(&dir, b"x").unwrap();
    let config = format!(
        r#"{{"apiServer":"http://h","apiPath":"/c","scope":"s","applicationId":"a",
        "tenantId":"t","certificatePath":{:?},"fullyFilteredEvents":[]}}"#,
        dir.to_string_lossy()
    );
    assert!(TextFilter::from_properties(1, &config).is_none());
    let _ = std::fs::remove_file(&dir);
}
