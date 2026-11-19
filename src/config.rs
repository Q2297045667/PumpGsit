//! The on-disk TOML mirrors the upstream gameplay settings.
use serde::Deserialize;
use std::{fs, io::ErrorKind, path::Path};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
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
    pub material_blacklist: Vec<String>,
    pub command_blacklist: Vec<String>,
    pub sit: Sit,
    pub player_sit: PlayerSit,
    pub pose: Pose,
    pub crawl: Crawl,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sit {
    pub materials: Vec<String>,
    pub bottom_part_only: bool,
    pub empty_hand_only: bool,
    pub max_distance: f64,
    pub default_enabled: bool,
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
    let template = include_str!("../config.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::write(&path, template).map_err(|error| format!("{}: {error}", path.display()))?;
            template.to_owned()
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let cfg: Config = toml::from_str(&contents).map_err(|error| format!("config.toml: {error}"))?;
    if [cfg.sit.max_distance, cfg.player_sit.max_distance]
        .iter()
        .any(|n| !n.is_finite() || *n < 0.0)
        || !(-90.0..=90.0).contains(&cfg.crawl.double_sneak_pitch)
        || cfg.crawl.layers == 0
    {
        return Err("config.toml: invalid distance, double_sneak_pitch or layers".into());
    }
    for entry in &cfg.sit.materials {
        if let Some((_, offset)) = entry.rsplit_once(';')
            && !offset.trim().parse::<f64>().is_ok_and(f64::is_finite)
        {
            return Err(format!("config.toml: invalid seat height in {entry}"));
        }
    }
    Ok(cfg)
}
