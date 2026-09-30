use std::sync::atomic::{AtomicU64, Ordering};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Point,
    RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

use crate::settings::{Settings, TrailStyle};

pub struct Ctx<'a> {
    pub dt: f32,
    pub now: f32,
    pub mouse: (f32, f32),
    pub vel: (f32, f32),
    pub clicked: bool,
    pub moving: f32,
    pub cfg: &'a Settings,
    pub screen: (f32, f32),
}

pub trait Effect {
    fn update(&mut self, ctx: &Ctx);
    fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32);
    fn bounds(&self, cfg: &Settings) -> Option<RectF>;
}

#[derive(Clone, Copy, Debug)]
pub struct RectF {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl RectF {
    fn point(x: f32, y: f32, r: f32) -> Self {
        Self { x0: x - r, y0: y - r, x1: x + r, y1: y + r }
    }

    fn merge(&mut self, o: RectF) {
        self.x0 = self.x0.min(o.x0);
        self.y0 = self.y0.min(o.y0);
        self.x1 = self.x1.max(o.x1);
        self.y1 = self.y1.max(o.y1);
    }

    pub fn union(a: Option<RectF>, b: Option<RectF>) -> Option<RectF> {
        match (a, b) {
            (None, None) => None,
            (Some(x), None) => Some(x),
            (None, Some(y)) => Some(y),
            (Some(mut x), Some(y)) => {
                x.merge(y);
                Some(x)
            }
        }
    }
}

// --- tiny random -----------------------------------------------------------

static RNG: AtomicU64 = AtomicU64::new(88172645463325252);

pub fn seed_rng() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    RNG.store(n | 1, Ordering::Relaxed);
}

fn rnd01() -> f32 {
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG.store(x, Ordering::Relaxed);
    ((x >> 40) as f32) / 16777216.0
}

fn rnd_range(a: f32, b: f32) -> f32 {
    a + (b - a) * rnd01()
}

// --- drawing helpers --------------------------------------------------------
// tiny-skia writes RGBA byte order, but the layered-window bitmap is BGRA, so
// every color we create has R and B pre-swapped. Nothing else may use Color
// directly, or red/blue will come out flipped on screen.

pub fn col(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color::from_rgba(
        b.clamp(0.0, 1.0),
        g.clamp(0.0, 1.0),
        r.clamp(0.0, 1.0),
        a.clamp(0.0, 1.0),
    )
    .unwrap()
}

fn rgb(c: &[u8; 3]) -> (f32, f32, f32) {
    (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0)
}

