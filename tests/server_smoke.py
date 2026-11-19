"""Exercise pumpgsit through Pumpkin's console in disposable, loopback-only servers."""
import argparse
from contextlib import contextmanager
from pathlib import Path
import os
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import tomllib

with (Path(__file__).resolve().parents[1] / "Cargo.toml").open("rb") as manifest:
    VERSION = os.environ.get("PUMPGSIT_BUILD_VERSION", tomllib.load(manifest)["package"]["version"])

PLAYER = "11111111-1111-4111-8111-111111111111"
TABLE = "gsit_sit_toggle"


def wait_for(process, log, text, offset=0, timeout=45):
    """Wait for new logged output and include actual server output on failure."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        content = log.read_text(errors="replace")
        if text in content[offset:]:
            return
        if process.poll() is not None:
            break
        time.sleep(0.1)
    raise RuntimeError(f"Missing {text!r} in {log}\n{log.read_text(errors='replace')[-4000:]}")


@contextmanager
def boot(server, component, root, run, expected=f"Loaded pumpgsit ({VERSION})"):
    """Start the target binary and ensure it unloads plugins and exits at the end."""
    plugins = root / "plugins"
    plugins.mkdir(parents=True, exist_ok=True)
    if component is not None:
        shutil.copyfile(component, plugins / "pumpgsit.wasm")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    (root / "pumpkin.toml").write_text(f'''allow_nether = false
allow_end = false
[networking.java]
address = "127.0.0.1:{port}"
online_mode = false
encryption = false
view_distance = 2
simulation_distance = 2
[networking.bedrock]
enabled = false
[networking.lan_broadcast]
enabled = false
[logging]
color = false
[commands]
use_console = true
use_tty = false
[update_checker]
enabled = false
[telemetry]
enabled = false
[plugins.overrides.pumpgsit]
allowed_permissions = ["fs.read.data", "fs.write.data"]
''')
    log = root / f"smoke-{run}.log"
    with log.open("w") as out:
        process = subprocess.Popen([server], cwd=root, stdin=subprocess.PIPE, stdout=out,
                                   stderr=subprocess.STDOUT, text=True,
                                   env={**os.environ, "NO_COLOR": "1", "RUST_LOG": "info", "TOKIO_WORKER_THREADS": "2", "RAYON_NUM_THREADS": "2"})
        try:
            wait_for(process, log, "Server is now running")
            wait_for(process, log, expected)
            def command(line, result):
                """Send one command and match feedback produced after that command."""
                offset = len(log.read_text(errors="replace"))
                process.stdin.write(line + "\n")
                process.stdin.flush()
                wait_for(process, log, result, offset)
            yield command
        finally:
            if process.poll() is None:
                process.stdin.write("stop\n")
                process.stdin.flush()
                try:
                    process.wait(30)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(10)
            process.stdin.close()
    content = log.read_text()
    if process.returncode or "All plugins unloaded successfully" not in content or "The server has stopped" not in content:
        raise RuntimeError(f"Unclean shutdown, see {log}")


def row_count(db):
    """Read this test player's exception from a stopped server database."""
    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as connection:
        return connection.execute(f"SELECT count(*) FROM {TABLE} WHERE uuid=?", (PLAYER,)).fetchone()[0]


def exercise(args, root):
    """Check a new install, idempotent preferences, reload rollback, and a restarted old database."""
    root.mkdir(parents=True, exist_ok=True)
    data = root / "plugins/data/pumpgsit"
    db = data / "data/data.db"
    with boot(args.server, args.component, root, "fresh") as cmd:
        cmd("sit", "Run this command as a player.")
        cmd("pumpgsit help", "Repeat the current command")
        cmd("gsit help", "Repeat the current command")
        cmd("plugin unload pumpgsit", "Plugin pumpgsit unloaded successfully")
        shutil.copyfile(args.component, root / "reload.wasm")
        cmd("plugin load reload.wasm", "Plugin reload.wasm loaded successfully")
        cmd("sit help", "Repeat the current command")
        cmd("pumpgsitreload", "pumpgsit configuration reloaded.")
        cmd("gsit toggle unknown-offline-name on", "original player's UUID")
        for _ in range(2):
            cmd(f"sit toggle {PLAYER} off", "Click-to-sit disabled.")
        cfg = data / "config.toml"
        valid = cfg.read_text()
        cfg.write_text("invalid [")
        cmd("gsitreload", "previous settings are still active")
        cfg.write_text(valid.replace('language = "en_us"', 'language = "zh_cn"'))
        cmd("gsitrl", "pumpgsit 配置已重新加载")
        cmd("glayback", "请以玩家身份")
    assert not (root / "plugins/data/gsit").exists()
    assert row_count(db) == 1
    with boot(args.server, args.component, root, "restart") as cmd:
        cmd(f"sit toggle {PLAYER}", "已开启右键方块坐下")
    assert row_count(db) == 0
    legacy = Path(__file__).resolve().parents[1] / "doc/fixtures/gsit-3.8.0.db"
    shutil.copyfile(legacy, db)
    with sqlite3.connect(db) as src:
        old_uuid, = src.execute(f"SELECT uuid FROM {TABLE} LIMIT 1").fetchone()
    with boot(args.server, args.component, root, "legacy") as cmd:
        cmd(f"sit toggle {old_uuid}", "已开启右键方块坐下")
    with sqlite3.connect(db) as conn:
        assert conn.execute(f"SELECT count(*) FROM {TABLE} WHERE uuid=?", (old_uuid,)).fetchone()[0] == 0
        assert conn.execute("PRAGMA integrity_check").fetchone() == ("ok",)
    print(f"Passed fresh install, aliases, console, SQLite, reload, i18n and restart in {root}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=lambda p: Path(p).resolve())
    parser.add_argument("component", type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    exercise(args, Path(tempfile.mkdtemp(prefix="pumpgsit-server-test-")))
