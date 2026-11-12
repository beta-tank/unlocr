//! Persisted GUI settings (provider mode + defaults).
//!
//! Backed by the SQLite store (`unlocr.db`, see `db.rs`): a single-row `settings`
//! table (id = 1) holds the Settings panel's state across restarts. Missing row or
//! a DB error falls back to `Settings::default()` so a first launch or a corrupt
//! store never wedges the app. Holds the local/remote provider mode, the remote
//! endpoint, and the engine-option defaults the Workspace controls seed from.
//!
//! `remoteApiKey` is stored in the OS credential manager (keychain/secret
//! service) via the `keyring` dep, NOT in the DB. The `settings` row holds only a
//! `__saved__` marker; `load_settings`/`save_settings` mask it to `••••••••` for
//! the UI. A legacy plaintext key found in the row is migrated into the keychain
//! on load, then cleared from the DB.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use unlocr::OcrOptions;

/// Persisted settings. camelCase on the wire so the JS side reads
/// `settings.remoteBaseUrl` etc. without a rename layer. Every field has a
/// default so an older/partial state still fills in (serde `default`), and a
/// missing DB row returns `Settings::default()` wholesale.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(flatten)]
    pub configuration: Configuration,
    /// "local" | "remote": which provider the Load button targets.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Remote OpenAI-compatible base URL (no trailing slash needed). New profiles
    /// start blank; migration preserves the exact saved value, including empty.
    #[serde(default = "default_remote_base_url")]
    pub remote_base_url: String,
    /// Optional bearer token for the remote endpoint. Plaintext (see module note).
    #[serde(default)]
    pub remote_api_key: String,
    /// Optional model name sent in the request body. Required by multi-model
    /// gateways (litellm/vLLM); a bare remote llama-server ignores it.
    #[serde(default)]
    pub remote_model: String,
    /// Default quant the Workspace + local Load use.
    #[serde(default = "default_quant")]
    pub default_quant: String,
    /// Optional explicit llama-server path (empty = resolve from PATH/Homebrew).
    #[serde(default)]
    pub llama_bin: String,
    #[serde(default = "default_dpi")]
    pub default_dpi: u32,
    #[serde(default = "default_max_tokens")]
    pub default_max_tokens: u32,
    #[serde(default = "default_prompt")]
    pub default_prompt: String,
    /// Drop the warm model after this many idle minutes to reclaim the GGUF RAM
    /// (~6-8 GB). 0 disables (model stays warm until explicit unload / app exit).
    /// The watcher in lib.rs reads this each tick; an in-flight run is protected by
    /// the model lock (try_lock fails while a run holds it), so it never unloads
    /// mid-run. Default 15.
    #[serde(default = "default_idle_unload_minutes")]
    pub idle_unload_minutes: u32,
    /// Last selected connection: llamacpp (local) or custom (remote). The v6
    /// migration maps the removed gpu/vllm/sglang presets to custom.
    #[serde(default = "default_engine_preset")]
    pub engine_preset: String,
    /// Startup-only DeepSeek-OCR vision-token budget (`--image-max-tokens`).
    /// `None` = model default (the field left blank).
    #[serde(default)]
    pub image_max_tokens: Option<u32>,
    /// llama-server `--chat-template` override. Empty = model default.
    #[serde(default)]
    pub chat_template: String,
    /// Per-run sampling repeat penalty. `None` = local-default/server-default
    /// (see `cmd_run/ocr.rs`'s injected 1.3 on the local backend).
    #[serde(default)]
    pub repeat_penalty: Option<f64>,
    /// DRY sampler strength. `None` = local default (1.0); 0 is a real,
    /// explicit "off" value distinct from unset.
    #[serde(default)]
    pub dry_multiplier: Option<f64>,
    /// DRY sampler growth-rate base. `None` = server default (1.75).
    #[serde(default)]
    pub dry_base: Option<f64>,
    /// Keep rasterized page PNGs after a run (workspace "Keep page images").
    #[serde(default)]
    pub keep_images: bool,
    /// Pages selector mode: "all" | "single" | "range".
    #[serde(default = "default_pages_mode")]
    pub pages_mode: String,
    /// Pages selector lower bound (single/range modes). `None` = unset.
    #[serde(default)]
    pub page_from: Option<u32>,
    /// Pages selector upper bound (range mode only). `None` = unset.
    #[serde(default)]
    pub page_to: Option<u32>,
    /// Custom local GGUF model path override (skips the managed download).
    /// Empty = no override.
    #[serde(default)]
    pub model_file: String,
    /// Custom local mmproj GGUF path override. Empty = no override.
    #[serde(default)]
    pub mmproj_file: String,
    /// Output mode: "single" | "pages" | "both".
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
    /// Sampling temperature. `None` = 0.0 (deterministic OCR, the historical
    /// hardcoded default). Unlike the DRY/repeat-penalty knobs, applies to both
    /// local and remote modes.
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Versioned profile metadata and controls, stored together as JSON. Credentials
/// deliberately remain in the existing keyring-backed field, never in profiles.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Configuration {
    pub configuration_version: u32,
    pub stock_model_profile: String,
    pub stock_model_profile_version: u32,
    pub compatibility_status: String,
    pub vision_mode: String,
    pub repetition_profile: String,
    pub result_format: String,
    pub dry_allowed_length: Option<u32>,
    pub dry_penalty_last_n: Option<i32>,
    pub anti_loop: bool,
    pub local_config: serde_json::Value,
    pub remote_config: serde_json::Value,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            configuration_version: 2,
            stock_model_profile: "atomicchat/unlimited-ocr-2".into(),
            stock_model_profile_version: 2,
            compatibility_status: "stock-v2".into(),
            vision_mode: "auto".into(),
            repetition_profile: "default".into(),
            result_format: "markdown".into(),
            dry_allowed_length: None,
            dry_penalty_last_n: None,
            anti_loop: false,
            local_config: serde_json::json!({}),
            remote_config: serde_json::json!({}),
        }
    }
}

