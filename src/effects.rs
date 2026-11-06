use std::sync::atomic::{AtomicU64, Ordering};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, Paint, Path, PathBuilder, PixmapMut, Point,
    RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

use crate::settings::{BubbleShape, SparkShape, TrailStyle};
use crate::store::EffectSettings;

pub struct Ctx<'a> {
    pub dt: f32,
    pub now: f32,
    pub mouse: (f32, f32),
    pub vel: (f32, f32),
    pub clicked: bool,
    pub moving: f32,
    pub cfg: &'a EffectSettings,
    pub screen: (f32, f32),
}

pub trait Effect {
    fn update(&mut self, ctx: &Ctx);
    fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32);
    fn bounds(&self, cfg: &EffectSettings) -> Option<RectF>;
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

fn rgb3(c: &[u8; 3]) -> [f32; 3] {
    let (r, g, b) = rgb(c);
    [r, g, b]
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

/// Resolve the Soft style's (core, edge1, edge2) colors from the palette,
/// falling back to a white core with pink/blue halos when unset.
fn soft_colors(palette: &[[u8; 3]]) -> ([f32; 3], [f32; 3], [f32; 3]) {
    const CORE: [f32; 3] = [0.97, 0.98, 1.0];
    const EDGE_A: [f32; 3] = [1.0, 0.72, 0.86];
    const EDGE_B: [f32; 3] = [0.70, 0.82, 1.0];
    match palette.len() {
        0 => (CORE, EDGE_A, EDGE_B),
        1 => (rgb3(&palette[0]), EDGE_A, EDGE_B),
        2 => (rgb3(&palette[0]), rgb3(&palette[1]), EDGE_B),
        _ => (rgb3(&palette[0]), rgb3(&palette[1]), rgb3(&palette[2])),
    }
}

/// Pick the Striped style color for a point `arc` px behind the trail head:
/// the palette cycles every `stripe_len` px, falling back to a red/green
/// pairing when no palette is configured.
fn stripe_color(palette: &[[u8; 3]], arc: f32, stripe_len: f32) -> (f32, f32, f32) {
    const FALLBACK: [[u8; 3]; 2] = [[235, 60, 63], [64, 180, 90]];
    if palette.len() == 1 {
        return rgb(&palette[0]);
    }
    let colors: &[[u8; 3]] = if palette.len() >= 2 { palette } else { &FALLBACK };
    let step = stripe_len.max(1.0);
    let idx = (arc / step).floor() as i64;
    rgb(&colors[idx.rem_euclid(colors.len() as i64) as usize])
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
    radial_fill_circle_at(pm, x, y, r, x, y, r, stops);
}

/// Radial-gradient fill where the filled circle and the gradient circle are
/// independent — lets a shaded sphere keep its fill clipped to the
/// silhouette while the gradient center rides toward the "light". The fill
/// never extends beyond `(fx, fy, fr)`, which the dirty-rect accounting
/// relies on.
fn radial_fill_circle_at(
    pm: &mut PixmapMut,
    fx: f32,
    fy: f32,
    fr: f32,
    gx: f32,
    gy: f32,
    gr: f32,
    stops: &[(f32, Color)],
) {
    if fr < 0.5 {
        return;
    }
    let ss: Vec<GradientStop> = stops.iter().map(|(p, c)| GradientStop::new(*p, *c)).collect();
    let shader = RadialGradient::new(
        Point::from_xy(gx, gy),
        Point::from_xy(gx, gy),
        gr,
        ss,
        SpreadMode::Pad,
        Transform::identity(),
    );
    if let Some(sh) = shader {
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.shader = sh;
        let mut pb = PathBuilder::new();
        add_circle(&mut pb, fx, fy, fr);
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

        self.hue = (self.hue + ctx.dt * c.hue_speed.value() * 0.5).rem_euclid(1.0);

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

    fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32) {
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
                        let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
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
                            let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
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
                fill_circle(pm, head.x, head.y, (c.width.value() * 0.35).max(1.5), col(1.0, 1.0, 1.0, 0.75));
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
                            stroke_path(pm, &path, c.width.value() * 2.4, col(r0, g0, b0, 0.20));
                            stroke_path(pm, &path, c.width.value() * 0.75, col(r0, g0, b0, 0.95));
                        } else {
                            stroke_path(pm, &path, c.width.value(), col(r0, g0, b0, 0.35));
                        }
                    }
                }
            }
            TrailStyle::Soft => {
                let (core, edge_a, edge_b) = soft_colors(&c.palette);
                for run in &runs {
                    let m = run.len();
                    for j in 1..m - 1 {
                        let idx = run[j];
                        let age = now - self.pts[idx].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k <= 0.0 {
                            continue;
                        }
                        let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
                        let start =
                            if j == 1 { pt(run[0]) } else { mid(pt(run[j - 1]), pt(idx)) };
                        let end = mid(pt(idx), pt(run[j + 1]));
                        // Two wide tinted halos under a bright core: the
                        // outer ring shows edge_b, the middle edge_a, the
                        // center the core color.
                        stroke_quad(
                            pm,
                            start,
                            pt(idx),
                            end,
                            width * 2.15,
                            col(edge_b[0], edge_b[1], edge_b[2], 0.14 * k),
                        );
                        stroke_quad(
                            pm,
                            start,
                            pt(idx),
                            end,
                            width * 1.55,
                            col(edge_a[0], edge_a[1], edge_a[2], 0.20 * k),
                        );
                        stroke_quad(
                            pm,
                            start,
                            pt(idx),
                            end,
                            width * 0.72,
                            col(core[0], core[1], core[2], 0.9 * k),
                        );
                    }
                    // Trailing tip from the last anchor to the final point.
                    let li = *run.last().unwrap();
                    if m >= 2 {
                        let age = now - self.pts[li].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k > 0.0 {
                            let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
                            let start = mid(pt(run[m - 2]), pt(li));
                            stroke_segment(
                                pm,
                                start.0,
                                start.1,
                                self.pts[li].x,
                                self.pts[li].y,
                                width * 0.72,
                                col(core[0], core[1], core[2], 0.9 * k),
                            );
                        }
                    }
                }
                // Crisp head right under the cursor, in the core color.
                let head = self.pts.back().unwrap();
                fill_circle(
                    pm,
                    head.x,
                    head.y,
                    (c.width.value() * 0.35).max(1.5),
                    col(core[0], core[1], core[2], 0.75),
                );
            }
            TrailStyle::Striped => {
                for run in &runs {
                    let m = run.len();
                    // Arc length measured back from the newest point, so
                    // stripes stay anchored to the cursor end and extrude
                    // behind it as the trail grows.
                    let mut arc = vec![0.0f32; m];
                    for j in (0..m - 1).rev() {
                        let (ax, ay) = pt(run[j]);
                        let (bx, by) = pt(run[j + 1]);
                        arc[j] = arc[j + 1] + ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
                    }
                    for j in 1..m - 1 {
                        let idx = run[j];
                        let age = now - self.pts[idx].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k <= 0.0 {
                            continue;
                        }
                        let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
                        let (r, g, b) = stripe_color(&c.palette, arc[j], c.stripe_len);
                        let start =
                            if j == 1 { pt(run[0]) } else { mid(pt(run[j - 1]), pt(idx)) };
                        let end = mid(pt(idx), pt(run[j + 1]));
                        stroke_quad(pm, start, pt(idx), end, width, col(r, g, b, 0.9 * k));
                    }
                    let li = *run.last().unwrap();
                    if m >= 2 {
                        let age = now - self.pts[li].t;
                        let k = (1.0 - age / len).clamp(0.0, 1.0);
                        if k > 0.0 {
                            let width = (c.width.value() * (0.25 + 0.75 * k)).max(0.6);
                            let (r, g, b) = stripe_color(&c.palette, arc[m - 1], c.stripe_len);
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
                // White head dot keeps the tip crisp, like Rainbow.
                let head = self.pts.back().unwrap();
                fill_circle(pm, head.x, head.y, (c.width.value() * 0.35).max(1.5), col(1.0, 1.0, 1.0, 0.75));
            }
        }
    }

    fn bounds(&self, cfg: &EffectSettings) -> Option<RectF> {
        let c = &cfg.trail;
        if !c.enabled || self.pts.is_empty() {
            return None;
        }
        // The Neon (2.4×) and Soft (2.15×) halos are wider than the core
        // stroke — the pad must cover the widest, or style switches leave
        // stuck glow slivers.
        let pad = c.width.value() * 2.5 + 6.0;
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
    /// Resolved RGB at spawn (palette entry or classic hue).
    color: [f32; 3],
    born: f32,
    life: f32,
    wob_phase: f32,
    wob_freq: f32,
    /// Current rotation (rad) — snowflakes and ornaments.
    rot: f32,
    /// Angular velocity (rad/s), decays with movement drag.
    spin: f32,
    /// Snowflake arm variant seed.
    variant: u32,
}

const BUBBLE_HUES: [f32; 6] = [0.55, 0.75, 0.90, 0.10, 0.40, 0.63];
/// Number of distinct snowflake silhouettes.
const SNOWFLAKE_VARIANTS: u32 = 8;
/// How strongly cursor speed turns into ornament spin (rad/s per px/s).
const ORNAMENT_SPIN_PER_SPEED: f32 = 0.015;

pub struct Bubbles {
    list: Vec<Bubble>,
    acc: f32,
    next_hue: usize,
    /// Smoothed speed-dependent emission multiplier (1/3 slow … 1 at speed).
    speed_mod: f32,
    /// Seconds since the cursor last moved — fades emission out entirely
    /// when the cursor is fully stationary (speed modulation only applies
    /// while it actually moves).
    idle: f32,
}

impl Default for Bubbles {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            acc: 0.0,
            next_hue: 0,
            speed_mod: IDLE_RATE_FRACTION,
            idle: 0.0,
        }
    }
}

