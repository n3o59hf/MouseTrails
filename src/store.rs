//! Durable storage: a root app config, per-theme effect profiles, and theme
//! definitions, all under the app data directory:
//!
//! ```text
//! %APPDATA%\MouseTrails\
//!   config.json            root app config (AppConfig)
//!   config.json.bak        pre-migration backup of the legacy flat config
//!   themes\<id>\
//!     theme.json           theme definition (customs only; built-ins live in
//!                          the binary, but a file here overrides them)
//!     main.json            the preset's primary profile: a full
//!                          EffectSettings document
//!     <name>.json          named profiles
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use crate::settings::{BubbleCfg, LimitsCfg, RippleCfg, SparkleCfg, TrailCfg};

pub const CONFIG_VERSION: u32 = 2;

/// The profile selected for a theme when nothing else is recorded, and the
/// name of every preset's primary profile.
pub const DEFAULT_PROFILE: &str = "main";

/// Root config: which theme is active, which profile each theme uses, and
/// process-wide knobs that don't describe how the effects look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub theme: String,
    /// Theme id → active profile name for that theme.
    pub profiles: BTreeMap<String, String>,
    pub render_fps: u32,
    /// Whether the "how to find me in the tray" hint was shown after the
    /// user closed the settings window for the first time.
    pub tray_hint_shown: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            theme: "default".into(),
            profiles: BTreeMap::new(),
            render_fps: 60,
            tray_hint_shown: false,
        }
    }
}

/// The effect-tuning half of the old flat config — what a theme profile
/// stores. One document per profile file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectSettings {
    pub trail: TrailCfg,
    pub bubbles: BubbleCfg,
    pub sparkles: SparkleCfg,
    pub ripples: RippleCfg,
    /// Population caps — the performance half of the profile.
    pub limits: LimitsCfg,
}

/// A theme definition: which effect sections the theme restyles. Sections
/// left as `None` are not themed — switching keeps whatever the user had.
/// Definitions are starting values only; the settings UI uses one global
/// slider range per knob.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeDef {
    /// Display name; the directory name is the stable id.
    pub name: String,
    pub description: String,
    pub trail: Option<TrailCfg>,
    pub bubbles: Option<BubbleCfg>,
    pub sparkles: Option<SparkleCfg>,
    pub ripples: Option<RippleCfg>,
    /// Population caps for looks that need denser particle counts.
    pub limits: Option<LimitsCfg>,
}

/// The legacy flat config that everything before themes used: effect
/// sections and app knobs in one document. Only exists to be migrated.
#[derive(Deserialize)]
#[serde(default)]
struct LegacySettings {
    trail: TrailCfg,
    bubbles: BubbleCfg,
    sparkles: SparkleCfg,
    ripples: RippleCfg,
    render_fps: u32,
    tray_hint_shown: bool,
}

impl Default for LegacySettings {
    fn default() -> Self {
        let d = EffectSettings::default();
        Self {
            trail: d.trail,
            bubbles: d.bubbles,
            sparkles: d.sparkles,
            ripples: d.ripples,
            render_fps: 60,
            tray_hint_shown: false,
        }
    }
}

/// App data directory: `%APPDATA%\MouseTrails`, with a temp-dir fallback for
/// exotic environments.
pub fn config_dir() -> PathBuf {
    std::env::var("APPDATA")
        .map(|d| PathBuf::from(d).join("MouseTrails"))
        .unwrap_or_else(|_| std::env::temp_dir().join("MouseTrails"))
}