/// Seed profiles once from legacy fields. Explicit empty connection values are
/// meaningful and are never replaced with guessed server/repository names.
fn migrate_configuration(s: &mut Settings) {
    if s.configuration.configuration_version >= 2 {
        return;
    }
    if matches!(s.engine_preset.as_str(), "gpu" | "vllm" | "sglang") || s.mode == "remote" {
        s.mode = "remote".into();
        s.engine_preset = "custom".into();
    } else {
        s.engine_preset = "llamacpp".into();
    }
    let custom = !s.model_file.is_empty() || !s.mmproj_file.is_empty();
    if !custom && s.chat_template == "deepseek-ocr" {
        s.chat_template.clear();
    }
    let c = &mut s.configuration;
    c.configuration_version = 2;
    c.compatibility_status = if custom {
        "custom-unverified"
    } else {
        "stock-v2"
    }
    .into();
    c.vision_mode = if s.image_max_tokens.is_some() {
        "expert"
    } else {
        "auto"
    }
    .into();
    c.repetition_profile = if s.repeat_penalty.is_some()
        || s.dry_multiplier.is_some()
        || s.dry_base.is_some()
        || c.anti_loop
        || c.dry_allowed_length.is_some()
        || c.dry_penalty_last_n.is_some()
    {
        "manual"
    } else {
        "default"
    }
    .into();
    c.local_config = serde_json::json!({
        "defaultQuant": s.default_quant, "modelFile": s.model_file,
        "mmprojFile": s.mmproj_file, "llamaBin": s.llama_bin,
        "visionMode": c.vision_mode, "imageMaxTokens": s.image_max_tokens,
        "chatTemplate": s.chat_template, "repetitionProfile": c.repetition_profile,
        "repeatPenalty": s.repeat_penalty, "dryMultiplier": s.dry_multiplier,
        "dryBase": s.dry_base, "dryAllowedLength": c.dry_allowed_length,
        "dryPenaltyLastN": c.dry_penalty_last_n, "antiLoop": c.anti_loop,
        "temperature": s.temperature, "defaultMaxTokens": s.default_max_tokens,
        "defaultPrompt": s.default_prompt
    });
    c.remote_config = serde_json::json!({
        "remoteBaseUrl": s.remote_base_url, "remoteModel": s.remote_model,
        "temperature": s.temperature, "defaultMaxTokens": s.default_max_tokens,
        "defaultPrompt": s.default_prompt, "repetitionProfile": "default"
    });
}

