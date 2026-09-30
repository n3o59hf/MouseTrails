use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use eframe::egui;
use windows::core::{w, PCWSTR};

use crate::icon;
use crate::settings::{self, Settings, TrailStyle};
use crate::startup;

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

    pub fn open(&mut self, shared: &Arc<settings::SharedSettings>) {
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

fn run_ui(shared: Arc<settings::SharedSettings>, rx: Receiver<UiMsg>) {
    let icon_rgba = icon::icon_rgba_straight();
    let icon = egui::IconData { width: 32, height: 32, rgba: icon_rgba };

    let saved = shared.settings.read().unwrap().clone();
    let app = SettingsApp {
        shared,
        rx,
        startup: startup::is_enabled(),
        startup_err: None,
        saved,
        updates: 0,
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MouseTrails Settings")
            .with_inner_size([440.0, 660.0])
            .with_min_inner_size([400.0, 480.0])
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
fn hide_settings_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    // The first time the settings window goes away, tell the user where
    // MouseTrails lives from now on.
    if !settings::tray_hint_shown() {
        settings::set_tray_hint_shown();
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
    shared: Arc<settings::SharedSettings>,
    rx: Receiver<UiMsg>,
    startup: bool,
    startup_err: Option<String>,
    saved: Settings,
    updates: u64,
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.updates += 1;
        if std::env::var("MOUSETRAILS_DEBUG").is_ok() && self.updates % 100 == 0 {
            crate::overlay::debug_log(format!("ui: update #{} visible={}", self.updates,
                true));
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                UiMsg::Show => {
                    if std::env::var("MOUSETRAILS_DEBUG").is_ok() {
                        crate::overlay::debug_log("ui: Show received".into());
                    }
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
        // Keep polling the channel even when nothing else repaints.
        ctx.request_repaint_after(std::time::Duration::from_millis(150));

        // Intercept both the X button and the Close button — hide, don't die.
        if ctx.input(|i| i.viewport().close_requested()) {
            if std::env::var("MOUSETRAILS_DEBUG").is_ok() {
                crate::overlay::debug_log("ui: close_requested intercepted".into());
            }
            hide_settings_window(ctx);
            return;
        }

        let mut s = self.shared.settings.read().unwrap().clone();
        let mut startup = self.startup;
        let startup_err: Option<String> = None;
        let mut toggle_startup: Option<bool> = None;
        let mut close = false;

        // Footer as a bottom panel so it's always visible — putting it inside
        // the CentralPanel let the scroll area push it below the window edge.
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.checkbox(&mut startup, "Start with Windows").changed() {
                    toggle_startup = Some(startup);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui.button("Reset to defaults").clicked() {
                        // Keep UI-state flags; reset everything else.
                        s = Settings { tray_hint_shown: s.tray_hint_shown, ..Settings::default() };
                    }
                });
            });
            if let Some(err) = &startup_err {
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
                    ui.label(
                        egui::RichText::new("fun for your cursor").small().weak(),
                    );
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
                        egui::Slider::new(&mut s.render_fps, 24..=120)
                            .text("Render FPS")
                            .suffix(" fps"),
                    );
                });
                ui.add_space(6.0);
                let (status, available) = {
                    let st = self.shared.update.lock().unwrap();
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
                            crate::updater::spawn_check(self.shared.clone());
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Install and restart"))
                            .clicked()
                        {
                            crate::updater::spawn_install(self.shared.clone());
                        }
                    });
                });
            });
        });

        if let Some(v) = toggle_startup {
            match startup::set_enabled(v) {
                Ok(()) => {
                    self.startup = v;
                    self.startup_err = None;
                }
                Err(e) => {
                    self.startup_err = Some(format!("Could not update startup entry: {e}"));
                }
            }
        } else {
            self.startup = startup;
            self.startup_err = startup_err;
        }

        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if s != self.saved {
            settings::Settings::save(&s);
            *self.shared.settings.write().unwrap() = s.clone();
            self.saved = s;
        }
    }
}

fn trail_section(ui: &mut egui::Ui, s: &mut Settings) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.trail.enabled, "Mouse Trail");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_source("trail_style")
                    .selected_text(match s.trail.style {
                        TrailStyle::Rainbow => "Rainbow ribbon",
                        TrailStyle::Neon => "Neon comet",
                        TrailStyle::Ghost => "Ghost fade",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Rainbow, "Rainbow ribbon");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Neon, "Neon comet");
                        ui.selectable_value(&mut s.trail.style, TrailStyle::Ghost, "Ghost fade");
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
            ui.add(egui::Slider::new(&mut s.trail.width, 2.0..=24.0).text("Thickness"));
            match s.trail.style {
                TrailStyle::Rainbow => {
                    ui.add(
                        egui::Slider::new(&mut s.trail.hue_speed, 0.0..=3.0)
                            .text("Rainbow speed"),
                    );
                }
                TrailStyle::Neon | TrailStyle::Ghost => {
                    ui.horizontal(|ui| {
                        ui.label("Color");
                        ui.color_edit_button_srgb(&mut s.trail.color);
                    });
                }
            }
        }
    });
}

fn bubbles_section(ui: &mut egui::Ui, s: &mut Settings) {
    ui.group(|ui| {
        ui.checkbox(&mut s.bubbles.enabled, "Bubbles");
        ui.label(
            egui::RichText::new(
                "Soap bubbles drift up from the cursor — more when you move fast, fewer when you drift slowly. Click to blast them outward.",
            )
            .small()
            .weak(),
        );
        if s.bubbles.enabled {
            ui.add(
                egui::Slider::new(&mut s.bubbles.rate, 1.0..=30.0)
                    .text("Bubbles per second"),
            );
            ui.add(egui::Slider::new(&mut s.bubbles.size, 4.0..=40.0).text("Size"));
            ui.add(egui::Slider::new(&mut s.bubbles.buoyancy, 0.0..=300.0).text("Float strength"));
            ui.add(
                egui::Slider::new(&mut s.bubbles.lifetime, 0.5..=5.0)
                    .text("Lifetime")
                    .suffix(" s"),
            );
            ui.add(egui::Slider::new(&mut s.bubbles.wobble, 0.0..=100.0).text("Wobble"));
        }
    });
}

fn sparkles_section(ui: &mut egui::Ui, s: &mut Settings) {
    ui.group(|ui| {
        ui.checkbox(&mut s.sparkles.enabled, "Sparkles");
        ui.label(
            egui::RichText::new("Tiny sparkles scatter as the cursor moves.").small().weak(),
        );
        if s.sparkles.enabled {
            ui.add(
                egui::Slider::new(&mut s.sparkles.rate, 1.0..=60.0)
                    .text("Sparkles per second"),
            );
            ui.add(egui::Slider::new(&mut s.sparkles.size, 1.0..=12.0).text("Size"));
            ui.add(
                egui::Slider::new(&mut s.sparkles.gravity, -200.0..=200.0).text("Gravity"),
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
        }
    });
}

fn ripples_section(ui: &mut egui::Ui, s: &mut Settings) {
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
                egui::Slider::new(&mut s.ripples.speed, 150.0..=1200.0).text("Expand speed"),
            );
            ui.add(
                egui::Slider::new(&mut s.ripples.lifetime, 0.2..=1.5)
                    .text("Lifetime")
                    .suffix(" s"),
            );
            ui.add(egui::Slider::new(&mut s.ripples.width, 1.0..=12.0).text("Line width"));
            ui.horizontal(|ui| {
                ui.label("Color");
                ui.color_edit_button_srgb(&mut s.ripples.color);
            });
        }
    });
}