fn hsv(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = h.fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

fn add_circle(pb: &mut PathBuilder, x: f32, y: f32, r: f32) {
    let k = 0.5522847498 * r;
    pb.move_to(x + r, y);
    pb.cubic_to(x + r, y + k, x + k, y + r, x, y + r);
    pb.cubic_to(x - k, y + r, x - r, y + k, x - r, y);
    pb.cubic_to(x - r, y - k, x - k, y - r, x, y - r);
    pb.cubic_to(x + k, y - r, x + r, y - k, x + r, y);
    pb.close();
}

fn paint_solid(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.anti_alias = true;
    p.shader = Shader::SolidColor(color);
    p
}

fn fill_path(pm: &mut PixmapMut, path: &Path, paint: &Paint) {
    pm.fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
}

fn fill_circle(pm: &mut PixmapMut, x: f32, y: f32, r: f32, color: Color) {
    if r < 0.2 {
        return;
    }
    let mut pb = PathBuilder::new();
    add_circle(&mut pb, x, y, r);
    if let Some(p) = pb.finish() {
        fill_path(pm, &p, &paint_solid(color));
    }
}

fn stroke_path(pm: &mut PixmapMut, path: &Path, width: f32, color: Color) {
    let stroke = Stroke {
        width: width.max(0.15),
        miter_limit: 4.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        dash: None,
    };
    if let Some(sp) = path.stroke(&stroke, 1.0) {
        fill_path(pm, &sp, &paint_solid(color));
    }
}

fn stroke_segment(pm: &mut PixmapMut, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, color: Color) {
    let mut pb = PathBuilder::new();
    pb.move_to(x0, y0);
    pb.line_to(x1, y1);
    if let Some(p) = pb.finish() {
        stroke_path(pm, &p, w, color);
    }
}

fn stroke_circle(pm: &mut PixmapMut, x: f32, y: f32, r: f32, w: f32, color: Color) {
    if r < 0.4 {
        return;
    }
    let mut pb = PathBuilder::new();
    add_circle(&mut pb, x, y, r);
    if let Some(p) = pb.finish() {
        stroke_path(pm, &p, w, color);
    }
}

fn radial_fill_circle(pm: &mut PixmapMut, x: f32, y: f32, r: f32, stops: &[(f32, Color)]) {
    if r < 0.5 {
        return;
    }
    let ss: Vec<GradientStop> = stops.iter().map(|(p, c)| GradientStop::new(*p, *c)).collect();
    let shader = RadialGradient::new(
        Point::from_xy(x, y),
        Point::from_xy(x, y),
        r,
        ss,
        SpreadMode::Pad,
        Transform::identity(),
    );
    if let Some(sh) = shader {
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.shader = sh;
        let mut pb = PathBuilder::new();
        add_circle(&mut pb, x, y, r);
        if let Some(p) = pb.finish() {
            fill_path(pm, &p, &paint);
        }
    }
}

// --- Mouse trail ------------------------------------------------------------

#[derive(Clone, Copy)]
struct TrailPoint {
    x: f32,
    y: f32,
    t: f32,
}

/// Max distance between stored trail points (px). Dense cursor samples get
/// interpolated down to this spacing.
const SPACING_PX: f32 = 4.0;
/// Larger jumps are real teleports (cursor snapping, RDP) — break the ribbon.
const TELEPORT_PX: f32 = 150.0;
/// Cap on stored points (≈ path length / spacing).
const TRAIL_MAX_POINTS: usize = 1024;
/// After the cursor stops moving, sparkle emission fades out over this many
/// seconds instead of cutting off instantly. (Bubbles instead slow to a
/// constant idle rate — see the speed modulation below.)
const EMISSION_RAMP_DOWN_SECS: f32 = 0.5;
/// Fraction of the configured bubble rate used while the cursor is idle.
const IDLE_RATE_FRACTION: f32 = 1.0 / 3.0;
/// Cursor speeds (px/s) between which the bubble rate interpolates from the
/// idle fraction to the full configured rate.
const SLOW_CURSOR_PX_S: f32 = 150.0;
const FAST_CURSOR_PX_S: f32 = 700.0;
/// Clicks shove bubbles within this range outward from the click point.
const SHOCKWAVE_RADIUS: f32 = 240.0;

pub struct Trail {
    pts: std::collections::VecDeque<TrailPoint>,
    /// Raw high-frequency cursor samples waiting to be consumed by update().
    pending: Vec<TrailPoint>,
    hue: f32,
}

impl Default for Trail {
    fn default() -> Self {
        Self { pts: Default::default(), pending: Vec::new(), hue: 0.0 }
    }
}

impl Trail {
    /// Queue cursor positions sampled by the high-frequency sampler thread.
    /// Coordinates are window-relative; t is seconds since start (same clock
    /// as `Ctx::now`).
    pub fn push_samples(&mut self, samples: Vec<(f32, f32, f32)>) {
        for (x, y, t) in samples {
            self.pending.push(TrailPoint { x, y, t });
        }
    }

    fn clear(&mut self) {
        self.pts.clear();
        self.pending.clear();
    }
}

fn mid(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// Stroke a quadratic segment (start → ctrl → end). tiny-skia has no quad
/// curves, so this is the exact cubic equivalent.
fn stroke_quad(
    pm: &mut PixmapMut,
    start: (f32, f32),
    ctrl: (f32, f32),
    end: (f32, f32),
    width: f32,
    color: Color,
) {
    let mut pb = PathBuilder::new();
    pb.move_to(start.0, start.1);
    let c1 = (start.0 + (ctrl.0 - start.0) * 2.0 / 3.0, start.1 + (ctrl.1 - start.1) * 2.0 / 3.0);
    let c2 = (end.0 + (ctrl.0 - end.0) * 2.0 / 3.0, end.1 + (ctrl.1 - end.1) * 2.0 / 3.0);
    pb.cubic_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
    if let Some(p) = pb.finish() {
        stroke_path(pm, &p, width, color);
    }
}

impl Effect for Trail {
    fn update(&mut self, ctx: &Ctx) {
        let c = &ctx.cfg.trail;
        if !c.enabled {
            self.clear();
            return;
        }

        self.hue = (self.hue + ctx.dt * c.hue_speed * 0.5).rem_euclid(1.0);

        // Consume the sampler's raw cursor positions, interpolating each hop
        // down to SPACING_PX so fast movement stays a smooth, dense path.
        let mut prev: Option<TrailPoint> = self.pts.back().cloned();
        for s in self.pending.drain(..) {
            match prev {
                None => self.pts.push_back(s),
                Some(p) => {
                    let d = ((s.x - p.x).powi(2) + (s.y - p.y).powi(2)).sqrt();
                    if d > TELEPORT_PX {
                        self.pts.push_back(s);
                    } else if d > 0.05 {
                        let steps = ((d / SPACING_PX).ceil() as u32).clamp(1, 64);
                        for i in 1..=steps {
                            let f = i as f32 / steps as f32;
                            self.pts.push_back(TrailPoint {
                                x: p.x + (s.x - p.x) * f,
                                y: p.y + (s.y - p.y) * f,
                                t: p.t + (s.t - p.t) * f,
                            });
                        }
                    }
                }
            }
            prev = Some(s);
        }

        let len = c.length_secs.max(0.05);
        while let Some(front) = self.pts.front() {
            if ctx.now - front.t > len {
                self.pts.pop_front();
            } else {
                break;
            }
        }
        while self.pts.len() > TRAIL_MAX_POINTS {
            self.pts.pop_front();
        }
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32) {
        let c = &cfg.trail;
        if !c.enabled || self.pts.len() < 2 {
            return;
        }
        let len = c.length_secs.max(0.05);

        let pt = |i: usize| (self.pts[i].x, self.pts[i].y);
        let n = self.pts.len();
        let gap = |a: usize, b: usize| {
            let (ax, ay) = pt(a);
            let (bx, by) = pt(b);
            ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt() > TELEPORT_PX
        };

        // Split into continuous runs (teleports break the ribbon).
        let mut runs: Vec<Vec<usize>> = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        for i in 0..n {
            if i > 0 && gap(i - 1, i) && cur.len() > 1 {
                runs.push(std::mem::take(&mut cur));
            } else if i > 0 && gap(i - 1, i) {
                cur.clear();
            }
            cur.push(i);
        }
        if cur.len() > 1 {
            runs.push(cur);
        }

        match c.style {
            TrailStyle::Rainbow => {
                for run in &runs {
                    let m = run.len();
                    // Quadratic midpoint smoothing: anchors are midpoints, the
                    // stored points act as controls — corners round off.
                    for j in 1..m - 1 {
                        let idx = run[j];
                        let age = now - self.pts[idx].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k <= 0.0 {
                            continue;
                        }
                        let width = (c.width * (0.25 + 0.75 * k)).max(0.6);
                        let h = (self.hue + idx as f32 * 0.004).rem_euclid(1.0);
                        let (r, g, b) = hsv(h, 0.9, 1.0);
                        let start = if j == 1 { pt(run[0]) } else { mid(pt(run[j - 1]), pt(idx)) };
                        let end = mid(pt(idx), pt(run[j + 1]));
                        stroke_quad(pm, start, pt(idx), end, width, col(r, g, b, 0.9 * k));
                    }
                    // Trailing tip from the last anchor to the final point.
                    let li = *run.last().unwrap();
                    if m >= 2 {
                        let age = now - self.pts[li].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k > 0.0 {
                            let width = (c.width * (0.25 + 0.75 * k)).max(0.6);
                            let h = (self.hue + li as f32 * 0.004).rem_euclid(1.0);
                            let (r, g, b) = hsv(h, 0.9, 1.0);
                            let start = mid(pt(run[m - 2]), pt(li));
                            stroke_segment(
                                pm,
                                start.0,
                                start.1,
                                self.pts[li].x,
                                self.pts[li].y,
                                width,
                                col(r, g, b, 0.9 * k),
                            );
                        }
                    }
                }
                // Crisp bright head right under the cursor.
                let head = self.pts.back().unwrap();
                fill_circle(pm, head.x, head.y, (c.width * 0.35).max(1.5), col(1.0, 1.0, 1.0, 0.75));
            }
            TrailStyle::Neon | TrailStyle::Ghost => {
                let (r0, g0, b0) = if c.style == TrailStyle::Neon {
                    rgb(&c.color)
                } else {
                    (0.95, 0.97, 1.0)
                };
                for run in &runs {
                    let m = run.len();
                    let mut pb = PathBuilder::new();
                    pb.move_to(self.pts[run[0]].x, self.pts[run[0]].y);
                    for j in 1..m - 1 {
                        let idx = run[j];
                        let end = mid(pt(idx), pt(run[j + 1]));
                        // quad converted to cubic
                        let (sx, sy) = if j == 1 {
                            (self.pts[run[0]].x, self.pts[run[0]].y)
                        } else {
                            let s = mid(pt(run[j - 1]), pt(idx));
                            (s.0, s.1)
                        };
                        let cx = self.pts[idx].x;
                        let cy = self.pts[idx].y;
                        let q1 = (sx + (cx - sx) * 2.0 / 3.0, sy + (cy - sy) * 2.0 / 3.0);
                        let q2 = (end.0 + (cx - end.0) * 2.0 / 3.0, end.1 + (cy - end.1) * 2.0 / 3.0);
                        pb.cubic_to(q1.0, q1.1, q2.0, q2.1, end.0, end.1);
                    }
                    pb.line_to(self.pts[*run.last().unwrap()].x, self.pts[*run.last().unwrap()].y);
                    if let Some(path) = pb.finish() {
                        if c.style == TrailStyle::Neon {
                            stroke_path(pm, &path, c.width * 2.4, col(r0, g0, b0, 0.20));
                            stroke_path(pm, &path, c.width * 0.75, col(r0, g0, b0, 0.95));
                        } else {
                            stroke_path(pm, &path, c.width, col(r0, g0, b0, 0.35));
                        }
                    }
                }
            }
        }
    }

    fn bounds(&self, cfg: &Settings) -> Option<RectF> {
        let c = &cfg.trail;
        if !c.enabled || self.pts.is_empty() {
            return None;
        }
        let pad = c.width * 1.4 + 6.0;
        let mut b = RectF::point(self.pts.front().unwrap().x, self.pts.front().unwrap().y, pad);
        for p in &self.pts {
            b.merge(RectF::point(p.x, p.y, pad));
        }
        Some(b)
    }
}

// --- Bubbles ----------------------------------------------------------------

struct Bubble {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    r: f32,
    hue: f32,
    born: f32,
    life: f32,
    wob_phase: f32,
    wob_freq: f32,
}

const BUBBLE_HUES: [f32; 6] = [0.55, 0.75, 0.90, 0.10, 0.40, 0.63];

pub struct Bubbles {
    list: Vec<Bubble>,
    acc: f32,
    next_hue: usize,
    /// Smoothed speed-dependent emission multiplier (1/3 idle … 1 at speed).
    speed_mod: f32,
}

impl Default for Bubbles {
    fn default() -> Self {
        Self { list: Vec::new(), acc: 0.0, next_hue: 0, speed_mod: IDLE_RATE_FRACTION }
    }
}

impl Bubbles {
    fn clear(&mut self) {
        self.list.clear();
        self.acc = 0.0;
    }
}

impl Effect for Bubbles {
    fn update(&mut self, ctx: &Ctx) {
        let c = &ctx.cfg.bubbles;
        if !c.enabled {
            self.clear();
            return;
        }

        // Emission scales with cursor speed: a third of the configured rate
        // while idle or drifting slowly, the full rate during real movement.
        // Ramps up fast when the cursor accelerates and relaxes a bit more
        // slowly when it slows down — bubbles keep trickling even at rest.
        let speed = (ctx.vel.0 * ctx.vel.0 + ctx.vel.1 * ctx.vel.1).sqrt();
        let target = IDLE_RATE_FRACTION
            + (1.0 - IDLE_RATE_FRACTION)
                * ((speed - SLOW_CURSOR_PX_S) / (FAST_CURSOR_PX_S - SLOW_CURSOR_PX_S))
                    .clamp(0.0, 1.0);
        let k = if target > self.speed_mod { 16.0 } else { 5.0 };
        self.speed_mod += (target - self.speed_mod) * (1.0 - (-k * ctx.dt).exp());
        self.acc += c.rate.max(0.0) * ctx.dt * self.speed_mod;
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            if self.list.len() < 90 {
                self.list.push(Bubble {
                    x: ctx.mouse.0 + rnd_range(-8.0, 8.0),
                    y: ctx.mouse.1 + rnd_range(-8.0, 8.0),
                    vx: ctx.vel.0 * 0.35 + rnd_range(-60.0, 60.0),
                    vy: ctx.vel.1 * 0.35 + rnd_range(-60.0, 20.0),
                    r: (c.size * rnd_range(0.6, 1.3)).max(2.0),
                    hue: BUBBLE_HUES[self.next_hue % BUBBLE_HUES.len()],
                    born: ctx.now,
                    life: c.lifetime * rnd_range(0.7, 1.2),
                    wob_phase: rnd01() * std::f32::consts::TAU,
                    wob_freq: rnd_range(1.5, 3.2),
                });
                self.next_hue += 1;
            } else {
                self.acc = 0.0;
            }
        }

        // Clicking blasts bubbles outward from the click point like a
        // shockwave — tapered with distance, with a little angular jitter so
        // the scatter looks organic.
        if ctx.clicked {
            for b in self.list.iter_mut() {
                let dx = b.x - ctx.mouse.0;
                let dy = b.y - ctx.mouse.1;
                let d = (dx * dx + dy * dy).sqrt();
                if d < SHOCKWAVE_RADIUS {
                    let fall = 1.0 - d / SHOCKWAVE_RADIUS;
                    let mag = 250.0 + 950.0 * fall;
                    let ang = dy.atan2(dx) + rnd_range(-0.35, 0.35);
                    b.vx += ang.cos() * mag;
                    b.vy += ang.sin() * mag;
                }
            }
        }

        self.list.retain(|b| {
            let age = ctx.now - b.born;
            if age > b.life || b.y < -b.r - 40.0 {
                return false;
            }
            if b.x < -80.0 || b.x > ctx.screen.0 + 80.0 || b.y > ctx.screen.1 + 80.0 {
                return false;
            }
            true
        });

        // Physics: buoyancy pulls up, drag calms everything, wobble sways.
        for b in self.list.iter_mut() {
            b.vy -= c.buoyancy * ctx.dt;
            b.vx += (ctx.now * b.wob_freq + b.wob_phase).sin() * c.wobble * 4.0 * ctx.dt;
            let drag = (-2.2 * ctx.dt).exp();
            b.vx *= drag;
            b.vy *= drag;
            b.x += b.vx * ctx.dt;
            b.y += b.vy * ctx.dt;
        }

        resolve_bubble_collisions(&mut self.list);
        resolve_cursor_collisions(&mut self.list, ctx.mouse, ctx.vel);
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32) {
        let c = &cfg.bubbles;
        if !c.enabled {
            return;
        }
        for b in &self.list {
            let k = (1.0 - (now - b.born) / b.life).clamp(0.0, 1.0);
            if k <= 0.0 {
                continue;
            }
            let r = b.r;
            let (cr, cg, cb) = hsv(b.hue, 0.55, 0.95);
            let alpha = 0.85 * k;

            // Soap-bubble body: nearly transparent center, colorful rim.
            radial_fill_circle(
                pm,
                b.x,
                b.y,
                r,
                &[
                    (0.0, col(cr, cg, cb, alpha * 0.03)),
                    (0.55, col(cr, cg, cb, alpha * 0.07)),
                    (0.85, col(cr, cg, cb, alpha * 0.32)),
                    (0.96, col(cr, cg, cb, alpha * 0.55)),
                    (1.0, col(cr, cg, cb, alpha * 0.15)),
                ],
            );
            stroke_circle(pm, b.x, b.y, r, 1.4, col(cr, cg, cb, 0.55 * k));
            // Specular highlight.
            fill_circle(
                pm,
                b.x - r * 0.35,
                b.y - r * 0.40,
                (r * 0.22).max(0.8),
                col(1.0, 1.0, 1.0, 0.70 * k),
            );
        }
    }

    fn bounds(&self, cfg: &Settings) -> Option<RectF> {
        let c = &cfg.bubbles;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for bub in &self.list {
            let rect = RectF::point(bub.x, bub.y, bub.r + 8.0);
            match &mut b {
                Some(acc) => acc.merge(rect),
                None => b = Some(rect),
            }
        }
        b
    }
}

/// Effective collision radius of the cursor (it shoves bubbles around like a
/// small moving ball).
const CURSOR_RADIUS: f32 = 12.0;

/// Soft circle-vs-circle collisions between bubbles: positional separation
/// plus an impulse along the collision normal when they approach each other.
/// Mass scales with radius, so big bubbles shove small ones aside.
fn resolve_bubble_collisions(list: &mut [Bubble]) {
    let n = list.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = list[j].x - list[i].x;
            let dy = list[j].y - list[i].y;
            let rsum = list[i].r + list[j].r;
            let d2 = dx * dx + dy * dy;
            if d2 >= rsum * rsum || d2 < 0.0001 {
                continue;
            }
            let d = d2.sqrt();
            let (nx, ny) = (dx / d, dy / d);
            let overlap = rsum - d;

            // Split positional correction by inverse mass.
            let (mi, mj) = (list[i].r, list[j].r);
            let (wi, wj) = (mj / (mi + mj), mi / (mi + mj));
            let corr = overlap * 0.5;
            let (a, b) = list.split_at_mut(j);
            let bi = &mut a[i];
            let bj = &mut b[0];
            bi.x -= nx * corr * wi;
            bi.y -= ny * corr * wi;
            bj.x += nx * corr * wj;
            bj.y += ny * corr * wj;

            // Impulse if they are moving toward each other.
            let rvx = bj.vx - bi.vx;
            let rvy = bj.vy - bi.vy;
            let rel_n = rvx * nx + rvy * ny;
            if rel_n < 0.0 {
                let restitution = 0.35;
                let jimp = -(1.0 + restitution) * rel_n / (1.0 / mi + 1.0 / mj);
                bi.vx -= nx * jimp / mi;
                bi.vy -= ny * jimp / mi;
                bj.vx += nx * jimp / mj;
                bj.vy += ny * jimp / mj;
            }
        }
    }
}

