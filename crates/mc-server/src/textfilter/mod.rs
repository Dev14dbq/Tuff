//! The external text filter (`text-filtering-config`): a moderation service
//! that chat, sign and book text goes through before other players see it.
//!
//! Requests never run on the tick thread or under a lock. Each player has
//! their own queue, so their texts are judged in the order they were written
//! (the game's `ConsecutiveExecutor`), and at most `maxConcurrentRequests`
//! requests are out at once across all players. Without a config there is no
//! filter at all and nothing here runs.

pub mod config;

use std::sync::Arc;
use std::time::Duration;

use mc_protocol::filter::FilteredText;
use serde_json::Value;
use tokio::sync::{Semaphore, mpsc};
use tracing::warn;

pub use config::{Flavor, IgnoreStrategy, Settings};

/// Where the bearer token for version 1 comes from.
pub type TokenSource = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

enum Auth {
    Basic(String),
    Bearer(TokenSource),
}

struct Shared {
    settings: Settings,
    auth: Auth,
    agent: ureq::Agent,
    permits: Semaphore,
}

/// The configured service. Cloning shares it.
#[derive(Clone)]
pub struct TextFilter {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for TextFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextFilter")
            .field("chat_url", &self.shared.settings.chat_url)
            .finish_non_exhaustive()
    }
}

impl TextFilter {
    /// The filter `text-filtering-config` and `text-filtering-version` ask
    /// for. `None` (with a warning) when there is none or it cannot be set up;
    /// chat then goes out as written, as in the game.
    pub fn from_properties(version: i64, config: &str) -> Option<Self> {
        if config.trim().is_empty() {
            return None;
        }
        match version {
            0 => match Settings::parse_legacy(config) {
                Ok(settings) => Some(Self::new(settings, None)),
                Err(e) => {
                    warn!("Failed to parse chat filter config {config}: {e}");
                    None
                }
            },
            1 => {
                let settings = match Settings::parse_safety(config) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Failed to parse chat filter config {config}: {e}");
                        return None;
                    }
                };
                if let Flavor::Safety(s) = &settings.flavor
                    && std::fs::File::open(&s.certificate_path).is_err()
                {
                    warn!("Failed to open certificate file");
                    return None;
                }
                // The token comes from Azure by certificate login (MSAL in
                // the game); that is not built here, so there is no way to
                // call the service and chat is left unfiltered.
                warn!(
                    "text-filtering-version 1 needs certificate login to Azure, which Tuff does not do yet; chat is delivered as written"
                );
                None
            }
            _ => {
                warn!("Could not create text filter - unsupported text filtering version used");
                None
            }
        }
    }

    /// A filter over `settings`. Version 1 settings need a `token` source.
    pub fn new(settings: Settings, token: Option<TokenSource>) -> Self {
        let auth = match (&settings.flavor, token) {
            (Flavor::Legacy(l), _) => Auth::Basic(l.auth_key.clone()),
            (Flavor::Safety(_), Some(source)) => Auth::Bearer(source),
            (Flavor::Safety(_), None) => {
                Auth::Bearer(Arc::new(|| Err("no token source".to_owned())))
            }
        };
        let read = Some(Duration::from_millis(settings.read_timeout_ms));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(read)
            .timeout_recv_body(read)
            .max_redirects(0)
            .build()
            .into();
        let permits = Semaphore::new(settings.max_concurrent);
        Self {
            shared: Arc::new(Shared {
                settings,
                auth,
                agent,
                permits,
            }),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.shared.settings
    }

    /// The player's own queue to the service. Must be called inside the
    /// tokio runtime.
    pub fn context(&self, id: u128, name: &str) -> PlayerFilter {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run_queue(self.shared.clone(), id, name.to_owned(), rx));
        PlayerFilter { tx }
    }
}

impl TextFilter {
    /// Sends a text whose verdict nobody waits for (an item name typed on an
    /// anvil). Does nothing outside the tokio runtime.
    pub fn report(&self, id: u128, name: &str, text: &str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (shared, name, text) = (self.shared.clone(), name.to_owned(), text.to_owned());
        runtime.spawn(async move {
            judge(&shared, id, &name, text).await;
        });
    }
}

