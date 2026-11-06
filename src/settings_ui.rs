use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use eframe::egui;
use windows::core::{w, PCWSTR};

use crate::expr::FrameExprs;
use crate::icon;
use crate::preview::ThemePreview;
use crate::settings::{BubbleShape, NumOrExpr, SparkShape, TrailStyle};
use crate::startup;
use crate::store::{self, AppConfig, EffectSettings, SharedSettings, ThemeDef};
use crate::themes;

pub enum UiMsg {
    Show,
    StartupChanged(bool),
}

/// Owned by the overlay: opens the settings window, reusing a running one.
pub struct SettingsUiManager {
    tx: Option<Sender<UiMsg>>,
    alive: Arc<AtomicBool>,
}

impl Default for SettingsUiManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsUiManager {
    pub fn new() -> Self {
        Self { tx: None, alive: Arc::new(AtomicBool::new(false)) }
    }

    pub fn open(&mut self, shared: &Arc<SharedSettings>) {
        if self.alive.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(tx) = &self.tx {
                if tx.send(UiMsg::Show).is_ok() {
                    // The persistent window may be hidden — and while hidden,
                    // eframe's event loop is fully dormant (request_repaint
                    // never wakes it), so the queued Show would never be
                    // processed. Wake it directly instead.
                    show_settings_window();
                    return;
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let shared = shared.clone();
        let alive2 = alive.clone();
        let spawned = std::thread::Builder::new()
            .name("settings-ui".into())
            .spawn(move || {
                run_ui(shared, rx);
                alive2.store(false, std::sync::atomic::Ordering::Relaxed);
            });
        if spawned.is_ok() {
            self.tx = Some(tx);
            self.alive = alive;
        }
    }

    pub fn notify_startup_changed(&self, on: bool) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(UiMsg::StartupChanged(on));
        }
    }
}

/// Brings the (hidden) settings window back on screen from outside the egui
/// loop. A no-op when the window doesn't exist yet.
fn show_settings_window() {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, ShowWindow, SW_SHOW};
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("MouseTrails Settings")) {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
    }
}

