"""Check original Paper GSit and PumpGsit against the same disposable MySQL database."""

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
from uuid import uuid4

from paper_roundtrip import FEATURES, NEW_PLAYER, change, check_pie, paper, prepare_servers
from support import PLAYER, boot


@contextmanager
def database(args, password):
    """Create a uniquely named test schema and remove only that schema on exit."""
    name = "pumpgsit_test_" + uuid4().hex
    opts = ["--no-defaults", "--protocol=TCP", "--host", args.host, "--port", str(args.port),
            "--user", args.user]
    env = {**os.environ, "MYSQL_PWD": password}

    def sql(statement, schema=name):
        """Run SQL in batch mode; all identifiers and UUIDs in test SQL are generated locally."""
        command = ["mysql", *opts, "--batch", "--skip-column-names"]
        if schema:
            command.append(schema)
        return subprocess.check_output(command, env=env, input=statement, text=True)

    def backup():
        """Return a consistent, restorable dump of the stopped server's test schema."""
        return subprocess.check_output(["mysqldump", *opts, "--single-transaction", "--no-tablespaces",
                                        name], env=env, text=True)

    sql(f"CREATE DATABASE {name}", schema=None)
    try:
        yield name, sql, backup
    finally:
        sql(f"DROP DATABASE IF EXISTS {name}", schema=None)


def pumpkin_config(settings):
    """Keep the example gameplay settings and replace its database table with test settings."""
    template = (Path(__file__).resolve().parents[1] / "assets/config.toml").read_text()
    template = re.sub(r"(?ms)^\[database\]\s*\n.*?(?=^\[|\Z)", "", template)
    return template.rstrip() + "\n\n[database]\n" + "".join(
        f"{key} = {json.dumps(value, ensure_ascii=False)}\n" for key, value in settings.items())


def counts(sql, player):
    """Count physical rows in each original table so lost duplicates cannot pass as equivalent."""
    query = "SELECT " + ", ".join(
        f"(SELECT COUNT(*) FROM gsit_{feature}_toggle WHERE uuid = '{player}')" for feature in FEATURES)
    return tuple(map(int, sql(query).split()))


def schema(sql):
    """Record the exact table DDL including collation, engine, keys and default values."""
    return tuple(sql(f"SHOW CREATE TABLE gsit_{feature}_toggle") for feature in FEATURES)


