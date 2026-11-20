//! Disposable loopback servers for exercising the same packaged component on both platforms.

use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const PLAYER: &str = "11111111-1111-4111-8111-111111111111";
pub const NEW_PLAYER: &str = "22222222-2222-4222-8222-222222222222";
pub const PIE_FILENAME: &str = "pumpkin_pie_plugin.wasm";
pub const LEGACY_DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gsit-3.8.0.db");

/// Resolves a required test input before starting a server in its own working directory.
pub fn input(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("Set {name} to run this test")))
        .canonicalize()
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// Returns the canonical plugin data directory below the disposable server.
pub fn data_dir(root: &Path) -> PathBuf {
    root.join("plugins/data/PumpGsit")
}

/// Selects a local port without relying on any existing server's configuration.
fn port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An isolated server with a piped console and a persistent, per-run diagnostic log.
pub struct Server {
    child: Option<Child>,
    log_path: PathBuf,
    pub port: u16,
    is_paper: bool,
}

impl Server {
    /// Starts the server and redirects both output streams to a log for this run.
    fn start(mut command: Command, root: &Path, label: &str, port: u16, is_paper: bool) -> Self {
        let log_path = root.join(format!("{label}.log"));
        let out = fs::File::create(&log_path).unwrap();
        let child = command
            .current_dir(root)
            .stdin(Stdio::piped())
            .stderr(out.try_clone().unwrap())
            .stdout(out)
            .spawn()
            .unwrap();
        Self {
            child: Some(child),
            log_path,
            port,
            is_paper,
        }
    }

    /// Reads the entire diagnostic log, tolerating non-UTF-8 JVM output.
    pub fn log(&self) -> String {
        self.since(0)
    }