/// Windows reserved device names — unusable as file or folder names.
const RESERVED_IDS: [&str; 22] = [
    "con", "prn", "aux", "nul",
    "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Maximum length of a profile or theme id (used as a file stem).
pub const MAX_ID_LEN: usize = 40;

/// Turn a user-typed name into a safe file-stem id: lowercased, safe
/// characters only, trimmed, length-capped. Returns `None` when nothing
/// usable remains or the id is reserved — Windows device names, or
/// `theme`, which would collide with a theme's `theme.json` definition
/// file. Non-alphanumeric characters (including path separators and dots)
/// become `-`, so the result can never traverse paths.
pub fn sanitize_id(name: &str) -> Option<String> {
    let mut t: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    t = t.trim_matches('-').to_string();
    if t.len() > MAX_ID_LEN {
        t = t[..MAX_ID_LEN].trim_matches('-').to_string();
    }
    if t.is_empty() || t == "theme" || RESERVED_IDS.contains(&t.as_str()) {
        return None;
    }
    Some(t)
}

/// True when any root config exists yet (first-launch detection).
pub fn config_exists() -> bool {
    config_dir().join("config.json").exists()
}

#[derive(Clone, Debug)]
pub struct Store {
    base: PathBuf,
}

impl Store {
    pub fn from_default() -> Self {
        Self { base: config_dir() }
    }

    /// Test/temp-directory constructor (the app itself always uses
    /// [`Store::from_default`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    fn app_path(&self) -> PathBuf {
        self.base.join("config.json")
    }

    fn theme_dir(&self, theme: &str) -> PathBuf {
        self.base.join("themes").join(theme)
    }

    fn profile_path(&self, theme: &str, profile: &str) -> PathBuf {
        self.theme_dir(theme).join(format!("{}.json", profile))
    }

    fn theme_def_path(&self, theme: &str) -> PathBuf {
        self.theme_dir(theme).join("theme.json")
    }

    // -- root app config ----------------------------------------------------

    /// Load the root config, migrating a legacy flat config on the way. Never
    /// fails: unreadable or corrupt state falls back to defaults.
    pub fn load_app(&self) -> AppConfig {
        self.migrate();
        self.read_app().unwrap_or_default()
    }

    fn read_app(&self) -> Option<AppConfig> {
        let text = std::fs::read_to_string(self.app_path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_app(&self, app: &AppConfig) {
        write_json(&self.app_path(), app);
    }

    /// A document counts as already-migrated only when it carries a v2
    /// marker key. Parsing as `AppConfig` is not enough: a legacy document
    /// shares `render_fps`/`tray_hint_shown` and would parse fine with its
    /// effect sections silently ignored.
    fn is_migrated(text: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .map(|v| {
                v.get("version").is_some()
                    || v.get("theme").is_some()
                    || v.get("profiles").is_some()
            })
            .unwrap_or(false)
    }

    /// One-time conversions, run on every load (each step idempotent):
    /// legacy flat config -> v2 layout, then the primary profile rename
    /// "default" -> "main".
    fn migrate(&self) {
        let app_path = self.app_path();
        let text = match std::fs::read_to_string(&app_path) {
            Ok(t) => t,
            Err(_) => {
                // Fresh install: put a v2 config on disk from the start.
                self.save_app(&AppConfig::default());
                return;
            }
        };
        if Self::is_migrated(&text) {
            self.rename_default_profiles();
            if let Ok(mut app) = serde_json::from_str::<AppConfig>(&text) {
                let mut changed = false;
                for v in app.profiles.values_mut() {
                    if v == "default" {
                        *v = "main".into();
                        changed = true;
                    }
                }
                if changed {
                    self.save_app(&app);
                }
            }
            return; // already the new format
        }
        // Legacy (or corrupt) document: back it up, then split it.
        let _ = std::fs::copy(&app_path, self.base.join("config.json.bak"));
        let legacy: LegacySettings = serde_json::from_str(&text).unwrap_or_default();
        self.save_profile(
            "default",
            DEFAULT_PROFILE,
            &EffectSettings {
                trail: legacy.trail,
                bubbles: legacy.bubbles,
                sparkles: legacy.sparkles,
                ripples: legacy.ripples,
                ..Default::default()
            },
        );
        self.save_app(&AppConfig {
            version: CONFIG_VERSION,
            theme: "default".into(),
            profiles: BTreeMap::new(),
            render_fps: legacy.render_fps,
            tray_hint_shown: legacy.tray_hint_shown,
        });
    }

    /// Every theme id the store can see: folders under `themes\` plus the
    /// embedded built-ins, which need no folder until the user materializes
    /// them.
    pub fn theme_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = crate::themes::BUILTINS
            .iter()
            .map(|b| b.id.to_string())
            .collect();
        for id in self.list_themes() {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    /// Rename every preset's `default.json` profile to `main.json`. The
    /// primary profile used to share its name with the default preset,
    /// which read confusingly once profiles became the selector. Never
    /// clobbers an existing `main.json`.
    fn rename_default_profiles(&self) {
        for id in self.theme_ids() {
            let old = self.profile_path(&id, "default");
            let new = self.profile_path(&id, "main");
            if old.exists() && !new.exists() {
                let _ = std::fs::rename(&old, &new);
            }
        }
    }

    /// Drop `profiles` entries that no longer point at a real preset and
    /// profile: deleted presets, deleted or renamed profiles, and themes
    /// removed by hand. Without this the root config keeps growing stale
    /// keys, and re-creating a preset under a previously-used id would
    /// silently resume a profile name the user has since deleted.
    ///
    /// Profile names are compared case-insensitively, like NTFS.
    pub fn prune_app_profiles(&self, app: &mut AppConfig) -> bool {
        let before = app.profiles.len();
        let ids = self.theme_ids();
        app.profiles.retain(|theme, profile| {
            ids.iter().any(|t| t == theme) && self.profile_exists(theme, profile)
        });
        app.profiles.len() != before
    }

    // -- effect profiles ----------------------------------------------------

    pub fn load_profile(&self, theme: &str, profile: &str) -> Option<EffectSettings> {
        let text = std::fs::read_to_string(self.profile_path(theme, profile)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_profile(&self, theme: &str, profile: &str, es: &EffectSettings) {
        write_json(&self.profile_path(theme, profile), es);
    }

    /// Load a profile for use, preserving an unreadable one first.
    ///
    /// `None` means "no usable profile file" — either it's missing (first
    /// use) or it exists but doesn't parse. In the latter case the caller is
    /// about to overwrite it, so keep a `.corrupt` copy: a truncated write
    /// must never quietly eat the user's tuning. Every path that
    /// materializes a missing profile goes through here rather than
    /// `load_profile`, so the backup can't be forgotten.
    pub fn load_profile_for_use(&self, theme: &str, profile: &str) -> Option<EffectSettings> {
        let es = self.load_profile(theme, profile);
        if es.is_none() {
            self.back_up_corrupt_profile(theme, profile);
        }
        es
    }

    /// Write `es` only when the profile file doesn't exist yet — used to
    /// materialize a theme's first profile without clobbering user edits.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ensure_profile(&self, theme: &str, profile: &str, es: &EffectSettings) {
        let path = self.profile_path(theme, profile);
        if !path.exists() {
            write_json(&path, es);
        }
    }

    /// Profile names present for a theme (sorted, without extension). The
    /// theme definition file is not a profile.
    pub fn list_profiles(&self, theme: &str) -> Vec<String> {
        list_stems(&self.theme_dir(theme), "theme.json")
    }

    /// Case-insensitive profile existence — NTFS compares case-insensitively
    /// by default, so "Cozy" on disk must block creating "cozy".
    pub fn profile_exists(&self, theme: &str, id: &str) -> bool {
        self.list_profiles(theme).iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    /// Case-insensitive custom-theme-folder existence.
    pub fn theme_exists(&self, id: &str) -> bool {
        self.list_themes().iter().any(|t| t.eq_ignore_ascii_case(id))
    }

    /// Delete a profile file. Returns whether something was removed. A
    /// preset may end up with zero profiles — it stays available as a base
    /// for new ones.
    pub fn delete_profile(&self, theme: &str, profile: &str) -> bool {
        std::fs::remove_file(self.profile_path(theme, profile)).is_ok()
    }

    /// When a profile file exists but won't parse, rename it to
    /// `<profile>.json.corrupt` so replacing it doesn't destroy the user's
    /// tuning — the same courtesy the legacy-config migration extends to
    /// config.json. Missing files are left alone.
    pub fn back_up_corrupt_profile(&self, theme: &str, profile: &str) {
        let path = self.profile_path(theme, profile);
        if path.is_file() {
            let bak = path.with_extension("json.corrupt");
            let _ = std::fs::rename(&path, &bak);
        }
    }

    // -- theme definitions --------------------------------------------------

    /// The custom theme definition stored on disk, if any. Built-in themes
    /// have their definition in the binary; a file here overrides it.
    pub fn load_theme_def(&self, theme: &str) -> Option<ThemeDef> {
        let text = std::fs::read_to_string(self.theme_def_path(theme)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_theme_def(&self, theme: &str, def: &ThemeDef) {
        write_json(&self.theme_def_path(theme), def);
    }

    /// Theme ids with a folder under `themes\` (sorted).
    pub fn list_themes(&self) -> Vec<String> {
        let dir = self.base.join("themes");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }

    /// Existence fingerprint of the themes tree: every preset folder with
    /// its sorted profile file names. File *contents* deliberately don't
    /// count — a change here means a folder or profile file was added or
    /// removed by hand (Explorer, another editor), which is what the
    /// settings UI watches to refresh its gallery.
    pub fn themes_fingerprint(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for theme in self.list_themes() {
            parts.push(format!("{}:{}", theme, self.list_profiles(&theme).join(",")));
        }
        parts.join("|")
    }

    /// Remove a custom theme folder (definition + profiles). Not reachable
    /// from the settings UI yet — custom presets are removed by deleting
    /// their folder by hand — but part of the store's tested API.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn delete_theme(&self, theme: &str) -> bool {
        std::fs::remove_dir_all(self.theme_dir(theme)).is_ok()
    }
}

/// Serialize to a temp file next to `path`, then rename over it — a crash
/// or power loss mid-write must not leave a truncated config. The rename
/// replaces atomically on the same volume. Each call uses its own temp name
/// so two threads writing the same path can't interleave into one temp.
fn write_json<T: serde::Serialize>(path: &Path, value: &T) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(value) {
        static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp{}", n));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Stems of the .json files directly inside `dir`, sorted, excluding
/// `exclude` (compared case-insensitively like the NTFS default).
fn list_stems(dir: &Path, exclude: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().map(|x| x == "json").unwrap_or(false))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|s| !s.eq_ignore_ascii_case(exclude.trim_end_matches(".json")))
        .collect();
    stems.sort();
    stems
}

/// State shared between the overlay thread (main) and the settings UI thread.
pub struct SharedSettings {
    pub store: Store,
    /// Effect settings of the active theme profile; the overlay re-reads
    /// this every frame.
    pub effects: RwLock<EffectSettings>,
    /// Root app config: active theme, per-theme profile selection, render
    /// FPS, first-run hints.
    pub app: RwLock<AppConfig>,
    /// Self-update status, written by the updater's check threads and read by
    /// the tray menu and settings window.
    pub update: Mutex<crate::updater::UpdateState>,
    /// HWND of the overlay window (as isize), set once at creation. The
    /// settings thread uses it to ask the overlay to exit during updates.
    pub overlay_hwnd: std::sync::atomic::AtomicIsize,
}

impl SharedSettings {
    /// Name of the profile active for `theme` ("default" unless recorded).
    pub fn active_profile(&self, theme: &str) -> String {
        let app = self.app.read().unwrap();
        active_profile_in(&app, theme)
    }

    /// Persist the current effect settings into the active theme's active
    /// profile file. Effects and app are snapshotted together (both read
    /// locks held, effects first — the same order switch actions take their
    /// write locks in), so a concurrent profile switch can't land between
    /// the two reads and persist new effects into the old profile.
    pub fn save_effects(&self) {
        let es = self.effects.read().unwrap().clone();
        let app = self.app.read().unwrap();
        let theme = app.theme.clone();
        let profile = active_profile_in(&app, &theme);
        drop(app);
        self.store.save_profile(&theme, &profile, &es);
    }

    /// Persist the current effects only when they differ from what is already
    /// on disk. Safe to call at shutdown or from any thread: a no-op in the
    /// common case where the settings window already wrote them.
    pub fn flush_pending_effects(&self) {
        let stored = {
            let app = self.app.read().unwrap();
            let theme = app.theme.clone();
            let profile = active_profile_in(&app, &theme);
            (theme, profile)
        };
        let es = self.effects.read().unwrap().clone();
        if self.store.load_profile(&stored.0, &stored.1).as_ref() == Some(&es) {
            return;
        }
        self.store.save_profile(&stored.0, &stored.1, &es);
    }

    pub fn tray_hint_shown(&self) -> bool {
        self.app.read().unwrap().tray_hint_shown
    }

    pub fn set_tray_hint_shown(&self) {
        let mut app = self.app.write().unwrap();
        app.tray_hint_shown = true;
        self.store.save_app(&app);
    }
}

/// The profile selected for `theme` in `app` ([`DEFAULT_PROFILE`] unless a
/// selection is recorded).
pub fn active_profile_in(app: &AppConfig, theme: &str) -> String {
    app.profiles.get(theme).cloned().unwrap_or_else(|| DEFAULT_PROFILE.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::NumOrExpr;

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "MouseTrails-store-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        (Store::new(&dir), dir)
    }

    #[test]
    fn fresh_install_gets_a_v2_config_and_default_profile() {
        let (store, dir) = temp_store("fresh");
        let app = store.load_app();
        assert_eq!(app.version, CONFIG_VERSION);
        assert_eq!(app.theme, "default");
        assert_eq!(app.render_fps, 60);
        // The root config exists on disk now; the default profile is
        // materialized by the caller (main), not by migration.
        assert!(!store.profile_path("default", "default").exists());
        assert!(dir.join("config.json").exists());
    }

    #[test]
    fn legacy_config_migrates_and_backs_up() {
        let (store, dir) = temp_store("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = r#"{
            "trail": {"enabled": true, "style": "neon", "length_secs": 0.8,
                      "width": 12.0, "hue_speed": 1.0, "color": [255, 0, 128]},
            "bubbles": {"enabled": true, "rate": 12.0, "size": 18.0,
                        "buoyancy": 90.0, "lifetime": 2.0, "wobble": 25.0},
            "sparkles": {"enabled": true, "rate": 33.0, "size": 5.0,
                         "gravity": -10.0, "lifetime": 1.0, "color": [1, 2, 3]},
            "ripples": {"enabled": false, "rings": 3, "speed": 500.0,
                        "lifetime": 0.7, "width": 4.0, "color": [9, 9, 9]},
            "render_fps": 90,
            "tray_hint_shown": true
        }"#;
        std::fs::write(dir.join("config.json"), legacy).unwrap();

        let app = store.load_app();
        assert_eq!(app.version, CONFIG_VERSION);
        assert_eq!(app.render_fps, 90);
        assert!(app.tray_hint_shown);

        let es = store.load_profile("default", "main").unwrap();
        assert_eq!(es.trail.style, crate::settings::TrailStyle::Neon);
        assert_eq!(es.trail.width, NumOrExpr::Num(12.0));
        assert_eq!(es.sparkles.color, [1, 2, 3]);
        assert!(!es.ripples.enabled);

        // The original document survives as a backup, untouched.
        let bak = std::fs::read_to_string(dir.join("config.json.bak")).unwrap();
        assert_eq!(bak, legacy);
        // And the root config is now the new format.
        let root = std::fs::read_to_string(dir.join("config.json")).unwrap();
        assert!(root.contains("\"version\": 2"));
    }

    #[test]
    fn corrupt_legacy_config_falls_back_to_defaults_with_backup() {
        let (store, dir) = temp_store("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), "{ not json !!!").unwrap();

        let app = store.load_app();
        assert_eq!(app, AppConfig::default());
        let es = store.load_profile("default", "main").unwrap();
        assert_eq!(es, EffectSettings::default());
        let bak = std::fs::read_to_string(dir.join("config.json.bak")).unwrap();
        assert_eq!(bak, "{ not json !!!");
    }

    #[test]
    fn migration_is_idempotent() {
        let (store, dir) = temp_store("idempotent");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = r#"{ "render_fps": 75, "trail": {}, "bubbles": {}, "sparkles": {}, "ripples": {} }"#;
        std::fs::write(dir.join("config.json"), legacy).unwrap();

        let first = store.load_app();
        assert_eq!(first.render_fps, 75);

        // A second load must not re-migrate (which would reset user changes
        // made since): tweak the v2 config, load again, check it survives.
        let mut app = first.clone();
        app.render_fps = 111;
        store.save_app(&app);
        assert_eq!(store.load_app().render_fps, 111);
        // And the backup still holds the original legacy text.
        let bak = std::fs::read_to_string(dir.join("config.json.bak")).unwrap();
        assert_eq!(bak, legacy);
    }

    #[test]
    fn profiles_round_trip_and_list_and_delete() {
        let (store, _dir) = temp_store("profiles");
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Num(7.5);
        store.save_profile("default", "cozy", &es);
        store.save_profile("default", "default", &EffectSettings::default());

        assert_eq!(store.list_profiles("default"), vec!["cozy".to_string(), "default".to_string()]);
        assert_eq!(store.load_profile("default", "cozy").unwrap().trail.width, NumOrExpr::Num(7.5));

        assert!(store.delete_profile("default", "cozy"));
        assert_eq!(store.list_profiles("default"), vec!["default".to_string()]);
        // A preset may end up with zero profiles — nothing enforces a floor.
        assert!(store.delete_profile("default", "default"));
        assert!(store.list_profiles("default").is_empty());
    }

    #[test]
    fn theme_defs_round_trip_with_partial_sections() {
        let (store, _dir) = temp_store("themedef");
        let def = ThemeDef {
            name: "Frost".into(),
            description: "Cool whites".into(),
            trail: Some(TrailCfg::default()),
            bubbles: None,
            sparkles: None,
            ripples: None,
            limits: Some(LimitsCfg {
                bubbles: 120,
                sparkles: 300,
                ripples: 24,
            }),
        };
        store.save_theme_def("frost", &def);
        let back = store.load_theme_def("frost").unwrap();
        assert_eq!(back, def);
        assert!(back.bubbles.is_none(), "unthemed sections stay None");

        assert_eq!(store.list_themes(), vec!["frost".to_string()]);
        assert!(store.delete_theme("frost"));
        assert!(store.list_themes().is_empty());
        assert!(store.load_theme_def("frost").is_none());
    }

    #[test]
    fn default_profiles_rename_to_main() {
        let (store, dir) = temp_store("rename-main");
        std::fs::create_dir_all(&dir).unwrap();
        // A v2 config pointing at a "default" profile, plus the old file.
        let v2 = r#"{ "version": 2, "theme": "winter", "profiles": {"winter": "default"},
                      "render_fps": 60, "tray_hint_shown": false }"#;
        std::fs::write(dir.join("config.json"), v2).unwrap();
        store.save_profile("winter", "default", &EffectSettings::default());
        assert!(store.profile_path("winter", "default").exists());

        let app = store.load_app();
        assert!(store.profile_path("winter", "main").exists(), "file renamed");
        assert!(!store.profile_path("winter", "default").exists(), "old name gone");
        assert_eq!(app.profiles.get("winter").map(|s| s.as_str()), Some("main"));

        // Idempotent: a second load keeps everything in place.
        let app2 = store.load_app();
        assert_eq!(app2.profiles.get("winter").map(|s| s.as_str()), Some("main"));
        assert!(store.profile_path("winter", "main").exists());
    }

    #[test]
    fn ensure_profile_never_clobbers() {
        let (store, _dir) = temp_store("ensure");
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Num(1.0);
        store.ensure_profile("default", "default", &es);
        assert_eq!(store.load_profile("default", "default").unwrap().trail.width, NumOrExpr::Num(1.0));
        es.trail.width = NumOrExpr::Num(99.0);
        store.ensure_profile("default", "default", &es);
        assert_eq!(store.load_profile("default", "default").unwrap().trail.width, NumOrExpr::Num(1.0));
    }

    #[test]
    fn limits_round_trip_and_default_when_absent() {
        let (store, _dir) = temp_store("limits");
        let mut es = EffectSettings::default();
        es.limits.sparkles = 600;
        store.save_profile("default", "cozy", &es);
        assert_eq!(
            store.load_profile("default", "cozy").unwrap().limits.sparkles,
            600
        );

        // Profiles written before limits existed load with the defaults.
        std::fs::write(
            store.profile_path("default", "old"),
            r#"{ "trail": {}, "bubbles": {}, "sparkles": {}, "ripples": {} }"#,
        )
        .unwrap();
        assert_eq!(
            store.load_profile("default", "old").unwrap().limits,
            LimitsCfg::default()
        );
    }

    #[test]
    fn themes_fingerprint_tracks_existence_not_content() {
        let (store, _dir) = temp_store("fingerprint");
        let base = store.themes_fingerprint();

        // A profile file added (by hand or by the app) counts…
        store.save_profile("default", "main", &EffectSettings::default());
        let with_profile = store.themes_fingerprint();
        assert_ne!(with_profile, base);

        // …but content edits to an existing file never do.
        let mut es = EffectSettings::default();
        es.trail.width = NumOrExpr::Num(9.0);
        store.save_profile("default", "main", &es);
        assert_eq!(store.themes_fingerprint(), with_profile);

        // A whole preset folder added, then removed by hand.
        store.save_theme_def("frost", &ThemeDef::default());
        let with_theme = store.themes_fingerprint();
        assert_ne!(with_theme, with_profile);
        assert!(store.delete_theme("frost"));
        assert_eq!(store.themes_fingerprint(), with_profile);

        // A profile file deleted by hand: the (now empty) folder itself
        // remains, so the fingerprint settles on folder-with-no-profiles.
        assert!(store.delete_profile("default", "main"));
        assert_eq!(store.themes_fingerprint(), "default:");
        assert_ne!(store.themes_fingerprint(), with_profile);
    }

    #[test]
    fn corrupt_profiles_are_backed_up_before_replacement() {
        let (store, _dir) = temp_store("corrupt-profile");
        store.save_profile("default", "cozy", &EffectSettings::default());
        std::fs::write(
            store.profile_path("default", "cozy"),
            "{ truncated !!!",
        )
        .unwrap();

        // What main.rs does when load_profile returns None for an existing
        // file: back it up, then write the materialized settings.
        assert!(store.load_profile("default", "cozy").is_none());
        store.back_up_corrupt_profile("default", "cozy");
        let bak = std::fs::read_to_string(
            store.profile_path("default", "cozy").with_extension("json.corrupt"),
        )
        .unwrap();
        assert_eq!(bak, "{ truncated !!!");
        store.save_profile("default", "cozy", &EffectSettings::default());
        assert!(store.load_profile("default", "cozy").is_some());
        // The backup doesn't show up as a profile.
        assert_eq!(store.list_profiles("default"), vec!["cozy".to_string()]);
    }

    #[test]
    fn load_profile_for_use_backs_up_unreadable_files_only() {
        let (store, _dir) = temp_store("load-for-use");

        // Missing file: nothing to preserve, nothing created.
        assert!(store.load_profile_for_use("default", "absent").is_none());
        assert!(!store.profile_path("default", "absent").exists());
        assert!(!store
            .profile_path("default", "absent")
            .with_extension("json.corrupt")
            .exists());

        // Readable file: returned as-is, no backup appears.
        store.save_profile("default", "cozy", &EffectSettings::default());
        assert!(store.load_profile_for_use("default", "cozy").is_some());
        assert!(!store
            .profile_path("default", "cozy")
            .with_extension("json.corrupt")
            .exists());

        // Unreadable file: backed up in the same call, so a caller that
        // immediately overwrites it can't lose the original.
        std::fs::write(store.profile_path("default", "cozy"), "{ truncated !!!").unwrap();
        assert!(store.load_profile_for_use("default", "cozy").is_none());
        let bak = std::fs::read_to_string(
            store.profile_path("default", "cozy").with_extension("json.corrupt"),
        )
        .unwrap();
        assert_eq!(bak, "{ truncated !!!");
        assert!(!store.profile_path("default", "cozy").exists(), "file moved aside");
    }

    #[test]
    fn prune_app_profiles_drops_dangling_selections() {
        let (store, _dir) = temp_store("prune");
        store.save_profile("winter", "main", &EffectSettings::default());
        store.save_profile("winter", "cozy", &EffectSettings::default());
        // A custom preset on disk with its primary profile materialized.
        store.save_theme_def("frost", &ThemeDef::default());
        store.save_profile("frost", "main", &EffectSettings::default());

        let mut app = AppConfig::default();
        // A deleted profile's entry goes; entries for real profiles stay.
        app.profiles.insert("winter".into(), "gone".into());
        app.profiles.insert("frost".into(), "main".into());
        assert!(store.prune_app_profiles(&mut app));
        assert_eq!(app.profiles.get("winter"), None);
        assert!(app.profiles.contains_key("frost"));

        // Selecting a surviving profile is kept as-is.
        app.profiles.insert("winter".into(), "main".into());
        assert!(!store.prune_app_profiles(&mut app));
        assert_eq!(app.profiles.get("winter").map(String::as_str), Some("main"));

        // Re-running on an already-pruned config is a no-op, so callers can
        // gate their save on the return value.
        assert!(!store.prune_app_profiles(&mut app));

        // A preset folder deleted by hand takes its entry with it; the
        // built-ins survive on their own with no folder.
        let mut app2 = AppConfig::default();
        app2.profiles.insert("ghost".into(), "main".into());
        app2.profiles.insert("christmas".into(), "main".into());
        assert!(store.prune_app_profiles(&mut app2));
        assert!(!app2.profiles.contains_key("ghost"));
        assert!(!app2.profiles.contains_key("christmas"), "no profile on disk");
        assert_eq!(store.theme_ids()[0], "default", "built-ins come first");

        // Stale-but-real themes keep their entry even with capitalization
        // differences, matching NTFS.
        let mut app3 = AppConfig::default();
        app3.profiles.insert("WINTER".into(), "MAIN".into());
        // The theme key itself is matched exactly (ids are lowercase on
        // disk); only the profile name compares case-insensitively.
        assert!(store.prune_app_profiles(&mut app3));
    }

    #[test]
    fn flush_pending_effects_writes_only_on_a_real_difference() {
        let (store, _dir) = super::tests::temp_store("flush");
        // Seed the active profile so there is something to compare against.
        store.save_profile("default", "main", &EffectSettings::default());
        let mut app = AppConfig::default();
        app.profiles.insert("default".into(), "main".into());
        let shared = super::SharedSettings {
            store,
            effects: RwLock::new(EffectSettings::default()),
            app: RwLock::new(app),
            update: Mutex::new(crate::updater::UpdateState::initial()),
            overlay_hwnd: std::sync::atomic::AtomicIsize::new(0),
        };

        // Matching memory and disk: nothing is written, so the mtime stands.
        let path = shared.store.profile_path("default", "main");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        shared.flush_pending_effects();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);

        // A debounced edit still in memory gets written.
        shared.effects.write().unwrap().trail.length_secs = 1.25;
        shared.flush_pending_effects();
        let on_disk = shared.store.load_profile("default", "main").unwrap();
        assert_eq!(on_disk.trail.length_secs, 1.25);
    }

    #[test]
    fn sanitize_id_blocks_reserved_and_unsafe_names() {
        assert_eq!(sanitize_id("  Cozy! "), Some("cozy".to_string()));
        assert_eq!(sanitize_id("My Theme v2"), Some("my-theme-v2".to_string()));
        // Path traversal is impossible: every non-alnum becomes '-', and the
        // leading run is trimmed — "../../config" is just "config".
        assert_eq!(sanitize_id("../../config"), Some("config".to_string()));
        // Empty after cleaning.
        assert_eq!(sanitize_id(" ... "), None);
        assert_eq!(sanitize_id("   "), None);
        // The theme-definition file name is reserved for profiles/themes.
        assert_eq!(sanitize_id("theme"), None);
        assert_eq!(sanitize_id("THEME"), None);
        // Windows reserved device names.
        for reserved in ["con", "NUL", "Com1", "lpt9", "aux", "prn"] {
            assert_eq!(sanitize_id(reserved), None, "{reserved} must be rejected");
        }
        // Length cap.
        let long = "a".repeat(80);
        assert_eq!(sanitize_id(&long).unwrap().len(), MAX_ID_LEN);
    }

    #[test]
    fn existence_checks_are_case_insensitive() {
        let (store, _dir) = temp_store("case");
        store.save_profile("default", "Cozy", &EffectSettings::default());
        assert!(store.profile_exists("default", "cozy"), "NTFS is case-insensitive");
        assert!(store.profile_exists("default", "COZY"));
        assert!(!store.profile_exists("default", "snug"));

        let mut def = ThemeDef::default();
        def.name = "Frost".into();
        store.save_theme_def("Frost", &def);
        assert!(store.theme_exists("frost"));
        assert!(!store.theme_exists("winter"));
    }
}
