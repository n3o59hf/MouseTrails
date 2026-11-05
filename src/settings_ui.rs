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
        new_profile_name: String::new(),
        new_theme_name: String::new(),
        window_visible: true,
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MouseTrails Settings")
            .with_inner_size([520.0, 780.0])
            .with_min_inner_size([460.0, 560.0])
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
    /// Error from the last theme/profile action, shown until it clears.
    theme_err: Option<String>,
    new_profile_name: String,
    new_theme_name: String,
    /// Tracked hide/show state of the window (drives preview animation).
    window_visible: bool,
}

struct PreviewCard {
    id: String,
    name: String,
    desc: String,
    builtin: bool,
    sim: ThemePreview,
    exprs: FrameExprs,
    texture: Option<egui::TextureHandle>,
    /// Sim frames played; the preview freezes at PREVIEW_FRAMES and replays
    /// when the theme's parameters change.
    played: u32,
}

const PREVIEW_W: u32 = 132;
const PREVIEW_H: u32 = 74;
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

/// (Re)build the preview cards. Expensive-ish (file reads + parses), so it
/// only runs when the theme set or profiles change.
fn build_cards(shared: &SharedSettings) -> (Vec<PreviewCard>, Option<ThemeDef>) {
    let app = shared.app.read().unwrap();
    let mut cards = Vec::new();
    let mut active_def = None;
    for id in theme_ids(shared) {
        let def = themes::load_def(&shared.store, &id).unwrap_or_default();
        let (name, desc) = if def.name.is_empty() {
            (id.clone(), String::new())
        } else {
            (def.name.clone(), def.description.clone())
        };
        let profile = active_profile_in(&app, &id);
        let es = shared.store.load_profile(&id, &profile).unwrap_or_else(|| {
            // Not materialized yet: preview the definition over factory
            // defaults without writing anything.
            themes::apply_def(&def, &EffectSettings::default())
        });
        let is_active = id == app.theme;
        if is_active {
            active_def = Some(def.clone());
        }
        let is_builtin = themes::builtin(&id).is_some();
        cards.push(PreviewCard {
            id,
            name,
            desc,
            builtin: is_builtin,
            sim: ThemePreview::new(PREVIEW_W, PREVIEW_H),
            exprs: FrameExprs::build(&es),
            texture: None,
            played: 0,
        });
    }
    (cards, active_def)
}

/// The profile active for `theme` in `app` ("default" unless recorded).
fn active_profile_in(app: &AppConfig, theme: &str) -> String {
    app.profiles.get(theme).cloned().unwrap_or_else(|| store::DEFAULT_PROFILE.into())
}