type Done = Box<dyn FnOnce(Vec<FilteredText>) + Send>;

enum Job {
    Join,
    Leave,
    Texts(Vec<String>, Done),
}

/// One player's line to the filter. Dropping it lets the queue finish what
/// it holds (a leave notice, say) and end.
#[derive(Debug)]
pub struct PlayerFilter {
    tx: mpsc::UnboundedSender<Job>,
}

impl PlayerFilter {
    /// Tells the service the player is here (version 0 only).
    pub fn join(&self) {
        let _ = self.tx.send(Job::Join);
    }

    /// Tells the service the player is gone (version 0 only).
    pub fn leave(&self) {
        let _ = self.tx.send(Job::Leave);
    }

    /// Judges `texts` one after the other, then calls `done` with the
    /// verdicts in the same order (from the filter's own task, so it must be
    /// quick). A text that could not be judged comes back fully filtered.
    pub fn texts(&self, texts: Vec<String>, done: impl FnOnce(Vec<FilteredText>) + Send + 'static) {
        let _ = self.tx.send(Job::Texts(texts, Box::new(done)));
    }
}

async fn run_queue(
    shared: Arc<Shared>,
    id: u128,
    name: String,
    mut jobs: mpsc::UnboundedReceiver<Job>,
) {
    while let Some(job) = jobs.recv().await {
        match job {
            Job::Join | Job::Leave => {
                let join = matches!(job, Job::Join);
                let Flavor::Legacy(l) = &shared.settings.flavor else {
                    continue;
                };
                let url = if join { &l.join_url } else { &l.leave_url }.clone();
                let Some(body) = shared.settings.presence_request(id, &name) else {
                    continue;
                };
                let outcome = request(&shared, url.clone(), body).await;
                if let Err(e) = outcome {
                    warn!("Failed to send join/leave packet to {url} for player {name}: {e}");
                }
            }
            Job::Texts(texts, done) => {
                let mut verdicts = Vec::with_capacity(texts.len());
                for text in texts {
                    verdicts.push(judge(&shared, id, &name, text).await);
                }
                done(verdicts);
            }
        }
    }
}

async fn judge(shared: &Arc<Shared>, id: u128, name: &str, message: String) -> FilteredText {
    if message.is_empty() {
        return FilteredText::pass_through("");
    }
    let body = shared.settings.chat_request(id, name, &message);
    let url = shared.settings.chat_url.clone();
    let verdict = request(shared, url, body)
        .await
        // No content: nothing to read the verdict from.
        .map(|answer| answer.unwrap_or_else(|| Value::Object(Default::default())))
        .and_then(|answer| shared.settings.verdict(&message, &answer));
    verdict.unwrap_or_else(|e| {
        warn!("Failed to validate message '{message}': {e}");
        FilteredText::fully_filtered(&message)
    })
}

/// One POST on a worker thread, within the concurrency limit. `Ok(None)` is
/// an empty (204) answer.
async fn request(shared: &Arc<Shared>, url: String, body: Value) -> Result<Option<Value>, String> {
    let _permit = shared
        .permits
        .acquire()
        .await
        .map_err(|_| "the filter is closed".to_owned())?;
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || post(&shared, &url, &body))
        .await
        .map_err(|e| e.to_string())?
}

fn post(shared: &Shared, url: &str, body: &Value) -> Result<Option<Value>, String> {
    let authorization = match &shared.auth {
        Auth::Basic(key) => format!("Basic {key}"),
        Auth::Bearer(source) => format!("Bearer {}", source()?),
    };
    let payload = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut response = shared
        .agent
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            format!("Minecraft server{}", mc_protocol::VERSION_NAME),
        )
        .header("Authorization", authorization)
        .send(&payload[..])
        .map_err(|e| e.to_string())?;
    if response.status().as_u16() == 204 {
        return Ok(None);
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