fn run_ui(shared: Arc<SharedSettings>, rx: Receiver<UiMsg>) {
    let icon_rgba = icon::icon_rgba_straight();
    let icon = egui::IconData { width: 32, height: 32, rgba: icon_rgba };

    let saved = shared.effects.read().unwrap().clone();
    let saved_app = shared.app.read().unwrap().clone();
    let (cards, active_def) = build_cards(&shared);
    let app = SettingsApp {
        shared,
        rx,
        startup: startup::is_enabled(),
        startup_err: None,
        saved,
        saved_app,
        updates: 0,
        cards,
        cards_dirty: false,
        active_def,
        theme_err: None,
        delete_arm: None,
        preview_cell_w: 190.0,
        profile_dialog: DialogPreviews::build(Vec::new()),
        preset_dialog: DialogPreviews::build(Vec::new()),
        // Debug builds open both dialogs for visual verification.
        dialog_open: std::env::var("MOUSETRAILS_DEBUG").is_ok(),
        dialog_name: String::new(),
        dialog_source: None,
        preset_dialog_open: std::env::var("MOUSETRAILS_DEBUG").is_ok(),
        preset_dialog_name: String::new(),
        preset_dialog_source: None,
        window_visible: true,
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MouseTrails Settings")
            // Wide default: effect sliders stretch horizontally, so a
            // wider window means longer sliders, not dead space.
            .with_inner_size([680.0, 780.0])
            .with_min_inner_size([520.0, 560.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    // The settings window runs on its own thread (the main thread owns the
    // fullscreen overlay), so winit must be allowed to create its event loop
    // off the main thread — officially supported on Windows.
    options.event_loop_builder = Some(Box::new(|builder| {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }));

    let _ = eframe::run_native(
        "MouseTrails Settings",
        options,
        Box::new(move |_cc| Ok(Box::new(app) as Box<dyn eframe::App>)),
    );
}

/// Hide instead of close: the settings window is a singleton that lives as
/// long as the process, so reopening from the tray is instant and we never
/// have to spin up a second eframe event loop.
fn hide_settings_window(ctx: &egui::Context, shared: &SharedSettings) {
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    // The first time the settings window goes away, tell the user where
    // MouseTrails lives from now on.
    if !shared.tray_hint_shown() {
        shared.set_tray_hint_shown();
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                MessageBoxW, MB_ICONINFORMATION, MB_SETFOREGROUND, MB_TOPMOST,
            };
            let _ = MessageBoxW(
                None,
                w!(
                    "MouseTrails keeps running in your system tray, near the clock.\n\n\
                     Left-click the tray icon to open Settings again.\n\
                     Right-click it for more options \u{2014} including Exit."
                ),
                w!("MouseTrails"),
                MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
            );
        }
        // Keep showing the window loop responsive while the box was up.
        ctx.request_repaint();
    }
}

/// Modal scrim over a whole window: dims it and swallows all pointer input
/// while a child dialog window is open, so only one window is active at a
/// time. The scrimmed window keeps rendering (previews stay animated under
/// the dim); its own title bar stays OS-native and closeable.
fn modal_scrim(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    egui::Area::new(egui::Id::new("modal-scrim"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            ui.painter()
                .rect_filled(resp.rect, 0.0, egui::Color32::from_black_alpha(120));
        });
}

struct SettingsApp {
    shared: Arc<SharedSettings>,
    rx: Receiver<UiMsg>,
    startup: bool,
    startup_err: Option<String>,
    saved: EffectSettings,
    saved_app: AppConfig,
    updates: u64,

    /// One animated preview card per available theme. Rebuilt when
    /// `cards_dirty` is set (theme/profile changes).
    cards: Vec<PreviewCard>,
    cards_dirty: bool,
    /// The active theme's definition — supplies slider bounds overrides.
    active_def: Option<ThemeDef>,
    /// Error from the last profile/preset action, shown until it clears.
    theme_err: Option<String>,
    /// Armed delete-profile button: first click arms, second (within 4s)
    /// confirms.
    delete_arm: Option<std::time::Instant>,
    /// Measured gallery cell width — previews re-render at exactly this
    /// size (updated by the widget pass, applied by the next step loop).
    preview_cell_w: f32,
    /// Per-preset previews for the create dialog, rendered from preset
    /// definitions rather than profiles. Rebuilt when the dialog opens.
    /// Per-dialog preview sets, each sized to its own window.
    profile_dialog: DialogPreviews,
    preset_dialog: DialogPreviews,
    /// The create-profile dialog state.
    dialog_open: bool,
    dialog_name: String,
    dialog_source: Option<DialogSource>,
    /// The new-preset window state.
    preset_dialog_open: bool,
    preset_dialog_name: String,
    preset_dialog_source: Option<PresetSource>,
    /// Tracked hide/show state of the window (drives preview animation).
    window_visible: bool,
}

/// What a new profile starts from, chosen in the create dialog.
#[derive(Clone, Debug, PartialEq)]
enum DialogSource {
    /// Start from a preset's themed values over the current settings.
    Preset(String),
    /// Exact copy of an existing profile.
    Duplicate(String, String),
}

/// What a new preset starts from, chosen in the new-preset window.
#[derive(Clone, Debug, PartialEq)]
enum PresetSource {
    /// No themed sections — a blank theme.json to fill in by hand.
    Empty,
    /// Copy of an existing preset's definition.
    CopyPreset(String),
    /// Snapshot of an existing profile: every section themed to its values.
    CopyProfile(String, String),
}

/// One preview tile inside a dialog window.
struct DialogPreview {
    /// Stable identity within the dialog ("current", "preset:winter",
    /// "profile:default/main").
    key: String,
    title: String,
    sub: String,
    sim: ThemePreview,
    exprs: FrameExprs,
    texture: Option<egui::TextureHandle>,
    played: u32,
}

/// All previews backing one dialog window. Sized to that dialog's own tile
/// measurement — deliberately independent of the main-window gallery and of
/// the other dialog, so windows never inherit each other's sizing.
struct DialogPreviews {
    /// Measured tile width (outer), recorded by the widget pass and applied
    /// by the next step.
    cell_w: f32,
    items: Vec<DialogPreview>,
}

impl DialogPreviews {
    fn build(items: Vec<DialogPreview>) -> Self {
        Self { cell_w: 0.0, items }
    }

    fn get(&self, key: &str) -> Option<&DialogPreview> {
        self.items.iter().find(|i| i.key == key)
    }

    /// Re-read a profile from disk into its preview and replay it.
    fn refresh_profile(&mut self, shared: &SharedSettings, theme: &str, profile: &str) {
        let key = format!("profile:{}/{}", theme, profile);
        if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
            if let Some(es) = shared.store.load_profile(theme, profile) {
                item.exprs = FrameExprs::build(&es);
                item.sim.reset();
                item.played = 0;
            }
        }
    }

    /// Swap an item's expressions and replay it (settings changed).
    fn replay(&mut self, key: &str, exprs: FrameExprs) {
        if let Some(item) = self.items.iter_mut().find(|i| i.key == key) {
            item.exprs = exprs;
            item.sim.reset();
            item.played = 0;
        }
    }

    /// Step every non-frozen preview at `want_w` px, uploading textures.
    fn step(&mut self, want_w: u32, ctx: &egui::Context) {
        let want_h = (want_w as f32 * PREVIEW_ASPECT).round() as u32;
        for item in &mut self.items {
            if item.sim.resize(want_w, want_h) {
                // The old texture has the wrong dimensions — replay.
                item.played = 0;
            }
            if item.played >= PREVIEW_FRAMES {
                continue; // frozen
            }
            item.sim.step(&item.exprs);
            item.played += 1;
            if let Some(img) = item.sim.image().cloned() {
                match &mut item.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        item.texture = Some(ctx.load_texture(
                            format!("dlg-preview-{}", item.key),
                            img,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }
    }
}

struct PreviewCard {
    /// Owning preset id (the theme folder).
    theme: String,
    /// Profile name within that preset.
    profile: String,
    /// Display label: bare profile name for the default preset, qualified
    /// otherwise.
    label: String,
    sim: ThemePreview,
    exprs: FrameExprs,
    texture: Option<egui::TextureHandle>,
    /// Sim frames played; the preview freezes at PREVIEW_FRAMES and replays
    /// when the profile's parameters change.
    played: u32,
}

/// Preview tile aspect (height / width) — sims re-render at whatever pixel
/// size the layout gives them, keeping this ratio.
const PREVIEW_ASPECT: f32 = 74.0 / 132.0;
const TILE_SPACING: f32 = 10.0;
/// Smallest allowed dialog tile width; dialog column counts adapt to it.
const MIN_TILE_W: f32 = 160.0;
/// Fixed column count of the profile gallery.
const GALLERY_COLS: usize = 3;
/// Column count of the create dialog's source grid.
const DIALOG_COLS: usize = 3;
/// ~3.7 sim-seconds at the fixed 30 fps step: trail fully grown, particles
/// mid-life, first click blast happened — a lively freeze frame.
const PREVIEW_FRAMES: u32 = 110;

/// All theme ids: built-ins first, then any custom theme folders on disk.
fn theme_ids(shared: &SharedSettings) -> Vec<String> {
    let mut ids: Vec<String> = themes::BUILTINS.iter().map(|b| b.id.to_string()).collect();
    for id in shared.store.list_themes() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// (Re)build the preview cards: one per (preset, profile) pair — the
/// profiles are the selector, so previews show the user's actual settings.
/// Expensive-ish (file reads + parses), so it only runs when the theme set
/// or profiles change.
fn dialog_preview(key: String, title: String, sub: String, es: &EffectSettings) -> DialogPreview {
    DialogPreview {
        key,
        title,
        sub,
        sim: ThemePreview::new(182, 102),
        exprs: FrameExprs::build(es),
        texture: None,
        played: 0,
    }
}

/// Previews for the create-profile window: the current profile plus every
/// preset (rendered from its definition over factory defaults).
fn build_profile_dialog_previews(
    shared: &SharedSettings,
    app: &AppConfig,
    active: &EffectSettings,
) -> DialogPreviews {
    let mut items = Vec::new();
    let active_theme = app.theme.clone();
    let active_profile = active_profile_in(app, &app.theme);
    let label = if active_theme == "default" {
        active_profile.clone()
    } else {
        format!("{} \u{00b7} {}", active_theme, active_profile)
    };
    items.push(dialog_preview(
        "current".into(),
        "Current profile".into(),
        format!("duplicate of {}", label),
        active,
    ));
    for theme in theme_ids(shared) {
        let title = themes::load_def(&shared.store, &theme)
            .map(|d| d.name)
            .unwrap_or_else(|| theme.clone());
        let es = themes::materialize_profile(&shared.store, &theme);
        items.push(dialog_preview(
            format!("preset:{}", theme),
            title,
            format!("preset: {}", theme),
            &es,
        ));
    }
    DialogPreviews::build(items)
}

/// Previews for the new-preset window: an empty slate (a blank theme.json to
/// fill in by hand), every preset (from its definition) and every profile
/// (from its file), as snapshot sources.
fn build_preset_dialog_previews(shared: &SharedSettings) -> DialogPreviews {
    let mut items = Vec::new();
    // The empty definition themes nothing, so its preview shows the factory
    // defaults the preset would fall back to.
    items.push(dialog_preview(
        "empty".into(),
        "Empty".into(),
        "blank theme.json to fill in".to_string(),
        &EffectSettings::default(),
    ));
    for theme in theme_ids(shared) {
        let title = themes::load_def(&shared.store, &theme)
            .map(|d| d.name)
            .unwrap_or_else(|| theme.clone());
        let es = themes::materialize_profile(&shared.store, &theme);
        items.push(dialog_preview(
            format!("preset:{}", theme),
            title,
            format!("copy of preset: {}", theme),
            &es,
        ));
    }
    for theme in theme_ids(shared) {
        for profile in shared.store.list_profiles(&theme) {
            let label = if theme == "default" {
                profile.clone()
            } else {
                format!("{} \u{00b7} {}", theme, profile)
            };
            let es = shared
                .store
                .load_profile(&theme, &profile)
                .unwrap_or_else(|| themes::materialize_profile(&shared.store, &theme));
            items.push(dialog_preview(
                format!("profile:{}/{}", theme, profile),
                label,
                "copy of profile".to_string(),
                &es,
            ));
        }
    }
    DialogPreviews::build(items)
}

fn build_cards(shared: &SharedSettings) -> (Vec<PreviewCard>, Option<ThemeDef>) {
    let app = shared.app.read().unwrap();
    let mut cards = Vec::new();
    let mut active_def = None;
    for theme in theme_ids(shared) {
        let def = themes::load_def(&shared.store, &theme).unwrap_or_default();
        if theme == app.theme {
            active_def = Some(def.clone());
        }
        for profile in shared.store.list_profiles(&theme) {
            let es = shared.store.load_profile(&theme, &profile).unwrap_or_else(|| {
                // Not materialized yet: preview the definition over factory
                // defaults without writing anything.
                themes::apply_def(&def, &EffectSettings::default())
            });
            let label = if theme == "default" {
                profile.clone()
            } else {
                format!("{} \u{00b7} {}", theme, profile)
            };
            cards.push(PreviewCard {
                theme: theme.clone(),
                profile,
                label,
                sim: ThemePreview::new(182, 102),
                exprs: FrameExprs::build(&es),
                texture: None,
                played: 0,
            });
        }
    }
    (cards, active_def)
}

/// The profile active for `theme` in `app` ("default" unless recorded).
fn active_profile_in(app: &AppConfig, theme: &str) -> String {
    app.profiles.get(theme).cloned().unwrap_or_else(|| store::DEFAULT_PROFILE.into())
}

/// Deferred profile/preset mutations, applied after the widget pass so the
/// snapshots can be refreshed before the write-back diff runs.
#[derive(Debug)]
enum Action {
    /// Switch to a profile of a preset (theme folder).
    SwitchProfile(String, String),
    /// Create a profile under `base` preset and switch to it.
    NewProfile(String, String),
    /// Copy an existing profile under its own preset and switch to it.
    DuplicateProfile(String, String, String),
    DeleteProfile(String, String),
    /// Create a custom preset from the given source, with a default
    /// profile built from it.
    CreateCustom(String, PresetSource),
    OpenFolder,
}

/// Load `theme`'s `profile`, materializing it (definition over the current
/// settings) when the file doesn't exist yet.
fn load_or_materialize(shared: &SharedSettings, theme: &str, profile: &str) -> EffectSettings {
    if let Some(es) = shared.store.load_profile(theme, profile) {
        return es;
    }
    let current = shared.effects.read().unwrap().clone();
    let def = themes::load_def(&shared.store, theme).unwrap_or_default();
    let es = themes::apply_def(&def, &current);
    shared.store.save_profile(theme, profile, &es);
    es
}

fn perform_action(
    shared: &SharedSettings,
    action: Action,
) -> Result<Vec<(String, String)>, String> {
    // Profiles whose preview cards need their settings re-read after the
    // action (a switch refreshes the old and the new active card without
    // rebuilding every card).
    fn ok(refresh: Vec<(String, String)>) -> Result<Vec<(String, String)>, String> {
        Ok(refresh)
    }
    match action {
        Action::SwitchProfile(theme, profile) => {
            let previous = {
                let app = shared.app.read().unwrap();
                active_profile_in(&app, &app.theme)
            };
            let previous_theme = shared.app.read().unwrap().theme.clone();
            let es = load_or_materialize(shared, &theme, &profile);
            *shared.effects.write().unwrap() = es;
            let mut app = shared.app.write().unwrap();
            app.theme = theme.clone();
            app.profiles.insert(theme.clone(), profile.clone());
            let snapshot = app.clone();
            shared.store.save_app(&snapshot);
            ok(vec![(previous_theme, previous), (theme, profile)])
        }
        Action::NewProfile(base, name) => {
            if themes::builtin(&base).is_none() && !shared.store.theme_exists(&base) {
                return Err(format!("Preset \u{201C}{}\u{201D} does not exist.", base));
            }
            let id = store::sanitize_id(&name)
                .ok_or_else(|| "Profile name is empty or reserved.".to_string())?;
            if shared.store.profile_exists(&base, &id) {
                return Err(format!("A profile named \u{201C}{}\u{201D} already exists.", id));
            }
            // The new profile starts from the base preset's themed values
            // over the current settings, and becomes active.
            let current = shared.effects.read().unwrap().clone();
            let def = themes::load_def(&shared.store, &base).unwrap_or_default();
            let es = themes::apply_def(&def, &current);
            shared.store.save_profile(&base, &id, &es);
            perform_action(shared, Action::SwitchProfile(base, id))
            .map(|_| Vec::new())
        }
        Action::DeleteProfile(theme, profile) => {
            // If it's the active one, move to any other profile first. When
            // none remain anywhere, the in-memory settings simply stay
            // active until a new profile is created.
            if shared.active_profile(&theme) == profile {
                let mut remaining: Vec<(String, String)> = Vec::new();
                for t in theme_ids(shared) {
                    for pr in shared.store.list_profiles(&t) {
                        if !(t == theme && pr == profile) {
                            remaining.push((t.clone(), pr));
                        }
                    }
                }
                if let Some((next_theme, next_profile)) = remaining.first() {
                    let es = shared
                        .store
                        .load_profile(next_theme, next_profile)
                        .unwrap_or_else(|| {
                            themes::materialize_profile(&shared.store, next_theme)
                        });
                    *shared.effects.write().unwrap() = es;
                    let mut app = shared.app.write().unwrap();
                    app.theme = next_theme.clone();
                    app.profiles.insert(next_theme.clone(), next_profile.clone());
                    shared.store.save_app(&app);
                }
            }
            if !shared.store.delete_profile(&theme, &profile) {
                return Err(format!("Could not delete profile \u{201C}{}\u{201D}.", profile));
            }
            ok(Vec::new())
        }
        Action::DuplicateProfile(src_theme, src_profile, name) => {
            let id = store::sanitize_id(&name)
                .ok_or_else(|| "Profile name is empty or reserved.".to_string())?;
            if shared.store.profile_exists(&src_theme, &id) {
                return Err(format!("A profile named \u{201C}{}\u{201D} already exists.", id));
            }
            let es = shared.store.load_profile(&src_theme, &src_profile).ok_or_else(|| {
                format!(
                    "Profile \u{201C}{}\u{201D} no longer exists.",
                    src_profile
                )
            })?;
            shared.store.save_profile(&src_theme, &id, &es);
            perform_action(shared, Action::SwitchProfile(src_theme, id))
        }
        Action::CreateCustom(name, src) => {
            let display = name.trim().to_string();
            let id = store::sanitize_id(&name)
                .ok_or_else(|| "Preset name is empty or reserved.".to_string())?;
            if themes::builtin(&id).is_some() || shared.store.theme_exists(&id) {
                return Err(format!("A preset named \u{201C}{}\u{201D} already exists.", id));
            }
            let mut def = match &src {
                PresetSource::Empty => ThemeDef {
                    name: id.clone(),
                    description: String::new(),
                    ..ThemeDef::default()
                },
                PresetSource::CopyPreset(from) => {
                    themes::load_def(&shared.store, from).unwrap_or_default()
                }
                PresetSource::CopyProfile(from_theme, from_profile) => {
                    let es = shared.store.load_profile(from_theme, from_profile).ok_or_else(
                        || {
                            format!(
                                "Profile \u{201C}{}\u{201D} no longer exists.",
                                from_profile
                            )
                        },
                    )?;
                    ThemeDef {
                        name: id.clone(),
                        description: format!("Snapshot of {} \u{00b7} {}", from_theme, from_profile),
                        trail: Some(es.trail),
                        bubbles: Some(es.bubbles),
                        sparkles: Some(es.sparkles),
                        ripples: Some(es.ripples),
                    }
                }
            };
            def.name = if display.is_empty() { id.clone() } else { display };
            shared.store.save_theme_def(&id, &def);
            let current = shared.effects.read().unwrap().clone();
            let es = themes::apply_def(&def, &current);
            shared.store.save_profile(&id, store::DEFAULT_PROFILE, &es);
            // Open the new preset's folder right away — editing theme.json
            // is the whole point of creating one.
            let dir = shared.store.base().join("themes").join(&id);
            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            ok(Vec::new())
        }
        Action::OpenFolder => {
            let dir = shared.store.base().join("themes");
            let _ = std::fs::create_dir_all(&dir);
            std::process::Command::new("explorer")
                .arg(&dir)
                .spawn()
                .map_err(|e| format!("Could not open the folder: {}", e))?;
            ok(Vec::new())
        }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.updates += 1;
        if std::env::var("MOUSETRAILS_DEBUG").is_ok() && self.updates % 100 == 0 {
            crate::overlay::debug_log(format!("ui: update #{} visible={}", self.updates, true));
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::Show => {
                    if std::env::var("MOUSETRAILS_DEBUG").is_ok() {
                        crate::overlay::debug_log("ui: Show received".into());
                    }
                    self.window_visible = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                UiMsg::StartupChanged(v) => {
                    self.startup = v;
                    self.startup_err = None;
                }
            }
        }

        let mut s = self.shared.effects.read().unwrap().clone();
        let mut app = self.shared.app.read().unwrap().clone();

        // egui pins sliders to spacing.slider_width (100 px default) — it
        // never stretches on its own. Drive it from the window width so a
        // wider window means longer sliders: reserve room for the value box
        // and the label to the right of the rail.
        let rail_width = (ctx.screen_rect().width() - 250.0).max(100.0);
        ctx.style_mut(|style| style.spacing.slider_width = rail_width);

        // Split the struct so the egui closures below can capture disjoint
        // fields without borrowing all of self.
        let SettingsApp {
            shared,
            cards,
            cards_dirty,
            active_def,
            theme_err,
            preview_cell_w,
            profile_dialog,
            preset_dialog,
            delete_arm,
            dialog_open,
            dialog_name,
            dialog_source,
            preset_dialog_open,
            preset_dialog_name,
            preset_dialog_source,
            saved,
            saved_app,
            startup,
            startup_err,
            ..
        } = self;

        // Animate the previews only while the window is shown; keep polling
        // the message channel (slowly) while hidden. (egui 0.28's viewport
        // info has no visible flag, so we track the hide/show paths.)
        let visible = self.window_visible;
        let any_playing = cards.iter().any(|c| c.played < PREVIEW_FRAMES)
            || profile_dialog.items.iter().any(|i| i.played < PREVIEW_FRAMES)
            || preset_dialog.items.iter().any(|i| i.played < PREVIEW_FRAMES);
        ctx.request_repaint_after(std::time::Duration::from_millis(
            if visible && any_playing { 33 } else { 150 },
        ));

        if *cards_dirty {
            let (c, d) = build_cards(shared);
            *cards = c;
            *active_def = d;
            *cards_dirty = false;
            // Structural changes (created/deleted presets or profiles) also
            // invalidate the dialog previews; they rebuild on next open.
            profile_dialog.items.clear();
            preset_dialog.items.clear();
        }
        // Each dialog's previews live while that dialog is open, rendered at
        // the dialog's own measured tile size.
        if *dialog_open && profile_dialog.items.is_empty() {
            *profile_dialog = build_profile_dialog_previews(shared, &app, &s);
        }
        if *preset_dialog_open && preset_dialog.items.is_empty() {
            *preset_dialog = build_preset_dialog_previews(shared);
        }
        if visible {
            let want_w = ((*preview_cell_w - 12.0).round() as u32).max(96);
            let want_h = (want_w as f32 * PREVIEW_ASPECT).round() as u32;
            for card in cards.iter_mut() {
                // A size change invalidates the old texture — replay at the
                // new size.
                if card.sim.resize(want_w, want_h) {
                    card.played = 0;
                }
                if card.played >= PREVIEW_FRAMES {
                    continue; // frozen
                }
                card.sim.step(&card.exprs);
                card.played += 1;
                if let Some(img) = card.sim.image().cloned() {
                    match &mut card.texture {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => {
                            card.texture = Some(ctx.load_texture(
                                format!("theme-preview-{}-{}", card.theme, card.profile),
                                img,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                }
            }
            // Each dialog steps its own preview set at the size its widget
            // pass measured last frame (the closures run later in update).
            // Skipped until the first measurement lands.
            if *dialog_open && profile_dialog.cell_w > 0.0 {
                let w = ((profile_dialog.cell_w - 12.0).round() as u32).max(96);
                profile_dialog.step(w, ctx);
            }
            if *preset_dialog_open && preset_dialog.cell_w > 0.0 {
                let w = ((preset_dialog.cell_w - 12.0).round() as u32).max(96);
                preset_dialog.step(w, ctx);
            }
        }

        // Snapshot for the footer's reset handler (the panels capture
        // disjoint fields).
        let active_def_snapshot = active_def.clone();

        let mut startup_v = *startup;
        let mut toggle_startup: Option<bool> = None;
        let mut close = false;
        let mut action: Option<Action> = None;
        let mut fps = app.render_fps;

        // Intercept both the X button and the Close button — hide, don't die.
        if ctx.input(|i| i.viewport().close_requested()) {
            if std::env::var("MOUSETRAILS_DEBUG").is_ok() {
                crate::overlay::debug_log("ui: close_requested intercepted".into());
            }
            hide_settings_window(ctx, shared);
            self.window_visible = false;
            // Hiding the settings window takes its modal children with it.
            // The explicit Close matters: a hidden window gets no further
            // egui passes, so the flag alone would leave the dialog OS
            // windows orphaned on screen.
            *dialog_open = false;
            *preset_dialog_open = false;
            for id in ["new-profile-dialog", "new-preset-dialog"] {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId(egui::Id::new(id)),
                    egui::ViewportCommand::Close,
                );
            }
            return;
        }

        // Footer as a bottom panel so it's always visible — putting it inside
        // the CentralPanel let the scroll area push it below the window edge.
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.checkbox(&mut startup_v, "Start with Windows").changed() {
                    toggle_startup = Some(startup_v);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui.button("Reset to defaults").clicked() {
                        // Restore the active theme's own values for its
                        // themed sections; unthemed sections go back to
                        // factory defaults. App-level state (theme choice,
                        // FPS, hints) and other themes' profiles are
                        // untouched.
                        let def = active_def_snapshot.clone().unwrap_or_default();
                        s = themes::apply_def(&def, &EffectSettings::default());
                    }
                });
            });
            if let Some(err) = &*startup_err {
                ui.colored_label(egui::Color32::RED, err.clone());
            }
            ui.label(
                egui::RichText::new("Changes apply instantly and save automatically.")
                    .small()
                    .weak(),
            );
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("MouseTrails");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("fun for your cursor").small().weak());
                });
            });
            ui.label(
                egui::RichText::new(
                    "Playful effects that follow your cursor. \
                     MouseTrails lives in the system tray, near the clock.",
                )
                .small()
                .weak(),
            );
            ui.separator();

            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                profiles_group(
                    ui,
                    preview_cell_w,
                    delete_arm,
                    &app,
                    cards,
                    theme_err,
                    dialog_open,
                    &mut action,
                );
                ui.add_space(6.0);
                trail_section(ui, &mut s);
                ui.add_space(6.0);
                bubbles_section(ui, &mut s);
                ui.add_space(6.0);
                sparkles_section(ui, &mut s);
                ui.add_space(6.0);
                ripples_section(ui, &mut s);
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Performance").strong());
                    ui.add(
                        egui::Slider::new(&mut fps, 24..=240).text("Render FPS").suffix(" fps"),
                    );
                });
                ui.add_space(6.0);
                let (status, available) = {
                    let st = shared.update.lock().unwrap();
                    (st.status.clone(), st.available_sha.is_some())
                };
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Updates").strong());
                    ui.label(
                        egui::RichText::new(format!(
                            "{} (build {}, {})",
                            status,
                            crate::updater::build_sha(),
                            crate::updater::build_date()
                        ))
                        .small(),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Check now").clicked() {
                            crate::updater::spawn_check(shared.clone());
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Install and restart"))
                            .clicked()
                        {
                            crate::updater::spawn_install(shared.clone());
                        }
                    });
                });
            });
        });

        // Modal: while the create-profile dialog is open, the settings
        // window itself is dimmed and takes no input.
        if *dialog_open {
            modal_scrim(ctx);
        }

        // The create-profile popup is its own OS window (a viewport): it
        // gets the same native title bar as the main window and is never
        // clipped by it. The body asks for closure by return value, and the
        // viewport's own X button is honored through close_requested.
        if *dialog_open {
            let mut close_requested = false;
            ctx.show_viewport_immediate(
                egui::ViewportId(egui::Id::new("new-profile-dialog")),
                egui::ViewportBuilder::default()
                    .with_title("New profile")
                    .with_inner_size([660.0, 560.0]),
                |vp_ctx, _class| {
                    egui::CentralPanel::default().show(vp_ctx, |ui| {
                        if vp_ctx.input(|i| i.viewport().close_requested()) {
                            close_requested = true;
                        }
                        close_requested |= dialog_body(
                            vp_ctx,
                            shared,
                            &app,
                            profile_dialog,
                            theme_err,
                            dialog_name,
                            dialog_source,
                            preset_dialog_open,
                            &mut action,
                        );
                        // Modal: while the new-preset window is open, this
                        // dialog is dimmed and takes no input.
                        if *preset_dialog_open {
                            modal_scrim(vp_ctx);
                        }
                    });
                },
            );
            if close_requested {
                *dialog_open = false;
            }
        }

        // The new-preset window: its own OS window, same chrome.
        if *preset_dialog_open {
            let mut close_requested = false;
            ctx.show_viewport_immediate(
                egui::ViewportId(egui::Id::new("new-preset-dialog")),
                egui::ViewportBuilder::default()
                    .with_title("New preset")
                    .with_inner_size([660.0, 640.0]),
                |vp_ctx, _class| {
                    egui::CentralPanel::default().show(vp_ctx, |ui| {
                        if vp_ctx.input(|i| i.viewport().close_requested()) {
                            close_requested = true;
                        }
                        close_requested |= preset_dialog_body(
                            vp_ctx,
                            shared,
                            preset_dialog,
                            theme_err,
                            preset_dialog_name,
                            preset_dialog_source,
                            &mut action,
                        );
                    });
                },
            );
            if close_requested {
                *preset_dialog_open = false;
            }
        }

