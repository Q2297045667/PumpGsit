"""Shared fixtures and process lifecycle for PumpGsit's real-server tests."""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import time
import tomllib

with (Path(__file__).resolve().parents[1] / "Cargo.toml").open("rb") as manifest:
    VERSION = os.environ.get("PUMPGSIT_BUILD_VERSION", tomllib.load(manifest)["package"]["version"])

PLAYER = "11111111-1111-4111-8111-111111111111"
LEGACY_DB = Path(__file__).parent / "fixtures" / "gsit-3.8.0.db"
PIE_FILENAME = "pumpkin_pie_plugin.wasm"


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
def boot(server, component, root, run, expected=f"Loaded PumpGsit ({VERSION})", *, mysql=False):
    """Start the target binary and ensure it unloads plugins and exits at the end."""
    plugins = root / "plugins"
    plugins.mkdir(parents=True, exist_ok=True)
    if component is not None:
        shutil.copyfile(component, plugins / "PumpGsit.wasm")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    network = ["network.tcp.connect", "network.dns"]
    allowed = ["fs.read.data", "fs.write.data"] + (network if mysql else [])
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
[plugins]
allow_unsigned = false
[plugins.overrides.PumpkinPIE]
allow_unsigned = true
[plugins.overrides.PumpGsit]
allow_unsigned = true
allowed_permissions = {json.dumps(allowed)}
blocked_permissions = {json.dumps([] if mysql else network)}
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
    for line in content.splitlines():
        if "Wasm plugin store driver failed" in line or "Wasm scheduled task failed" in line:
            raise RuntimeError(f"Plugin callback failed in {log}\n{line}")
    if process.returncode or "All plugins unloaded successfully" not in content or "The server has stopped" not in content:
        raise RuntimeError(f"Unclean shutdown, see {log}")