impl Bubbles {
    fn clear(&mut self) {
        self.list.clear();
        self.acc = 0.0;
    }

    /// Next particle color: the theme palette cycled per spawn, or the
    /// classic soap-bubble hues when no palette is configured.
    fn next_color(&mut self, palette: &[[u8; 3]]) -> [f32; 3] {
        let c = if palette.is_empty() {
            let (r, g, b) = hsv(BUBBLE_HUES[self.next_hue % BUBBLE_HUES.len()], 0.55, 0.95);
            [r, g, b]
        } else {
            rgb3(&palette[self.next_hue % palette.len()])
        };
        self.next_hue += 1;
        c
    }
}

impl Effect for Bubbles {
    fn update(&mut self, ctx: &Ctx) {
        let c = &ctx.cfg.bubbles;
        if !c.enabled {
            self.clear();
            return;
        }

        // Emission scales with cursor speed while the cursor is moving: a
        // third of the configured rate for slow drift, the full rate for real
        // movement, ramping up fast on acceleration and relaxing more slowly.
        // A fully stationary cursor fades out over EMISSION_RAMP_DOWN_SECS,
        // like the sparkles.
        let speed = (ctx.vel.0 * ctx.vel.0 + ctx.vel.1 * ctx.vel.1).sqrt();
        let target = IDLE_RATE_FRACTION
            + (1.0 - IDLE_RATE_FRACTION)
                * ((speed - SLOW_CURSOR_PX_S) / (FAST_CURSOR_PX_S - SLOW_CURSOR_PX_S))
                    .clamp(0.0, 1.0);
        let k = if target > self.speed_mod { 16.0 } else { 5.0 };
        self.speed_mod += (target - self.speed_mod) * (1.0 - (-k * ctx.dt).exp());

        if ctx.moving > 0.5 {
            self.idle = 0.0;
        } else {
            self.idle += ctx.dt;
        }
        let fade = (1.0 - self.idle / EMISSION_RAMP_DOWN_SECS).clamp(0.0, 1.0);

        self.acc += c.rate.value().max(0.0) * ctx.dt * self.speed_mod * fade;
        let momentum = c.momentum.clamp(0.0, 1.0);
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            if self.list.len() < 90 {
                let color = self.next_color(&c.palette);
                let dir = if rnd01() < 0.5 { -1.0 } else { 1.0 };
                let speed = (ctx.vel.0 * ctx.vel.0 + ctx.vel.1 * ctx.vel.1).sqrt();
                self.list.push(Bubble {
                    x: ctx.mouse.0 + rnd_range(-8.0, 8.0),
                    y: ctx.mouse.1 + rnd_range(-8.0, 8.0),
                    vx: ctx.vel.0 * momentum + rnd_range(-60.0, 60.0),
                    vy: ctx.vel.1 * momentum + rnd_range(-60.0, 20.0),
                    r: (c.size * rnd_range(0.6, 1.3)).max(2.0),
                    color,
                    born: ctx.now,
                    life: c.lifetime * rnd_range(0.7, 1.2),
                    wob_phase: rnd01() * std::f32::consts::TAU,
                    wob_freq: rnd_range(1.5, 3.2),
                    rot: rnd01() * std::f32::consts::TAU,
                    // Snowflakes tumble at the theme rate in a random
                    // direction; ornaments pick up spin from cursor speed,
                    // capped so a violent flick can't go dizzy.
                    spin: match c.shape {
                        BubbleShape::Snowflake => c.spin.value() * dir,
                        BubbleShape::Ornament => {
                            (speed * ORNAMENT_SPIN_PER_SPEED * c.spin.value().max(0.0)).min(12.0) * dir
                        }
                        BubbleShape::Bubble => 0.0,
                    },
                    variant: (rnd01() * SNOWFLAKE_VARIANTS as f32) as u32,
                });
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

        // Physics: buoyancy pulls up (negative values sink), drag calms
        // everything, wobble sways. Rotatable shapes keep spinning at their
        // spawn rate — heavy damping belongs to movement, not to the tumble.
        let drag = (-c.drag.max(0.0) * ctx.dt).exp();
        let rotatable = matches!(c.shape, BubbleShape::Snowflake | BubbleShape::Ornament);
        for b in self.list.iter_mut() {
            b.vy -= c.buoyancy.value() * ctx.dt;
            b.vx += (ctx.now * b.wob_freq + b.wob_phase).sin() * c.wobble.value() * 4.0 * ctx.dt;
            b.vx *= drag;
            b.vy *= drag;
            b.x += b.vx * ctx.dt;
            b.y += b.vy * ctx.dt;
            if rotatable {
                b.rot += b.spin * ctx.dt;
            }
        }

        // Snowflakes drift like weather — they don't collide with each other
        // or get plowed by the cursor.
        if c.shape != BubbleShape::Snowflake {
            resolve_bubble_collisions(&mut self.list);
            resolve_cursor_collisions(&mut self.list, ctx.mouse, ctx.vel);
        }
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32) {
        let c = &cfg.bubbles;
        if !c.enabled {
            return;
        }
        for b in &self.list {
            let k = (1.0 - (now - b.born) / b.life).clamp(0.0, 1.0);
            if k <= 0.0 {
                continue;
            }
            match c.shape {
                BubbleShape::Bubble => render_soap_bubble(pm, b, k),
                BubbleShape::Snowflake => render_snowflake(pm, b, k),
                BubbleShape::Ornament => render_ornament(pm, b, k),
            }
        }
    }

    fn bounds(&self, cfg: &EffectSettings) -> Option<RectF> {
        let c = &cfg.bubbles;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for bub in &self.list {
            // 1.3× the radius so ornaments' hanger cap (1.2 r) and stroke
            // anti-aliasing stay inside the dirty rect — anything outside is
            // never erased and leaves stuck pixels once the particle dies.
            let rect = RectF::point(bub.x, bub.y, bub.r * 1.3 + 10.0);
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

// --- Bubble shapes ----------------------------------------------------------

fn render_soap_bubble(pm: &mut PixmapMut, b: &Bubble, k: f32) {
    let (cr, cg, cb) = (b.color[0], b.color[1], b.color[2]);
    let r = b.r;
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

/// One six-arm snowflake as a single batched path: a spoke per arm plus two
/// side-branch pairs, with the variant seed tweaking branch angles and
/// placement so flakes differ. Bit 0 adds a third, near-tip branch pair —
/// dense "fern" flakes vs sparse stars — so all eight seeds draw distinct
/// silhouettes. The extra pair tops out at ~1.09 r from the center (arm tip
/// + half stroke), well inside the bubbles' 1.3 r dirty-rect budget.
fn snowflake_path(pb: &mut PathBuilder, x: f32, y: f32, r: f32, rot: f32, variant: u32) {
    let branch_angle = 0.55 + ((variant >> 1) & 1) as f32 * 0.3;
    let inner = 0.45 + ((variant >> 2) & 1) as f32 * 0.12;
    let pairs: &[(f32, f32)] = if variant & 1 == 0 {
        &[(inner, 0.30), (0.72, 0.22)]
    } else {
        &[(inner, 0.30), (0.72, 0.22), (0.86, 0.18)]
    };
    for arm in 0..6 {
        let a = rot + arm as f32 * std::f32::consts::TAU / 6.0;
        let (ca, sa) = (a.cos(), a.sin());
        pb.move_to(x, y);
        pb.line_to(x + ca * r, y + sa * r);
        for &(t, len) in pairs {
            let bx = x + ca * r * t;
            let by = y + sa * r * t;
            for s in [-1.0f32, 1.0] {
                let ba = a + s * branch_angle;
                pb.move_to(bx, by);
                pb.line_to(bx + ba.cos() * r * len, by + ba.sin() * r * len);
            }
        }
    }
}

fn render_snowflake(pm: &mut PixmapMut, b: &Bubble, k: f32) {
    let (cr, cg, cb) = (b.color[0], b.color[1], b.color[2]);
    let mut pb = PathBuilder::new();
    snowflake_path(&mut pb, b.x, b.y, b.r, b.rot, b.variant);
    if let Some(p) = pb.finish() {
        stroke_path(pm, &p, (b.r * 0.14).max(1.0), col(cr, cg, cb, 0.85 * k));
    }
}

fn render_ornament(pm: &mut PixmapMut, b: &Bubble, k: f32) {
    let (cr, cg, cb) = (b.color[0], b.color[1], b.color[2]);
    let r = b.r;
    let alpha = 0.95 * k;
    let (ca, sa) = (b.rot.cos(), b.rot.sin());

    // Hanger cap: orbits with the spin so the whole ornament reads as one
    // rigid body. Drawn before the ball so the ball's fill covers the
    // junction — blending cap over ball darkened the intersection while
    // fading.
    fill_circle(
        pm,
        b.x + sa * r * 1.02,
        b.y - ca * r * 1.02,
        (r * 0.18).max(0.7),
        col(0.85, 0.68, 0.25, 0.9 * k),
    );

    // Shaded ball: the fill is clipped to the silhouette; the gradient
    // center rides toward the upper-left "light" so the bottom-right reads
    // darker. (An offset fill circle would paint past the silhouette and
    // leave stale pixels outside the dirty rect.)
    radial_fill_circle_at(
        pm,
        b.x,
        b.y,
        r,
        b.x - r * 0.35,
        b.y - r * 0.35,
        r * 1.55,
        &[
            (0.0, col((cr + 1.0) * 0.5, (cg + 1.0) * 0.5, (cb + 1.0) * 0.5, alpha)),
            (0.5, col(cr, cg, cb, alpha)),
            (1.0, col(cr * 0.5, cg * 0.5, cb * 0.5, alpha)),
        ],
    );
    // Thin sheen line across the sphere, perpendicular to the cap axis —
    // the equator of the spinning ball, kept subtle rather than striped.
    stroke_segment(
        pm,
        b.x + ca * r * 0.55,
        b.y + sa * r * 0.55,
        b.x - ca * r * 0.55,
        b.y - sa * r * 0.55,
        (r * 0.14).max(1.2),
        col((cr + 1.0) * 0.5, (cg + 1.0) * 0.5, (cb + 1.0) * 0.5, 0.35 * k),
    );
    // Dark rim and fixed glossy highlight finish it off.
    stroke_circle(pm, b.x, b.y, r, 1.2, col(cr * 0.4, cg * 0.4, cb * 0.4, 0.35 * k));
    fill_circle(pm, b.x - r * 0.35, b.y - r * 0.40, (r * 0.18).max(0.7), col(1.0, 1.0, 1.0, 0.8 * k));
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
    /// Resolved RGB at spawn (palette entry or the configured color).
    color: [f32; 3],
    /// Generation: 0 = spawned at the cursor, children inherit parent + 1.
    gen: u32,
    /// Fractional child-emission accumulator.
    emit_acc: f32,
}

pub struct Sparkles {
    list: Vec<Spark>,
    acc: f32,
    /// Seconds since the cursor last moved — drives the emission ramp-down.
    idle: f32,
    /// Palette cycling counter for top-level spawns.
    next_color: usize,
}

impl Default for Sparkles {
    fn default() -> Self {
        Self { list: Vec::new(), acc: 0.0, idle: 0.0, next_color: 0 }
    }
}

impl Sparkles {
    fn clear(&mut self) {
        self.list.clear();
        self.acc = 0.0;
    }

    /// Next top-level spark color: the theme palette cycled per spawn, or the
    /// configured color when no palette is set.
    fn next_color(&mut self, base: &[u8; 3], palette: &[[u8; 3]]) -> [f32; 3] {
        let c = if palette.is_empty() {
            rgb3(base)
        } else {
            rgb3(&palette[self.next_color % palette.len()])
        };
        self.next_color += 1;
        c
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
            self.acc += c.rate.value().max(0.0) * ctx.dt * ramp;
        }
        let momentum = c.momentum.clamp(0.0, 1.0);
        while self.acc >= 1.0 {
            self.acc -= 1.0;
            if self.list.len() < 300 {
                let ang = rnd01() * std::f32::consts::TAU;
                let speed = rnd_range(30.0, 160.0);
                let color = self.next_color(&c.color, &c.palette);
                self.list.push(Spark {
                    x: ctx.mouse.0 + rnd_range(-6.0, 6.0),
                    y: ctx.mouse.1 + rnd_range(-6.0, 6.0),
                    vx: ang.cos() * speed + ctx.vel.0 * momentum,
                    vy: ang.sin() * speed + ctx.vel.1 * momentum,
                    born: ctx.now,
                    life: c.lifetime * rnd_range(0.6, 1.25),
                    size: c.size * rnd_range(0.6, 1.4),
                    phase: rnd01() * std::f32::consts::TAU,
                    freq: rnd_range(6.0, 14.0),
                    color,
                    gen: 0,
                    emit_acc: 0.0,
                });
            } else {
                self.acc = 0.0;
            }
        }

        // Bengal fire: living sparks crackle off smaller children of their
        // own color, for as many generations as the theme allows.
        if c.max_generations > 1 && c.child_rate > 0.0 {
            let mut children: Vec<Spark> = Vec::new();
            let mut population = self.list.len();
            for s in self.list.iter_mut() {
                if s.gen + 1 >= c.max_generations {
                    continue;
                }
                s.emit_acc += c.child_rate.max(0.0) * ctx.dt;
                while s.emit_acc >= 1.0 {
                    s.emit_acc -= 1.0;
                    if population >= 300 {
                        s.emit_acc = 0.0;
                        break;
                    }
                    let ang = rnd01() * std::f32::consts::TAU;
                    let speed = rnd_range(20.0, 110.0);
                    children.push(Spark {
                        x: s.x,
                        y: s.y,
                        vx: s.vx * 0.3 + ang.cos() * speed,
                        vy: s.vy * 0.3 + ang.sin() * speed,
                        born: ctx.now,
                        life: (s.life * 0.6).max(0.15),
                        size: (s.size * c.child_size).max(0.8),
                        phase: rnd01() * std::f32::consts::TAU,
                        freq: rnd_range(6.0, 14.0),
                        color: s.color,
                        gen: s.gen + 1,
                        emit_acc: 0.0,
                    });
                    population += 1;
                }
            }
            self.list.extend(children);
        }

        self.list.retain(|s| ctx.now - s.born < s.life);

        for s in self.list.iter_mut() {
            s.vy += c.gravity.value() * ctx.dt;
            let drag = (-c.drag.max(0.0) * ctx.dt).exp();
            s.vx *= drag;
            s.vy *= drag;
            s.x += s.vx * ctx.dt;
            s.y += s.vy * ctx.dt;
        }
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32) {
        let c = &cfg.sparkles;
        if !c.enabled {
            return;
        }
        for s in &self.list {
            let k = (1.0 - (now - s.born) / s.life).clamp(0.0, 1.0);
            let tw = 0.55 + 0.45 * (now * s.freq + s.phase).sin();
            let alpha = 0.9 * k * tw;
            if alpha <= 0.01 {
                continue;
            }
            let sz = (s.size * (0.35 + 0.65 * k)).max(0.8);
            let (cr, cg, cb) = (s.color[0], s.color[1], s.color[2]);

            // Motion streak: a fading line back along the velocity, capped so
            // a fast spark can't smear across the screen.
            if c.streak_secs > 0.0 {
                let mut tx = s.x - s.vx * c.streak_secs;
                let mut ty = s.y - s.vy * c.streak_secs;
                let d = (tx - s.x).hypot(ty - s.y);
                if d > 150.0 {
                    let f = 150.0 / d;
                    tx = s.x + (tx - s.x) * f;
                    ty = s.y + (ty - s.y) * f;
                }
                stroke_segment(pm, s.x, s.y, tx, ty, (sz * 0.5).max(0.8), col(cr, cg, cb, alpha * 0.5));
            }

            match c.shape {
                SparkShape::Star => {
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
                }
                SparkShape::Dot => {
                    radial_fill_circle(
                        pm,
                        s.x,
                        s.y,
                        sz,
                        &[
                            (0.0, col(cr, cg, cb, alpha)),
                            (0.5, col(cr, cg, cb, alpha * 0.55)),
                            (1.0, col(cr, cg, cb, 0.0)),
                        ],
                    );
                }
            }
            fill_circle(pm, s.x, s.y, (sz * 0.22).max(0.6), col(1.0, 1.0, 1.0, alpha));
        }
    }

    fn bounds(&self, cfg: &EffectSettings) -> Option<RectF> {
        let c = &cfg.sparkles;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for s in &self.list {
            let streak = if c.streak_secs > 0.0 {
                (s.vx.hypot(s.vy) * c.streak_secs).min(150.0)
            } else {
                0.0
            };
            let rect = RectF::point(s.x, s.y, s.size * 2.0 + 6.0 + streak);
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
    /// Random wobble phase so consecutive clicks don't repeat the shape.
    phase: f32,
}

/// Polyline segments per organic ring. Wobble lobes are sampled by this
/// polyline, so it needs at least ~4 samples per lobe at the 24-lobe cap —
/// fewer alias into a wrong lobe count (40 segments even drew 20 lobes as a
/// clean circle: the sine is zero at every sample).
const RING_SEGMENTS: usize = 96;
/// Duration of the bright radial flash at the click point.
const FLASH_SECS: f32 = 0.15;

/// Wobble amplitude `start → end` across a ring's life, with both ends
/// clamped to safe values.
fn wobble_amplitude(start: f32, end: f32, t: f32) -> f32 {
    let w0 = start.clamp(0.0, 0.35);
    let w1 = end.clamp(0.0, 0.35);
    w0 + (w1 - w0) * t.clamp(0.0, 1.0)
}

/// 1-(1-t)³ — fast start, gentle settle.
fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Ring color: the ripple palette cycled per ring, or the configured color
/// when no palette is set.
fn ring_color(base: &[u8; 3], palette: &[[u8; 3]], ring: usize) -> (f32, f32, f32) {
    if palette.is_empty() {
        rgb(base)
    } else {
        rgb(&palette[ring % palette.len()])
    }
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
        // One entry per click; the staggered rings are drawn from it.
        if ctx.clicked && self.list.len() < 24 {
            self.list.push(Ripple {
                x: ctx.mouse.0,
                y: ctx.mouse.1,
                born: ctx.now,
                phase: rnd01() * std::f32::consts::TAU,
            });
        }
        let total = c.lifetime + (c.rings.saturating_sub(1)) as f32 * 0.12;
        self.list.retain(|r| ctx.now - r.born < total.max(FLASH_SECS));
    }

    fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32) {
        let c = &cfg.ripples;
        if !c.enabled {
            return;
        }
        for r in &self.list {
            // Click-point flash: a quick bright pulse, gone before the rings
            // have gotten far.
            let flash_t = (now - r.born) / FLASH_SECS;
            if (0.0..1.0).contains(&flash_t) {
                let (fr, fg, fb) = ring_color(&c.color, &c.palette, 0);
                let radius = (c.width * 4.0 * (0.4 + 0.6 * flash_t)).max(1.0);
                radial_fill_circle(
                    pm,
                    r.x,
                    r.y,
                    radius,
                    &[
                        (0.0, col(fr, fg, fb, 0.55 * (1.0 - flash_t))),
                        (1.0, col(fr, fg, fb, 0.0)),
                    ],
                );
            }

            for ring in 0..c.rings.clamp(1, 6) {
                let age = now - r.born - ring as f32 * 0.12;
                if age <= 0.0 || age >= c.lifetime {
                    continue;
                }
                let t = age / c.lifetime;
                let k = 1.0 - t;
                // Ease-out expansion: starts fast, settles like a real
                // disturbance. Same total reach as the old linear motion.
                let radius = c.speed * c.lifetime * ease_out_cubic(t);
                let (cr, cg, cb) = ring_color(&c.color, &c.palette, ring as usize);

                // Angular wobble, interpolating from birth amplitude to
                // death amplitude: start > end settles into a clean circle,
                // start < end breaks apart as it expands.
                let wobble = wobble_amplitude(c.wobble, c.wobble_end, t);
                let lobes = c.wobble_lobes.clamp(1, 24) as f32;
                let width = (c.width * (0.4 + 0.6 * k)).max(0.5);

                let mut pb = PathBuilder::new();
                for seg in 0..RING_SEGMENTS {
                    let th = seg as f32 * std::f32::consts::TAU / RING_SEGMENTS as f32;
                    let rr = radius * (1.0 + wobble * (lobes * th + r.phase).sin());
                    let px = r.x + rr * th.cos();
                    let py = r.y + rr * th.sin();
                    if seg == 0 {
                        pb.move_to(px, py);
                    } else {
                        pb.line_to(px, py);
                    }
                }
                pb.close();
                if let Some(p) = pb.finish() {
                    // Two-pass glow: wide soft halo under a bright core.
                    stroke_path(pm, &p, width * 2.2, col(cr, cg, cb, 0.12 * k));
                    stroke_path(pm, &p, width, col(cr, cg, cb, 0.85 * k.powf(1.6)));
                }
            }
        }
    }

    fn bounds(&self, cfg: &EffectSettings) -> Option<RectF> {
        let c = &cfg.ripples;
        if !c.enabled || self.list.is_empty() {
            return None;
        }
        let mut b: Option<RectF> = None;
        for r in &self.list {
            let max_radius = c.speed * (c.lifetime + (c.rings.saturating_sub(1)) as f32 * 0.12);
            // Wobble lobes reach max_radius * (1 + amplitude) with the
            // amplitude the larger of birth and death values; the glow pass
            // is 2.2× and the flash 4× the core width. Anything narrower
            // than that leaves stuck pixels when the ripple dies.
            let wobble =
                wobble_amplitude(c.wobble, c.wobble_end, 1.0).max(wobble_amplitude(c.wobble, c.wobble_end, 0.0));
            let rect = RectF::point(r.x, r.y, max_radius * (1.0 + wobble) + c.width * 4.0 + 6.0);
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

    pub fn render(&mut self, pm: &mut PixmapMut, cfg: &EffectSettings, now: f32) {
        self.trail.render(pm, cfg, now);
        self.bubbles.render(pm, cfg, now);
        self.sparkles.render(pm, cfg, now);
        self.ripples.render(pm, cfg, now);
    }

    pub fn bounds(&self, cfg: &EffectSettings) -> Option<RectF> {
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
    use crate::settings::{BubbleCfg, NumOrExpr};

    fn bubble(x: f32, y: f32) -> Bubble {
        Bubble {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            r: 10.0,
            color: [1.0, 1.0, 1.0],
            born: 0.0,
            life: 100.0,
            wob_phase: 0.0,
            wob_freq: 1.0,
            rot: 0.0,
            spin: 0.0,
            variant: 0,
        }
    }

    fn spark(x: f32, y: f32) -> Spark {
        Spark {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            born: 0.0,
            life: 100.0,
            size: 4.0,
            phase: 0.0,
            freq: 8.0,
            color: [1.0, 1.0, 1.0],
            gen: 0,
            emit_acc: 0.0,
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
            // Generous bounds: emission tests must not lose bubbles to the
            // cursor-plow flinging them off-screen.
            screen: (10000.0, 10000.0),
        }
    }

    fn ctx_vel(mouse: (f32, f32), vel: (f32, f32)) -> Ctx<'static> {
        ctx_at(0.5, mouse, vel, 30.0, false)
    }

    fn ctx_full(mouse: (f32, f32), vel: (f32, f32), moving: f32, clicked: bool) -> Ctx<'static> {
        ctx_at(0.5, mouse, vel, moving, clicked)
    }

    fn ctx_at(
        now: f32,
        mouse: (f32, f32),
        vel: (f32, f32),
        moving: f32,
        clicked: bool,
    ) -> Ctx<'static> {
        Ctx {
            dt: 1.0 / 60.0,
            now,
            mouse,
            vel,
            clicked,
            moving,
            cfg: test_settings(),
            // Generous bounds: emission tests must not lose bubbles to the
            // cursor-plow flinging them off-screen.
            screen: (10000.0, 10000.0),
        }
    }

    fn test_settings() -> &'static EffectSettings {
        // Defaults are small consts; a leaked static keeps the borrow simple.
        Box::leak(Box::new(EffectSettings::default()))
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
            "starts at the slow fraction"
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
        // ~500ms of stillness should relax back near the slow fraction.
        for _ in 0..60 {
            b.update(&ctx_vel((100.0, 100.0), (0.0, 0.0)));
        }
        assert!(
            b.speed_mod < 0.45,
            "should relax to the slow fraction, got {}",
            b.speed_mod
        );
    }

    #[test]
    fn slow_drift_spawns_at_third_rate_and_stops_when_still() {
        // Advance the clock per frame so lifetime culling works — with a
        // frozen clock the empty-check below would race bubbles floating
        // off-screen instead (CI caught that flake).
        let mut b = Bubbles::default();
        let mut now = 0.5_f32;
        let step = |now: &mut f32| *now += 1.0 / 60.0;

        // 4s of slow drift (moving, ~100 px/s): steady state ≈ rate/3 ×
        // lifetime (9/3 × 2.4 ≈ 7) — well below the full-rate state (≈22).
        for _ in 0..240 {
            step(&mut now);
            b.update(&ctx_at(now, (5000.0, 5000.0), (100.0, 0.0), 2.0, false));
        }
        assert!(
            (3..=14).contains(&b.list.len()),
            "slow-drift population should hover around a third of full, got {}",
            b.list.len()
        );

        // 4s fully stopped: emission fades to zero and every bubble ages out
        // (max lifetime 1.2 × 2.4s < 4s), so the list must be exactly empty.
        for _ in 0..240 {
            step(&mut now);
            b.update(&ctx_at(now, (5000.0, 5000.0), (0.0, 0.0), 0.0, false));
        }
        assert!(
            b.list.is_empty(),
            "a stationary cursor must stop spawning bubbles entirely, got {}",
            b.list.len()
        );

        // 4s of fast movement for contrast: near the full steady state.
        for i in 0..240 {
            step(&mut now);
            b.update(&ctx_at(now, (5000.0, 5000.0), (2000.0, 0.0), 30.0, false));
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

    #[test]
    fn stripe_color_cycles_palette_and_falls_back() {
        let pal = vec![[255, 0, 0], [0, 255, 0]];
        let red = stripe_color(&pal, 0.0, 14.0);
        let green = stripe_color(&pal, 14.0, 14.0);
        let red_again = stripe_color(&pal, 28.0, 14.0);
        assert!(red.0 > 0.99 && red.1 < 0.01, "first stripe takes palette[0]");
        assert!(green.1 > 0.99 && green.0 < 0.01, "second stripe takes palette[1]");
        assert!(red_again.0 > 0.99, "palette cycles every stripe_len px");

        let one = stripe_color(&[[9, 9, 9]], 100.0, 14.0);
        assert!(one.0 < 0.05 && one.1 < 0.05, "a single palette entry is constant");

        let a = stripe_color(&[], 0.0, 14.0);
        let b = stripe_color(&[], 14.0, 14.0);
        assert!(a.0 > 0.8 && a.1 < 0.4, "empty palette falls back to red first");
        assert!(b.1 > 0.6 && b.0 < 0.4, "empty palette cycles to green next");
    }

    #[test]
    fn soft_colors_map_palette_roles() {
        let (core, e1, e2) = soft_colors(&[[255, 255, 255], [255, 0, 0], [0, 0, 255]]);
        assert!(core[0] > 0.99 && core[1] > 0.99, "core takes palette[0]");
        assert!(e1[0] > 0.99 && e1[1] < 0.01, "edge1 takes palette[1]");
        assert!(e2[2] > 0.99 && e2[0] < 0.01, "edge2 takes palette[2]");

        let (core, _, _) = soft_colors(&[]);
        assert!(core[0] > 0.9 && core[2] > 0.9, "empty palette falls back to a white core");

        // Two entries: edge2 keeps the built-in blue.
        let (_, e1, e2) = soft_colors(&[[255, 255, 255], [255, 0, 0]]);
        assert!(e1[0] > 0.99 && e1[1] < 0.01);
        assert!(e2[2] > 0.9 && e2[0] < 0.9);
    }

    #[test]
    fn soft_and_striped_trails_render_pixels() {
        for style in [TrailStyle::Soft, TrailStyle::Striped] {
            let mut settings = EffectSettings::default();
            settings.trail.style = style;
            settings.trail.length_secs = 1.0;
            let cfg: &'static EffectSettings = Box::leak(Box::new(settings));

            let mut t = Trail::default();
            t.push_samples(vec![(10.0, 50.0, 0.0), (30.0, 50.0, 0.02), (60.0, 50.0, 0.04)]);
            let ctx = Ctx {
                dt: 1.0 / 60.0,
                now: 0.05,
                mouse: (60.0, 50.0),
                vel: (600.0, 0.0),
                clicked: false,
                moving: 30.0,
                cfg,
                screen: (100.0, 100.0),
            };
            t.update(&ctx);
            let mut pm = tiny_skia::Pixmap::new(100, 100).unwrap();
            t.render(&mut pm.as_mut(), cfg, 0.05);
            let painted = pm.data().chunks_exact(4).filter(|p| p[3] > 0).count();
            assert!(painted > 50, "{:?} trail should paint pixels, got {}", style, painted);
        }
    }

    #[test]
    fn bubble_bounds_cover_ornament_cap() {
        let cfg: &'static EffectSettings = Box::leak(Box::new(EffectSettings::default()));
        let mut b = Bubbles::default();
        // Large ball: its hanger cap reaches 1.2 r above the center, which
        // must stay inside the dirty rect or it leaves stuck pixels.
        let mut bub = bubble(500.0, 500.0);
        bub.r = 60.0;
        b.list.push(bub);
        let rect = b.bounds(cfg).expect("bounds exist");
        assert!(
            500.0 - rect.y0 >= 60.0 * 1.2 + 1.0,
            "bounds must cover the hanger cap, pad={}",
            500.0 - rect.y0
        );
        assert!(rect.x1 - 500.0 >= 60.0, "bounds must cover the ball body");
    }

    #[test]
    fn ease_out_cubic_is_monotonic_fast_and_bounded() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..=20 {
            let v = ease_out_cubic(i as f32 / 20.0);
            assert!(v > prev, "must be strictly increasing");
            prev = v;
        }
        // Fast start: over 3/4 of the distance is covered in the first half.
        assert!(ease_out_cubic(0.5) > 0.75);
    }

    #[test]
    fn ring_color_cycles_palette_and_falls_back() {
        let base = [10, 20, 30];
        let pal = vec![[255, 0, 0], [0, 255, 0]];
        assert!(ring_color(&base, &pal, 0).0 > 0.99, "ring 0 takes palette[0]");
        assert!(ring_color(&base, &pal, 1).1 > 0.99, "ring 1 takes palette[1]");
        assert!(ring_color(&base, &pal, 2).0 > 0.99, "palette cycles");
        let (r, g, b) = ring_color(&base, &[], 3);
        assert!((r - 10.0 / 255.0).abs() < 1e-4 && (g - 20.0 / 255.0).abs() < 1e-4);
        assert!((b - 30.0 / 255.0).abs() < 1e-4, "no palette falls back to color");
    }

    #[test]
    fn click_spawns_one_ripple_and_flash_renders() {
        let mut rp = Ripples::default();
        rp.update(&ctx((100.0, 100.0), true, 0.0));
        assert_eq!(rp.list.len(), 1, "one Ripple entry per click, not one per ring");

        let cfg: &'static EffectSettings = Box::leak(Box::new(EffectSettings::default()));
        let mut pm = tiny_skia::Pixmap::new(200, 200).unwrap();
        // 20 ms after the click: inside the flash window.
        rp.render(&mut pm.as_mut(), cfg, 0.52);
        let painted = pm.data().chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(painted > 10, "click flash should paint pixels, got {}", painted);
    }

    #[test]
    fn wobble_amplitude_interpolates_and_clamps() {
        // Settling: full wobble at birth, clean circle at death.
        assert_eq!(wobble_amplitude(0.3, 0.0, 0.0), 0.3);
        assert_eq!(wobble_amplitude(0.3, 0.0, 1.0), 0.0);
        // Breaking apart: clean circle at birth, wobbly at death.
        assert!((wobble_amplitude(0.0, 0.2, 0.5) - 0.1).abs() < 1e-6);
        // Both ends clamp.
        assert_eq!(wobble_amplitude(0.9, 0.0, 0.0), 0.35);
        assert_eq!(wobble_amplitude(0.0, 0.9, 1.0), 0.35);
    }

    #[test]
    fn wobbled_ripple_bounds_cover_the_lobes() {
        let mut s = EffectSettings::default();
        // Worst case: the ring is widest at the END of its life (radius is
        // largest there), so drive the amplitude via wobble_end.
        s.ripples.wobble = 0.0;
        s.ripples.wobble_end = 0.35;
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));
        let mut rp = Ripples::default();
        rp.list.push(Ripple { x: 500.0, y: 500.0, born: 0.0, phase: 0.0 });
        let rect = rp.bounds(cfg).expect("bounds exist");
        // Lobes reach max ring radius * (1 + amplitude) at the end of life.
        let lobes = cfg.ripples.speed * cfg.ripples.lifetime * (1.0 + 0.35);
        assert!(
            500.0 - rect.y0 >= lobes - 0.5,
            "wobble lobes must stay inside the dirty rect, pad={}",
            500.0 - rect.y0
        );
    }

    #[test]
    fn trail_bounds_cover_the_widest_style_halo() {
        let mut s = EffectSettings::default();
        s.trail.style = TrailStyle::Neon;
        s.trail.width = NumOrExpr::Num(24.0);
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));
        let mut t = Trail::default();
        t.push_samples(vec![(100.0, 100.0, 0.0), (110.0, 100.0, 0.01)]);
        t.update(&ctx_at(0.5, (110.0, 100.0), (0.0, 0.0), 0.0, false));
        let rect = t.bounds(cfg).expect("bounds exist");
        assert!(
            rect.x0 <= 100.0 - 24.0 * 2.4 + 0.5,
            "neon halo must fit inside the dirty rect, x0={}",
            rect.x0
        );
    }