if fps != app.render_fps {
            app.render_fps = fps;
        }

        if let Some(v) = toggle_startup {
            match startup::set_enabled(v) {
                Ok(()) => {
                    *startup = v;
                    *startup_err = None;
                }
                Err(e) => {
                    *startup_err = Some(format!("Could not update startup entry: {e}"));
                }
            }
        }

        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Theme/profile mutations run after the widget pass, then everything
        // the write-back diff compares against is refreshed — otherwise this
        // frame's stale snapshots would overwrite the switch.
        if let Some(a) = action {
            let structural = matches!(
                a,
                Action::NewProfile(..)
                    | Action::DuplicateProfile(..)
                    | Action::CreateCustom(..)
                    | Action::DeleteProfile(..)
            );
            match perform_action(shared, a) {
                Ok(refresh) => {
                    *theme_err = None;
                    // Targeted refresh: re-read the named profiles into
                    // their existing cards without restarting the rest.
                    for (theme, profile) in &refresh {
                        if let Some(card) = cards
                            .iter_mut()
                            .find(|c| &c.theme == theme && &c.profile == profile)
                        {
                            if let Some(es) = shared.store.load_profile(theme, profile) {
                                card.exprs = FrameExprs::build(&es);
                                card.sim.reset();
                                card.played = 0;
                            }
                        }
                    }
                    // A switch changes which profile is "current": the
                    // dialog's current-profile preview rebuilds with it.
                    if !refresh.is_empty() {
                        profile_dialog.items.clear();
                    }
                    if structural {
                        *cards_dirty = true;
                    }
                }
                Err(e) => *theme_err = Some(e),
            }
            s = shared.effects.read().unwrap().clone();
            app = shared.app.read().unwrap().clone();
            *saved = s.clone();
            *saved_app = app.clone();
        }

        // Any widget mutation lands in the shared state and on disk: effect
        // settings go to the active theme's profile file, app knobs to the
        // root config. The active card's preview replays with the new
        // settings (this is the only point where settings actually change —
        // a same-frame save/update pair is invisible to a next-frame
        // comparison).
        if s != *saved {
            *shared.effects.write().unwrap() = s.clone();
            shared.save_effects();
            let active = active_profile_in(&app, &app.theme);
            if let Some(card) = cards
                .iter_mut()
                .find(|c| c.theme == app.theme && c.profile == active)
            {
                card.exprs = FrameExprs::build(&s);
                card.sim.reset();
                card.played = 0;
            }
            // The create-profile window's "Current profile" tile follows
            // live tweaks as well.
            profile_dialog.replay("current", FrameExprs::build(&s));
            *saved = s;
        }
        if app != *saved_app {
            shared.store.save_app(&app);
            *shared.app.write().unwrap() = app.clone();
            *saved_app = app;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn profiles_group(
    ui: &mut egui::Ui,
    preview_cell_w: &mut f32,
    delete_arm: &mut Option<std::time::Instant>,
    app: &AppConfig,
    cards: &[PreviewCard],
    theme_err: &mut Option<String>,
    dialog_open: &mut bool,
    action: &mut Option<Action>,
) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Profiles").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("pick one, or + to create").small().weak(),
                );
            });
        });

        // Gallery: fixed three columns; tiles stretch to fill the row and
        // previews re-render at exactly the tile size. The width is
        // measured at the group level (the outer page scroll area has
        // already reserved its scrollbar there) — measuring inside a nested
        // ScrollArea fed its own content width back and tiles never shrank.
        let cell_w = ((ui.available_width() - 2.0 * TILE_SPACING - 8.0)
            / GALLERY_COLS as f32)
            .max(140.0);
        *preview_cell_w = cell_w;
        egui::Grid::new("profile_gallery")
            .spacing([TILE_SPACING, TILE_SPACING])
            .num_columns(GALLERY_COLS)
            .show(ui, |ui| {
                // egui's Grid does not auto-wrap at num_columns — rows must
                // be ended explicitly.
                let mut placed = 0;
                for card in cards {
                    let selected = app.theme == card.theme
                        && active_profile_in(app, &card.theme) == card.profile;
                    let resp = gallery_card(ui, card, selected, cell_w);
                    if resp.clicked() && !selected {
                        *action = Some(Action::SwitchProfile(
                            card.theme.clone(),
                            card.profile.clone(),
                        ));
                    }
                    placed += 1;
                    if placed % GALLERY_COLS == 0 {
                        ui.end_row();
                    }
                }
                let plus = plus_tile(ui, cell_w);
                if plus.clicked() {
                    *dialog_open = true;
                }
                placed += 1;
                if placed % GALLERY_COLS != 0 {
                    ui.end_row();
                }
            });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let active = active_profile_in(app, &app.theme);
            let armed = delete_arm
                .as_ref()
                .map(|t| t.elapsed().as_secs() < 4)
                .unwrap_or(false);
            let label = if armed { "Really delete?" } else { "Delete profile" };
            if ui.button(label).clicked() {
                if armed {
                    *action =
                        Some(Action::DeleteProfile(app.theme.clone(), active.clone()));
                    *delete_arm = None;
                } else {
                    *delete_arm = Some(std::time::Instant::now());
                }
            } else if !armed {
                *delete_arm = None;
            }
            if ui.button("Open presets folder").clicked() {
                *action = Some(Action::OpenFolder);
            }
        });
        if let Some(err) = &*theme_err {
            ui.colored_label(egui::Color32::RED, err.clone());
        }
    });
}

