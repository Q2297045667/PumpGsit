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

/// Connection settings for the player preference store.
#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Database {
    Sqlite {
        #[serde(default = "default_sqlite_path")]
        path: PathBuf,
    },
    Mysql(MySqlConfig),
}

/// Returns the default preference database path inside the plugin data directory.
fn default_sqlite_path() -> PathBuf {
    "data/data.db".into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Loads the packaged defaults as editable TOML so each case changes only the setting it exercises.
    fn defaults() -> toml::Table {
        include_str!("../assets/config.toml").parse().unwrap()
    }

    /// Reads an isolated file through the same configuration loader used on startup and reload.
    fn read_text(contents: &str) -> Result<Config, String> {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), contents).unwrap();
        read(dir.path())
    }

    /// SQLite uses the same file with an explicit path or when only its backend is set.
    #[test]
    fn sqlite_defaults_to_the_plugin_data_file() {
        let dir = tempdir().unwrap();
        let fresh = read(dir.path()).unwrap();
        assert!(dir.path().join("config.toml").is_file());

        let mut settings = defaults();
        settings["database"].as_table_mut().unwrap().remove("path");
        let without_path = read_text(&toml::to_string(&settings).unwrap()).unwrap();
        for config in [fresh, without_path] {
            assert!(
                matches!(config.database, Database::Sqlite { path } if path == Path::new("data/data.db"))
            );
        }
    }

    /// Configuration files must declare which preference backend they use.
    #[test]
    fn requires_database_section() {
        let mut settings = defaults();
        settings.remove("database");
        assert!(read_text(&toml::to_string(&settings).unwrap()).is_err());
    }

    /// Operators may nest SQLite files under the data directory but cannot select a parent or root path.
    #[test]
    fn sqlite_paths_are_relative_to_plugin_data() {
        let mut settings = defaults();
        for path in ["", ".", "../old.db", "data/../old.db", "/outside/data.db"] {
            settings["database"]["path"] = path.into();
            let result = read_text(&toml::to_string(&settings).unwrap());
            assert!(result.err().unwrap().contains("database.path"), "{path:?}");
        }
        settings["database"]["path"] = "legacy/data.db".into();
        let config = read_text(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(
            matches!(config.database, Database::Sqlite { path } if path == Path::new("legacy/data.db"))
        );
    }

    /// A migrated MySQL configuration defaults to port 3306 with server identity verification.
    #[test]
    fn mysql_defaults_and_ca_file_are_validated_together() {
        let mut settings = defaults();
        let mysql: toml::Table = r#"
            type = "mysql"
            host = "db.example"
            database = "gsit"
            user = "plugin"
            password = ""
            ca_file = "certs/ca.pem"
        "#
        .parse()
        .unwrap();
        settings["database"] = mysql.into();
        let config = read_text(&toml::to_string(&settings).unwrap()).unwrap();
        let Database::Mysql(connection) = config.database else {
            panic!("expected MySQL")
        };
        assert_eq!(connection.port, 3306);
        assert!(connection.tls == TlsMode::VerifyIdentity);
        assert_eq!(connection.ca_file.as_deref(), Some("certs/ca.pem"));

        for (mode, path) in [
            ("disabled", "certs/ca.pem"),
            ("verify_identity", "../ca.pem"),
        ] {
            let database = settings["database"].as_table_mut().unwrap();
            database.insert("tls".into(), mode.into());
            database["ca_file"] = path.into();
            let result = read_text(&toml::to_string(&settings).unwrap());
            assert!(
                result.err().unwrap().contains("database.ca_file"),
                "{mode} {path}"
            );
        }
    }

    /// Invalid rules are skipped individually and both accepted offset spellings preserve the same state.
    #[test]
    fn material_rules_preserve_offsets_and_skip_only_bad_entries() {
        let mut settings = defaults();
        settings["sit"]["materials"] = vec![
            "#SLABS",
            "oak_slab[type=]",
            "OAK_STAIRS;-0.125[Half=Bottom,Facing=NORTH]",
            "oak_stairs[half=bottom,half=top]",
            "oak_slab;NaN",
            "oak_stairs[half=bottom,facing=north];-0.125",
        ]
        .into();
        let config = read_text(&toml::to_string(&settings).unwrap()).unwrap();
        let [tag, prefix, suffix] = config.sit.materials.as_slice() else {
            panic!("expected only the tag and the two valid state rules")
        };
        assert_eq!(tag.name, "#slabs");
        assert!(tag.properties.is_none());
        assert_eq!(tag.offset, 0.0);
        let properties = vec![
            ("half".into(), "bottom".into()),
            ("facing".into(), "north".into()),
        ];
        for rule in [prefix, suffix] {
            assert_eq!(rule.name, "oak_stairs");
            assert_eq!(rule.properties.as_ref(), Some(&properties));
            assert_eq!(rule.offset, -0.125);
        }
    }

    /// Invalid gameplay bounds are rejected before a candidate configuration becomes active.
    #[test]
    fn distances_pitch_and_layers_reject_invalid_values() {
        for (section, key, value) in [
            ("sit", "max_distance", (-0.25).into()),
            ("player_sit", "max_distance", f64::INFINITY.into()),
            ("crawl", "double_sneak_pitch", 91.into()),
            ("crawl", "layers", 0.into()),
        ] {
            let mut settings = defaults();
            settings[section][key] = value;
            let result = read_text(&toml::to_string(&settings).unwrap());
            assert!(
                result
                    .err()
                    .unwrap()
                    .contains("invalid distance, double_sneak_pitch or layers"),
                "{section}.{key}"
            );
        }
    }

    /// A malformed password line reports its location without echoing its contents into server logs.
    #[test]
    fn toml_errors_omit_credential_values() {
        let result =
            read_text("[database]\ntype = \"mysql\"\npassword = 'sensitive-value' trailing");
        assert_eq!(
            result.err().unwrap(),
            "config.toml: invalid configuration near line 3"
        );
    }
}