def exercise(args, root, name, sql, backup, password):
    """Verify shared storage, failed reload, restarts, rollback and a fresh Pumpkin install."""
    java, wasm = prepare_servers(args, root)
    settings = dict(type="mysql", host=args.host, port=args.port, database=name,
                    user=args.user, password=password)
    paper_data = java / "plugins/GSit/data"
    paper_data.mkdir(parents=True)
    # GSit appends Database.args verbatim to its JDBC query string.
    paper_cfg = {**settings, "port": str(args.port), "args": "&sslMode=DISABLED&allowPublicKeyRetrieval=true"}
    (paper_data / "data.yml").write_text("Database:\n" + "".join(
        f"  {key}: {json.dumps(value, ensure_ascii=False)}\n" for key, value in paper_cfg.items()))

    data = wasm / "plugins/data/PumpGsit"
    data.mkdir(parents=True)
    cfg = data / "config.toml"
    if args.cold_user:
        settings["user"] = args.cold_user
        settings["password"] = os.environ.get("PUMPGSIT_TEST_MYSQL_COLD_PASSWORD", password)
    if args.ca:
        shutil.copyfile(args.ca, data / "mysql-ca.pem")
        settings["ca_file"] = "mysql-ca.pem"  # Exercise the default verify_identity mode.
    else:
        settings["tls"] = "disabled"
    valid = pumpkin_config(settings)
    cfg.write_text(valid)

    original, changed = (False, True, False), (True, False, True)
    all_on, all_off = (True,) * 3, (False,) * 3
    with paper(args, java, "create") as command:
        change(command, PLAYER, original, explicit=True)
        change(command, PLAYER, original, explicit=True)
    assert counts(sql, PLAYER) == (2, 0, 2)
    before_schema = schema(sql)
    # An unrelated table in the same database must survive migration and rollback.
    sql("CREATE TABLE unrelated (marker INT); INSERT INTO unrelated VALUES (7); "
        "INSERT INTO gsit_sit_toggle VALUES (NULL), ('legacy-unknown')")
    before = backup()
    with boot(args.server, args.component, wasm, "no-network", expected="MySQL:"):
        pass
    assert not (data / "data/data.db").exists() and counts(sql, PLAYER) == (2, 0, 2)

    with boot(args.server, args.component, wasm, "migrate", mysql=True) as command:
        check_pie(command, wasm, "migrate", PLAYER, original)
        change(command, PLAYER, changed, explicit=False)
        if args.ca:
            subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt",
                            "ec_paramgen_curve:P-256", "-nodes", "-days", "1", "-subj",
                            "/CN=PumpGsit Unrelated Test CA", "-keyout", root / "wrong-ca.key",
                            "-out", data / "wrong-ca.pem"], check=True, stderr=subprocess.DEVNULL)
            cfg.write_text(pumpkin_config({**settings, "ca_file": "wrong-ca.pem"}))
            command("pumpgsitreload", "previous settings are still active")
            check_pie(command, wasm, "migrate", PLAYER, changed)
        denied = "invalid-" + uuid4().hex
        cfg.write_text(pumpkin_config({**settings, "password": denied}))
        command("pumpgsitreload", "previous settings are still active")
        change(command, NEW_PLAYER, all_off, explicit=True)
        assert denied not in (wasm / "smoke-migrate.log").read_text()
        cfg.write_text(valid)
        command("pumpgsitreload", "plugin was successfully reloaded")
        change(command, NEW_PLAYER, all_off, explicit=True)
    assert counts(sql, PLAYER) == (0, 1, 0) and counts(sql, NEW_PLAYER) == (2, 2, 2)
    assert schema(sql) == before_schema
    assert sql("SELECT COUNT(*) FROM gsit_sit_toggle WHERE uuid IS NULL OR uuid='legacy-unknown'").strip() == "2"
    with boot(args.server, args.component, wasm, "restart", mysql=True) as command:
        check_pie(command, wasm, "restart", PLAYER, changed)
        check_pie(command, wasm, "restart", NEW_PLAYER, all_off)
        cfg.write_text(valid.replace("default_enabled = true", "default_enabled = false"))
        command("pumpgsitreload", "plugin was successfully reloaded")
        check_pie(command, wasm, "restart", PLAYER, original)
        check_pie(command, wasm, "restart", NEW_PLAYER, all_on)
        cfg.write_text(valid)

    with paper(args, java, "return") as command:
        change(command, PLAYER, original, explicit=False)
        change(command, NEW_PLAYER, all_on, explicit=False)
    assert counts(sql, PLAYER) == (1, 0, 1) and counts(sql, NEW_PLAYER) == (0, 0, 0)
    sql(before)
    assert counts(sql, PLAYER) == (2, 0, 2)
    with paper(args, java, "restore") as command:
        change(command, PLAYER, changed, explicit=False)
    assert counts(sql, PLAYER) == (0, 1, 0)

    sql("DROP TABLE " + ", ".join(f"gsit_{feature}_toggle" for feature in FEATURES))
    with boot(args.server, args.component, wasm, "fresh", mysql=True) as command:
        check_pie(command, wasm, "fresh", PLAYER, all_on)
        change(command, PLAYER, all_off, explicit=True)
    assert counts(sql, PLAYER) == (1, 1, 1) and schema(sql) == before_schema
    sql("DROP TABLE gsit_sit_toggle, gsit_player_toggle; RENAME TABLE gsit_crawl_toggle TO preserved_crawl; "
        "CREATE TABLE gsit_crawl_toggle (unexpected INT); INSERT INTO gsit_crawl_toggle VALUES (42)")
    with boot(args.server, args.component, wasm, "bad-schema",
              expected="gsit_crawl_toggle: expected one writable uuid TEXT column", mysql=True):
        pass
    assert not sql("SHOW TABLES LIKE 'gsit_sit_toggle'").strip()
    assert not sql("SHOW TABLES LIKE 'gsit_player_toggle'").strip()
    assert sql("SELECT uuid FROM preserved_crawl").strip() == PLAYER
    assert sql("SELECT unexpected FROM gsit_crawl_toggle").strip() == "42"
    assert sql("SELECT marker FROM unrelated").strip() == "7"
    sql(f"DROP DATABASE {name}", schema=None)
    with boot(args.server, args.component, wasm, "create-database", mysql=True) as command:
        change(command, PLAYER, all_off, explicit=True)
    assert counts(sql, PLAYER) == (1, 1, 1) and schema(sql) == before_schema
    assert not (data / "data/data.db").exists() and not (paper_data / "data.db").exists()
    print("Passed Paper/MySQL → Pumpkin, duplicate UUIDs, failed reload, restart, return to Paper, "
          "dump restore, fresh database creation and rejection of an incompatible table.")


def main():
    """Run on a loopback test MySQL instance using local mysql and mysqldump commands."""
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("java", "paper", "gsit", "paper-cache", "server", "component", "pie"):
        parser.add_argument("--" + arg, type=lambda path: Path(path).resolve(), required=True)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=3306)
    parser.add_argument("--user", required=True, help="test account with rights on pumpgsit_test_*")
    parser.add_argument("--ca", type=Path, help="exercise verified TLS using a MySQL server signed by this private test CA")
    parser.add_argument("--cold-user", help="unused caching_sha2_password account for the first Pumpkin login")
    args = parser.parse_args()
    password = os.environ["PUMPGSIT_TEST_MYSQL_PASSWORD"]
    with tempfile.TemporaryDirectory(prefix="PumpGsit-mysql-") as root, database(args, password) as db:
        exercise(args, Path(root), *db, password)


if __name__ == "__main__":
    main()