    /// Returns only new output after a previously captured byte offset.
    fn since(&self, offset: u64) -> String {
        let mut file = fs::File::open(&self.log_path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Polls for the expected log text while checking the process is still running.
    fn wait_from(&mut self, offset: u64, needle: &str, seconds: u64) -> String {
        let until = Instant::now() + Duration::from_secs(seconds);
        loop {
            let output = self.since(offset);
            if output.contains(needle) {
                return output;
            }
            if Instant::now() >= until || self.child.as_mut().unwrap().try_wait().unwrap().is_some()
            {
                let tail = self
                    .log()
                    .lines()
                    .rev()
                    .take(45)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                panic!("Missing {needle:?} in {}\n{tail}", self.log_path.display());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits for a join, disconnect or startup message.
    pub fn wait(&mut self, expected: &str) {
        self.wait_from(0, expected, if self.is_paper { 150 } else { 45 });
    }

    /// Sends one console line, then returns its newly logged feedback.
    pub fn command(&mut self, line: &str, expected: &str) -> String {
        let offset = fs::metadata(&self.log_path).unwrap().len();
        let stdin = self.child.as_mut().unwrap().stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
        self.wait_from(offset, expected, 45)
    }

    /// Shuts down cleanly and checks plugin task, unload and server-exit diagnostics.
    pub fn stop(mut self) {
        let child = self.child.as_mut().unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "stop").unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < until,
                "Server failed to stop: {}",
                self.log_path.display()
            );
            thread::sleep(Duration::from_millis(100));
        };
        self.child.take();
        let log = self.log();
        assert!(status.success(), "{log}");
        if self.is_paper {
            assert!(log.contains("Disabling GSit"), "{log}");
        } else {
            assert!(
                log.contains("All plugins unloaded successfully")
                    && log.contains("The server has stopped"),
                "{log}"
            );
            assert!(
                !log.contains("Wasm plugin store driver failed")
                    && !log.contains("Wasm scheduled task failed"),
                "{log}"
            );
        }
    }
}

impl Drop for Server {
    /// Reaps the child on panic and ensures temporary servers never outlive the test.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Starts Pumpkin with a fresh configuration and optionally installs the component.
pub fn boot(
    root: &Path,
    label: &str,
    component: bool,
    mysql: bool,
    expected: Option<&str>,
) -> Server {
    fs::create_dir_all(root.join("plugins")).unwrap();
    if component {
        fs::copy(
            input("PUMPGSIT_COMPONENT"),
            root.join("plugins/PumpGsit.wasm"),
        )
        .unwrap();
    }
    let port = port();
    let network = r#""network.tcp.connect", "network.dns""#;
    let allowed = if mysql {
        format!(", {network}")
    } else {
        String::new()
    };
    let blocked = if mysql { "" } else { network };
    fs::write(
        root.join("pumpkin.toml"),
        format!(
            r#"allow_nether = false
allow_end = false
[networking.java]
address = "127.0.0.1:{port}"
online_mode = false
encryption = false
view_distance = 2
simulation_distance = 2
[networking.java.compression]
enabled = false
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
allowed_permissions = ["fs.read.data", "fs.write.data"{allowed}]
blocked_permissions = [{blocked}]
"#
        ),
    )
    .unwrap();
    let mut cmd = Command::new(input("PUMPGSIT_SERVER"));
    cmd.envs([
        ("NO_COLOR", "1"),
        ("RUST_LOG", "info"),
        ("TOKIO_WORKER_THREADS", "2"),
        ("RAYON_NUM_THREADS", "2"),
    ]);
    let mut server = Server::start(cmd, root, label, port, false);
    server.wait("Server is now running");
    let version = std::env::var("PUMPKIN_PLUGIN_VERSION")
        .or_else(|_| std::env::var("PUMPGSIT_BUILD_VERSION"))
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into());
    server.wait(expected.unwrap_or(&format!("Loaded PumpGsit ({version})")));
    server
}

/// Copies a local Paper cache into a fully independent test installation.
fn copy_tree(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for item in fs::read_dir(source).unwrap() {
        let item = item.unwrap();
        let target = dest.join(item.file_name());
        if item.path().is_dir() {
            copy_tree(&item.path(), &target);
        } else {
            fs::copy(item.path(), target).unwrap();
        }
    }
}

/// Stages both disposable servers, reusing an already accepted Paper EULA and cached libraries.
pub fn prepare_servers(root: &Path) -> (PathBuf, PathBuf) {
    let java = root.join("paper");
    let wasm = root.join("pumpkin");
    fs::create_dir_all(java.join("plugins")).unwrap();
    fs::create_dir_all(wasm.join("plugins")).unwrap();
    fs::copy(input("PUMPGSIT_GSIT_JAR"), java.join("plugins/GSit.jar")).unwrap();
    fs::copy(
        input("PUMPGSIT_PIE"),
        wasm.join("plugins").join(PIE_FILENAME),
    )
    .unwrap();
    let cache = input("PUMPGSIT_PAPER_CACHE");
    for name in ["cache", "libraries", "versions"] {
        if cache.join(name).is_dir() {
            copy_tree(&cache.join(name), &java.join(name));
        }
    }
    fs::copy(cache.join("eula.txt"), java.join("eula.txt")).unwrap();
    fs::write(java.join("server.properties"), format!(r#"server-ip=127.0.0.1
server-port={}
online-mode=false
enable-status=false
view-distance=2
simulation-distance=2
level-type=minecraft:flat
generator-settings={{"layers":[{{"block":"minecraft:stone","height":1}}],"biome":"minecraft:plains"}}
"#, port())).unwrap();
    (java, wasm)
}

/// Starts upstream Paper using the supplied JDK and upstream GSit jar.
pub fn paper(root: &Path, label: &str) -> Server {
    let mut cmd = Command::new(input("PUMPGSIT_JAVA"));
    cmd.args(["-Xms128m", "-Xmx700m", "-XX:ActiveProcessorCount=2", "-jar"])
        .arg(input("PUMPGSIT_PAPER_JAR"))
        .arg("--nogui");
    let mut server = Server::start(cmd, root, label, 0, true);
    server.wait("Done (");
    server
}

/// Applies original commands, checking each effective value in upstream feedback.
pub fn change(server: &mut Server, player: &str, values: [bool; 3], explicit: bool) {
    for (command, enabled) in ["sit toggle", "sit playertoggle", "crawl toggle"]
        .into_iter()
        .zip(values)
    {
        let tail = match (explicit, enabled) {
            (false, _) => "",
            (true, true) => " ON",
            (true, false) => " OFF",
        };
        server.command(
            &format!("{command} {player}{tail}"),
            if enabled {
                "have enabled"
            } else {
                "have disabled"
            },
        );
    }
}

/// Waits for PIE's registration retry and verifies the full expansion via its public command.
pub fn resolve_pie(server: &mut Server, expression: &str, expected: &str) {
    let until = Instant::now() + Duration::from_secs(35);
    loop {
        let output = server.command(&format!("pie parse Read={expression}"), "Read=");
        if output.contains(&format!("Read={expected}")) {
            return;
        }
        assert!(Instant::now() < until, "Expected {expected:?}: {output}");
        thread::sleep(Duration::from_millis(250));
    }
}

/// Checks every preference through a real, separately installed PumpkinPIE provider.
pub fn check_pie(server: &mut Server, player: &str, values: [bool; 3]) {
    let expression = ["toggle", "playertoggle", "crawltoggle"]
        .map(|name| format!("%gsit_{name}:{player}%"))
        .join(" ");
    resolve_pie(
        server,
        &expression,
        &values.map(|value| value.to_string()).join(" "),
    );
}