/// The cursor behaves like an infinitely heavy moving ball: bubbles inside its
/// reach are pushed out of the way and pick up the cursor's velocity, so a
/// quick swipe plows a bubble along instead of just repelling it.
fn resolve_cursor_collisions(list: &mut [Bubble], mouse: (f32, f32), cursor_vel: (f32, f32)) {
    for b in list.iter_mut() {
        let dx = b.x - mouse.0;
        let dy = b.y - mouse.1;
        let rr = b.r + CURSOR_RADIUS;
        let d2 = dx * dx + dy * dy;
        if d2 >= rr * rr || d2 < 0.0001 {
            continue;
        }
        let d = d2.sqrt();
        let (nx, ny) = (dx / d, dy / d);
        let overlap = rr - d;

        // Cursor is immovable: push the bubble out of the overlap entirely.
        b.x += nx * overlap;
        b.y += ny * overlap;

        // Impulse against the cursor's velocity, with restitution.
        let rvx = b.vx - cursor_vel.0;
        let rvy = b.vy - cursor_vel.1;
        let rel_n = rvx * nx + rvy * ny;
        if rel_n < 0.0 {
            let restitution = 0.55;
            let jimp = (-(1.0 + restitution) * rel_n).min(2600.0);
            b.vx += nx * jimp;
            b.vy += ny * jimp;
        }
    }
}

