//! Reads and updates GSit's three original UUID exception tables.
mod mysql;
mod sqlite;

use crate::{bindings::pumpkin::plugin::uuid, config::Database};
use std::path::Path;

/// Order of the three flags returned by the snapshot query.
#[derive(Clone, Copy, Eq, PartialEq)]
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

    /// Builds GSit's single-row insert or delete-all-duplicates operation.
    fn write_sql(self, exception: bool) -> String {
        let table = self.table();
        if exception {
            format!("INSERT INTO {table} (uuid) VALUES (?)")
        } else {
            format!("DELETE FROM {table} WHERE uuid = ?")
        }
    }
}

pub(crate) const FEATURES: [Preference; 3] =
    [Preference::Sit, Preference::PlayerSit, Preference::Crawl];

// Separate parameters let each comparison use the legacy column's own collation.
const SNAPSHOT: &str = "SELECT
    EXISTS(SELECT 1 FROM gsit_sit_toggle WHERE uuid = ?),
    EXISTS(SELECT 1 FROM gsit_player_toggle WHERE uuid = ?),
    EXISTS(SELECT 1 FROM gsit_crawl_toggle WHERE uuid = ?)";

/// Encodes the host's typed UUID exactly as Java UUID.toString() does.
fn player_key(id: uuid::Uuid) -> String {
    ::uuid::Uuid::from_u64_pair(id.high, id.low).to_string()
}

/// Opens the selected database, validates existing tables and creates missing tables.
pub fn initialize(settings: &Database, data_dir: &Path) -> Result<(), String> {
    match settings {
        Database::Sqlite { path } => sqlite::initialize(&data_dir.join(path)),
        Database::Mysql(cfg) => mysql::initialize(cfg, data_dir),
    }
}

/// Reads all three exception flags from one consistent statement on one connection.
pub fn exceptions(settings: &Database, dir: &Path, id: uuid::Uuid) -> Result<[bool; 3], String> {
    read(settings, dir, id, SNAPSHOT)
}

/// Looks up just the requested table for an uncached offline placeholder.
pub fn exception(
    settings: &Database,
    dir: &Path,
    id: uuid::Uuid,
    feature: Preference,
) -> Result<bool, String> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM {} WHERE uuid = ?)",
        feature.table()
    );
    let [found] = read(settings, dir, id, &sql)?;
    Ok(found)
}

/// Selects the configured backend for a single-row query of N exception flags.
fn read<const N: usize>(
    settings: &Database,
    dir: &Path,
    id: uuid::Uuid,
    sql: &str,
) -> Result<[bool; N], String> {
    let key = player_key(id);
    match settings {
        Database::Sqlite { path } => sqlite::read(&dir.join(path), sql, &key),
        Database::Mysql(cfg) => mysql::read(cfg, dir, sql, &key),
    }
}

/// Applies an explicit setting or atomically flips membership and returns the committed value.
pub fn change(
    settings: &Database,
    dir: &Path,
    id: uuid::Uuid,
    feature: Preference,
    default: bool,
    desired: Option<bool>,
) -> Result<bool, String> {
    let key = player_key(id);
    let exception = desired.map(|value| value != default);
    let stored = match settings {
        Database::Sqlite { path } => sqlite::change(&dir.join(path), feature, &key, exception),
        Database::Mysql(cfg) => mysql::change(cfg, dir, feature, &key, exception),
    }?;
    Ok(stored != default)
}
