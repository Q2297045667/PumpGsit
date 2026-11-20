//! Optional PumpkinPIE 0.2.6 expansion using its version-zero JSON IPC protocol.

use crate::{
    bindings::pumpkin::plugin::{ipc, uuid},
    command::default_for,
    crawl, data_dir,
    seat::{self, Activity},
    server,
    state::State,
    storage::{self, Preference},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const PROVIDER: &str = "PumpkinPIE";
// Existing templates use the original PlaceholderAPI namespace even after the plugin rename.
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
fn resolve(callback: &Callback) -> Result<Option<bool>, String> {
    if !NAMES.contains(&callback.name.as_str()) {
        return Ok(None);
    }
    let server = server();
    let (id, player) = if let Some(argument) = &callback.argument {
        let Some(id) = uuid::parse(argument) else {
            return Ok(None);
        };
        (id, server.get_player_by_uuid(id))
    } else if let Some(viewer) = &callback.viewer {
        let Some(player) = server.get_player_by_name(viewer) else {
            return Ok(None);
        };
        (player.get_id(), Some(player))
    } else {
        return Ok(None);
    };

    let current = player.as_ref().and_then(seat::activity);
    let preference = match callback.name.as_str() {
        "crawling" => return Ok(Some(player.as_ref().is_some_and(crawl::active))),
        "sitting" => return Ok(Some(matches!(current, Some(Activity::Sit)))),
        "posing" => return Ok(Some(current.is_some_and(Activity::is_pose))),
        "toggle" => Preference::Sit,
        "playertoggle" => Preference::PlayerSit,
        "crawltoggle" => Preference::Crawl,
        _ => return Ok(None),
    };
    if let Some(player) = player {
        return Ok(Some(crate::enabled(&player, preference)));
    }
    let value = storage::enabled(
        &data_dir(),
        preference,
        &uuid::to_string(id),
        default_for(preference),
    );
    Ok(Some(crate::legacy_read(value)))
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
    let value = resolve(&callback)?;
    let result = match value {
        Some(value) => json!({"value":value.to_string()}),
        None => json!({}),
    };
    serde_json::to_vec(&json!({"ok":true, "on_request":result})).map_err(|e| e.to_string())
}