// --- Sparkles ---------------------------------------------------------------

struct Spark {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    born: f32,
    life: f32,
    size: f32,
    phase: f32,
    freq: f32,
}

pub struct Sparkles {
    list: Vec<Spark>,
    acc: f32,
    /// Seconds since the cursor last moved — drives the emission ramp-down.
    idle: f32,
}

impl Default for Sparkles {
    fn default() -> Self {
        Self { list: Vec::new(), acc: 0.0, idle: 0.0 }
    }
}

impl Sparkles {
    fn clear(&mut self) {
        self.list.clear();
        self.acc = 0.0;
    }
}

impl Effect for Sparkles {
    fn update(&mut self, ctx: &Ctx) {
        let c = &ctx.cfg.sparkles;
        if !c.enabled {
            self.clear();
            return;
        }

        // Same emission ramp-down as bubbles.
        if ctx.moving > 0.5 {
            self.idle = 0.0;
        } else {
            self.idle += ctx.dt;
        }
        let ramp = (1.0 - self.idle / EMISSION_RAMP_DOWN_SECS).clamp(0.0, 1.0);
        if ramp > 0.0 {
            self.acc += c.rate.max(0.0) * ctx.dt * ramp;
        }
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            if self.list.len() < 300 {
                let ang = rnd01() * std::f32::consts::TAU;
                let speed = rnd_range(30.0, 160.0);
                self.list.push(Spark {
                    x: ctx.mouse.0 + rnd_range(-6.0, 6.0),
                    y: ctx.mouse.1 + rnd_range(-6.0, 6.0),
                    vx: ang.cos() * speed + ctx.vel.0 * 0.25,
                    vy: ang.sin() * speed + ctx.vel.1 * 0.25,
                    born: ctx.now,
                    life: c.lifetime * rnd_range(0.6, 1.25),
                    size: c.size * rnd_range(0.6, 1.4),
                    phase: rnd01() * std::f32::consts::TAU,
                    freq: rnd_range(6.0, 14.0),
                });
            } else {
                self.acc = 0.0;
            }
        }