/// Called inside the schema transaction, so profile conversion and its version
/// marker commit atomically with the new columns.
pub(crate) fn migrate_profiles(conn: &Connection) -> Result<(), String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM settings WHERE id = 1)",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Ok(());
    }
    let mut s = fetch(conn)?;
    migrate_configuration(&mut s);
    persist(conn, &s)
}

fn default_mode() -> String {
    "local".to_string()
}
fn default_remote_base_url() -> String {
    String::new()
}
fn default_quant() -> String {
    OcrOptions::default().quant
}
fn default_dpi() -> u32 {
    OcrOptions::default().dpi
}
fn default_max_tokens() -> u32 {
    OcrOptions::default().max_tokens
}
// Empty by default: the per-run Prompt box is an optional override, and an empty box
// falls back to the selected Task preset (options.js). A non-empty persistent value here
// seeds the run box. NOT OcrOptions::default().prompt: that would pre-fill the box and
// read like mandatory boilerplate (Unlimited-OCR uses no system prompt).
fn default_prompt() -> String {
    String::new()
}
fn default_idle_unload_minutes() -> u32 {
    15
}
fn default_engine_preset() -> String {
    "llamacpp".to_string()
}
fn default_pages_mode() -> String {
    "all".to_string()
}
fn default_output_mode() -> String {
    "single".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            configuration: Configuration::default(),
            mode: default_mode(),
            remote_base_url: default_remote_base_url(),
            remote_api_key: String::new(),
            remote_model: String::new(),
            default_quant: default_quant(),
            llama_bin: String::new(),
            default_dpi: default_dpi(),
            default_max_tokens: default_max_tokens(),
            default_prompt: default_prompt(),
            idle_unload_minutes: default_idle_unload_minutes(),
            engine_preset: default_engine_preset(),
            image_max_tokens: None,
            chat_template: String::new(),
            repeat_penalty: None,
            dry_multiplier: None,
            dry_base: None,
            keep_images: false,
            pages_mode: default_pages_mode(),
            page_from: None,
            page_to: None,
            model_file: String::new(),
            mmproj_file: String::new(),
            output_mode: default_output_mode(),
            temperature: None,
        }
    }
}

// --- DB layer ---------------------------------------------------------------
//
// Pure functions over a `&Connection` (split from the `with_db` wrappers so tests
// drive them against an in-memory DB). The settings table is a singleton (id = 1).

