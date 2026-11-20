//! Dispatches GSit preference operations to the configured database.
mod mysql;
mod sqlite;

use crate::config::Database;
use std::path::Path;
use uuid::Uuid;

/// The click or sneak action whose configured default a player may override.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum Preference {
    Sit,
    PlayerSit,
    Crawl,
}

impl Preference {
    /// Returns the upstream table name. Only these constants enter SQL identifiers.
    fn table(self) -> &'static str {
        match self {
            Self::Sit => "gsit_sit_toggle",
            Self::PlayerSit => "gsit_player_toggle",
            Self::Crawl => "gsit_crawl_toggle",
        }
    }
}

const FEATURES: [Preference; 3] = [Preference::Sit, Preference::PlayerSit, Preference::Crawl];

/// Converts a supplied UUID into the lowercase, hyphenated text used by Java UUID.toString().
fn player_key(uuid: &str) -> Result<String, String> {
    Uuid::parse_str(uuid)
        .map(|id| id.hyphenated().to_string())
        .map_err(|error| format!("invalid player UUID {uuid}: {error}"))
}

/// Opens the selected database, validates existing tables and creates missing tables.
pub fn initialize(settings: &Database, data_dir: &Path) -> Result<(), String> {
    match settings {
        Database::Sqlite { path } => sqlite::initialize(&data_dir.join(path)),
        Database::Mysql(cfg) => mysql::initialize(cfg, data_dir),
    }
}

/// Reads the legacy UUID exception from the selected backend and applies the current default.
pub fn enabled(
    settings: &Database,
    data_dir: &Path,
    feature: Preference,
    uuid: &str,
    default: bool,
) -> Result<bool, String> {
    let key = player_key(uuid)?;
    match settings {
        Database::Sqlite { path } => sqlite::enabled(&data_dir.join(path), feature, &key, default),
        Database::Mysql(cfg) => mysql::enabled(cfg, data_dir, feature, &key, default),
    }
}

/// Applies one explicit setting using GSit's insert-or-delete semantics on the active backend.
pub fn set_enabled(
    settings: &Database,
    data_dir: &Path,
    feature: Preference,
    uuid: &str,
    default: bool,
    enabled: bool,
) -> Result<(), String> {
    let key = player_key(uuid)?;
    match settings {
        Database::Sqlite { path } => {
            sqlite::set_enabled(&data_dir.join(path), feature, &key, default, enabled)
        }
        Database::Mysql(cfg) => mysql::set_enabled(cfg, data_dir, feature, &key, default, enabled),
    }
}
