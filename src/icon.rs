//! Procedurally paints the MouseTrails icon: a rainbow ribbon arc with a
//! bright head. Returned in three flavors for the different consumers
//! (win32 tray icon wants premultiplied BGRA, egui wants straight RGBA).

const SIZE: usize = 32;
const TAU: f32 = std::f32::consts::TAU;

pub fn icon_bgra_premul() -> Vec<u8> {
    let mut b = paint();
    for px in b.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    b
}

pub fn icon_rgba_straight() -> Vec<u8> {
    let mut b = paint();
    for px in b.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 {
            for ch in 0..3 {
                px[ch] = (((px[ch] as u32 * 255 + a / 2) / a).min(255)) as u8;
            }
        }
    }
    b
}

fn paint() -> Vec<u8> {
    let mut buf = vec![0u8; SIZE * SIZE * 4];
    let n = 11;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let x = 3.0 + 24.0 * t;
        let y = 27.0 - 21.0 * t + 2.5 * (t * TAU).sin();
        let r = 1.7 + 6.0 * t * t;
        let a = 0.25 + 0.70 * t;
        let hue = 0.02 + 0.75 * t;
        let (cr, cg, cb) = hsv(hue, 0.85, 1.0);
        blend_dot(&mut buf, x, y, r, cr, cg, cb, a);
    }
    // Bright head at the end of the ribbon.
    blend_dot(&mut buf, 27.0, 6.0, 2.6, 1.0, 1.0, 1.0, 0.95);
    buf
}

fn blend_dot(buf: &mut [u8], cx: f32, cy: f32, r: f32, cr: f32, cg: f32, cb: f32, alpha: f32) {
    let r2 = r * r;
    for y in 0..SIZE {
        let dy = y as f32 + 0.5 - cy;
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - cx;
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let edge = (1.0 - d2.sqrt() / r).clamp(0.0, 1.0);
            let a = (alpha * edge).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let i = (y * SIZE + x) * 4;
            let da = buf[i + 3] as f32 / 255.0;
            for (ch, src) in [(0usize, cr), (1, cg), (2, cb)] {
                let dst = buf[i + ch] as f32 / 255.0;
                // Premultiplied source-over.
                buf[i + ch] = ((src * a + dst * (1.0 - a)).clamp(0.0, 1.0) * 255.0) as u8;
            }
            buf[i + 3] = ((a + da * (1.0 - a)).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
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