    #[test]
    fn snowflake_spawns_rotating_with_little_momentum() {
        let settings = bubble_settings(BubbleShape::Snowflake, 0.05, 3.5, 0.5);
        let cfg: &'static EffectSettings = Box::leak(Box::new(settings));
        let mut b = Bubbles::default();
        for frame in 0..30 {
            b.update(&ctx_at_cfg(0.5 + frame as f32 / 60.0, (100.0, 100.0), (1000.0, 0.0), 30.0, false, cfg));
        }
        assert!(!b.list.is_empty(), "snowflakes should spawn while moving");
        for flake in &b.list {
            assert!(
                (flake.spin.abs() - 0.5).abs() < 1e-4,
                "every flake spins at the theme rate (direction is random), got {}",
                flake.spin
            );
            assert!(
                flake.vx.abs() < 0.05 * 1000.0 + 61.0,
                "snowflake momentum inheritance should be tiny, vx={}",
                flake.vx
            );
            assert!(flake.variant < SNOWFLAKE_VARIANTS);
        }
    }

    #[test]
    fn snowflake_variants_draw_eight_distinct_silhouettes() {
        // Every seed must produce its own raster: bit 0 adds a near-tip
        // branch pair, bit 1 widens the branch angle, bit 2 moves the inner
        // pair outward. Rendering is deterministic, so identical lit-pixel
        // sets would mean two seeds share one silhouette.
        let mut silhouettes = std::collections::HashSet::new();
        for variant in 0..SNOWFLAKE_VARIANTS {
            let b = Bubble {
                x: 60.0,
                y: 60.0,
                vx: 0.0,
                vy: 0.0,
                r: 42.0,
                color: [1.0, 1.0, 1.0],
                born: 0.0,
                life: 1.0,
                wob_phase: 0.0,
                wob_freq: 0.0,
                rot: 0.0,
                spin: 0.0,
                variant,
            };
            let mut pm = tiny_skia::Pixmap::new(120, 120).unwrap();
            render_snowflake(&mut pm.as_mut(), &b, 1.0);
            let lit: Vec<(u32, u32)> = pm
                .data()
                .chunks_exact(4)
                .enumerate()
                .filter(|(_, p)| p[3] > 0)
                .map(|(i, _)| (i as u32 % 120, i as u32 / 120))
                .collect();
            assert!(!lit.is_empty(), "variant {} must draw pixels", variant);
            silhouettes.insert(lit);
        }
        assert_eq!(
            silhouettes.len(),
            SNOWFLAKE_VARIANTS as usize,
            "all eight snowflake seeds must draw distinct silhouettes"
        );
    }