        self.list.retain(|s| ctx.now - s.born < s.life);

        for s in self.list.iter_mut() {
            s.vy += c.gravity * ctx.dt;
            let drag = (-1.6 * ctx.dt).exp();
            s.vx *= drag;
            s.vy *= drag;
            s.x += s.vx * ctx.dt;
            s.y += s.vy * ctx.dt;
        }
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32) {
        let c = &cfg.sparkles;
        if !c.enabled {
            return;
        }
        let (cr, cg, cb) = rgb(&c.color);
        for s in &self.list {
            let k = (1.0 - (now - s.born) / s.life).clamp(0.0, 1.0);
            let tw = 0.55 + 0.45 * (now * s.freq + s.phase).sin();
            let alpha = 0.9 * k * tw;
            if alpha <= 0.01 {
                continue;
            }
            let sz = (s.size * (0.35 + 0.65 * k)).max(0.8);
            let thin = sz * 0.30;
            // Four-point star: one vertical and one horizontal diamond.
            let mut pb = PathBuilder::new();
            pb.move_to(s.x, s.y - sz);
            pb.line_to(s.x + thin, s.y);
            pb.line_to(s.x, s.y + sz);
            pb.line_to(s.x - thin, s.y);
            pb.close();
            pb.move_to(s.x - sz, s.y);
            pb.line_to(s.x, s.y - thin);
            pb.line_to(s.x + sz, s.y);
            pb.line_to(s.x, s.y + thin);
            pb.close();
            if let Some(p) = pb.finish() {
                fill_path(pm, &p, &paint_solid(col(cr, cg, cb, alpha)));
            }
            fill_circle(pm, s.x, s.y, (sz * 0.22).max(0.6), col(1.0, 1.0, 1.0, alpha));
        }
    }

    fn bounds(&self, cfg: &Settings) -> Option<RectF> {
        let c = &cfg.sparkles;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for s in &self.list {
            let rect = RectF::point(s.x, s.y, s.size * 2.0 + 6.0);
            match &mut b {
                Some(acc) => acc.merge(rect),
                None => b = Some(rect),
            }
        }
        b
    }
}

