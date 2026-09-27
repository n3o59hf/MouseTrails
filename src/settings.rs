use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TrailStyle {
    #[default]
    Rainbow,
    Neon,
    Ghost,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailCfg {
    pub enabled: bool,
    pub style: TrailStyle,
    pub length_secs: f32,
    pub width: f32,
    pub hue_speed: f32,
    pub color: [u8; 3],
}

impl Default for TrailCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            style: TrailStyle::Rainbow,
            length_secs: 0.55,
            width: 10.0,
            hue_speed: 1.0,
            color: [51, 217, 255],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BubbleCfg {
    pub enabled: bool,
    pub rate: f32,
    pub size: f32,
    pub buoyancy: f32,
    pub lifetime: f32,
    pub wobble: f32,
}

impl Default for BubbleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            rate: 9.0,
            size: 14.0,
            buoyancy: 85.0,
            lifetime: 2.4,
            wobble: 30.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SparkleCfg {
    pub enabled: bool,
    pub rate: f32,
    pub size: f32,
    pub gravity: f32,
    pub lifetime: f32,
    pub color: [u8; 3],
}

impl Default for SparkleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            rate: 40.0,
            size: 4.0,
            gravity: -30.0,
            lifetime: 0.9,
            color: [255, 212, 64],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RippleCfg {
    pub enabled: bool,
    pub rings: u32,
    pub speed: f32,
    pub lifetime: f32,
    pub width: f32,
    pub color: [u8; 3],
}

impl Default for RippleCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            rings: 2,
            speed: 450.0,
            lifetime: 0.6,
            width: 3.0,
            color: [140, 230, 255],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub trail: TrailCfg,
    pub bubbles: BubbleCfg,
    pub sparkles: SparkleCfg,
    pub ripples: RippleCfg,
    pub render_fps: u32,
    /// Whether the "how to find me in the tray" hint was shown after the
    /// user closed the settings window for the first time.
    pub tray_hint_shown: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trail: TrailCfg::default(),
            bubbles: BubbleCfg::default(),
            sparkles: SparkleCfg::default(),
            ripples: RippleCfg::default(),
            render_fps: 60,
            tray_hint_shown: false,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                return s;
            }
        }
        Settings::default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

pub fn config_path() -> PathBuf {
    std::env::var("APPDATA")
        .map(|d| PathBuf::from(d).join("MouseTrails").join("config.json"))
        .unwrap_or_else(|_| std::env::temp_dir().join("MouseTrails-config.json"))
}

/// True when no config file exists yet (i.e. this is the first launch).
pub fn config_exists() -> bool {
    config_path().exists()
}

pub fn tray_hint_shown() -> bool {
    Settings::load().tray_hint_shown
}

/// Mark the first-close tray hint as shown and persist it.
pub fn set_tray_hint_shown() {
    let mut s = Settings::load();
    s.tray_hint_shown = true;
    s.save();
}

/// State shared between the overlay thread (main) and the settings UI thread.
pub struct SharedSettings {
    pub settings: RwLock<Settings>,
}
