//! Player preferences shared by gestures, commands and the optional placeholder expansion.
use crate::{
    bindings::pumpkin::plugin::{player::Player, uuid},
    config, data_dir,
    state::State,
    storage::{self, FEATURES, Preference},
    warn,
};
use std::{collections::HashMap, sync::LazyLock};

#[derive(Default)]
struct Cache {
    players: HashMap<(u64, u64), [bool; 3]>,
    // A read already in progress must not repopulate data invalidated by a write, leave or reload.
    generation: u64,
}
static PREFERENCES: LazyLock<State<Cache>> = LazyLock::new(State::default);

/// Returns the configured default used when the UUID is absent from its legacy table.
fn default_for(cfg: &crate::config::Config, feature: Preference) -> bool {
    match feature {
        Preference::Sit => cfg.sit.default_enabled,
        Preference::PlayerSit => cfg.player_sit.default_enabled,
        Preference::Crawl => cfg.crawl.default_enabled,
    }
}

/// Loads all three effective settings together. Only connected players occupy cache entries.
pub(crate) fn snapshot(id: uuid::Uuid, online: bool) -> Result<[bool; 3], String> {
    let key = (id.high, id.low);
    let (cached, generation) =
        PREFERENCES.with_borrow(|cache| (cache.players.get(&key).copied(), cache.generation));
    if online && let Some(values) = cached {
        return Ok(values);
    }
    let cfg = config();
    let exceptions = storage::exceptions(&cfg.database, &data_dir(), id)?;
    let values = FEATURES.map(|f| default_for(&cfg, f) != exceptions[f as usize]);
    if online {
        PREFERENCES.with_borrow_mut(|cache| {
            if generation == cache.generation {
                cache.players.insert(key, values);
            }
        });
    }
    Ok(values)
}

/// Reads one offline placeholder without retaining the UUID or scanning the other two tables.
pub(crate) fn offline(id: uuid::Uuid, feature: Preference) -> bool {
    let cfg = config();
    let found = storage::exception(&cfg.database, &data_dir(), id, feature);
    legacy_read(found.map(|exception| default_for(&cfg, feature) != exception))
}

/// Invalidates all flags after a write attempt. Triggers can change other flags, and a lost
/// MySQL commit acknowledgement can leave a successful write behind an error result.
pub(crate) fn change(
    id: uuid::Uuid,
    feature: Preference,
    desired: Option<bool>,
) -> Result<bool, String> {
    let cfg = config();
    let value = storage::change(
        &cfg.database,
        &data_dir(),
        id,
        feature,
        default_for(&cfg, feature),
        desired,
    );
    forget(id);
    value
}

/// Invalidates this player's snapshot after a write or disconnect.
pub(crate) fn forget(id: uuid::Uuid) {
    PREFERENCES.with_borrow_mut(|cache| {
        cache.generation = cache.generation.wrapping_add(1);
        cache.players.remove(&(id.high, id.low));
    });
}

/// Checks one gesture and follows GSit's enabled fallback if storage is unavailable.
pub(crate) fn enabled(player: &Player, feature: Preference) -> bool {
    legacy_read(snapshot(player.get_id(), true).map(|values| values[feature as usize]))
}

/// GSit logs failed preference reads and allows the interaction until a later query succeeds.
fn legacy_read(result: Result<bool, String>) -> bool {
    result.unwrap_or_else(|error| {
        warn(&error);
        true
    })
}

/// Invalidates cached values after the defaults or backing database change on reload.
pub(crate) fn clear() {
    PREFERENCES.with_borrow_mut(|cache| {
        cache.generation = cache.generation.wrapping_add(1);
        cache.players.clear();
    });
}
