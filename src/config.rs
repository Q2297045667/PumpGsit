//! The on-disk TOML mirrors the upstream gameplay settings.
use serde::{Deserialize, Deserializer};
use std::{
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub database: Database,
    pub language: String,
    pub client_language: bool,
    pub get_up_damage: bool,
    pub get_up_sneak: bool,
    pub get_up_return: bool,
    pub get_up_break: bool,
    pub allow_unsafe: bool,
    pub same_block_rest: bool,
    pub center_block: bool,
    pub custom_message: bool,
    pub world_blacklist: Vec<String>,
    pub world_whitelist: Vec<String>,
    #[serde(deserialize_with = "material_rules")]
    pub material_blacklist: Vec<MaterialRule>,
    pub command_blacklist: Vec<String>,
    pub sit: Sit,
    pub player_sit: PlayerSit,
    pub pose: Pose,
    pub crawl: Crawl,
}

/// Connection settings selected by the operator. Old configurations use the original SQLite file.
#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Database {
    Sqlite {
        #[serde(default = "legacy_sqlite_path")]
        path: PathBuf,
    },
    Mysql(MySqlConfig),
}

/// Returns the same relative path used by earlier PumpGsit releases.
fn legacy_sqlite_path() -> PathBuf {
    "data/data.db".into()
}

impl Default for Database {
    /// Selects SQLite when an existing configuration predates backend selection.
    fn default() -> Self {
        Self::Sqlite {
            path: legacy_sqlite_path(),
        }
    }
}

/// MySQL authenticates the server and verifies the host name unless explicitly disabled.
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    VerifyIdentity,
    Disabled,
}

/// Credentials and destination for direct access to GSit's tables.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MySqlConfig {
    pub host: String,
    #[serde(default = "mysql_port")]
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
    #[serde(default)]
    pub tls: TlsMode,
    pub ca_file: Option<String>,
}

/// Returns the standard MySQL TCP port when the configuration omits it.
fn mysql_port() -> u16 {
    3306
}

/// Checks that the path names a file below the WASI plugin data directory.
fn data_file(path: &Path) -> bool {
    path.file_name().is_some()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

impl Database {
    /// Validates the candidate before changing a live backend or opening files or sockets.
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Sqlite { path } if !data_file(path) => Err(
                "config.toml: database.path must name a file inside the plugin data directory"
                    .into(),
            ),
            Self::Mysql(cfg) => {
                if cfg.host.trim().is_empty()
                    || cfg.database.trim().is_empty()
                    || cfg.user.trim().is_empty()
                    || cfg.port == 0
                    || [&cfg.host, &cfg.database, &cfg.user]
                        .iter()
                        .any(|field| field.contains('\0'))
                {
                    return Err(
                        "config.toml: MySQL requires host, nonzero port, database and user".into(),
                    );
                }
                if cfg
                    .ca_file
                    .as_ref()
                    .is_some_and(|path| cfg.tls == TlsMode::Disabled || !data_file(Path::new(path)))
                {
                    return Err("config.toml: database.ca_file requires TLS and a file inside the plugin data directory".into());
                }
                Ok(())
            }
            Self::Sqlite { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sit {
    #[serde(deserialize_with = "material_rules")]
    pub materials: Vec<MaterialRule>,
    pub bottom_part_only: bool,
    pub empty_hand_only: bool,
    pub max_distance: f64,
    pub default_enabled: bool,
}

/// A material or tag, optionally restricted to one concrete block state.
#[derive(Clone)]
pub struct MaterialRule {
    pub name: String,
    pub properties: Option<Vec<(String, String)>>,
    pub offset: f64,
}

/// Parses GSit's height suffix on either side of its bracketed block state.
fn parse_material_rule(mut text: String) -> Option<MaterialRule> {
    let mut offset = 0.0;
    if let Some(semi) = text.find(';') {
        let end = text[semi + 1..]
            .find('[')
            .map_or(text.len(), |n| semi + 1 + n);
        offset = text[semi + 1..end].trim().parse::<f64>().ok()?;
        if !offset.is_finite() {
            return None;
        }
        text.replace_range(semi..end, "");
    }
    let text = text.trim().to_ascii_lowercase();
    let (name, properties) = if let Some((name, state)) = text.split_once('[') {
        let state = state.strip_suffix(']')?;
        let mut properties = Vec::new();
        if !state.is_empty() {
            for pair in state.split(',') {
                let (key, value) = pair.split_once('=')?;
                let (key, value) = (key.trim(), value.trim());
                if key.is_empty() || value.is_empty() || properties.iter().any(|(k, _)| k == key) {
                    return None;
                }
                properties.push((key.to_owned(), value.to_owned()));
            }
        }
        (name, Some(properties))
    } else {
        (text.as_str(), None)
    };
    Some(MaterialRule {
        name: name.to_owned(),
        properties,
        offset,
    })
}

/// Loads each rule independently, like GSit's per-entry handling of invalid materials.
fn material_rules<'de, D: Deserializer<'de>>(input: D) -> Result<Vec<MaterialRule>, D::Error> {
    Ok(Vec::<String>::deserialize(input)?
        .into_iter()
        .filter_map(parse_material_rule)
        .collect())
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerSit {
    pub allow_sit: bool,
    pub max_stack: usize,
    pub sneak_ejects: bool,
    pub bottom_return: bool,
    pub empty_hand_only: bool,
    pub max_distance: f64,
    pub default_enabled: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pose {
    pub interact: bool,
    pub lay_rest: bool,
    pub lay_night_skip: bool,
    pub lay_snoring_sounds: bool,
    pub lay_snoring_night_only: bool,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Crawl {
    pub get_up_sneak: bool,
    pub corridor_crawl: bool,
    pub double_sneak: bool,
    pub double_sneak_pitch: f32,
    pub double_sneak_time_ms: u64,
    pub layers: u8,
    pub default_enabled: bool,
}

/// Creates the example on first use and parses a complete candidate before a live reload.
pub fn read(dir: &Path) -> Result<Config, String> {
    let path = dir.join("config.toml");
    let template = include_str!("../assets/config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::write(&path, template).map_err(|error| format!("{}: {error}", path.display()))?;
            template.to_owned()
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    // TOML's formatted errors quote source lines, which can contain the database password.
    let cfg: Config = toml::from_str(&contents).map_err(|error| {
        let offset = error
            .span()
            .map_or(0, |span| span.start)
            .min(contents.len());
        let line = 1 + contents.as_bytes()[..offset]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        format!("config.toml: invalid configuration near line {line}")
    })?;
    cfg.database.validate()?;
    if [cfg.sit.max_distance, cfg.player_sit.max_distance]
        .iter()
        .any(|n| !n.is_finite() || *n < 0.0)
        || !(-90.0..=90.0).contains(&cfg.crawl.double_sneak_pitch)
        || cfg.crawl.layers == 0
    {
        return Err("config.toml: invalid distance, double_sneak_pitch or layers".into());
    }
    Ok(cfg)
}
