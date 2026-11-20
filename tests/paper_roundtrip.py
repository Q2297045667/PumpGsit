"""Exercise both directions of a GSit SQLite migration and the original backup on real servers."""
import argparse
from contextlib import closing, contextmanager
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile

from support import PIE_FILENAME, PLAYER, boot, wait_for

NEW_PLAYER = "22222222-2222-4222-8222-222222222222"
COMMANDS = ("sit toggle", "sit playertoggle", "crawl toggle")
FEATURES = ("sit", "player", "crawl")
PLACEHOLDERS = ("toggle", "playertoggle", "crawltoggle")


def snapshot(source, dest):
    """Back up a stopped server's database, verifying the source and restored copy."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(source.as_uri() + "?mode=ro", uri=True)) as src, closing(sqlite3.connect(dest)) as dst:
        assert src.execute("PRAGMA integrity_check").fetchone() == ("ok",)
        src.backup(dst)
        assert dst.execute("PRAGMA integrity_check").fetchone() == ("ok",)


def values(db, player):
    """Read the three effective preferences with both servers' default configuration."""
    with closing(sqlite3.connect(db.as_uri() + "?mode=ro", uri=True)) as conn:
        return tuple(not conn.execute(f"SELECT 1 FROM gsit_{feature}_toggle WHERE uuid=?", (player,)).fetchone()
                     for feature in FEATURES)


@contextmanager
def paper(args, root, label):
    """Run the upstream jar on a disposable loopback server with a bounded heap."""
    log = root / f"{label}.log"
    with log.open("w") as out:
        proc = subprocess.Popen([args.java, "-Xms128m", "-Xmx700m", "-XX:ActiveProcessorCount=2",
                                 "-jar", args.paper, "--nogui"], cwd=root, stdin=subprocess.PIPE,
                                stdout=out, stderr=subprocess.STDOUT, text=True)
        try:
            wait_for(proc, log, "Done (", timeout=150)

            def command(line, expected):
                """Match only feedback produced after this command."""
                offset = len(log.read_text(errors="replace"))
                proc.stdin.write(line + "\n")
                proc.stdin.flush()
                wait_for(proc, log, expected, offset)
            yield command
        finally:
            if proc.poll() is None:
                proc.stdin.write("stop\n")
                proc.stdin.flush()
                try:
                    proc.wait(45)
                except subprocess.TimeoutExpired:
                    proc.terminate()
                    proc.wait(15)
            proc.stdin.close()
    assert proc.returncode == 0 and "Disabling GSit" in log.read_text(), log


def change(command, player, value, explicit):
    """Use each legacy command and verify the on/off confirmation from the plugin."""
    for cmd, enabled in zip(COMMANDS, value, strict=True):
        tail = (" ON" if enabled else " OFF") if explicit else ""
        command(f"{cmd} {player}{tail}", "have enabled" if enabled else "have disabled")


def check_pie(command, root, run, player, expected):
    """Observe all three values through the optional production placeholder integration."""
    from time import monotonic, sleep
    log = root / f"smoke-{run}.log"
    expression = " ".join(f"%gsit_{name}:{player}%" for name in PLACEHOLDERS)
    value = " ".join(str(v).lower() for v in expected)
    deadline = monotonic() + 30
    while monotonic() < deadline:
        offset = len(log.read_text())
        command(f"pie parse Read={expression}", "Read=")
        if "Read=" + value in log.read_text()[offset:]:
            return
        sleep(.25)
    raise AssertionError(f"{log}: expected Read={value}")


def prepare_servers(args, root):
    """Stage a disposable Paper server and Pumpkin plugins for a database round trip."""
    java, wasm = root / "paper", root / "pumpkin"
    (java / "plugins").mkdir(parents=True)
    (wasm / "plugins").mkdir(parents=True)
    shutil.copyfile(args.gsit, java / "plugins/GSit.jar")
    shutil.copyfile(args.pie, wasm / "plugins" / PIE_FILENAME)
    for name in ("cache", "libraries", "versions"):
        if (args.paper_cache / name).is_dir():
            shutil.copytree(args.paper_cache / name, java / name)
    # The caller supplies a local installation with an accepted Minecraft EULA.
    shutil.copyfile(args.paper_cache / "eula.txt", java / "eula.txt")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    (java / "server.properties").write_text(f"server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\n"
        "enable-status=false\nview-distance=2\nsimulation-distance=2\nlevel-type=minecraft:flat\n"
        'generator-settings={"layers":[{"block":"minecraft:stone","height":1}],"biome":"minecraft:plains"}\n')
    return java, wasm


def exercise(args, root):
    """Create data on Paper, modify on Pumpkin, restart, roll back and restore the original backup."""
    java, wasm = prepare_servers(args, root)
    original = (False, True, False)
    changed = (True, False, True)
    all_off = (False,) * 3
    all_on = (True,) * 3
    old = java / "plugins/GSit/data/data.db"
    new = wasm / "plugins/data/PumpGsit/data/data.db"
    with paper(args, java, "create") as command:
        change(command, PLAYER, original, explicit=True)
    assert values(old, PLAYER) == original
    snapshot(old, root / "before.db")
    snapshot(root / "before.db", new)
    with boot(args.server, args.component, wasm, "migrate") as command:
        check_pie(command, wasm, "migrate", PLAYER, original)
        change(command, PLAYER, changed, explicit=False)
        change(command, NEW_PLAYER, all_off, explicit=True)
    assert values(new, PLAYER) == changed and values(new, NEW_PLAYER) == all_off
    with boot(args.server, args.component, wasm, "restart") as command:
        check_pie(command, wasm, "restart", PLAYER, changed)
        check_pie(command, wasm, "restart", NEW_PLAYER, all_off)
    snapshot(new, root / "after.db")
    snapshot(root / "after.db", old)
    with paper(args, java, "return") as command:
        change(command, PLAYER, original, explicit=False)
        change(command, NEW_PLAYER, all_on, explicit=False)
    assert values(old, PLAYER) == original and values(old, NEW_PLAYER) == all_on
    snapshot(root / "before.db", old)
    assert values(old, PLAYER) == original and values(old, NEW_PLAYER) == all_on
    with paper(args, java, "restore") as command:
        change(command, PLAYER, changed, explicit=False)
    assert values(old, PLAYER) == changed
    print("Passed Paper → Pumpkin, Pumpkin restart, return to Paper with new player, and restore of before.db.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("java", "paper", "gsit", "paper-cache", "server", "component", "pie"):
        parser.add_argument("--" + arg, type=lambda path: Path(path).resolve(), required=True)
    with tempfile.TemporaryDirectory(prefix="PumpGsit-roundtrip-") as temp:
        exercise(parser.parse_args(), Path(temp))
