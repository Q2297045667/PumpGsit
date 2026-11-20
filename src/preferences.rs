//! Player preference defaults and the online cache shared by gameplay, commands and placeholders.
use crate::{
    bindings::pumpkin::plugin::{player::Player, uuid},
    config, data_dir,
    state::State,
    storage::{self, Preference},
    warn,
};
use std::{collections::HashMap, sync::LazyLock};

type Preferences = HashMap<(u64, u64, Preference), bool>;
static PREFERENCES: LazyLock<State<Preferences>> = LazyLock::new(State::default);

/// Returns the configured default used when the UUID is absent from the legacy table.
pub(crate) fn default_for(cfg: &crate::config::Config, feature: Preference) -> bool {
    match feature {
        Preference::Sit => cfg.sit.default_enabled,
        Preference::PlayerSit => cfg.player_sit.default_enabled,
        Preference::Crawl => cfg.crawl.default_enabled,
    }
}

/// Resolves an online player's preference, reading the database once per settings generation.
fn online_preference(player: &Player, feature: Preference) -> Result<bool, String> {
    let id = player.get_id();
    let key = (id.high, id.low, feature);
    if let Some(value) = PREFERENCES.with_borrow(|p| p.get(&key).copied()) {
        return Ok(value);
    }
    let cfg = config();
    let value = storage::enabled(
        &cfg.database,
        &data_dir(),
        feature,
        &uuid::to_string(id),
        default_for(&cfg, feature),
    )?;
    if std::sync::Arc::ptr_eq(&cfg, &config()) {
        PREFERENCES.with_borrow_mut(|p| p.insert(key, value));
    }
    Ok(value)
}

/// Drops a player's three cached values after a write or disconnect.
pub(crate) fn forget(id: uuid::Uuid) {
    PREFERENCES.with_borrow_mut(|p| {
        for feature in [Preference::Sit, Preference::PlayerSit, Preference::Crawl] {
            p.remove(&(id.high, id.low, feature));
        }
    });
}

/// Matches GSit's enabled fallback on a read error without caching the failed query.
pub(crate) fn enabled(player: &Player, feature: Preference) -> bool {
    legacy_read(online_preference(player, feature))
}

/// GSit logs failed preference reads and allows the interaction until a later query succeeds.
pub(crate) fn legacy_read(result: Result<bool, String>) -> bool {
    result.unwrap_or_else(|error| {
        warn(&error);
        true
    })
}

/// Invalidates cached values after the defaults or backing database change on reload.
pub(crate) fn clear() {
    PREFERENCES.with_borrow_mut(HashMap::clear);
}
