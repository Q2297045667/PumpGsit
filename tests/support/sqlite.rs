//! SQLite acceptance with original GSit databases, committed WAL files and Paper.

use crate::support::*;
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};
use tempfile::tempdir;

const WAL_PLAYER: &str = "3e96862c-1d7e-4b0a-a535-b12bc6d586cb";

/// Copies a quiescent writer's uncheckpointed base, WAL and shared memory files.
/// Keeping its last connection open through the copy leaves the committed frames outstanding.
fn incoming_wal(root: &Path, schema: &str) -> PathBuf {
    let stage = root.join("writer");
    let target = data_dir(root).join("data");
    fs::create_dir_all(&stage).unwrap();
    fs::create_dir_all(&target).unwrap();
    let source = stage.join("data.db");
    fs::copy(LEGACY_DB, &source).unwrap();
    let writer = Connection::open(source).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    writer.execute_batch(schema).unwrap();
    assert!(fs::metadata(stage.join("data.db-wal")).unwrap().len() > 32);
    for file in fs::read_dir(stage).unwrap() {
        let file = file.unwrap();
        fs::copy(file.path(), target.join(file.file_name())).unwrap();
    }
    target.join("data.db")
}

/// Reads the original database's effective preferences under the three enabled defaults.
fn flags(db: &Path, player: &str) -> [bool; 3] {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    ["sit", "player", "crawl"].map(|table| {
        conn.query_row(
            &format!("SELECT NOT EXISTS(SELECT 1 FROM gsit_{table}_toggle WHERE uuid=?1)"),
            [player],
            |row| row.get(0),
        )
        .unwrap()
    })
}

/// Verifies a snapshot's contents and uses SQLite's own consistent backup API.
fn snapshot(source: &Path, dest: &Path) {
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    conn.backup("main", dest, None::<fn(_)>).unwrap();
    assert_eq!(
        Connection::open(dest)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

/// Reads every byte of a database directory for the failed-migration invariant.
fn files(dir: &Path) -> BTreeMap<OsString, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|f| {
            let f = f.unwrap();
            (f.file_name(), fs::read(f.path()).unwrap())
        })
        .collect()
}

/// Verifies each variant against the Wasm SQLite VFS, including byte-for-byte rejection.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn sqlite_compat() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("wal");
    let path = incoming_wal(
        &root,
        &format!(
            r#"BEGIN;
        ALTER TABLE gsit_sit_toggle RENAME TO previous;
        CREATE TABLE "GSit_Sit_Toggle"("UUID" text);
        INSERT INTO Gsit_Sit_Toggle SELECT * FROM previous;
        DROP TABLE previous;
        DROP TABLE gsit_player_toggle;
        CREATE TABLE observed(id TEXT);
        CREATE INDEX by_player ON gsit_sit_toggle(uuid);
        CREATE TRIGGER record_insert AFTER INSERT ON gsit_sit_toggle
            BEGIN INSERT INTO observed VALUES (new.uuid); END;
        PRAGMA application_id=1978;
        PRAGMA user_version=21;
        INSERT INTO gsit_sit_toggle VALUES ('{WAL_PLAYER}');
        COMMIT;"#
        ),
    );
    // Opening a copy of the base alone proves the additional player exists only in WAL frames.
    let base = root.join("base.db");
    fs::copy(&path, &base).unwrap();
    assert_eq!(flags(&base, WAL_PLAYER), [true; 3]);
    let mut server = boot(&root, "incoming-wal", true, false, None);
    server.command(&format!("sit toggle {WAL_PLAYER}"), "have enabled");
    for _ in 0..2 {
        server.command(&format!("sit toggle {WAL_PLAYER} off"), "have disabled");
    }
    server.stop();
    let db = Connection::open(&path).unwrap();
    for (sql, expected) in [
        ("PRAGMA user_version", 21),
        ("PRAGMA application_id", 1978),
        ("SELECT count(*) FROM gsit_player_toggle", 0),
        (
            "SELECT count(*) FROM sqlite_schema WHERE name='by_player'",
            1,
        ),
    ] {
        assert_eq!(
            db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap(),
            expected
        );
    }
    for (sql, expected) in [
        ("SELECT count(*) FROM gsit_sit_toggle WHERE uuid=?1", 2),
        ("SELECT count(*) FROM observed WHERE id=?1", 3),
    ] {
        assert_eq!(
            db.query_row(sql, [WAL_PLAYER], |r| r.get::<_, i64>(0))
                .unwrap(),
            expected
        );
    }
    for (sql, expected) in [
        ("PRAGMA journal_mode", "wal"),
        ("PRAGMA integrity_check", "ok"),
    ] {
        assert_eq!(
            db.query_row(sql, [], |r| r.get::<_, String>(0)).unwrap(),
            expected
        );
    }
    drop(db);
    let mut server = boot(&root, "restart", true, false, None);
    server.command(&format!("sit toggle {WAL_PLAYER}"), "have enabled");
    server.stop();
    assert_eq!(flags(&path, WAL_PLAYER), [true; 3]);

    let reject = temp.path().join("reject");
    let path = incoming_wal(
        &reject,
        "BEGIN; DROP TABLE gsit_sit_toggle; ALTER TABLE gsit_crawl_toggle ADD COLUMN unknown_field BLOB; COMMIT;",
    );
    let dir = path.parent().unwrap();
    let before = files(dir);
    boot(
        &reject,
        "rejected",
        true,
        false,
        Some("gsit_crawl_toggle: expected one writable uuid TEXT column"),
    )
    .stop();
    assert_eq!(files(dir), before);
}

/// Creates preferences on Paper, modifies and restarts on Pumpkin and checks both rollback options.
#[test]
#[ignore = "requires Pumpkin, Paper, JDK, GSit and PumpkinPIE"]
fn paper_roundtrip() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let (java, wasm) = prepare_servers(root);
    let old = java.join("plugins/GSit/data/data.db");
    let new = data_dir(&wasm).join("data/data.db");
    let before = root.join("before.db");
    let after = root.join("after.db");
    let original = [false, true, false];
    let changed = [true, false, true];

    let mut server = paper(&java, "create");
    change(&mut server, PLAYER, original, true);
    server.stop();
    assert_eq!(flags(&old, PLAYER), original);
    snapshot(&old, &before);
    snapshot(&before, &new);

    let mut server = boot(&wasm, "migrate", true, false, None);
    check_pie(&mut server, PLAYER, original);
    change(&mut server, PLAYER, changed, false);
    change(&mut server, NEW_PLAYER, [false; 3], true);
    server.stop();
    assert_eq!(flags(&new, PLAYER), changed);
    assert_eq!(flags(&new, NEW_PLAYER), [false; 3]);
    let mut server = boot(&wasm, "restart", true, false, None);
    check_pie(&mut server, PLAYER, changed);
    check_pie(&mut server, NEW_PLAYER, [false; 3]);
    server.stop();
    snapshot(&new, &after);
    snapshot(&after, &old);

    let mut server = paper(&java, "return");
    change(&mut server, PLAYER, original, false);
    change(&mut server, NEW_PLAYER, [true; 3], false);
    server.stop();
    assert_eq!(flags(&old, PLAYER), original);
    assert_eq!(flags(&old, NEW_PLAYER), [true; 3]);
    snapshot(&before, &old);
    assert_eq!(flags(&old, PLAYER), original);
    assert_eq!(flags(&old, NEW_PLAYER), [true; 3]);
    let mut server = paper(&java, "restore");
    change(&mut server, PLAYER, changed, false);
    server.stop();
    assert_eq!(flags(&old, PLAYER), changed);
}
