//! Paper and Pumpkin round trips against a dedicated, disposable MySQL schema.
use crate::support::{
    NEW_PLAYER, PLAYER, boot, change, check_pie, data_dir, paper, prepare_servers,
};
use serde_json::json;
use std::{
    env, fs,
    io::Write,
    process::{Command, Stdio},
};

const TABLES: [&str; 3] = ["gsit_sit_toggle", "gsit_player_toggle", "gsit_crawl_toggle"];

/// Runs a MySQL command with its password in the environment and its SQL on stdin.
fn output(cmd: &mut Command, input: &str) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .map_err(|e| e.to_string())?;
    let result = child.wait_with_output().map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    String::from_utf8(result.stdout).map_err(|e| e.to_string())
}

struct Database {
    name: String,
    host: String,
    port: String,
    user: String,
    password: String,
    created: bool,
}

impl Database {
    /// Creates a schema named after the test's uniquely and exclusively created temporary directory.
    fn new(name: &str) -> Self {
        assert!(
            name.starts_with("pumpgsit_test_")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        let mut db = Self {
            name: name.to_owned(),
            host: env::var("PUMPGSIT_TEST_MYSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: env::var("PUMPGSIT_TEST_MYSQL_PORT").unwrap_or_else(|_| "3306".into()),
            user: env::var("PUMPGSIT_TEST_MYSQL_USER").expect("set PUMPGSIT_TEST_MYSQL_USER"),
            password: env::var("PUMPGSIT_TEST_MYSQL_PASSWORD")
                .expect("set PUMPGSIT_TEST_MYSQL_PASSWORD"),
            created: false,
        };
        // Establish ownership before constructing a Drop guard that can remove this schema.
        output(
            &mut db.client("mysql"),
            &format!("CREATE DATABASE {}", db.name),
        )
        .unwrap();
        db.created = true;
        db
    }

    /// Builds the mysql or mysqldump invocation for this configured test account.
    fn client(&self, binary: &str) -> Command {
        let mut cmd = Command::new(binary);
        cmd.args([
            "--no-defaults",
            "--protocol=TCP",
            "--host",
            &self.host,
            "--port",
            &self.port,
            "--user",
            &self.user,
        ])
        .env("MYSQL_PWD", &self.password);
        cmd
    }

    /// Runs an assertion, mutation or dump restore in the owned schema.
    fn sql(&self, statement: &str) -> String {
        output(
            self.client("mysql")
                .args(["--batch", "--skip-column-names", &self.name]),
            statement,
        )
        .unwrap()
    }

    /// Records physical row counts, including repeats of explicit non-default settings.
    fn counts(&self, id: &str) -> Vec<usize> {
        let parts = TABLES.map(|t| format!("(SELECT COUNT(*) FROM {t} WHERE uuid = '{id}')"));
        self.sql(&format!("SELECT {}", parts.join(", ")))
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect()
    }

    /// Records complete table definitions to catch changes to keys, types, engines and collations.
    fn schema(&self) -> [String; 3] {
        TABLES.map(|table| self.sql(&format!("SHOW CREATE TABLE {table}")))
    }
}

impl Drop for Database {
    /// Removes only the ephemeral schema owned by this test, including on assertion failure.
    fn drop(&mut self) {
        if self.created
            && let Err(error) = output(
                &mut self.client("mysql"),
                &format!("DROP DATABASE IF EXISTS {}", self.name),
            )
        {
            eprintln!("Could not clean up {}: {error}", self.name);
        }
    }
}

/// Verifies a Paper-generated database, restart, failure paths, rollback and fresh creation.
#[test]
#[ignore = "requires Paper/GSit, Pumpkin/PIE, mysql, mysqldump and a test account"]
fn mysql_paper_roundtrip() {
    let dir = tempfile::Builder::new()
        .prefix("pumpgsit_test_")
        .rand_bytes(16)
        .tempdir()
        .unwrap();
    let db = Database::new(dir.path().file_name().unwrap().to_str().unwrap());
    let (java, wasm) = prepare_servers(dir.path());
    let legacy = java.join("plugins/GSit/data");
    fs::create_dir_all(&legacy).unwrap();
    let jdbc = json!({"Database": {"type":"mysql", "host":db.host, "port":db.port, "database":db.name,
        "user":db.user, "password":db.password, "args":"&sslMode=DISABLED&allowPublicKeyRetrieval=true"}});
    fs::write(
        legacy.join("data.yml"),
        serde_yaml_ng::to_string(&jdbc).unwrap(),
    )
    .unwrap();
    let data = data_dir(&wasm);
    fs::create_dir_all(&data).unwrap();
    let cfg = data.join("config.toml");
    let mut config: toml::Table = include_str!("../../assets/config.toml").parse().unwrap();
    let account = env::var("PUMPGSIT_TEST_MYSQL_COLD_USER").unwrap_or_else(|_| db.user.clone());
    let password =
        env::var("PUMPGSIT_TEST_MYSQL_COLD_PASSWORD").unwrap_or_else(|_| db.password.clone());
    config["database"] = toml::Value::try_from(json!({"type":"mysql", "host":db.host,
        "port":db.port.parse::<u16>().unwrap(), "database":db.name, "user":account, "password":password})).unwrap();
    let ca = env::var_os("PUMPGSIT_TEST_MYSQL_CA");
    let database = config["database"].as_table_mut().unwrap();
    if let Some(path) = &ca {
        fs::copy(path, data.join("mysql-ca.pem")).unwrap();
        database.insert("ca_file".into(), "mysql-ca.pem".into());
    } else {
        database.insert("tls".into(), "disabled".into());
    }
    let valid = toml::to_string(&config).unwrap();
    fs::write(&cfg, &valid).unwrap();
    let original = [false, true, false];
    let changed = [true, false, true];

    // Restore the checked-in original fixture before asking Paper to produce a new database.
    db.sql(include_str!("../fixtures/gsit-3.8.0-mysql.sql"));
    assert_eq!(db.counts(PLAYER), [2, 0, 2]);
    assert_eq!(db.counts(NEW_PLAYER), [0, 1, 0]);
    db.sql(&format!("DROP TABLE {}", TABLES.join(", ")));
    let mut server = paper(&java, "create");
    change(&mut server, PLAYER, original, true);
    change(&mut server, PLAYER, original, true);
    server.stop();
    assert_eq!(db.counts(PLAYER), [2, 0, 2]);
    let schema = db.schema();
    db.sql(
        "CREATE TABLE unrelated(marker INT); INSERT INTO unrelated VALUES (7);
            INSERT INTO gsit_sit_toggle VALUES (NULL), ('legacy-unknown')",
    );
    let before = output(
        db.client("mysqldump")
            .args(["--single-transaction", "--no-tablespaces", &db.name]),
        "",
    )
    .unwrap();
    boot(&wasm, "no-network", true, false, Some("MySQL:")).stop();
    assert!(!data.join("data/data.db").exists());
    assert_eq!(db.counts(PLAYER), [2, 0, 2]);

    let mut server = boot(&wasm, "migrate", true, true, None);
    check_pie(&mut server, PLAYER, original);
    let panel = server.command(&format!("gsitprefs {PLAYER}"), "Sneak gestures to crawl");
    for (label, state) in [
        ("Right-click blocks to sit", "OFF"),
        ("Right-click players to stack", "ON"),
        ("Sneak gestures to crawl", "OFF"),
    ] {
        assert!(
            panel
                .lines()
                .any(|line| line.contains(label) && line.contains(state)),
            "{panel}"
        );
    }
    change(&mut server, PLAYER, changed, false);
    if ca.is_some() {
        assert!(
            Command::new("openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-days",
                    "1",
                    "-subj",
                    "/CN=Unrelated Test CA",
                    "-keyout"
                ])
                .arg(dir.path().join("wrong-ca.key"))
                .arg("-out")
                .arg(data.join("wrong-ca.pem"))
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        fs::write(&cfg, valid.replace("mysql-ca.pem", "wrong-ca.pem")).unwrap();
        server.command("gsitreload", "previous settings are still active");
        check_pie(&mut server, PLAYER, changed);
    }
    config["database"]["password"] = "incorrect-test-password".into();
    fs::write(&cfg, toml::to_string(&config).unwrap()).unwrap();
    server.command("gsitreload", "previous settings are still active");
    change(&mut server, NEW_PLAYER, [false; 3], true);
    assert!(!server.log().contains("incorrect-test-password"));
    fs::write(&cfg, &valid).unwrap();
    server.command("gsitreload", "plugin was successfully reloaded");
    change(&mut server, NEW_PLAYER, [false; 3], true);
    server.stop();
    assert_eq!(db.counts(PLAYER), [0, 1, 0]);
    assert_eq!(db.counts(NEW_PLAYER), [2, 2, 2]);
    assert_eq!(db.schema(), schema);
    assert_eq!(
        db.sql("SELECT COUNT(*) FROM gsit_sit_toggle WHERE uuid IS NULL OR uuid='legacy-unknown'")
            .trim(),
        "2"
    );

    let mut server = boot(&wasm, "restart", true, true, None);
    check_pie(&mut server, PLAYER, changed);
    check_pie(&mut server, NEW_PLAYER, [false; 3]);
    fs::write(
        &cfg,
        valid.replace("default_enabled = true", "default_enabled = false"),
    )
    .unwrap();
    server.command("gsitreload", "plugin was successfully reloaded");
    check_pie(&mut server, PLAYER, original);
    check_pie(&mut server, NEW_PLAYER, [true; 3]);
    server.stop();
    fs::write(&cfg, &valid).unwrap();

    let mut server = paper(&java, "return");
    change(&mut server, PLAYER, original, false);
    change(&mut server, NEW_PLAYER, [true; 3], false);
    server.stop();
    assert_eq!(db.counts(PLAYER), [1, 0, 1]);
    assert_eq!(db.counts(NEW_PLAYER), [0, 0, 0]);

    db.sql(&before);
    assert_eq!(db.counts(PLAYER), [2, 0, 2]);
    let mut server = paper(&java, "restore");
    change(&mut server, PLAYER, changed, false);
    server.stop();
    assert_eq!(db.counts(PLAYER), [0, 1, 0]);

    db.sql(&format!("DROP TABLE {}", TABLES.join(", ")));
    let mut server = boot(&wasm, "fresh", true, true, None);
    check_pie(&mut server, PLAYER, [true; 3]);
    change(&mut server, PLAYER, [false; 3], true);
    server.stop();
    assert_eq!(db.counts(PLAYER), [1, 1, 1]);
    assert_eq!(db.schema(), schema);
    db.sql("DROP TABLE gsit_sit_toggle, gsit_player_toggle; RENAME TABLE gsit_crawl_toggle TO preserved_crawl;
            CREATE TABLE gsit_crawl_toggle(unexpected INT); INSERT INTO gsit_crawl_toggle VALUES (42)");
    boot(
        &wasm,
        "bad-schema",
        true,
        true,
        Some("gsit_crawl_toggle: expected one writable uuid TEXT column"),
    )
    .stop();
    assert!(
        db.sql("SHOW TABLES LIKE 'gsit_sit_toggle'")
            .trim()
            .is_empty()
    );
    assert!(
        db.sql("SHOW TABLES LIKE 'gsit_player_toggle'")
            .trim()
            .is_empty()
    );
    assert_eq!(db.sql("SELECT uuid FROM preserved_crawl").trim(), PLAYER);
    assert_eq!(
        db.sql("SELECT unexpected FROM gsit_crawl_toggle").trim(),
        "42"
    );
    assert_eq!(db.sql("SELECT marker FROM unrelated").trim(), "7");
    db.sql(&format!("DROP DATABASE {}", db.name));
    let mut server = boot(&wasm, "create-database", true, true, None);
    change(&mut server, PLAYER, [false; 3], true);
    server.stop();
    assert_eq!(db.counts(PLAYER), [1, 1, 1]);
    assert_eq!(db.schema(), schema);
    assert!(!data.join("data/data.db").exists() && !legacy.join("data.db").exists());
}