    #[test]
    fn ornaments_pick_up_spin_and_damp_faster_than_bubbles() {
        // Fast movement should kick ornaments into a spin.
        let orn = bubble_settings(BubbleShape::Ornament, 0.35, 6.0, 1.0);
        let orn_cfg: &'static EffectSettings = Box::leak(Box::new(orn));
        let mut o = Bubbles::default();
        for frame in 0..12 {
            o.update(&ctx_at_cfg(0.5 + frame as f32 / 60.0, (100.0, 100.0), (2000.0, 0.0), 30.0, false, orn_cfg));
        }
        assert!(
            o.list.iter().any(|b| b.spin.abs() > 3.0),
            "fast movement should spin ornaments, spins={:?}",
            o.list.iter().map(|b| b.spin).collect::<Vec<_>>()
        );

        // Same starting velocity, heavier drag: ornaments must calm down
        // strictly faster than classic bubbles. Bubbles are placed by hand
        // with the cursor far away so no fresh spawns or collisions muddy the
        // comparison.
        let classic =
            EffectSettings { bubbles: BubbleCfg { drag: 2.2, ..orn_cfg.bubbles.clone() }, ..orn_cfg.clone() };
        let classic_cfg: &'static EffectSettings = Box::leak(Box::new(classic));
        let mut o = Bubbles::default();
        let mut c = Bubbles::default();
        let mut ob = bubble(5000.0, 5000.0);
        ob.vx = 700.0;
        let mut cb = bubble(5000.0, 5000.0);
        cb.vx = 700.0;
        o.list.push(ob);
        c.list.push(cb);
        for frame in 0..30 {
            let now = 1.0 + frame as f32 / 60.0;
            o.update(&ctx_at_cfg(now, (0.0, 0.0), (0.0, 0.0), 0.0, false, orn_cfg));
            c.update(&ctx_at_cfg(now, (0.0, 0.0), (0.0, 0.0), 0.0, false, classic_cfg));
        }
        let os = o.list[0].vx.hypot(o.list[0].vy);
        let cs = c.list[0].vx.hypot(c.list[0].vy);
        assert!(os < cs, "ornaments should damp faster (orn={} classic={})", os, cs);
    }