/// One gallery tile: preview image on top, profile label below. Returns the
/// click response for the whole tile.
fn gallery_card(
    ui: &mut egui::Ui,
    card: &PreviewCard,
    selected: bool,
    cell_w: f32,
) -> egui::Response {
    let (img_w, img_h) = card.sim.pixel_size();
    let fill = if selected { ui.visuals().selection.bg_fill } else { ui.visuals().faint_bg_color };
    let stroke = if selected { ui.visuals().selection.stroke } else { egui::Stroke::NONE };
    let (name_color, tag_color) = if selected {
        (egui::Color32::WHITE, egui::Color32::from_gray(215))
    } else {
        (ui.visuals().text_color(), ui.visuals().text_color())
    };
    let inner = egui::Frame::none()
        .fill(fill)
        .stroke(stroke)
        .rounding(6.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_min_width(cell_w - 12.0);
            ui.vertical(|ui| {
                if let Some(tex) = &card.texture {
                    ui.add(egui::Image::new((tex.id(), egui::vec2(img_w, img_h))));
                } else {
                    ui.set_min_size(egui::vec2(img_w, img_h));
                }
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&card.label).strong().color(name_color));
                ui.label(
                    egui::RichText::new(format!("preset: {}", card.theme))
                        .small()
                        .color(tag_color),
                );
            });
        });
    ui.interact(
        inner.response.rect,
        egui::Id::new(("profile-card", &card.theme, &card.profile)),
        egui::Sense::click(),
    )
}

