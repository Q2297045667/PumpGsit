//! Reads and writes the three exception tables used by GSit 3.8.0.

use rusqlite::{Connection, OpenFlags};
use std::{fs, io::Read, path::Path};
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

/// Opens the database with the requested access, permitting creation only during initialization.
fn connect(path: &Path, flags: OpenFlags) -> Result<Connection, String> {
    Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Checks version markers, the exact upstream table definitions and any triggers before writing.
fn validate_schema(db: &Connection) -> Result<(), String> {
    for pragma in ["user_version", "application_id"] {
        let value: i64 = db
            .pragma_query_value(None, pragma, |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if value != 0 {
            return Err(format!("unknown SQLite {pragma} {value} (expected 0)"));
        }
    }
    for feature in FEATURES {
        let table = feature.table();
        let sql: String = db
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(|error| format!("{table}: {error}"))?;
        let normalized: String = sql
            .split_ascii_whitespace()
            .collect::<String>()
            .to_ascii_lowercase();
        if normalized != format!("createtable{table}(uuidtext)") {
            return Err(format!("unknown GSit schema for {table}: {sql}"));
        }
        let trigger_count: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'trigger' AND tbl_name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if trigger_count != 0 {
            return Err(format!("unexpected trigger on {table}"));
        }
    }
    Ok(())
}

/// Creates a new SQLite database, or validates the existing upstream database before use.
/// The three tables are initialized together so a failed first start can be retried.
pub fn initialize(data_dir: &Path) -> Result<(), String> {
    let folder = data_dir.join("data");
    fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let path = folder.join("data.db");
    let fresh = match fs::metadata(&path) {
        Ok(entry) => entry.is_file() && entry.len() == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if !fresh {
        let mut header = [0; 20];
        fs::File::open(&path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if &header[..16] != b"SQLite format 3\0" {
            return Err(format!("{}: unknown database format", path.display()));
        }
        if header[18..20] != [1, 1] {
            return Err(format!(
                "{}: pumpgsit requires a SQLite rollback-journal snapshot; checkpoint WAL and switch the staged copy to DELETE mode",
                path.display()
            ));
        }
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
    if fresh {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut db = connect(&path, flags)?;
    if fresh {
        let tx = db.transaction().map_err(|error| error.to_string())?;
        for feature in FEATURES {
            tx.execute(&format!("CREATE TABLE {} (uuid TEXT)", feature.table()), [])
                .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())?;
    }
    validate_schema(&db).map_err(|reason| format!("{}: {reason}", path.display()))
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
/// Repeating the operation creates no duplicates. Resetting a UUID removes all its exceptions.
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
        format!(
            "INSERT INTO {table} (uuid) SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM {table} WHERE uuid = ?1)"
        )
    };
    let db = connect(
        &data_dir.join("data/data.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    db.execute(&query, [player_key(uuid)?])
        .map_err(|error| format!("{table}: {error}"))?;
    Ok(())
}