    #[test]
    fn bubble_palette_resolves_per_particle() {
        let mut settings = bubble_settings(BubbleShape::Bubble, 0.35, 2.2, 0.0);
        settings.bubbles.palette = vec![[255, 0, 0], [0, 0, 255]];
        let cfg: &'static EffectSettings = Box::leak(Box::new(settings));
        let mut b = Bubbles::default();
        for frame in 0..40 {
            b.update(&ctx_at_cfg(0.5 + frame as f32 / 60.0, (100.0, 100.0), (600.0, 0.0), 30.0, false, cfg));
        }
        assert!(b.list.len() >= 4, "should have spawned several, got {}", b.list.len());
        for bub in &b.list {
            let is_red = bub.color[0] > 0.99 && bub.color[1] < 0.01 && bub.color[2] < 0.01;
            let is_blue = bub.color[2] > 0.99 && bub.color[0] < 0.01 && bub.color[1] < 0.01;
            assert!(is_red || is_blue, "palette should resolve exactly, got {:?}", bub.color);
        }
        // Both palette entries must actually appear over several spawns.
        let reds = b.list.iter().filter(|b| b.color[0] > 0.99).count();
        assert!(reds > 0 && reds < b.list.len(), "palette should cycle, reds={} of {}", reds, b.list.len());
    }

