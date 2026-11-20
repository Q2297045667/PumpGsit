//! Startup, reload, configuration migration and optional placeholder acceptance tests.

use crate::support::*;
use rusqlite::Connection;
use std::{fs, path::Path};
use tempfile::tempdir;

/// Counts physical SQLite rows, preserving the GSit duplicate-row semantics.
fn count(db: &Path, player: &str) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM gsit_sit_toggle WHERE uuid=?1",
            [player],
            |row| row.get(0),
        )
        .unwrap()
}

/// Verifies that a read-only lookup prints all three labels and their effective values.
fn console_prefs(server: &mut Server, chinese: bool, sit: bool) {
    let (labels, on, off) = if chinese {
        (
            ["右键方块坐下", "右键玩家叠坐", "潜行手势爬行"],
            "开启",
            "关闭",
        )
    } else {
        (
            [
                "Right-click blocks to sit",
                "Right-click players to stack",
                "Sneak gestures to crawl",
            ],
            "ON",
            "OFF",
        )
    };
    let output = server.command(&format!("gsitprefs {PLAYER}"), labels[2]);
    assert!(output.contains(PLAYER), "{output}");
    for (label, value) in labels.into_iter().zip([sit, true, true]) {
        let line = output.lines().find(|line| line.contains(label)).unwrap();
        assert!(line.contains(if value { on } else { off }), "{line}");
    }
}

