//! Built-in themes, embedded from the repo's `themes/` directory. The same
//! JSON files double as templates for hand-authored custom themes. A theme
//! definition on disk (in the config directory) overrides the built-in one.

use crate::store::{EffectSettings, Store, ThemeDef};

pub const DEFAULT_JSON: &str = include_str!("../themes/default.json");
pub const WINTER_JSON: &str = include_str!("../themes/winter.json");
pub const CHRISTMAS_JSON: &str = include_str!("../themes/christmas.json");

pub struct BuiltinTheme {
    /// Stable id, used as the directory name on disk.
    pub id: &'static str,
    pub def_json: &'static str,
}

pub const BUILTINS: [BuiltinTheme; 3] = [
    BuiltinTheme { id: "default", def_json: DEFAULT_JSON },
    BuiltinTheme { id: "winter", def_json: WINTER_JSON },
    BuiltinTheme { id: "christmas", def_json: CHRISTMAS_JSON },
];

pub fn builtin(id: &str) -> Option<&'static BuiltinTheme> {
    BUILTINS.iter().find(|b| b.id == id)
}

/// The effective definition for a theme id: a definition file on disk wins
/// over the embedded one; unknown ids have no definition.
pub fn load_def(store: &Store, id: &str) -> Option<ThemeDef> {
    if let Some(def) = store.load_theme_def(id) {
        return Some(def);
    }
    builtin(id).and_then(|b| serde_json::from_str(b.def_json).ok())
}

/// Overlay a theme definition onto current settings: themed sections
/// (`Some`) are replaced, unthemed sections (`None`) keep their current
/// values. Used with factory defaults when materializing a fresh profile.
pub fn apply_def(def: &ThemeDef, current: &EffectSettings) -> EffectSettings {
    EffectSettings {
        trail: def.trail.clone().unwrap_or_else(|| current.trail.clone()),
        bubbles: def.bubbles.clone().unwrap_or_else(|| current.bubbles.clone()),
        sparkles: def.sparkles.clone().unwrap_or_else(|| current.sparkles.clone()),
        ripples: def.ripples.clone().unwrap_or_else(|| current.ripples.clone()),
    }
}

/// Build the first profile for a theme: its definition's themed sections
/// over factory defaults. Called when a profile file doesn't exist yet.
pub fn materialize_profile(store: &Store, theme: &str) -> EffectSettings {
    match load_def(store, theme) {
        Some(def) => apply_def(&def, &EffectSettings::default()),
        None => EffectSettings::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!(
            "MouseTrails-themes-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn all_builtin_jsons_parse() {
        for b in BUILTINS.iter() {
            let def: ThemeDef = serde_json::from_str(b.def_json)
                .unwrap_or_else(|e| panic!("{} must parse: {}", b.id, e));
            assert!(!def.name.is_empty(), "{} has a display name", b.id);
            assert!(!def.description.is_empty(), "{} has a description", b.id);
        }
    }

    #[test]
    fn default_theme_leaves_everything_unthemed() {
        let def: ThemeDef = serde_json::from_str(DEFAULT_JSON).unwrap();
        assert!(def.trail.is_none() && def.bubbles.is_none());
        assert!(def.sparkles.is_none() && def.ripples.is_none());
        // Applying it over anything keeps that anything.
        let current = EffectSettings::default();
        assert_eq!(apply_def(&def, &current), current);
    }

    #[test]
    fn winter_and_christmas_theme_their_sections() {
        let winter: ThemeDef = serde_json::from_str(WINTER_JSON).unwrap();
        assert_eq!(winter.trail.as_ref().unwrap().style, crate::settings::TrailStyle::Soft);
        assert_eq!(
            winter.bubbles.as_ref().unwrap().shape,
            crate::settings::BubbleShape::Snowflake
        );
        assert_eq!(
            winter.sparkles.as_ref().unwrap().shape,
            crate::settings::SparkShape::Dot
        );
        assert!(winter.bubbles.as_ref().unwrap().buoyancy.value() < 0.0, "snow falls");

        let christmas: ThemeDef = serde_json::from_str(CHRISTMAS_JSON).unwrap();
        assert_eq!(christmas.trail.as_ref().unwrap().style, crate::settings::TrailStyle::Striped);
        assert_eq!(
            christmas.bubbles.as_ref().unwrap().shape,
            crate::settings::BubbleShape::Ornament
        );
        assert_eq!(christmas.sparkles.as_ref().unwrap().max_generations, 3);
        assert!(christmas.sparkles.as_ref().unwrap().child_rate > 0.0);
        assert!(christmas.sparkles.as_ref().unwrap().streak_secs > 0.0);

        // Unthemed sections are preserved through apply: strip christmas's
        // ripples section and the current setting must survive.
        let mut current = EffectSettings::default();
        current.ripples.enabled = false;
        let mut partial = christmas.clone();
        partial.ripples = None;
        let applied = apply_def(&partial, &current);
        assert!(!applied.ripples.enabled, "ripples unthemed, stays as-is");
        assert_eq!(applied.trail.style, crate::settings::TrailStyle::Striped);
    }

    #[test]
    fn disk_def_overrides_builtin_and_materialization_works() {
        let store = temp_store("override");

        // Unknown id: factory defaults.
        assert_eq!(materialize_profile(&store, "nope"), EffectSettings::default());

        // Built-in with no disk file: winter profile from the embedded def.
        let w = materialize_profile(&store, "winter");
        assert_eq!(w.trail.style, crate::settings::TrailStyle::Soft);
        assert_eq!(w.bubbles.shape, crate::settings::BubbleShape::Snowflake);

        // A disk definition overrides the embedded one.
        let mut override_def: ThemeDef = serde_json::from_str(WINTER_JSON).unwrap();
        override_def.bubbles = None;
        store.save_theme_def("winter", &override_def);
        let w2 = materialize_profile(&store, "winter");
        assert_eq!(w2.bubbles, crate::settings::BubbleCfg::default(), "disk def wins");
        assert_eq!(w2.trail.style, crate::settings::TrailStyle::Soft, "rest still themed");
    }
}
