"""Check both load orders of PumpGsit and a real PumpkinPIE component in disposable servers."""

import argparse
from pathlib import Path
import shutil
import tempfile
import time

from support import LEGACY_DB, PIE_FILENAME, PLAYER, boot

NAMES = ("sitting", "posing", "crawling", "toggle", "playertoggle", "crawltoggle")
BARE = " ".join(f"%gsit_{name}%" for name in NAMES)
TARGETED = " ".join(f"%gsit_{name}:{PLAYER}%" for name in NAMES)
ORIGINAL = "false false false false true false"
MODIFIED = "false false false true true false"


def wait_for_registration(command, log, expected):
    """Query the provider until PumpGsit's registration retry succeeds or times out."""
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        offset = len(log.read_text())
        command(f"pie parse Probe={TARGETED}", "Probe=")
        if f"Probe={expected}" in log.read_text()[offset:]:
            return
        time.sleep(0.5)
    raise RuntimeError(f"PIE did not resolve all six values to {expected!r} in {log}")


def exercise(server, component, provider, root):
    """Read an upstream fixture through PIE, hot load both plugins, then verify after restart."""
    root.mkdir(parents=True, exist_ok=True)
    data = root / "plugins/data/PumpGsit/data"
    data.mkdir(parents=True)
    shutil.copyfile(LEGACY_DB, data / "data.db")
    shutil.copyfile(provider, root / PIE_FILENAME)
    with boot(server, component, root, "PumpGsit-first") as command:
        log = root / "smoke-PumpGsit-first.log"
        command(f"plugin load {PIE_FILENAME}", "loaded successfully")
        wait_for_registration(command, log, ORIGINAL)
        command("pie expansions", "%gsit%  from PumpGsit:")
        command(f"pie parse NoViewer={BARE}", f"NoViewer={BARE}")
        command("pie parse Bad=%gsit_toggle:unknown-name%", "unresolved: gsit_toggle")
        command(f"sit toggle {PLAYER} on", "have enabled")
        command(f"pie parse Changed={TARGETED}", f"Changed={MODIFIED}")
        command("plugin unload PumpkinPIE", "unloaded successfully")
        command("sit help", "Repeat the current command")
        command(f"plugin load {PIE_FILENAME}", "loaded successfully")
        wait_for_registration(command, log, MODIFIED)

    # Start with the provider alone, then load PumpGsit into its pre-existing registry.
    (root / "plugins/PumpGsit.wasm").rename(root / "PumpGsit.wasm")
    shutil.copyfile(provider, root / "plugins" / PIE_FILENAME)
    with boot(server, None, root, "pie-first", expected="Loaded PumpkinPIE (0.2.6)") as command:
        log = root / "smoke-pie-first.log"
        for _ in range(2):
            command("plugin load PumpGsit.wasm", "loaded successfully")
            wait_for_registration(command, log, MODIFIED)
            command("plugin unload PumpGsit", "unloaded successfully")
            command(f"pie parse Offline={TARGETED}", f"Offline={TARGETED}")
        command("plugin load PumpGsit.wasm", "loaded successfully")
        wait_for_registration(command, log, MODIFIED)


def main():
    """Run the optional integration test using caller-supplied Wasm files and a target server."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("server", "component", "pie"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="PumpGsit-pie-") as temp:
        exercise(args.server.resolve(), args.component.resolve(), args.pie.resolve(), Path(temp))
    print("PIE load orders, six placeholders, hot reload, SQLite restart and clean shutdown passed.")


if __name__ == "__main__":
    main()