/// Read the singleton settings row. A missing row (fresh install, cleared store)
/// yields `Settings::default()` rather than an error, mirroring the old
/// "missing file -> defaults" contract.
fn fetch(conn: &Connection) -> Result<Settings, String> {
    match conn.query_row(
        "SELECT mode, remote_base_url, remote_api_key, remote_model,
                default_quant, llama_bin, default_dpi, default_max_tokens,
                default_prompt, idle_unload_minutes,
                engine_preset, image_max_tokens, chat_template, repeat_penalty,
                dry_multiplier, dry_base, keep_images, pages_mode, page_from,
                page_to, model_file, mmproj_file, output_mode, temperature,
                configuration_json, dry_allowed_length, dry_penalty_last_n, anti_loop
         FROM settings WHERE id = 1",
        [],
        |row| {
            Ok(Settings {
                configuration: {
                    let json: String = row.get(24)?;
                    let mut c: Configuration = serde_json::from_str(&json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            24,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    c.dry_allowed_length = row.get(25)?;
                    c.dry_penalty_last_n = row.get(26)?;
                    c.anti_loop = row.get(27)?;
                    c
                },
                mode: row.get(0)?,
                remote_base_url: row.get(1)?,
                remote_api_key: row.get(2)?,
                remote_model: row.get(3)?,
                default_quant: row.get(4)?,
                llama_bin: row.get(5)?,
                default_dpi: row.get(6)?,
                default_max_tokens: row.get(7)?,
                default_prompt: row.get(8)?,
                idle_unload_minutes: row.get(9)?,
                engine_preset: row.get(10)?,
                image_max_tokens: row.get(11)?,
                chat_template: row.get(12)?,
                repeat_penalty: row.get(13)?,
                dry_multiplier: row.get(14)?,
                dry_base: row.get(15)?,
                keep_images: row.get(16)?,
                pages_mode: row.get(17)?,
                page_from: row.get(18)?,
                page_to: row.get(19)?,
                model_file: row.get(20)?,
                mmproj_file: row.get(21)?,
                output_mode: row.get(22)?,
                temperature: row.get(23)?,
            })
        },
    ) {
        Ok(s) => Ok(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Settings::default()),
        Err(e) => Err(format!("could not read settings: {e}")),
    }
}

/// Upsert the singleton settings row (INSERT OR REPLACE on id = 1).
fn persist(conn: &Connection, s: &Settings) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO settings
           (id, mode, remote_base_url, remote_api_key, remote_model,
            default_quant, llama_bin, default_dpi, default_max_tokens,
            default_prompt, idle_unload_minutes,
            engine_preset, image_max_tokens, chat_template, repeat_penalty,
            dry_multiplier, dry_base, keep_images, pages_mode, page_from,
            page_to, model_file, mmproj_file, output_mode, temperature,
            configuration_json, dry_allowed_length, dry_penalty_last_n, anti_loop)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24,
                 ?25, ?26, ?27, ?28)",
        rusqlite::params![
            s.mode,
            s.remote_base_url,
            s.remote_api_key,
            s.remote_model,
            s.default_quant,
            s.llama_bin,
            s.default_dpi,
            s.default_max_tokens,
            s.default_prompt,
            s.idle_unload_minutes,
            s.engine_preset,
            s.image_max_tokens,
            s.chat_template,
            s.repeat_penalty,
            s.dry_multiplier,
            s.dry_base,
            s.keep_images,
            s.pages_mode,
            s.page_from,
            s.page_to,
            s.model_file,
            s.mmproj_file,
            s.output_mode,
            s.temperature,
            serde_json::to_string(&s.configuration).map_err(|e| e.to_string())?,
            s.configuration.dry_allowed_length,
            s.configuration.dry_penalty_last_n,
            s.configuration.anti_loop,
        ],
    )
    .map_err(|e| format!("could not save settings: {e}"))?;
    Ok(())
}

// --- public accessors (the command-facing surface) --------------------------

/// Load settings, falling back to defaults on a missing row or a DB error.
pub fn load_settings() -> Settings {
    let mut s = crate::db::with_db(fetch).unwrap_or_else(|e| {
        eprintln!("[settings] load failed, using defaults: {e}");
        Settings::default()
    });

    // Check if there is an old plaintext key in the database
    if !s.remote_api_key.is_empty()
        && s.remote_api_key != "__saved__"
        && s.remote_api_key != "••••••••"
    {
        let key = s.remote_api_key.clone();
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            if entry.set_password(&key).is_ok() {
                // Clear plaintext from DB, mark as __saved__
                s.remote_api_key = "__saved__".to_string();
                if let Err(e) = crate::db::with_db(|c| persist(c, &s)) {
                    eprintln!("[settings] failed to clear plaintext api key from database: {e}");
                }
            }
        }
    }

    // Return a masked password to the UI if a key is saved, else empty
    if s.remote_api_key == "__saved__" {
        s.remote_api_key = "••••••••".to_string();
    } else {
        s.remote_api_key = String::new();
    }

    s
}

