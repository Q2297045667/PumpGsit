//! Optional PumpkinPIE expansion using its version-zero JSON IPC protocol.

use crate::{
    bindings::pumpkin::plugin::{ipc, uuid},
    gameplay::{
        crawl,
        seat::{self, Activity},
    },
    preferences, server,
    state::State,
    storage::Preference,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const PROVIDER: &str = "PumpkinPIE";
// Templates use the original GSit PlaceholderAPI namespace.
const NAMESPACE: &str = "gsit";
const NAMES: [&str; 6] = [
    "crawling",
    "crawltoggle",
    "playertoggle",
    "posing",
    "sitting",
    "toggle",
];

static NEXT_CHECK: State<Option<Instant>> = State::new(None);
static RETRY_SECONDS: State<u64> = State::new(2);

/// Sends one version-zero operation to PIE and returns the successful response envelope.
async fn request(operation: Value) -> Option<Value> {
    let response = ipc::send_ipc_message(PROVIDER.to_owned(), serde_json::to_vec(&operation).ok()?)
        .await
        .ok()?
        .ok()?;
    if response.len() > 1 << 20 {
        return None;
    }
    let value: Value = serde_json::from_slice(&response).ok()?;
    value.get("ok")?.as_bool()?.then_some(value)
}

/// Checks the live provider's protocol and idempotently claims GSit's six names.
async fn register() -> bool {
    let pong = request(json!({"op":"ping"})).await;
    if pong
        .as_ref()
        .and_then(|v| v.pointer("/ping/protocol"))
        .and_then(Value::as_u64)
        != Some(0)
    {
        return false;
    }
    let registration = request(json!({
        "op":"register_expansion", "namespace":NAMESPACE, "placeholders":NAMES, "cache":{"kind":"never"}
    })).await;
    registration
        .as_ref()
        .and_then(|r| r.pointer("/registered_expansion/namespace"))
        .and_then(Value::as_str)
        == Some(NAMESPACE)
}

/// Retries absent providers with backoff and restores the registry after a live PIE reload.
pub async fn refresh() {
    let now = Instant::now();
    if NEXT_CHECK.get().is_some_and(|next| next > now) {
        return;
    }
    // Reserve this interval before the first await because another timer can enter meanwhile.
    NEXT_CHECK.set(Some(now + Duration::from_secs(30)));
    let success = register().await;
    let interval = if success {
        RETRY_SECONDS.set(2);
        30
    } else {
        let retry = RETRY_SECONDS.get();
        RETRY_SECONDS.set((2 * retry).min(30));
        retry
    };
    NEXT_CHECK.set(Some(Instant::now() + Duration::from_secs(interval)));
}

/// The fields PIE sends to the namespace owner for a single proposed substitution.
#[derive(Deserialize)]
struct Callback {
    op: String,
    namespace: String,
    name: String,
    viewer: Option<String>,
    argument: Option<String>,
}

/// Resolves an explicit UUID or PIE's online viewer name. Unknown identities keep their token.
fn resolve(callback: &Callback) -> Option<bool> {
    if !NAMES.contains(&callback.name.as_str()) {
        return None;
    }
    let server = server()?;
    let (id, player) = if let Some(argument) = &callback.argument {
        let id = uuid::parse(argument)?;
        (id, server.get_player_by_uuid(id))
    } else {
        let player = server.get_player_by_name(callback.viewer.as_deref()?)?;
        (player.get_id(), Some(player))
    };

    let current = player.as_ref().and_then(seat::activity);
    let preference = match callback.name.as_str() {
        "crawling" => return Some(player.as_ref().is_some_and(crawl::active)),
        "sitting" => return Some(matches!(current, Some(Activity::Sit))),
        "posing" => return Some(current.is_some_and(Activity::is_pose)),
        "toggle" => Preference::Sit,
        "playertoggle" => Preference::PlayerSit,
        "crawltoggle" => Preference::Crawl,
        _ => return None,
    };
    Some(match player {
        Some(player) => preferences::enabled(&player, preference),
        None => preferences::offline(id, preference),
    })
}

/// Answers an authenticated PIE callback, declining missing viewers and undeclared names.
pub fn handle(sender: String, msg: Vec<u8>) -> Result<Vec<u8>, String> {
    if sender != PROVIDER || msg.len() > 1 << 20 {
        return Err("unexpected PumpGsit IPC sender or message size".into());
    }
    let callback: Callback = serde_json::from_slice(&msg).map_err(|e| e.to_string())?;
    if callback.op != "on_request" || callback.namespace != NAMESPACE {
        return Err("expected a gsit on_request callback".into());
    }
    let result = match resolve(&callback) {
        Some(value) => json!({"value":value.to_string()}),
        None => json!({}),
    };
    serde_json::to_vec(&json!({"ok":true, "on_request":result})).map_err(|e| e.to_string())
}