// --- Click ripples ----------------------------------------------------------

struct Ripple {
    x: f32,
    y: f32,
    born: f32,
}

pub struct Ripples {
    list: Vec<Ripple>,
}

impl Default for Ripples {
    fn default() -> Self {
        Self { list: Vec::new() }
    }
}

impl Ripples {
    fn clear(&mut self) {
        self.list.clear();
    }
}

impl Effect for Ripples {
    fn update(&mut self, ctx: &Ctx) {
        let c = &ctx.cfg.ripples;
        if !c.enabled {
            self.clear();
            return;
        }
        if ctx.clicked {
            let max = c.rings.clamp(1, 6) as usize;
            for _ in 0..max {
                if self.list.len() < 24 {
                    self.list.push(Ripple { x: ctx.mouse.0, y: ctx.mouse.1, born: ctx.now });
                }
            }
        }
        let total = c.lifetime + (c.rings.saturating_sub(1)) as f32 * 0.12;
        self.list.retain(|r| ctx.now - r.born < total);
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32) {
        let c = &cfg.ripples;
        if !c.enabled {
            return;
        }
        let (cr, cg, cb) = rgb(&c.color);
        for r in &self.list {
            for ring in 0..c.rings.clamp(1, 6) {
                let age = now - r.born - ring as f32 * 0.12;
                if age <= 0.0 || age >= c.lifetime {
                    continue;
                }
                let k = 1.0 - age / c.lifetime;
                let radius = c.speed * age;
                let width = (c.width * (0.4 + 0.6 * k)).max(0.5);
                stroke_circle(pm, r.x, r.y, radius, width, col(cr, cg, cb, 0.85 * k.powf(1.6)));
            }
        }
    }