/// Persist settings (upsert the singleton row).
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let mut s = settings.clone();
    let key = std::mem::take(&mut s.remote_api_key); // Clear key from settings to save in DB as empty string or placeholder

    if key == "••••••••" {
        // Kept as is! No change to keyring, preserve the "__saved__" marker in DB
        s.remote_api_key = "__saved__".to_string();
    } else if key.is_empty() {
        // Deleted! Remove from keyring, clear marker in DB
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            let _ = entry.delete_password();
        }
        s.remote_api_key = String::new();
    } else {
        // Modified/new key! Save to keyring, set marker in DB
        if let Ok(entry) = keyring::Entry::new("unlocr", "remote_api_key") {
            entry
                .set_password(&key)
                .map_err(|e| format!("failed to save API key to OS credential manager: {e}"))?;
        } else {
            return Err("OS credential manager not available".to_string());
        }
        s.remote_api_key = "__saved__".to_string();
    }

    crate::db::with_db(|c| persist(c, &s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_db(preset: &str, custom: bool, key: &str) -> Connection {
        let conn = mem_db();
        conn.execute_batch(
            "ALTER TABLE settings DROP COLUMN configuration_json;
            ALTER TABLE jobs DROP COLUMN effective_config; PRAGMA user_version = 5;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO settings (id, mode, engine_preset, remote_base_url,
            remote_model, remote_api_key, default_quant, default_dpi, default_max_tokens,
            default_prompt, chat_template, model_file, mmproj_file, repeat_penalty,
            dry_multiplier, dry_allowed_length, dry_penalty_last_n, anti_loop)
            VALUES (1, 'remote', ?1, '', '', ?2, 'AD-Q4_K', 144, 8000, 'custom prompt',
            'deepseek-ocr', ?3, ?4, 1.3, 1.0, 2, -1, 1)",
            rusqlite::params![
                preset,
                key,
                if custom { "/custom.gguf" } else { "" },
                if custom { "/projector.gguf" } else { "" }
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn legacy_remote_presets_keep_empty_connection_and_key_mechanism() {
        for preset in ["gpu", "vllm", "sglang", "custom"] {
            for key in ["__saved__", "legacy-plaintext"] {
                let conn = legacy_db(preset, false, key);
                if preset != "custom" {
                    // Old engine selection is authoritative even for inconsistent
                    // rows whose coarse mode was left at its local default.
                    conn.execute("UPDATE settings SET mode='local'", [])
                        .unwrap();
                }
                crate::db::init_conn(&conn).unwrap();
                let s = fetch(&conn).unwrap();
                assert_eq!(s.engine_preset, "custom");
                assert_eq!(s.mode, "remote");
                assert_eq!(s.remote_base_url, "");
                assert_eq!(s.remote_model, "");
                assert_eq!(s.remote_api_key, key);
                assert_eq!(s.configuration.remote_config["remoteBaseUrl"], "");
                assert!(!s.configuration.remote_config.to_string().contains(key));
                assert_eq!(s.default_dpi, 144);
                assert_eq!(s.repeat_penalty, Some(1.3));
                assert_eq!(s.dry_multiplier, Some(1.0));
                assert_eq!(s.configuration.repetition_profile, "manual");
                assert_eq!(s.configuration.dry_allowed_length, Some(2));
            }
        }
    }

    #[test]
    fn stock_template_migrates_but_custom_gguf_overrides_survive() {
        for custom in [false, true] {
            let conn = legacy_db("llamacpp", custom, "__saved__");
            conn.execute(
                "UPDATE settings SET mode='local', remote_base_url='https://server/v1',
                remote_model='served-alias'",
                [],
            )
            .unwrap();
            crate::db::init_conn(&conn).unwrap();
            crate::db::init_conn(&conn).unwrap(); // repeated startup is lossless
            let s = fetch(&conn).unwrap();
            assert_eq!(s.engine_preset, "llamacpp");
            assert_eq!(s.default_quant, "AD-Q4_K");
            assert_eq!(s.chat_template, if custom { "deepseek-ocr" } else { "" });
            assert_eq!(
                s.configuration.compatibility_status,
                if custom {
                    "custom-unverified"
                } else {
                    "stock-v2"
                }
            );
            assert_eq!(s.configuration.stock_model_profile_version, 2);
            assert_eq!(
                s.configuration.local_config["chatTemplate"],
                s.chat_template
            );
            assert_eq!(s.configuration.remote_config["remoteModel"], "served-alias");
            assert_eq!(
                s.configuration.remote_config["remoteBaseUrl"],
                "https://server/v1"
            );
            if custom {
                assert_eq!(s.model_file, "/custom.gguf");
                assert_eq!(s.mmproj_file, "/projector.gguf");
            }
        }
    }

    #[test]
    fn configuration_migration_rolls_back_partial_schema_changes() {
        let conn = legacy_db("gpu", false, "__saved__");
        conn.execute_batch("ALTER TABLE jobs ADD COLUMN effective_config TEXT;")
            .unwrap();
        assert!(crate::db::init_conn(&conn).is_err());
        assert!(conn
            .prepare("SELECT configuration_json FROM settings")
            .is_err());
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5);
        conn.execute_batch("ALTER TABLE jobs DROP COLUMN effective_config;")
            .unwrap();
        crate::db::init_conn(&conn).unwrap();
        assert_eq!(fetch(&conn).unwrap().engine_preset, "custom");
    }

    #[test]
    fn new_controls_and_separate_profiles_roundtrip() {
        let conn = mem_db();
        let mut s = Settings::default();
        s.configuration.vision_mode = "base".into();
        s.configuration.repetition_profile = "manual".into();
        s.configuration.result_format = "raw".into();
        s.configuration.dry_allowed_length = Some(4);
        s.configuration.dry_penalty_last_n = Some(-1);
        s.configuration.local_config = serde_json::json!({"defaultMaxTokens": 8000});
        s.configuration.remote_config = serde_json::json!({"defaultMaxTokens": 2000});
        let decoded: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        persist(&conn, &decoded).unwrap();
        assert_eq!(
            serde_json::to_value(fetch(&conn).unwrap()).unwrap(),
            serde_json::to_value(s).unwrap()
        );
    }

    /// Fresh in-memory DB with the schema applied.
    fn mem_db() -> Connection {
        crate::db::mem_db()
    }

    /// A fresh store has no settings row -> fetch returns the current defaults.
    #[test]
    fn missing_row_returns_default() {
        let conn = mem_db();
        let s = fetch(&conn).unwrap();
        assert_eq!(s.mode, "local");
        assert_eq!(s.remote_base_url, default_remote_base_url());
        assert_eq!(s.default_quant, OcrOptions::default().quant);
        assert_eq!(s.default_dpi, OcrOptions::default().dpi);
        assert_eq!(s.idle_unload_minutes, 15);
        assert!(s.remote_api_key.is_empty());
        assert_eq!(s.engine_preset, "llamacpp");
        assert_eq!(s.image_max_tokens, None);
        assert_eq!(s.repeat_penalty, None);
        assert!(!s.keep_images);
        assert_eq!(s.pages_mode, "all");
        assert_eq!(s.output_mode, "single");
    }

    /// Write non-defaults, read them back: every field round-trips through the row.
    #[test]
    fn upsert_then_get_roundtrips() {
        let conn = mem_db();
        let s = Settings {
            configuration: Configuration::default(),
            mode: "remote".into(),
            remote_base_url: "http://gpu:8000".into(),
            remote_api_key: "sk-secret".into(),
            remote_model: "baidu/Unlimited-OCR".into(),
            default_quant: "Q4_K_M".into(),
            llama_bin: "/opt/llama-server".into(),
            default_dpi: 300,
            default_max_tokens: 8192,
            default_prompt: "<|x|>".into(),
            idle_unload_minutes: 5,
            engine_preset: "vllm".into(),
            image_max_tokens: Some(1024),
            chat_template: "deepseek-ocr".into(),
            repeat_penalty: Some(1.15),
            dry_multiplier: Some(1.0),
            dry_base: Some(1.75),
            keep_images: true,
            pages_mode: "range".into(),
            page_from: Some(5),
            page_to: Some(9),
            model_file: "/models/custom.gguf".into(),
            mmproj_file: "/models/mmproj.gguf".into(),
            output_mode: "both".into(),
            temperature: Some(0.7),
        };
        persist(&conn, &s).unwrap();
        let got = fetch(&conn).unwrap();
        assert_eq!(got.mode, "remote");
        assert_eq!(got.remote_base_url, "http://gpu:8000");
        assert_eq!(got.remote_api_key, "sk-secret");
        assert_eq!(got.remote_model, "baidu/Unlimited-OCR");
        assert_eq!(got.default_quant, "Q4_K_M");
        assert_eq!(got.llama_bin, "/opt/llama-server");
        assert_eq!(got.default_dpi, 300);
        assert_eq!(got.default_max_tokens, 8192);
        assert_eq!(got.default_prompt, "<|x|>");
        assert_eq!(got.idle_unload_minutes, 5);
        assert_eq!(got.engine_preset, "vllm");
        assert_eq!(got.image_max_tokens, Some(1024));
        assert_eq!(got.chat_template, "deepseek-ocr");
        assert_eq!(got.repeat_penalty, Some(1.15));
        assert_eq!(got.dry_multiplier, Some(1.0));
        assert_eq!(got.dry_base, Some(1.75));
        assert!(got.keep_images);
        assert_eq!(got.pages_mode, "range");
        assert_eq!(got.page_from, Some(5));
        assert_eq!(got.page_to, Some(9));
        assert_eq!(got.model_file, "/models/custom.gguf");
        assert_eq!(got.mmproj_file, "/models/mmproj.gguf");
        assert_eq!(got.output_mode, "both");
        assert_eq!(got.temperature, Some(0.7));
    }

    /// Nullable numeric fields round-trip both a `Some(x)` and a `None` case (the
    /// established "blank means unset" semantic from options.js).
    #[test]
    fn nullable_numeric_fields_roundtrip_some_and_none() {
        let conn = mem_db();
        let s = Settings {
            image_max_tokens: Some(2048),
            repeat_penalty: Some(1.2),
            dry_multiplier: Some(0.0), // explicit "off", distinct from unset
            dry_base: None,
            page_from: Some(3),
            page_to: None,
            temperature: Some(0.0), // explicit "off"/deterministic, distinct from unset
            ..Settings::default()
        };
        persist(&conn, &s).unwrap();
        let got = fetch(&conn).unwrap();
        assert_eq!(got.image_max_tokens, Some(2048));
        assert_eq!(got.repeat_penalty, Some(1.2));
        assert_eq!(got.dry_multiplier, Some(0.0));
        assert_eq!(got.dry_base, None);
        assert_eq!(got.page_from, Some(3));
        assert_eq!(got.page_to, None);
        assert_eq!(got.temperature, Some(0.0));
    }

    /// The settings table is a singleton: a second save replaces, never adds a row.
    #[test]
    fn upsert_is_singleton() {
        let conn = mem_db();
        persist(&conn, &Settings::default()).unwrap();
        persist(&conn, &Settings::default()).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1, "settings must hold exactly one row (id = 1)");
    }

    /// A new remote connection must be configured explicitly.
    #[test]
    fn default_remote_url_is_blank() {
        assert!(Settings::default().remote_base_url.is_empty());
    }

    /// camelCase on the wire (regression guard for the serde rename the frontend
    /// depends on; the command return serializes this struct directly).
    #[test]
    fn serializes_camel_case() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains("\"remoteBaseUrl\""));
        assert!(json.contains("\"defaultMaxTokens\""));
        assert!(json.contains("\"idleUnloadMinutes\""));
        assert!(json.contains("\"enginePreset\""));
        assert!(json.contains("\"imageMaxTokens\""));
        assert!(json.contains("\"repeatPenalty\""));
        assert!(json.contains("\"dryMultiplier\""));
        assert!(json.contains("\"pagesMode\""));
        assert!(json.contains("\"outputMode\""));
        assert!(json.contains("\"temperature\""));
        assert!(!json.contains("\"remote_base_url\""));
    }
}
