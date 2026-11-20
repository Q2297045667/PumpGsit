"""Exercise PumpGsit through Pumpkin's console in disposable, loopback-only servers."""

import argparse
from pathlib import Path
import shutil
import sqlite3
import tempfile

from support import LEGACY_DB, PLAYER, boot

TABLE = "gsit_sit_toggle"


def row_count(db, player=PLAYER):
    """Read a test player's click-to-sit exception from a stopped server database."""
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as connection:
        return connection.execute(f"SELECT count(*) FROM {TABLE} WHERE uuid=?", (player,)).fetchone()[0]


def exercise(args, root):
    """Check a new install, repeated explicit preferences, reload rollback, and a restarted old database."""
    root.mkdir(parents=True, exist_ok=True)
    data = root / "plugins/data/PumpGsit"
    db = data / "data/data.db"
    with boot(args.server, args.component, root, "fresh") as cmd:
        cmd("sit", "can't run this command")
        cmd("pumpgsit help", "Repeat the current command")
        cmd("gsit help", "Repeat the current command")
        cmd("plugin unload PumpGsit", "Plugin PumpGsit unloaded successfully")
        shutil.copyfile(args.component, root / "reload.wasm")
        cmd("plugin load reload.wasm", "Plugin reload.wasm loaded successfully")
        cmd("sit help", "Repeat the current command")
        cmd("pumpgsitreload", "plugin was successfully reloaded")
        cmd("gsit toggle unknown-offline-name on", "original player's UUID")
        for _ in range(2):
            cmd(f"sit toggle {PLAYER} off", "have disabled")
        langs = data / "lang"
        assert len(list(langs.glob("*.yml"))) == 19
        english = langs / "en_us.yml"
        original = english.read_text()
        english.write_text(original.replace("The plugin was successfully reloaded.", "Applied custom EN."))
        cmd("gsitreload", "Applied custom EN.")
        english.write_text("Messages: [invalid")
        cmd("gsitreload", "previous settings are still active")
        english.write_text(original)
        cfg = data / "config.toml"
        valid = cfg.read_text()
        cfg.write_text("invalid [")
        cmd("gsitreload", "previous settings are still active")
        cfg.write_text(valid.replace('language = "en_us"', 'language = "de_de"'))
        cmd("gsitrl", "Das Plugin wurde erfolgreich neu geladen")
        cfg.write_text(valid.replace('language = "en_us"', 'language = "unknown_locale"'))
        cmd("gsitrl", "plugin was successfully reloaded")
        cfg.write_text(valid.replace('language = "en_us"', 'language = "zh_cn"'))
        cmd("gsitrl", "插件已成功重载")
        cmd("glayback", "当前发送人不能执行此命令")
    assert {entry.name for entry in data.parent.iterdir()} == {"PumpGsit"}
    assert row_count(db) == 2
    with boot(args.server, args.component, root, "restart") as cmd:
        cmd(f"sit toggle {PLAYER}", "你可以通过右键坐下了")
    assert row_count(db) == 0
    check_upgrade(args, root / "upgrade", cfg)
    print(f"Passed fresh install, aliases, console, SQLite, reload, i18n, restart and upgrade in {root}")


def check_upgrade(args, root, config):
    """Move a stopped server's lowercase data directory and verify its configuration and SQLite preferences."""
    old_data = root / "plugins/data/pumpgsit"
    (old_data / "data").mkdir(parents=True)
    shutil.copyfile(config, old_data / "config.toml")
    shutil.copyfile(LEGACY_DB, old_data / "data/data.db")
    # A distinct intermediate name also works on case-insensitive filesystems.
    staging = old_data.rename(old_data.with_name("rename-in-progress"))
    data = staging.rename(old_data.with_name("PumpGsit"))
    db = data / "data/data.db"
    with sqlite3.connect(db) as src:
        old_uuid, = src.execute(f"SELECT uuid FROM {TABLE} LIMIT 1").fetchone()
    with boot(args.server, args.component, root, "legacy") as cmd:
        cmd(f"sit toggle {old_uuid}", "你可以通过右键坐下了")
    with sqlite3.connect(db) as conn:
        assert conn.execute(f"SELECT count(*) FROM {TABLE} WHERE uuid=?", (old_uuid,)).fetchone()[0] == 0
        assert conn.execute("PRAGMA integrity_check").fetchone() == ("ok",)
    with boot(args.server, args.component, root, "post-upgrade") as cmd:
        cmd(f"sit toggle {old_uuid}", "你不能通过右键坐下了")
    assert row_count(db, old_uuid) == 1
    assert {entry.name for entry in data.parent.iterdir()} == {"PumpGsit"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda p: Path(p).resolve())
    parser.add_argument("component", type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    exercise(args, Path(tempfile.mkdtemp(prefix="PumpGsit-server-test-")))