/// The "+" tile that opens the create-profile dialog.
fn plus_tile(ui: &mut egui::Ui, cell_w: f32) -> egui::Response {
    // Match the gallery card's height: image + spacing + two label rows,
    // all inside the same 6px frame margins.
    let img_h = (cell_w - 12.0) * PREVIEW_ASPECT;
    let inner = egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .rounding(6.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_min_width(cell_w - 12.0);
            ui.set_min_height(img_h + 46.0);
            ui.centered_and_justified(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("+").size(17.0));
                    ui.label(egui::RichText::new("New profile").small());
                });
            });
        });
    ui.interact(
        inner.response.rect,
        egui::Id::new("profile-plus-tile"),
        egui::Sense::click(),
    )
}

/// Body of the create-profile window: a name field, then one unified list
/// of sources with live previews — the current profile (duplicated), every
/// preset — and a "+" tile that opens the new-preset window. Actions are
/// pinned to the bottom; the tile grid fills the rest and adapts its column
/// count to the window width.
fn dialog_body(
    vp_ctx: &egui::Context,
    shared: &SharedSettings,
    app: &AppConfig,
    previews: &mut DialogPreviews,
    theme_err: &mut Option<String>,
    name: &mut String,
    source: &mut Option<DialogSource>,
    open_preset_dialog: &mut bool,
    action: &mut Option<Action>,
) -> bool {
    let mut close = false;

    // Actions pinned to the bottom of the window.
    egui::TopBottomPanel::bottom("new-profile-actions").show(vp_ctx, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !name.trim().is_empty() && source.is_some();
            if ui.add_enabled(ready, egui::Button::new("Create")).clicked() {
                match source.clone() {
                    Some(DialogSource::Preset(base)) => {
                        *action = Some(Action::NewProfile(base, name.clone()));
                    }
                    Some(DialogSource::Duplicate(theme, profile)) => {
                        *action =
                            Some(Action::DuplicateProfile(theme, profile, name.clone()));
                    }
                    None => {}
                }
                name.clear();
                *source = None;
                close = true;
            }
            if ui.button("Cancel").clicked() {
                name.clear();
                *source = None;
                close = true;
            }
            if let Some(err) = &*theme_err {
                ui.colored_label(egui::Color32::RED, err.clone());
            }
        });
        ui.add_space(6.0);
    });

    egui::CentralPanel::default().show(vp_ctx, |ui| {
        ui.label("Name");
        ui.text_edit_singleline(name);
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Start from").strong());
        ui.add_space(2.0);

        // Columns adapt to the window width with a minimum tile size, so
        // the grid always fits.
        let avail = ui.available_width() - 8.0;
        let cols = ((avail + TILE_SPACING) / (MIN_TILE_W + TILE_SPACING))
            .floor()
            .max(1.0) as usize;
        let cell_w = (avail - TILE_SPACING * (cols as f32 - 1.0)) / cols as f32;
        previews.cell_w = cell_w;

        let tile = |ui: &mut egui::Ui,
                    selected: bool,
                    item: &DialogPreview|
         -> egui::Response {
            let (img_w, img_h) = item.sim.pixel_size();
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().faint_bg_color
            };
            let stroke =
                if selected { ui.visuals().selection.stroke } else { egui::Stroke::NONE };
            let inner = egui::Frame::none()
                .fill(fill)
                .stroke(stroke)
                .rounding(6.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_min_width(cell_w - 12.0);
                    ui.vertical(|ui| {
                        if let Some(tex) = &item.texture {
                            ui.add(egui::Image::new((
                                tex.id(),
                                egui::vec2(img_w, img_h),
                            )));
                        } else {
                            ui.set_min_size(egui::vec2(img_w, img_h));
                        }
                        ui.add_space(2.0);
                        if item.key == "new-preset" {
                            ui.label(
                                egui::RichText::new(&item.title).strong().size(20.0),
                            );
                        } else {
                            ui.label(egui::RichText::new(&item.title).strong());
                        }
                        ui.label(
                            egui::RichText::new(&item.sub)
                                .small()
                                .color(egui::Color32::from_gray(200)),
                        );
                    });
                });
            ui.interact(
                inner.response.rect,
                egui::Id::new(("source-tile", &item.key)),
                egui::Sense::click(),
            )
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("source_grid")
                .spacing([TILE_SPACING, TILE_SPACING])
                .num_columns(cols)
                .show(ui, |ui| {
                    let mut placed = 0;
                    for item in &*previews.items {
                        let selected = match (source.as_ref(), item.key.as_str()) {
                            (Some(DialogSource::Preset(a)), k) if
                                k.strip_prefix("preset:") == Some(a) => true,
                            (Some(DialogSource::Duplicate(t1, p1)), k) if
                                k == format!("profile:{}/{}", t1, p1) => true,
                            // The "current" tile is the active profile —
                            // highlight it while it is the duplicate source.
                            (Some(DialogSource::Duplicate(t1, p1)), "current") if
                                t1 == &app.theme
                                    && p1 == &active_profile_in(app, &app.theme) => true,
                            _ => false,
                        };
                        let resp = tile(ui, selected, item);
                        if resp.clicked() {
                            if item.key == "current" {
                                *source = Some(DialogSource::Duplicate(
                                    app.theme.clone(),
                                    active_profile_in(app, &app.theme),
                                ));
                            } else if let Some(preset) = item.key.strip_prefix("preset:") {
                                *source = Some(DialogSource::Preset(preset.to_string()));
                            } else if let Some(pair) = item.key.strip_prefix("profile:") {
                                if let Some((t, p)) = pair.split_once('/') {
                                    *source = Some(DialogSource::Duplicate(
                                        t.to_string(),
                                        p.to_string(),
                                    ));
                                }
                            }
                        }
                        placed += 1;
                        if placed % cols == 0 {
                            ui.end_row();
                        }
                    }
                    // "+" tile: opens the new-preset window.
                    let plus = plus_tile(ui, cell_w);
                    if plus.clicked() {
                        *open_preset_dialog = true;
                    }
                    placed += 1;
                    if placed % cols != 0 {
                        ui.end_row();
                    }
                });
        });
    });
    close
}

