//! Editable GSit language files and Pumpkin's namespaced translation and text APIs.
use crate::bindings::pumpkin::plugin::{common::Locale, i18n, text::TextComponent};
use crate::state::State;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

type Messages = BTreeMap<String, String>;
static LOCALES: State<Vec<String>> = State::new(Vec::new());

macro_rules! languages {
    ($($code:literal),+ $(,)?) => { &[$(($code, include_str!(concat!("../assets/lang/", $code, ".yml")))),+] };
}

const BUNDLED: &[(&str, &str)] = languages!(
    "cs_cz", "de_de", "en_us", "es_es", "fi_fi", "fr_fr", "id_id", "it_it", "ja_jp", "pl_pl",
    "pt_br", "ru_ru", "sk_sk", "sv_se", "tr_tr", "tt_ru", "uk_ua", "zh_cn", "zh_tw",
);

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Catalog {
    plugin: Messages,
    messages: Messages,
    #[serde(rename = "PumpGsit")]
    extensions: Messages,
}

impl Catalog {
    /// Collects upstream message keys and the additional messages for the Pumpkin platform.
    fn flatten(self) -> Messages {
        let mut messages = self.plugin;
        messages.extend(self.messages);
        messages.extend(self.extensions);
        messages
    }
}

/// Normalizes the two locale spellings used in Minecraft settings and language filenames.
fn code(name: &str) -> String {
    name.to_ascii_lowercase().replace('-', "_")
}

/// Loads all assets before publishing new translations, preserving existing editable files.
pub fn read(data: &Path) -> Result<BTreeMap<String, Messages>, String> {
    let dir = data.join("lang");
    let io_error = |e: io::Error| format!("{}: {e}", dir.display());
    fs::create_dir_all(&dir).map_err(io_error)?;
    let mut catalogs = BTreeMap::new();
    for &(locale, bundled) in BUNDLED {
        let path = dir.join(format!("{locale}.yml"));
        if !path.try_exists().map_err(io_error)? {
            fs::write(&path, bundled).map_err(io_error)?;
        }
        catalogs.insert(locale.to_owned(), parse(locale, bundled)?);
    }
    for entry in fs::read_dir(&dir).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path.extension().is_none_or(|ext| ext != "yml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let name = code(name);
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let custom = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        catalogs
            .entry(name)
            .or_default()
            .extend(parse(&path.display().to_string(), &custom)?);
    }
    Ok(catalogs)
}

/// Reports malformed administrator-supplied translations with their source filename.
fn parse(name: &str, yaml: &str) -> Result<Messages, String> {
    serde_yaml_ng::from_str::<Catalog>(yaml)
        .map(Catalog::flatten)
        .map_err(|e| format!("{name}: {e}"))
}

/// Registers complete message tables using the configured language as the per-key fallback.
pub fn install(catalogs: BTreeMap<String, Messages>, configured: &str) {
    let mut defaults = catalogs["en_us"].clone();
    if let Some(selected) = catalogs.get(&code(configured)) {
        defaults.extend(selected.clone());
    }
    let locales = catalogs.keys().cloned().collect();
    for (locale, messages) in catalogs {
        let mut table = defaults.clone();
        table.extend(messages);
        let prefix = table["plugin-prefix"].clone();
        for value in table.values_mut() {
            *value = value.replace("[P]", &prefix);
        }
        // This host maps several WIT locale variants to EnUs. Each table has its own namespace.
        i18n::load_translations(
            &format!("pumpgsit.{locale}"),
            &serde_json::to_string(&table).unwrap(),
            Locale::EnUs,
        );
    }
    LOCALES.set(locales);
}

/// Chooses the client's language, the configured server language, then English, in that order.
pub fn message(client: Option<String>, configured: &str, key: &str) -> TextComponent {
    let locale = LOCALES.with_borrow(|loaded| {
        client
            .as_deref()
            .into_iter()
            .chain([configured])
            .map(code)
            .find(|lang| loaded.contains(lang))
            .unwrap_or_else(|| "en_us".into())
    });
    let key = if key == "db" { "plugin-data" } else { key };
    let text = i18n::translate(&format!("pumpgsit.{locale}:{key}"), Locale::EnUs);
    let styled = TextComponent::from_legacy_string_with_code(&text, '&');
    if !text.contains("<lang:") {
        return styled;
    }
    // Preserve colors on the vanilla translation inside GSit's action-bar templates.
    let Ok(mut parsed) = serde_json::from_str(&styled.to_json()) else {
        return styled;
    };
    translate_tags(&mut parsed);
    TextComponent::from_json(&parsed.to_string()).unwrap_or(styled)
}

/// Turns a styled text run containing MiniMessage language tags into translatable children.
fn translate_tags(node: &mut Value) {
    if let Some(children) = node.get_mut("extra").and_then(Value::as_array_mut) {
        for child in children {
            translate_tags(child);
        }
    }
    let Some(mut rest) = node.get("text").and_then(Value::as_str) else {
        return;
    };
    let mut parts = Vec::new();
    while let Some((before, tag)) = rest.split_once("<lang:") {
        let Some((key, after)) = tag.split_once('>') else {
            break;
        };
        if !before.is_empty() {
            parts.push(json!({"text": before}));
        }
        parts.push(json!({"translate": key}));
        rest = after;
    }
    if !parts.is_empty() {
        if !rest.is_empty() {
            parts.push(json!({"text": rest}));
        }
        node["text"] = json!("");
        if let Some(old) = node["extra"].as_array_mut() {
            parts.append(old);
        }
        node["extra"] = Value::Array(parts);
    }
}
