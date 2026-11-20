//! Reads and writes the three exception tables used by GSit 3.8.0.

use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig};
use std::{fs, path::Path};
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

/// Uses a directory lock and SQLite's private WAL index on WASI, which has no shared memory.
/// Select exclusive mode before the first page read so committed WAL frames participate in recovery.
fn connect(path: &Path, flags: OpenFlags) -> Result<Connection, String> {
    let flags = flags | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags_and_vfs(path, flags, "unix-dotfile")
        .map_err(|error| format!("{}: {error}", path.display()))?;
    db.pragma_update(None, "locking_mode", "EXCLUSIVE")
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(db)
}

/// Inspects SQLite's view of the table. Equivalent quoting, constraints, indices and triggers
/// belong to the database owner. Unknown layouts must not be rewritten during initialization.
fn initialize_tables(db: &mut Connection) -> Result<(), Box<dyn std::error::Error>> {
    // Validate before creating any missing table or checkpointing the incoming WAL.
    let checkpoint_option = DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE;
    db.set_db_config(checkpoint_option, true)?;
    let tx = db.transaction()?;
    let mut missing = Vec::new();
    for feature in FEATURES {
        let table = feature.table();
        let layout: Option<bool> = tx
            .query_row(
                "SELECT type = 'table' AND ncol = 1 AND EXISTS (
                     SELECT 1 FROM pragma_table_xinfo(?1)
                     WHERE name = 'uuid' COLLATE NOCASE AND type = 'TEXT' COLLATE NOCASE AND hidden = 0
                 ) FROM pragma_table_list WHERE schema = 'main' AND name = ?1 COLLATE NOCASE",
                [table],
                |row| row.get(0),
            )
            .optional()?;
        match layout {
            None => missing.push(table),
            Some(false) => {
                return Err(format!("{table}: expected one writable uuid TEXT column").into());
            }
            Some(true) => {}
        }
    }
    for table in missing {
        tx.execute(&format!("CREATE TABLE {table} (uuid TEXT)"), [])?;
    }
    tx.commit()?;
    db.set_db_config(checkpoint_option, false)?;
    Ok(())
}

/// Initializes missing GSit tables atomically and retains database markers and unrelated objects.
/// If the base file was lost, never create an empty database over its remaining journal.
pub fn initialize(data_dir: &Path) -> Result<(), String> {
    let folder = data_dir.join("data");
    fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let path = folder.join("data.db");
    let fresh = match fs::metadata(&path) {
        Ok(entry) => entry.len() == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if fresh {
        for name in ["data.db-wal", "data.db-shm", "data.db-journal"] {
            let sidecar = folder.join(name);
            if sidecar
                .try_exists()
                .map_err(|error| format!("{}: {error}", sidecar.display()))?
            {
                return Err(format!(
                    "{}: missing or empty database with an existing {name}",
                    path.display()
                ));
            }
        }
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
    if fresh {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut db = connect(&path, flags)?;
    initialize_tables(&mut db).map_err(|error| format!("{}: {error}", path.display()))
}

/// Converts a supplied UUID into the lowercase, hyphenated text used by Java UUID.toString().
fn player_key(uuid: &str) -> Result<String, String> {
    Uuid::parse_str(uuid)
        .map(|id| id.hyphenated().to_string())
        .map_err(|error| format!("invalid player UUID {uuid}: {error}"))
}

/// Resolves the upstream presence-based exception against the current configuration default.
pub fn enabled(
    data_dir: &Path,
    feature: Preference,
    uuid: &str,
    default: bool,
) -> Result<bool, String> {
    let db = connect(
        &data_dir.join("data/data.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let query = format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE uuid = ?1)",
        feature.table()
    );
    let exception: bool = db
        .query_row(&query, [player_key(uuid)?], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    Ok(default != exception)
}

/// Makes one preference match the requested state in a single atomic statement.
/// GSit inserts on each explicit non-default setting and deletes all matching rows on reset.
pub fn set_enabled(
    data_dir: &Path,
    feature: Preference,
    uuid: &str,
    default: bool,
    enabled: bool,
) -> Result<(), String> {
    let table = feature.table();
    let query = if enabled == default {
        format!("DELETE FROM {table} WHERE uuid = ?1")
    } else {
        format!("INSERT INTO {table} (uuid) VALUES (?1)")
    };
    let db = connect(
        &data_dir.join("data/data.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    db.execute(&query, [player_key(uuid)?])
        .map_err(|error| format!("{table}: {error}"))?;
    Ok(())
}