/// Body of the new-preset window: a name and one unified list of sources —
/// an empty slate, any existing preset, or any profile as a full snapshot —
/// each with a live preview tile. Actions pinned to the bottom.
fn preset_dialog_body(
    vp_ctx: &egui::Context,
    shared: &SharedSettings,
    previews: &mut DialogPreviews,
    theme_err: &mut Option<String>,
    name: &mut String,
    source: &mut Option<PresetSource>,
    action: &mut Option<Action>,
) -> bool {
    let mut close = false;

    egui::TopBottomPanel::bottom("new-preset-actions").show(vp_ctx, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ready = !name.trim().is_empty() && source.is_some();
            if ui.add_enabled(ready, egui::Button::new("Create")).clicked() {
                if let Some(src) = source.clone() {
                    *action = Some(Action::CreateCustom(name.clone(), src));
                }
                name.clear();
                *source = None;
                close = true;
            }
            if ui.button("Cancel").clicked() {
                name.clear();
                *source = None;
                close = true;
            }
            if let Some(err) = &*theme_err {
                ui.colored_label(egui::Color32::RED, err.clone());
            }
        });
        ui.add_space(6.0);
    });

    egui::CentralPanel::default().show(vp_ctx, |ui| {
        ui.label("Name");
        ui.text_edit_singleline(name);
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Start from").strong());
        ui.add_space(2.0);

        let avail = ui.available_width() - 8.0;
        let cols = ((avail + TILE_SPACING) / (MIN_TILE_W + TILE_SPACING))
            .floor()
            .max(1.0) as usize;
        let cell_w = (avail - TILE_SPACING * (cols as f32 - 1.0)) / cols as f32;
        previews.cell_w = cell_w;

        let tile = |ui: &mut egui::Ui,
                    selected: bool,
                    item: &DialogPreview|
         -> egui::Response {
            let (img_w, img_h) = item.sim.pixel_size();
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().faint_bg_color
            };
            let stroke =
                if selected { ui.visuals().selection.stroke } else { egui::Stroke::NONE };
            let inner = egui::Frame::none()
                .fill(fill)
                .stroke(stroke)
                .rounding(6.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_min_width(cell_w - 12.0);
                    ui.vertical(|ui| {
                        if let Some(tex) = &item.texture {
                            ui.add(egui::Image::new((
                                tex.id(),
                                egui::vec2(img_w, img_h),
                            )));
                        } else {
                            ui.set_min_size(egui::vec2(img_w, img_h));
                        }
                        ui.add_space(2.0);
                        if item.key == "preset:" {
                            ui.label(
                                egui::RichText::new(&item.title).strong().size(20.0),
                            );
                        } else {
                            ui.label(egui::RichText::new(&item.title).strong());
                        }
                        ui.label(
                            egui::RichText::new(&item.sub)
                                .small()
                                .color(egui::Color32::from_gray(200)),
                        );
                    });
                });
            ui.interact(
                inner.response.rect,
                egui::Id::new(("preset-source-tile", &item.key)),
                egui::Sense::click(),
            )
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("preset_source_grid")
                .spacing([TILE_SPACING, TILE_SPACING])
                .num_columns(cols)
                .show(ui, |ui| {
                    let mut placed = 0;
                    for item in &*previews.items {
                        let selected = match (source.as_ref(), item.key.as_str()) {
                            (Some(PresetSource::Empty), "empty") => true,
                            (Some(PresetSource::CopyPreset(a)), k) if
                                k.strip_prefix("preset:") == Some(a) => true,
                            (Some(PresetSource::CopyProfile(t1, p1)), k) if
                                k == format!("profile:{}/{}", t1, p1) => true,
                            _ => false,
                        };
                        let resp = tile(ui, selected, item);
                        if resp.clicked() {
                            if item.key == "empty" {
                                *source = Some(PresetSource::Empty);
                            } else if let Some(preset) = item.key.strip_prefix("preset:") {
                                *source = Some(PresetSource::CopyPreset(preset.to_string()));
                            } else if let Some(pair) = item.key.strip_prefix("profile:") {
                                if let Some((t, p)) = pair.split_once('/') {
                                    *source = Some(PresetSource::CopyProfile(
                                        t.to_string(),
                                        p.to_string(),
                                    ));
                                }
                            }
                        }
                        placed += 1;
                        if placed % cols == 0 {
                            ui.end_row();
                        }
                    }
                });
        });
    });
    close
}

