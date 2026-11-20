//! SQLite compatibility, including recovery of legacy GSit WAL databases.
use super::{FEATURES, Preference};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig};
use std::{fs, path::Path};

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
pub(super) fn initialize(path: &Path) -> Result<(), String> {
    let folder = path.parent().ok_or("missing SQLite parent directory")?;
    fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let fresh = match fs::metadata(path) {
        Ok(entry) => entry.len() == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if fresh {
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            let sidecar = std::path::PathBuf::from(name);
            if sidecar
                .try_exists()
                .map_err(|error| format!("{}: {error}", sidecar.display()))?
            {
                return Err(format!(
                    "{}: missing or empty database with an existing {}",
                    path.display(),
                    sidecar.display()
                ));
            }
        }
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
    if fresh {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut db = connect(path, flags)?;
    initialize_tables(&mut db).map_err(|error| format!("{}: {error}", path.display()))
}

/// Opens the file once and obtains a consistent view even when the tables contain duplicates.
pub(super) fn read<const N: usize>(path: &Path, sql: &str, key: &str) -> Result<[bool; N], String> {
    let db = connect(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.query_row(sql, rusqlite::params_from_iter([key; N]), |row| {
        let mut flags = [false; N];
        for (index, flag) in flags.iter_mut().enumerate() {
            *flag = row.get(index)?;
        }
        Ok(flags)
    })
    .map_err(|error| format!("{}: {error}", path.display()))
}

/// Preserves GSit's explicit duplicate inserts. A toggle reads and writes under one writer lock.
pub(super) fn change(
    path: &Path,
    feature: Preference,
    key: &str,
    exception: Option<bool>,
) -> Result<bool, String> {
    let mut db = connect(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mut run = || -> rusqlite::Result<bool> {
        if let Some(exception) = exception {
            db.execute(&feature.write_sql(exception), [key])?;
            return Ok(exception);
        }
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // A matching row means the old value was an exception. Remove every copy.
        let insert = tx.execute(&feature.write_sql(false), [key])? == 0;
        if insert {
            tx.execute(&feature.write_sql(true), [key])?;
        }
        tx.commit()?;
        Ok(insert)
    };
    run().map_err(|error| format!("{} ({}): {error}", path.display(), feature.table()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use tempfile::{TempDir, tempdir};

    /// Reads the three exception tables in the same order as the player cache.
    fn exceptions(path: &Path, key: &str) -> Result<[bool; 3], String> {
        read(path, super::super::SNAPSHOT, key)
    }

    // The fixture was generated by GSit 3.8.0 and contains these two players.
    const FIRST: &str = "11111111-1111-4111-8111-111111111111";
    const SECOND: &str = "22222222-2222-4222-8222-222222222222";

    /// Copies the original database so each test can modify and reopen it independently.
    fn legacy_fixture() -> std::io::Result<TempDir> {
        let dir = tempdir()?;
        fs::write(
            dir.path().join("data.db"),
            include_bytes!("../../tests/fixtures/gsit-3.8.0.db"),
        )?;
        Ok(dir)
    }

    /// A stored GSit exception inverts either server default for only the matching preference.
    #[test]
    fn reads_all_legacy_preferences_with_either_default() -> Result<(), Box<dyn Error>> {
        let dir = legacy_fixture()?;
        let path = dir.path().join("data.db");
        initialize(&path)?;
        for (feature, exception, regular) in [
            (Preference::Sit, FIRST, SECOND),
            (Preference::PlayerSit, SECOND, FIRST),
            (Preference::Crawl, FIRST, SECOND),
        ] {
            for default in [true, false] {
                assert_eq!(
                    default != exceptions(&path, exception)?[feature as usize],
                    !default
                );
                assert_eq!(
                    default != exceptions(&path, regular)?[feature as usize],
                    default
                );
            }
        }
        Ok(())
    }

    /// Explicit exceptions append rows and resetting removes every copy for the specified player.
    #[test]
    fn duplicate_writes_and_reset_preserve_other_rows() -> Result<(), Box<dyn Error>> {
        let dir = legacy_fixture()?;
        let path = dir.path().join("data.db");
        initialize(&path)?;
        // Simulate another GSit instance writing to the same schema between plugin runs.
        Connection::open(&path)?.execute(
            "INSERT INTO gsit_sit_toggle VALUES (?1), ('unknown-old-id'), (NULL)",
            [SECOND],
        )?;
        for _ in 0..2 {
            change(&path, Preference::Sit, FIRST, Some(true))?;
        }
        let count: i64 = Connection::open(&path)?.query_row(
            "SELECT COUNT(*) FROM gsit_sit_toggle WHERE uuid = ?1",
            [FIRST],
            |row| row.get(0),
        )?;
        assert_eq!(count, 3); // One original exception and two explicit writes.
        assert!(exceptions(&path, FIRST)?[Preference::Sit as usize]);

        change(&path, Preference::Sit, FIRST, Some(false))?;
        initialize(&path)?;
        assert!(!exceptions(&path, FIRST)?[Preference::Sit as usize]);
        assert!(exceptions(&path, FIRST)?[Preference::Crawl as usize]);
        let db = Connection::open(&path)?;
        let remaining = db
            .prepare("SELECT uuid FROM gsit_sit_toggle ORDER BY rowid")?
            .query_map([], |row| row.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(
            remaining,
            [Some(SECOND.into()), Some("unknown-old-id".into()), None]
        );
        Ok(())
    }

    /// Bare toggles reverse membership once, including when Paper left several copies of a UUID.
    #[test]
    fn implicit_toggle_removes_duplicates_and_restores_one_exception() -> Result<(), Box<dyn Error>>
    {
        let dir = legacy_fixture()?;
        let db = dir.path().join("data.db");
        initialize(&db)?;
        change(&db, Preference::Sit, FIRST, Some(true))?;
        assert!(!change(&db, Preference::Sit, FIRST, None)?);
        assert_eq!(exceptions(&db, FIRST)?, [false, false, true]);
        assert!(change(&db, Preference::Sit, FIRST, None)?);
        let count: i64 = Connection::open(&db)?.query_row(
            "SELECT COUNT(*) FROM gsit_sit_toggle WHERE uuid = ?",
            [FIRST],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        assert_eq!(exceptions(&db, SECOND)?, [false, true, false]);
        Ok(())
    }

    /// A failure after the first CREATE rolls back previous DDL and preserves unrelated objects.
    #[test]
    fn conflicting_index_rolls_back_partial_table_creation() -> rusqlite::Result<()> {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE archived (note TEXT);
             INSERT INTO archived VALUES ('keep');
             CREATE INDEX gsit_player_toggle ON archived(note);
             PRAGMA user_version = 17;",
        )?;
        assert!(
            initialize_tables(&mut db)
                .unwrap_err()
                .to_string()
                .contains("gsit_player_toggle")
        );
        let tables: i64 = db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name GLOB 'gsit_*'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 0);
        let note: String = db.query_row(
            "SELECT note FROM archived INDEXED BY gsit_player_toggle",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(note, "keep");
        assert_eq!(
            db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
            17
        );
        Ok(())
    }

    /// An unknown column in an existing database must leave the original file intact.
    #[test]
    fn rejects_incompatible_legacy_schema_without_modifying_it() -> Result<(), Box<dyn Error>> {
        let dir = legacy_fixture()?;
        let path = dir.path().join("data.db");
        Connection::open(&path)?.execute_batch(
            "DROP TABLE gsit_player_toggle;
             ALTER TABLE gsit_crawl_toggle ADD COLUMN external_id INTEGER;",
        )?;
        let before = fs::read(&path)?;
        let error = initialize(&path).unwrap_err();
        assert!(error.contains("gsit_crawl_toggle: expected one writable uuid TEXT column"));
        assert_eq!(fs::read(path)?, before);
        Ok(())
    }

    /// Orphaned journal data must survive a failed attempt to open a missing database.
    #[test]
    fn refuses_to_create_a_database_over_orphaned_sidecars() -> std::io::Result<()> {
        for suffix in ["-wal", "-shm", "-journal"] {
            let dir = tempdir()?;
            let path = dir.path().join("data.db");
            let sidecar = dir.path().join(format!("data.db{suffix}"));
            fs::write(&sidecar, b"preserve for recovery")?;
            let error = initialize(&path).unwrap_err();
            assert!(error.contains(&format!("data.db{suffix}")));
            assert!(!path.exists());
            assert_eq!(fs::read(sidecar)?, b"preserve for recovery");
        }
        Ok(())
    }
}