/// Deferred theme/profile mutations, applied after the widget pass so the
/// snapshots can be refreshed before the write-back diff runs.
#[derive(Debug)]
enum Action {
    SwitchTheme(String),
    SwitchProfile(String),
    NewProfile(String),
    DeleteProfile(String),
    CreateCustom(String),
    DeleteTheme(String),
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

fn perform_action(shared: &SharedSettings, action: Action) -> Result<(), String> {
    match action {
        Action::SwitchTheme(id) => {
            let profile = shared.active_profile(&id);
            let es = load_or_materialize(shared, &id, &profile);
            *shared.effects.write().unwrap() = es;
            let mut app = shared.app.write().unwrap();
            app.theme = id;
            shared.store.save_app(&app);
            Ok(())
        }
        Action::SwitchProfile(profile) => {
            let theme = shared.app.read().unwrap().theme.clone();
            let es = load_or_materialize(shared, &theme, &profile);
            *shared.effects.write().unwrap() = es;
            let mut app = shared.app.write().unwrap();
            app.profiles.insert(theme, profile);
            shared.store.save_app(&app);
            Ok(())
        }
        Action::NewProfile(name) => {
            let theme = shared.app.read().unwrap().theme.clone();
            let id = store::sanitize_id(&name)
                .ok_or_else(|| "Profile name is empty or reserved.".to_string())?;
            if shared.store.profile_exists(&theme, &id) {
                return Err(format!("A profile named \u{201C}{}\u{201D} already exists.", id));
            }
            let current = shared.effects.read().unwrap().clone();
            shared.store.save_profile(&theme, &id, &current);
            perform_action(shared, Action::SwitchProfile(id))
        }
        Action::DeleteProfile(profile) => {
            let theme = shared.app.read().unwrap().theme.clone();
            // If it's the active one, move to another profile first.
            if shared.active_profile(&theme) == profile {
                let remaining: Vec<String> = shared
                    .store
                    .list_profiles(&theme)
                    .into_iter()
                    .filter(|p| *p != profile)
                    .collect();
                match remaining.first() {
                    Some(next) => {
                        let es = shared
                            .store
                            .load_profile(&theme, next)
                            .unwrap_or_else(|| themes::materialize_profile(&shared.store, &theme));
                        *shared.effects.write().unwrap() = es;
                        let mut app = shared.app.write().unwrap();
                        app.profiles.insert(theme.clone(), next.clone());
                        shared.store.save_app(&app);
                    }
                    None => return Err("Every theme keeps at least one profile.".to_string()),
                }
            }
            if !shared.store.delete_profile(&theme, &profile) {
                return Err(format!("Could not delete profile \u{201C}{}\u{201D}.", profile));
            }
            Ok(())
        }
        Action::CreateCustom(name) => {
            let display = name.trim().to_string();
            let id = store::sanitize_id(&name)
                .ok_or_else(|| "Theme name is empty or reserved.".to_string())?;
            if themes::builtin(&id).is_some() || shared.store.theme_exists(&id) {
                return Err(format!("A theme named \u{201C}{}\u{201D} already exists.", id));
            }
            // The custom theme starts as a copy of the current theme's
            // definition — edit it on disk (Open themes folder) to restyle.
            let current_theme = shared.app.read().unwrap().theme.clone();
            let mut def = themes::load_def(&shared.store, &current_theme).unwrap_or_default();
            def.name = if display.is_empty() { id.clone() } else { display };
            shared.store.save_theme_def(&id, &def);
            let current = shared.effects.read().unwrap().clone();
            let es = themes::apply_def(&def, &current);
            shared.store.save_profile(&id, store::DEFAULT_PROFILE, &es);
            perform_action(shared, Action::SwitchTheme(id))
        }
        Action::DeleteTheme(id) => {
            if themes::builtin(&id).is_some() {
                return Err("Built-in themes can't be deleted.".to_string());
            }
            if shared.app.read().unwrap().theme == id {
                perform_action(shared, Action::SwitchTheme("default".into()))?;
            }
            if !shared.store.delete_theme(&id) {
                return Err(format!("Could not delete theme \u{201C}{}\u{201D}.", id));
            }
            let mut app = shared.app.write().unwrap();
            app.profiles.remove(&id);
            shared.store.save_app(&app);
            Ok(())
        }
        Action::OpenFolder => {
            let dir = shared.store.base().join("themes");
            let _ = std::fs::create_dir_all(&dir);
            std::process::Command::new("explorer")
                .arg(&dir)
                .spawn()
                .map_err(|e| format!("Could not open the folder: {}", e))?;
            Ok(())
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

        // Split the struct so the egui closures below can capture disjoint
        // fields without borrowing all of self.
        let SettingsApp {
            shared,
            cards,
            cards_dirty,
            active_def,
            theme_err,
            new_profile_name,
            new_theme_name,
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
        let any_playing = cards.iter().any(|c| c.played < PREVIEW_FRAMES);
        ctx.request_repaint_after(std::time::Duration::from_millis(
            if visible && any_playing { 33 } else { 150 },
        ));

        if *cards_dirty {
            let (c, d) = build_cards(shared);
            *cards = c;
            *active_def = d;
            *cards_dirty = false;
        }
        // Live-follow: re-resolve the active card and replay its animation
        // whenever tweaks land.
        if s != *saved {
            if let Some(card) = cards.iter_mut().find(|c| c.id == app.theme) {
                card.exprs = FrameExprs::build(&s);
                card.sim.reset();
                card.played = 0;
            }
        }
        if visible {
            for card in cards.iter_mut() {
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
                                format!("theme-preview-{}", card.id),
                                img,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                }
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
                themes_group(
                    ui,
                    shared,
                    &app,
                    cards,
                    theme_err,
                    new_profile_name,
                    new_theme_name,
                    &mut action,
                );
                ui.add_space(6.0);
                trail_section(ui, &mut s, active_def.as_ref());
                ui.add_space(6.0);
                bubbles_section(ui, &mut s, active_def.as_ref());
                ui.add_space(6.0);
                sparkles_section(ui, &mut s, active_def.as_ref());
                ui.add_space(6.0);
                ripples_section(ui, &mut s, active_def.as_ref());
                ui.add_space(6.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Performance").strong());
                    ui.add(
                        egui::Slider::new(&mut fps, 24..=120).text("Render FPS").suffix(" fps"),
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
            *theme_err = perform_action(shared, a).err();
            *cards_dirty = true;
            s = shared.effects.read().unwrap().clone();
            app = shared.app.read().unwrap().clone();
            *saved = s.clone();
            *saved_app = app.clone();
        }

        // Any widget mutation lands in the shared state and on disk: effect
        // settings go to the active theme's profile file, app knobs to the
        // root config.
        if s != *saved {
            *shared.effects.write().unwrap() = s.clone();
            shared.save_effects();
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
fn themes_group(
    ui: &mut egui::Ui,
    shared: &SharedSettings,
    app: &AppConfig,
    cards: &[PreviewCard],
    theme_err: &mut Option<String>,
    new_profile_name: &mut String,
    new_theme_name: &mut String,
    action: &mut Option<Action>,
) {
    ui.group(|ui| {
        ui.label(egui::RichText::new("Themes").strong());
        for card in cards {
            let selected = app.theme == card.id;
            let fill = if selected {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().faint_bg_color
            };
            let stroke =
                if selected { ui.visuals().selection.stroke } else { egui::Stroke::NONE };
            // Explicit colors: the default weak gray is unreadable on the
            // selection-blue card fill.
            let (name_color, desc_color) = if selected {
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
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if let Some(tex) = &card.texture {
                            ui.add(egui::Image::new((
                                tex.id(),
                                egui::vec2(PREVIEW_W as f32, PREVIEW_H as f32),
                            )));
                        } else {
                            ui.add_space(PREVIEW_W as f32 + 2.0);
                        }
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&card.name)
                                        .strong()
                                        .color(name_color),
                                );
                                if selected {
                                    ui.label(
                                        egui::RichText::new("active")
                                            .small()
                                            .strong()
                                            .color(name_color),
                                    );
                                } else if !card.builtin {
                                    ui.label(egui::RichText::new("custom").small().weak());
                                }
                            });
                            // Body-size description (small was hard to read).
                            ui.label(egui::RichText::new(&card.desc).color(desc_color));
                        });
                    });
                });
            let resp = ui.interact(
                inner.response.rect,
                egui::Id::new(("theme-card", card.id.as_str())),
                egui::Sense::click(),
            );
            if resp.clicked() && !selected {
                *action = Some(Action::SwitchTheme(card.id.clone()));
            }
            ui.add_space(2.0);
        }

        // Profile management for the active theme.
        ui.add_space(4.0);
        let theme = app.theme.clone();
        let profiles = shared.store.list_profiles(&theme);
        let active = active_profile_in(app, &theme);
        ui.horizontal(|ui| {
            ui.label("Profile");
            let mut picked: Option<String> = None;
            egui::ComboBox::from_id_source("profile_combo")
                .selected_text(active.clone())
                .show_ui(ui, |ui| {
                    for p in &profiles {
                        if ui.selectable_label(*p == active, p).clicked() {
                            picked = Some(p.clone());
                        }
                    }
                });
            if let Some(p) = picked {
                *action = Some(Action::SwitchProfile(p));
            }
            if ui.button("Delete").clicked() {
                *action = Some(Action::DeleteProfile(active.clone()));
            }
        });
        ui.horizontal(|ui| {
            ui.label("New:");
            ui.text_edit_singleline(new_profile_name);
            if ui.button("Create profile").clicked() {
                *action = Some(Action::NewProfile(new_profile_name.clone()));
                new_profile_name.clear();
            }
        });

        // Custom theme management.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label("New theme:");
            ui.text_edit_singleline(new_theme_name);
            if ui.button("Create from current").clicked() {
                *action = Some(Action::CreateCustom(new_theme_name.clone()));
                new_theme_name.clear();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Open themes folder").clicked() {
                *action = Some(Action::OpenFolder);
            }
            let active_builtin =
                cards.iter().find(|c| c.id == app.theme).map(|c| c.builtin).unwrap_or(true);
            if !active_builtin && ui.button("Delete this theme").clicked() {
                *action = Some(Action::DeleteTheme(app.theme.clone()));
            }
        });
        if let Some(err) = &*theme_err {
            ui.colored_label(egui::Color32::RED, err.clone());
        }
    });
}