/// Exercises the console, localizations, rollback after invalid reloads and upgrades.
#[test]
#[ignore = "requires PUMPGSIT_SERVER and PUMPGSIT_COMPONENT"]
fn server_smoke() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let data = data_dir(root);
    let db = data.join("data/data.db");
    let mut server = boot(root, "fresh", true, false, None);
    server.command("sit", "can't run this command");
    server.command("pumpgsit help", "Repeat the current command");
    server.command("gsit help", "Repeat the current command");
    server.command(
        "plugin unload PumpGsit",
        "Plugin PumpGsit unloaded successfully",
    );
    fs::copy(input("PUMPGSIT_COMPONENT"), root.join("reload.wasm")).unwrap();
    server.command("plugin load reload.wasm", "loaded successfully");
    server.command("sit help", "Repeat the current command");
    server.command("pumpgsitreload", "plugin was successfully reloaded");
    for line in [
        "gsit toggle unknown-offline-name on",
        "gsitprefs",
        "gsitprefs unknown-offline-name",
    ] {
        server.command(line, "original player's UUID");
    }
    console_prefs(&mut server, false, true);
    for _ in 0..2 {
        server.command(&format!("sit toggle {PLAYER} off"), "have disabled");
    }
    console_prefs(&mut server, false, false);
    let langs = data.join("lang");
    assert_eq!(
        fs::read_dir(&langs)
            .unwrap()
            .filter(|p| p
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "yml"))
            .count(),
        19
    );
    let english = langs.join("en_us.yml");
    let original = fs::read_to_string(&english).unwrap();
    fs::write(
        &english,
        original.replace(
            "The plugin was successfully reloaded.",
            "Applied custom EN.",
        ),
    )
    .unwrap();
    server.command("gsitreload", "Applied custom EN.");
    fs::write(&english, "Messages: [invalid").unwrap();
    server.command("gsitreload", "previous settings are still active");
    fs::write(&english, original).unwrap();
    let cfg = data.join("config.toml");
    let valid = fs::read_to_string(&cfg).unwrap();
    fs::write(&cfg, "invalid [").unwrap();
    server.command("gsitreload", "previous settings are still active");
    for (language, expected) in [
        ("de_de", "Das Plugin wurde erfolgreich neu geladen"),
        ("unknown_locale", "plugin was successfully reloaded"),
        ("zh_cn", "插件已成功重载"),
    ] {
        fs::write(
            &cfg,
            valid.replace(
                "language = \"en_us\"",
                &format!("language = \"{language}\""),
            ),
        )
        .unwrap();
        server.command("gsitrl", expected);
    }
    server.command("glayback", "当前发送人不能执行此命令");
    console_prefs(&mut server, true, false);
    server.stop();
    assert_eq!(fs::read_dir(data.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(count(&db, PLAYER), 2);
    let mut server = boot(root, "restart", true, false, None);
    console_prefs(&mut server, true, false);
    server.command(&format!("sit toggle {PLAYER}"), "你可以通过右键坐下了");
    console_prefs(&mut server, true, true);
    server.stop();
    assert_eq!(count(&db, PLAYER), 0);
    upgrade(&root.join("upgrade"), &fs::read_to_string(cfg).unwrap());
}

/// Migrates an older lowercase installation without a database section and reloads custom paths.
fn upgrade(root: &Path, settings: &str) {
    let data = data_dir(root);
    let old = data.with_file_name("pumpgsit");
    let staging = data.with_file_name("rename-in-progress");
    fs::create_dir_all(old.join("data")).unwrap();
    let legacy = settings.split_once("\n[database]\n").unwrap().0;
    fs::write(old.join("config.toml"), legacy).unwrap();
    fs::copy(LEGACY_DB, old.join("data/data.db")).unwrap();
    fs::rename(old, &staging).unwrap();
    fs::rename(staging, &data).unwrap();
    let db = data.join("data/data.db");
    let cfg = data.join("config.toml");
    let mut server = boot(root, "legacy", true, false, None);
    server.command(&format!("sit toggle {PLAYER}"), "你可以通过右键坐下了");
    server.stop();
    assert_eq!(count(&db, PLAYER), 0);
    let mut server = boot(root, "post-upgrade", true, false, None);
    server.command(&format!("sit toggle {PLAYER}"), "你不能通过右键坐下了");
    let custom =
        format!("{legacy}\n[database]\ntype = \"sqlite\"\npath = \"new/preferences.db\"\n");
    fs::write(&cfg, &custom).unwrap();
    server.command("gsitreload", "插件已成功重载");
    server.command(&format!("sit toggle {PLAYER}"), "你不能通过右键坐下了");
    fs::write(&cfg, custom.replace("new/preferences.db", "../escaped.db")).unwrap();
    server.command("gsitreload", "之前的设置仍然生效");
    server.command(&format!("sit toggle {PLAYER}"), "你可以通过右键坐下了");
    server.stop();
    assert_eq!(count(&db, PLAYER), 1);
    assert_eq!(count(&data.join("new/preferences.db"), PLAYER), 0);
    assert!(!data.parent().unwrap().join("escaped.db").exists());
    assert_eq!(fs::read_dir(data.parent().unwrap()).unwrap().count(), 1);
}

/// Verifies all six placeholders with both plugin load orders, unloads and restarts.
#[test]
#[ignore = "requires PUMPGSIT_SERVER, PUMPGSIT_COMPONENT and PUMPGSIT_PIE"]
fn pie_smoke() {
    let temp = tempdir().unwrap();
    let root = temp.path();
    let data = data_dir(root).join("data");
    fs::create_dir_all(&data).unwrap();
    fs::copy(LEGACY_DB, data.join("data.db")).unwrap();
    fs::copy(input("PUMPGSIT_PIE"), root.join(PIE_FILENAME)).unwrap();
    let names = [
        "sitting",
        "posing",
        "crawling",
        "toggle",
        "playertoggle",
        "crawltoggle",
    ];
    let bare = names.map(|name| format!("%gsit_{name}%")).join(" ");
    let targeted = names
        .map(|name| format!("%gsit_{name}:{PLAYER}%"))
        .join(" ");
    let modified = "false false false true true false";
    let mut server = boot(root, "PumpGsit-first", true, false, None);
    server.command(
        &format!("plugin load {PIE_FILENAME}"),
        "loaded successfully",
    );
    resolve_pie(&mut server, &targeted, "false false false false true false");
    server.command("pie expansions", "%gsit%  from PumpGsit:");
    server.command(
        &format!("pie parse NoViewer={bare}"),
        &format!("NoViewer={bare}"),
    );
    server.command(
        "pie parse Bad=%gsit_toggle:unknown-name%",
        "unresolved: gsit_toggle",
    );
    server.command(&format!("sit toggle {PLAYER} on"), "have enabled");
    resolve_pie(&mut server, &targeted, modified);
    server.command("plugin unload PumpkinPIE", "unloaded successfully");
    server.command("sit help", "Repeat the current command");
    server.command(
        &format!("plugin load {PIE_FILENAME}"),
        "loaded successfully",
    );
    resolve_pie(&mut server, &targeted, modified);
    server.stop();
    fs::rename(
        root.join("plugins/PumpGsit.wasm"),
        root.join("PumpGsit.wasm"),
    )
    .unwrap();
    fs::copy(
        input("PUMPGSIT_PIE"),
        root.join("plugins").join(PIE_FILENAME),
    )
    .unwrap();
    let mut server = boot(root, "pie-first", false, false, Some("Loaded PumpkinPIE"));
    for _ in 0..2 {
        server.command("plugin load PumpGsit.wasm", "loaded successfully");
        resolve_pie(&mut server, &targeted, modified);
        server.command("plugin unload PumpGsit", "unloaded successfully");
        server.command(
            &format!("pie parse Offline={targeted}"),
            &format!("Offline={targeted}"),
        );
    }
    server.command("plugin load PumpGsit.wasm", "loaded successfully");
    resolve_pie(&mut server, &targeted, modified);
    server.stop();
}