fn trail_section(ui: &mut egui::Ui, s: &mut EffectSettings) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.trail.enabled, "Mouse Trail");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_source("trail_style")
                    .selected_text(match s.trail.style {
                        TrailStyle::Rainbow => "Rainbow ribbon",
                        TrailStyle::Neon => "Neon comet",
                        TrailStyle::Ghost => "Ghost fade",
                        TrailStyle::Soft => "Soft glow",
                        TrailStyle::Striped => "Candy stripes",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Rainbow, "Rainbow ribbon");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Neon, "Neon comet");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Ghost, "Ghost fade");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Soft, "Soft glow");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Striped, "Candy stripes");
                    });
            });
        });
        ui.label(
            egui::RichText::new("A ribbon that flows behind the cursor.").small().weak(),
        );
        if s.trail.enabled {
            ui.add(
                egui::Slider::new(&mut s.trail.length_secs, 0.15..=1.5)
                    .text("Length")
                    .suffix(" s"),
            );
            num_or_expr_slider(
                ui,
                &mut s.trail.width,
                2.0..=24.0,
                "Thickness",
            );
            match s.trail.style {
                TrailStyle::Rainbow => {
                    num_or_expr_slider(
                        ui,
                        &mut s.trail.hue_speed,
                        0.0..=3.0,
                        "Rainbow speed",
                    );
                }
                TrailStyle::Neon | TrailStyle::Ghost => {
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.color_edit_button_srgb(&mut s.trail.color);
                    });
                }
                TrailStyle::Soft | TrailStyle::Striped => {
                    palette_row(ui, &mut s.trail.palette);
                    if s.trail.style == TrailStyle::Striped {
                        ui.add(
                            egui::Slider::new(&mut s.trail.stripe_len, 6.0..=40.0)
                                .text("Stripe length")
                                .suffix(" px"),
                        );
                    }
                }
            }
        }
    });
}

/// Editor for a palette: one color swatch per entry plus add/remove. An empty
/// palette means the style's built-in default colors are used.
fn palette_row(ui: &mut egui::Ui, palette: &mut Vec<[u8; 3]>) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Colors");
        if palette.is_empty() {
            ui.label(egui::RichText::new("(style default)").small().weak());
        }
        let mut remove: Option<usize> = None;
        for (i, c) in palette.iter_mut().enumerate() {
            ui.color_edit_button_srgb(c);
            if ui.small_button("\u{00D7}").clicked() {
                remove = Some(i);
            }
        }
        if ui.small_button("+").clicked() {
            palette.push([200, 200, 200]);
        }
        if let Some(i) = remove {
            palette.remove(i);
        }
    });
}