/// Slider range for a field, overridden by the active theme's bounds.
fn bounds_range(
    def: Option<&ThemeDef>,
    key: &str,
    lo: f32,
    hi: f32,
) -> std::ops::RangeInclusive<f32> {
    match def.and_then(|d| d.bounds.get(key)).copied() {
        Some([a, b]) if b > a => a..=b,
        _ => lo..=hi,
    }
}

fn trail_section(ui: &mut egui::Ui, s: &mut EffectSettings, def: Option<&ThemeDef>) {
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
                bounds_range(def, "trail.width", 2.0, 24.0),
                "Thickness",
            );
            match s.trail.style {
                TrailStyle::Rainbow => {
                    num_or_expr_slider(
                        ui,
                        &mut s.trail.hue_speed,
                        bounds_range(def, "trail.hue_speed", 0.0, 3.0),
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
            if ui.small_button("\u{2715}").clicked() {
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
            if revert {
                let parsed = src.trim().parse::<f32>().unwrap_or(*range.start());
                *field = NumOrExpr::Num(parsed.clamp(*range.start(), *range.end()));
            }
        }
    }
}

fn bubbles_section(ui: &mut egui::Ui, s: &mut EffectSettings, def: Option<&ThemeDef>) {
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
                bounds_range(def, "bubbles.rate", 1.0, 30.0),
                "Bubbles per second",
            );
            ui.add(
                egui::Slider::new(&mut s.bubbles.size, bounds_range(def, "bubbles.size", 4.0, 40.0))
                    .text("Size"),
            );
            num_or_expr_slider(
                ui,
                &mut s.bubbles.buoyancy,
                bounds_range(def, "bubbles.buoyancy", -300.0, 300.0),
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
                bounds_range(def, "bubbles.wobble", 0.0, 100.0),
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
                    bounds_range(def, "bubbles.spin", 0.0, 3.0),
                    "Spin",
                );
            }
            palette_row(ui, &mut s.bubbles.palette);
        }
    });
}

fn sparkles_section(ui: &mut egui::Ui, s: &mut EffectSettings, def: Option<&ThemeDef>) {
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
                bounds_range(def, "sparkles.rate", 1.0, 60.0),
                "Sparkles per second",
            );
            ui.add(
                egui::Slider::new(
                    &mut s.sparkles.size,
                    bounds_range(def, "sparkles.size", 1.0, 12.0),
                )
                .text("Size"),
            );
            num_or_expr_slider(
                ui,
                &mut s.sparkles.gravity,
                bounds_range(def, "sparkles.gravity", -200.0, 200.0),
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

fn ripples_section(ui: &mut egui::Ui, s: &mut EffectSettings, def: Option<&ThemeDef>) {
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
                    bounds_range(def, "ripples.speed", 150.0, 1200.0),
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
                    bounds_range(def, "ripples.width", 1.0, 12.0),
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