    /// An EffectSettings with the bubbles section set for a given shape, everything
    /// else at defaults.
    fn bubble_settings(shape: BubbleShape, momentum: f32, drag: f32, spin: f32) -> EffectSettings {
        let mut s = EffectSettings::default();
        s.bubbles.shape = shape;
        s.bubbles.momentum = momentum;
        s.bubbles.drag = drag;
        s.bubbles.spin = NumOrExpr::Num(spin);
        s.bubbles.rate = NumOrExpr::Num(30.0);
        s
    }

    #[test]
    fn spark_children_appear_up_to_max_generations() {
        let mut s = EffectSettings::default();
        // rate 0: no cursor spawns, so the population dynamics carry no RNG
        // dependence — the test verifies the generational rules directly.
        s.sparkles.rate = NumOrExpr::Num(0.0);
        s.sparkles.child_rate = 60.0;
        s.sparkles.max_generations = 3;
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));

        let mut sp = Sparkles::default();
        // Three long-lived gen-1 parents: they may emit gen-2 children, but
        // nothing may reach gen 3.
        for i in 0..3 {
            let mut parent = spark(i as f32 * 10.0, 100.0);
            parent.gen = 1;
            sp.list.push(parent);
        }
        for frame in 0..30 {
            sp.update(&ctx_at_cfg(
                0.5 + frame as f32 / 60.0,
                (-500.0, -500.0),
                (0.0, 0.0),
                30.0,
                false,
                cfg,
            ));
        }
        assert!(
            sp.list.iter().any(|s| s.gen == 2),
            "gen-2 children must appear"
        );
        assert!(
            sp.list.iter().all(|s| s.gen < 3),
            "no spark may exceed max_generations"
        );
        assert!(sp.list.len() <= 300, "population cap must hold, got {}", sp.list.len());
    }

    #[test]
    fn sparks_stop_at_max_generations_two() {
        let mut s = EffectSettings::default();
        s.sparkles.rate = NumOrExpr::Num(0.0);
        s.sparkles.child_rate = 60.0;
        s.sparkles.max_generations = 2;
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));

        let mut sp = Sparkles::default();
        for i in 0..3 {
            let mut parent = spark(i as f32 * 10.0, 100.0);
            parent.gen = 1;
            sp.list.push(parent);
        }
        for frame in 0..30 {
            sp.update(&ctx_at_cfg(
                0.5 + frame as f32 / 60.0,
                (-500.0, -500.0),
                (0.0, 0.0),
                30.0,
                false,
                cfg,
            ));
        }
        assert_eq!(sp.list.len(), 3, "gen-1 sparks with max_generations 2 emit nothing");
        assert!(sp.list.iter().all(|s| s.gen == 1));
    }

    #[test]
    fn sparks_emit_no_children_by_default() {
        let mut sp = Sparkles::default();
        for frame in 0..60 {
            sp.update(&ctx_at(0.5 + frame as f32 / 60.0, (100.0, 100.0), (300.0, 0.0), 30.0, false));
        }
        assert!(!sp.list.is_empty());
        assert!(sp.list.iter().all(|s| s.gen == 0), "default config has no child emission");
    }

    #[test]
    fn spark_palette_cycles_and_children_inherit_color() {
        let mut s = EffectSettings::default();
        s.sparkles.rate = NumOrExpr::Num(40.0);
        s.sparkles.child_rate = 60.0;
        s.sparkles.max_generations = 2;
        s.sparkles.palette = vec![[255, 0, 0], [0, 255, 0]];
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));

        let mut sp = Sparkles::default();
        for frame in 0..90 {
            sp.update(&ctx_at_cfg(0.5 + frame as f32 / 60.0, (100.0, 100.0), (300.0, 0.0), 30.0, false, cfg));
        }
        assert!(sp.list.iter().any(|s| s.gen == 1), "children should exist");
        for sp in &sp.list {
            let is_red = sp.color[0] > 0.99 && sp.color[1] < 0.01;
            let is_green = sp.color[1] > 0.99 && sp.color[0] < 0.01;
            assert!(is_red || is_green, "palette should resolve exactly, got {:?}", sp.color);
        }
        // Children take the parent's color: every generation-1 spark's color
        // must match some top-level spark's color (they come from the same
        // two-entry palette, which the loop above already checks) — here we
        // just assert both palette entries occur overall.
        let reds = sp.list.iter().filter(|s| s.color[0] > 0.99).count();
        assert!(reds > 0 && reds < sp.list.len(), "palette should cycle");
    }

    #[test]
    fn streak_extends_bounds() {
        let mut s = EffectSettings::default();
        s.sparkles.streak_secs = 0.5;
        let cfg: &'static EffectSettings = Box::leak(Box::new(s));
        let mut sp = Sparkles::default();
        let mut spark = Spark {
            x: 500.0,
            y: 500.0,
            vx: 400.0,
            vy: 0.0,
            born: 0.0,
            life: 10.0,
            size: 4.0,
            phase: 0.0,
            freq: 8.0,
            color: [1.0, 0.8, 0.2],
            gen: 0,
            emit_acc: 0.0,
        };
        spark.born = 0.0;
        sp.list.push(spark);
        let b = sp.bounds(cfg).expect("bounds exist");
        // Streak reaches 400 px/s × 0.5 s = 200 px → capped at 150. Base
        // radius is size*2+6 = 14, so the half-extent must exceed it.
        assert!(b.x1 - b.x0 > 2.0 * 14.0 + 1.0, "streak should extend bounds");
        // Capped at 150: half-extent ≤ 14 + 150 + 1.
        assert!(b.x1 - b.x0 <= 2.0 * (14.0 + 150.0) + 1.0);
    }

    fn ctx_at_cfg(
        now: f32,
        mouse: (f32, f32),
        vel: (f32, f32),
        moving: f32,
        clicked: bool,
        cfg: &'static EffectSettings,
    ) -> Ctx<'static> {
        Ctx {
            dt: 1.0 / 60.0,
            now,
            mouse,
            vel,
            clicked,
            moving,
            cfg,
            screen: (10000.0, 10000.0),
        }
    }
}