    fn bounds(&self, cfg: &Settings) -> Option<RectF> {
        let c = &cfg.ripples;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for r in &self.list {
            let max_radius = c.speed * (c.lifetime + (c.rings.saturating_sub(1)) as f32 * 0.12);
            let rect = RectF::point(r.x, r.y, max_radius + c.width + 4.0);
            match &mut b {
                Some(acc) => acc.merge(rect),
                None => b = Some(rect),
            }
        }
        b
    }
}

// --- Manager ----------------------------------------------------------------

pub struct Effects {
    trail: Trail,
    bubbles: Bubbles,
    sparkles: Sparkles,
    ripples: Ripples,
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

impl Effects {
    pub fn new() -> Self {
        Self {
            trail: Trail::default(),
            bubbles: Bubbles::default(),
            sparkles: Sparkles::default(),
            ripples: Ripples::default(),
        }
    }

    /// Feed high-frequency cursor samples (window coords, seconds since start)
    /// into the trail.
    pub fn push_trail_samples(&mut self, samples: Vec<(f32, f32, f32)>) {
        self.trail.push_samples(samples);
    }

    pub fn update(&mut self, ctx: &Ctx) {
        self.trail.update(ctx);
        self.bubbles.update(ctx);
        self.sparkles.update(ctx);
        self.ripples.update(ctx);
    }

    pub fn render(&mut self, pm: &mut PixmapMut, cfg: &Settings, now: f32) {
        self.trail.render(pm, cfg, now);
        self.bubbles.render(pm, cfg, now);
        self.sparkles.render(pm, cfg, now);
        self.ripples.render(pm, cfg, now);
    }

    pub fn bounds(&self, cfg: &Settings) -> Option<RectF> {
        RectF::union(
            RectF::union(self.trail.bounds(cfg), self.bubbles.bounds(cfg)),
            RectF::union(self.sparkles.bounds(cfg), self.ripples.bounds(cfg)),
        )
    }

    /// Diagnostics: (trail points, bubbles, sparkles, ripples).
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.trail.pts.len(),
            self.bubbles.list.len(),
            self.sparkles.list.len(),
            self.ripples.list.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bubble(x: f32, y: f32) -> Bubble {
        Bubble {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            r: 10.0,
            hue: 0.0,
            born: 0.0,
            life: 100.0,
            wob_phase: 0.0,
            wob_freq: 1.0,
        }
    }

