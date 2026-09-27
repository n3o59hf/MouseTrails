//! Bakes `assets/mousetrails.ico` from the procedural icon painter.
//!
//! Usage: `cargo run --release --example make_icon`
//!
//! The .ico contains BMP-style 32bpp entries at several sizes (Windows picks
//! the best one for each context; small sizes get scaled-down geometry).

use mousetrails::icon::render_rgba_premul;
use std::io::Write;

const SIZES: [u8; 5] = [16, 24, 32, 48, 64];

fn main() {
    // One premultiplied RGBA raster per size.
    let rasters: Vec<Vec<u8>> = SIZES.iter().map(|&s| render_rgba_premul(s as u32)).collect();

    // ICO layout: 6-byte header, N×16-byte directory entries, then images.
    let mut images: Vec<Vec<u8>> = Vec::new();
    for (&sz, rgba) in SIZES.iter().zip(&rasters) {
        images.push(bmp_entry(sz as usize, rgba));
    }

    let mut out: Vec<u8> = Vec::new();
    // ICONDIR
    out.write_u16_le(0); // reserved
    out.write_u16_le(1); // type: icon
    out.write_u16_le(SIZES.len() as u16);
    // Directory entries
    let mut offset = 6 + 16 * SIZES.len();
    for (&sz, img) in SIZES.iter().zip(&images) {
        // Our sizes are all < 256, so the raw byte is the pixel dimension.
        out.push(sz); // width
        out.push(sz); // height
        out.push(0); // color count (truecolor)
        out.push(0); // reserved
        out.write_u16_le(1); // planes
        out.write_u16_le(32); // bits per pixel
        out.write_u32_le(img.len() as u32);
        out.write_u32_le(offset as u32);
        offset += img.len();
    }
    for img in &images {
        out.extend_from_slice(img);
    }

    std::fs::create_dir_all("assets").expect("create assets dir");
    let mut f = std::fs::File::create("assets/mousetrails.ico").expect("create ico");
    f.write_all(&out).expect("write ico");
    println!("wrote assets/mousetrails.ico ({} bytes, sizes {:?})", out.len(), SIZES);
}

/// BMP-style ICO image: BITMAPINFOHEADER + bottom-up BGRA pixels + AND mask.
fn bmp_entry(size: usize, rgba_premul: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    // BITMAPINFOHEADER — biHeight is doubled (XOR + AND masks).
    out.write_u32_le(40);
    out.write_i32_le(size as i32);
    out.write_i32_le((size * 2) as i32);
    out.write_u16_le(1); // planes
    out.write_u16_le(32); // bpp
    out.write_u32_le(0); // BI_RGB
    out.write_u32_le((size * size * 4 + and_mask_row(size) * size) as u32);
    out.write_i32_le(0); // x pels
    out.write_i32_le(0); // y pels
    out.write_u32_le(0); // colors used
    out.write_u32_le(0); // important colors

    // XOR mask: bottom-up rows of BGRA. Source is premultiplied RGBA
    // top-down; ICO wants straight-ish values but premultiplied is what
    // alpha-aware consumers pass through — unpremultiply for correctness.
    for y in (0..size).rev() {
        for x in 0..size {
            let i = (y * size + x) * 4;
            let a = rgba_premul[i + 3] as u32;
            let (r, g, b) = if a > 0 {
                (
                    ((rgba_premul[i] as u32 * 255 + a / 2) / a).min(255) as u8,
                    ((rgba_premul[i + 1] as u32 * 255 + a / 2) / a).min(255) as u8,
                    ((rgba_premul[i + 2] as u32 * 255 + a / 2) / a).min(255) as u8,
                )
            } else {
                (0, 0, 0)
            };
            out.push(b);
            out.push(g);
            out.push(r);
            out.push(rgba_premul[i + 3]);
        }
    }
    // AND mask: all opaque (alpha channel carries transparency).
    let row = and_mask_row(size);
    for _ in 0..size {
        out.extend(std::iter::repeat(0u8).take(row));
    }
    out
}

fn and_mask_row(size: usize) -> usize {
    ((size + 31) / 32) * 4
}

trait WriteLE {
    fn write_u16_le(&mut self, v: u16);
    fn write_u32_le(&mut self, v: u32);
    fn write_i32_le(&mut self, v: i32);
}
impl WriteLE for Vec<u8> {
    fn write_u16_le(&mut self, v: u16) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn write_u32_le(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes());
    }
    fn write_i32_le(&mut self, v: i32) {
        self.extend_from_slice(&v.to_le_bytes());
    }
}
