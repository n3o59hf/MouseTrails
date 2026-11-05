//! Animated theme previews for the settings window. Each preview runs its
//! own tiny `Effects` simulation on a small offscreen pixmap, driven by a
//! synthetic cursor on a lively Lissajous path, and hands the frame to egui
//! as a texture.
//!
//! Color note: the effect code creates colors through `col()`, which
//! pre-swaps R and B for the overlay's BGRA layered-window bitmap. tiny-skia
//! therefore stores [B G R A] bytes (premultiplied). egui wants premultiplied
//! RGBA, so converting is a pure channel swap of bytes 0 and 2 — no
//! un-premultiplying needed.

use eframe::egui;

use crate::effects::{Ctx, Effects};
use crate::expr::FrameExprs;

/// Fixed preview timestep — deterministic and cheap, ~30 steps/s.
const FIXED_DT: f32 = 1.0 / 30.0;

pub struct ThemePreview {
    effects: Effects,
    pixmap: tiny_skia::Pixmap,
    /// Simulation clock (s), also the `t` expression variable.
    t: f32,
    mouse: (f32, f32),
    vel: (f32, f32),
    /// Countdown to the next synthetic click (s) — shows blasts and ripples.
    next_click: f32,
    image: Option<egui::ColorImage>,
}

impl ThemePreview {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            effects: Effects::new(),
            pixmap: tiny_skia::Pixmap::new(w, h).expect("preview pixmap"),
            t: 0.0,
            mouse: (w as f32 * 0.5, h as f32 * 0.5),
            vel: (0.0, 0.0),
            next_click: 2.0,
            image: None,
        }
    }

    /// Restart the animation from the beginning (parameter change). The last
    /// rendered frame stays on screen until the next step overwrites it.
    pub fn reset(&mut self) {
        self.effects = Effects::new();
        self.t = 0.0;
        let (w, h) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        self.mouse = (w * 0.5, h * 0.5);
        self.vel = (0.0, 0.0);
        self.next_click = 2.0;
    }

    /// Advance one fixed step, resolving the theme's expressions against the
    /// synthetic cursor state, and re-render.
    pub fn step(&mut self, exprs: &FrameExprs) {
        let (w, h) = (self.pixmap.width() as f32, self.pixmap.height() as f32);
        self.t += FIXED_DT;

        // A lazy figure-eight that swings across the whole card fast enough
        // to cross the bubble emission ramp (small card, so frequencies are
        // higher than they'd be on a real desktop).
        let nx = 0.5 + 0.38 * (self.t * 3.4).sin() + 0.06 * (self.t * 6.2).sin();
        let ny = 0.5 + 0.34 * (self.t * 2.6 + 1.3).sin();
        let target = (nx * w, ny * h);
        let ivx = (target.0 - self.mouse.0) / FIXED_DT;
        let ivy = (target.1 - self.mouse.1) / FIXED_DT;
        self.vel = (self.vel.0 * 0.5 + ivx * 0.5, self.vel.1 * 0.5 + ivy * 0.5);
        let moved = (target.0 - self.mouse.0).hypot(target.1 - self.mouse.1);
        self.mouse = target;
        let speed = self.vel.0.hypot(self.vel.1);

        self.next_click -= FIXED_DT;
        let clicked = self.next_click <= 0.0;
        if clicked {
            self.next_click = 4.0;
        }

        self.effects.push_trail_samples(vec![(self.mouse.0, self.mouse.1, self.t)]);

        let es = exprs.resolve(self.t, speed, moved > 0.5);
        let ctx = Ctx {
            dt: FIXED_DT,
            now: self.t,
            mouse: self.mouse,
            vel: self.vel,
            clicked,
            moving: moved,
            cfg: &es,
            screen: (w, h),
        };
        self.effects.update(&ctx);

        self.pixmap.data_mut().fill(0);
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let mut pm =
            tiny_skia::PixmapMut::from_bytes(self.pixmap.data_mut(), w, h).expect("preview pixmap mut");
        self.effects.render(&mut pm, &es, self.t);
        drop(pm);

        // Premultiplied [B G R A] → premultiplied RGBA bytes (swap 0 and 2).
        let data = self.pixmap.data();
        let mut rgba = data.to_vec();
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        self.image = Some(egui::ColorImage::from_rgba_premultiplied(
            [self.pixmap.width() as usize, self.pixmap.height() as usize],
            &rgba,
        ));
    }

    pub fn image(&self) -> Option<&egui::ColorImage> {
        self.image.as_ref()
    }

    /// Current render size in pixels.
    pub fn pixel_size(&self) -> (f32, f32) {
        (self.pixmap.width() as f32, self.pixmap.height() as f32)
    }

    /// Re-render at a new size. The simulation state is kept (the animation
    /// continues seamlessly); particles outside the new bounds cull
    /// naturally. A no-op when the size already matches.
    pub fn resize(&mut self, w: u32, h: u32) {
        if self.pixmap.width() == w && self.pixmap.height() == h {
            return;
        }
        self.pixmap = tiny_skia::Pixmap::new(w, h).expect("preview pixmap");
        self.mouse = (w as f32 * 0.5, h as f32 * 0.5);
        self.vel = (0.0, 0.0);
    }
}