    fn ctx(mouse: (f32, f32), clicked: bool, moving: f32) -> Ctx<'static> {
        Ctx {
            dt: 1.0 / 60.0,
            now: 0.5,
            mouse,
            vel: (0.0, 0.0),
            clicked,
            moving,
            cfg: test_settings(),
            screen: (3440.0, 1440.0),
        }
    }

    fn ctx_vel(mouse: (f32, f32), vel: (f32, f32)) -> Ctx<'static> {
        Ctx {
            dt: 1.0 / 60.0,
            now: 0.5,
            mouse,
            vel,
            clicked: false,
            moving: 0.0,
            cfg: test_settings(),
            screen: (3440.0, 1440.0),
        }
    }

    fn test_settings() -> &'static Settings {
        // Defaults are small consts; a leaked static keeps the borrow simple.
        Box::leak(Box::new(Settings::default()))
    }

    #[test]
    fn click_blasts_all_bubbles_outward() {
        let mut b = Bubbles::default();
        // One under the cursor, one mid-range, one far outside the shockwave.
        b.list.push(bubble(0.0, 0.0));
        b.list.push(bubble(150.0, 0.0));
        b.list.push(bubble(2000.0, 0.0));

        b.update(&ctx((0.0, 0.0), true, 0.0));

        assert!(
            b.list[0].vx > 800.0,
            "bubble under the cursor should get the full blast, vx={}",
            b.list[0].vx
        );
        assert!(
            b.list[1].vx > 350.0,
            "mid-range bubble should be blasted outward, vx={}",
            b.list[1].vx
        );
        assert!(
            b.list[1].vy.abs() < 600.0,
            "blast should be roughly radial, vy={}",
            b.list[1].vy
        );
        assert!(
            b.list[2].vx.abs() < 5.0 && b.list[2].vy.abs() < 5.0,
            "bubble beyond the shockwave should be untouched, vx={} vy={}",
            b.list[2].vx,
            b.list[2].vy
        );
    }

    #[test]
    fn bubble_rate_ramps_with_cursor_speed() {
        let mut b = Bubbles::default();
        assert!(
            (b.speed_mod - IDLE_RATE_FRACTION).abs() < 1e-6,
            "starts at the idle fraction"
        );
        // ~100ms of fast movement should ramp most of the way up.
        for _ in 0..12 {
            b.update(&ctx_vel((100.0, 100.0), (2000.0, 0.0)));
        }
        assert!(
            b.speed_mod > 0.75,
            "should ramp up quickly, got {}",
            b.speed_mod
        );
        // ~500ms idle should relax back near the idle fraction.
        for _ in 0..60 {
            b.update(&ctx_vel((100.0, 100.0), (0.0, 0.0)));
        }
        assert!(
            b.speed_mod < 0.45,
            "should relax to idle rate, got {}",
            b.speed_mod
        );
    }

    #[test]
    fn idle_bubbles_keep_trickling_at_third_rate() {
        // 4s fully idle: steady-state population ≈ rate/3 × lifetime
        // (9/3 × 2.4 ≈ 7) — clearly non-zero, clearly below the full-rate
        // steady state (9 × 2.4 ≈ 22).
        let mut b = Bubbles::default();
        for _ in 0..240 {
            b.update(&ctx_vel((100.0, 100.0), (0.0, 0.0)));
        }
        assert!(
            (3..=14).contains(&b.list.len()),
            "idle population should hover around a third of full, got {}",
            b.list.len()
        );

        // 4s of fast movement for contrast: near the full steady state.
        for _ in 0..240 {
            b.update(&ctx_vel((100.0, 100.0), (2000.0, 0.0)));
        }
        assert!(
            b.list.len() > 14,
            "fast movement should reach the full-rate population, got {}",
            b.list.len()
        );
    }

    #[test]
    fn sparkles_stop_after_idle() {
        let mut s = Sparkles::default();
        // Moving: spawns happen.
        for _ in 0..30 {
            s.update(&ctx((100.0, 100.0), false, 30.0));
        }
        let while_moving = s.list.len();
        assert!(while_moving > 0);

        // Idle past the ramp window: no further spawns.
        for _ in 0..90 {
            s.update(&ctx((100.0, 100.0), false, 0.0));
        }
        let after_idle = s.list.len();
        for _ in 0..30 {
            s.update(&ctx((100.0, 100.0), false, 0.0));
        }
        assert!(
            s.list.len() <= after_idle,
            "no new sparkles once the ramp finished"
        );
    }
}