/// Slider for a plain number, or a monospace expression editor when the
/// field holds an expression (evaluated once per frame — see `expr`).
/// Dragging the slider converts an expression back to a constant parsed
/// from its own text.
fn num_or_expr_slider(
    ui: &mut egui::Ui,
    field: &mut NumOrExpr,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
) {
    match field {
        NumOrExpr::Num(n) => {
            let mut v = *n;
            if ui.add(egui::Slider::new(&mut v, range).text(text)).changed() {
                *field = NumOrExpr::Num(v);
            }
        }
        NumOrExpr::Expr(src) => {
            let mut revert = false;
            ui.horizontal(|ui| {
                ui.label(text);
                ui.add(
                    egui::TextEdit::singleline(src)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(180.0),
                );
                revert = ui.small_button("=").clicked();
            });
            // The overlay silently falls back to the field default on a bad
            // expression — flag it right at the field so the typo is seen.
            if let Err(e) = crate::expr::validate(src) {
                ui.label(egui::RichText::new(e).small().color(egui::Color32::RED));
            }
            if revert {
                let parsed = src.trim().parse::<f32>().unwrap_or(*range.start());
                *field = NumOrExpr::Num(parsed.clamp(*range.start(), *range.end()));
            }
        }
    }
}

fn bubbles_section(ui: &mut egui::Ui, s: &mut EffectSettings) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.bubbles.enabled, "Bubbles");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_source("bubble_shape")
                    .selected_text(match s.bubbles.shape {
                        BubbleShape::Bubble => "Soap bubbles",
                        BubbleShape::Snowflake => "Snowflakes",
                        BubbleShape::Ornament => "Ornament balls",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.bubbles.shape, BubbleShape::Bubble, "Soap bubbles");
                        ui.selectable_value(&mut s.bubbles.shape, BubbleShape::Snowflake, "Snowflakes");
                        ui.selectable_value(&mut s.bubbles.shape, BubbleShape::Ornament, "Ornament balls");
                    });
            });
        });
        ui.label(
            egui::RichText::new(
                "Bubbles drift up from the cursor — more when you move fast, fewer when you drift slowly. Click to blast them outward.",
            )
            .small()
            .weak(),
        );
        if s.bubbles.enabled {
            num_or_expr_slider(
                ui,
                &mut s.bubbles.rate,
                1.0..=30.0,
                "Bubbles per second",
            );
            ui.add(egui::Slider::new(&mut s.bubbles.size, 4.0..=64.0).text("Size"));
            num_or_expr_slider(
                ui,
                &mut s.bubbles.buoyancy,
                -300.0..=300.0,
                "Float (negative falls)",
            );
            ui.add(
                egui::Slider::new(&mut s.bubbles.lifetime, 0.5..=5.0)
                    .text("Lifetime")
                    .suffix(" s"),
            );
            num_or_expr_slider(
                ui,
                &mut s.bubbles.wobble,
                0.0..=100.0,
                "Wobble",
            );
            ui.add(
                egui::Slider::new(&mut s.bubbles.momentum, 0.0..=1.0)
                    .text("Cursor momentum"),
            );
            ui.add(egui::Slider::new(&mut s.bubbles.drag, 0.0..=10.0).text("Air drag"));
            if s.bubbles.shape != BubbleShape::Bubble {
                num_or_expr_slider(
                    ui,
                    &mut s.bubbles.spin,
                    0.0..=3.0,
                    "Spin",
                );
            }
            palette_row(ui, &mut s.bubbles.palette);
        }
    });
}

fn sparkles_section(ui: &mut egui::Ui, s: &mut EffectSettings) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.sparkles.enabled, "Sparkles");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_source("spark_shape")
                    .selected_text(match s.sparkles.shape {
                        SparkShape::Star => "Stars",
                        SparkShape::Dot => "Dots",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.sparkles.shape, SparkShape::Star, "Stars");
                        ui.selectable_value(&mut s.sparkles.shape, SparkShape::Dot, "Dots");
                    });
            });
        });
        ui.label(
            egui::RichText::new("Tiny sparkles scatter as the cursor moves.").small().weak(),
        );
        if s.sparkles.enabled {
            num_or_expr_slider(
                ui,
                &mut s.sparkles.rate,
                1.0..=60.0,
                "Sparkles per second",
            );
            ui.add(egui::Slider::new(&mut s.sparkles.size, 1.0..=12.0).text("Size"));
            num_or_expr_slider(
                ui,
                &mut s.sparkles.gravity,
                -200.0..=200.0,
                "Gravity",
            );
            ui.add(
                egui::Slider::new(&mut s.sparkles.lifetime, 0.3..=3.0)
                    .text("Lifetime")
                    .suffix(" s"),
            );
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgb(&mut s.sparkles.color);
            });
            palette_row(ui, &mut s.sparkles.palette);
            ui.add(
                egui::Slider::new(&mut s.sparkles.momentum, 0.0..=1.0)
                    .text("Cursor momentum"),
            );
            ui.add(egui::Slider::new(&mut s.sparkles.drag, 0.0..=10.0).text("Air drag"));
            ui.add(
                egui::Slider::new(&mut s.sparkles.streak_secs, 0.0..=0.5)
                    .text("Motion streak")
                    .suffix(" s"),
            );
            ui.add(
                egui::Slider::new(&mut s.sparkles.child_rate, 0.0..=60.0)
                    .text("Child sparks per second"),
            );
            if s.sparkles.child_rate > 0.0 {
                ui.add(
                    egui::Slider::new(&mut s.sparkles.child_size, 0.2..=1.0)
                        .text("Child size"),
                );
                ui.add(
                    egui::Slider::new(&mut s.sparkles.max_generations, 1..=4)
                        .text("Generations"),
                );
            }
        }
    });
}

fn ripples_section(ui: &mut egui::Ui, s: &mut EffectSettings) {
    ui.group(|ui| {
        ui.checkbox(&mut s.ripples.enabled, "Click ripples");
        ui.label(
            egui::RichText::new("Rings ripple out from the cursor when you click.")
                .small()
                .weak(),
        );
        if s.ripples.enabled {
            ui.add(egui::Slider::new(&mut s.ripples.rings, 1..=4).text("Rings"));
            ui.add(
                egui::Slider::new(
                    &mut s.ripples.speed,
                    150.0..=1200.0,
                )
                .text("Expand speed"),
            );
            ui.add(
                egui::Slider::new(&mut s.ripples.lifetime, 0.2..=1.5)
                    .text("Lifetime")
                    .suffix(" s"),
            );
            ui.add(
                egui::Slider::new(
                    &mut s.ripples.width,
                    1.0..=12.0,
                )
                .text("Line width"),
            );
            ui.add(
                egui::Slider::new(&mut s.ripples.wobble, 0.0..=0.35)
                    .text("Wobble (at birth)"),
            );
            ui.add(
                egui::Slider::new(&mut s.ripples.wobble_end, 0.0..=0.35)
                    .text("Wobble (at end)"),
            );
            ui.add(
                egui::Slider::new(&mut s.ripples.wobble_lobes, 1..=24)
                    .text("Wobble lobes"),
            );
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgb(&mut s.ripples.color);
            });
            palette_row(ui, &mut s.ripples.palette);
        }
    });
}
