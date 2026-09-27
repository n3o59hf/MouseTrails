//! Ground-truth screen capture via DXGI Desktop Duplication — sees exactly
//! what DWM composites to the monitor, including layered windows.
//!
//! Usage: `cargo run --release --example screen_dump [out.png]`
//! Captures primary monitor output ~3 seconds after start.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::*;

fn main() {
    let wait_ms: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let out = std::env::args().nth(1).unwrap_or_else(|| "screen_dump.png".into());
    if wait_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(wait_ms as u64));
    }
    unsafe {
        // D3D11 device
        let mut device: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        let flags = D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0);
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .expect("D3D11CreateDevice");
        let device = device.unwrap();
        let ctx = ctx.unwrap();

        let dxgi: IDXGIDevice = device.cast().expect("cast IDXGIDevice");
        let adapter = dxgi.GetAdapter().expect("GetAdapter");
        let output: IDXGIOutput = adapter.EnumOutputs(0).expect("EnumOutputs(0)");
        let output1: IDXGIOutput1 = output.cast().expect("cast IDXGIOutput1");

        let dup = output1
            .DuplicateOutput(&device)
            .expect("DuplicateOutput");

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // Keep acquiring until we get a fresh frame.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let hr = dup.AcquireNextFrame(500, &mut frame_info, &mut resource);
            if hr.is_ok() && frame_info.LastPresentTime != 0 {
                break;
            }
            if hr.is_ok() {
                let _ = dup.ReleaseFrame();
            }
            if std::time::Instant::now() > deadline {
                // Take whatever we have (may be the initial full frame).
                let hr2 = dup.AcquireNextFrame(1000, &mut frame_info, &mut resource);
                if hr2.is_err() {
                    panic!("no frames available: {hr2:?}");
                }
                break;
            }
        }
        let resource = resource.expect("frame resource");
        let tex: ID3D11Texture2D = resource.cast().expect("cast texture");
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);

        // staging copy
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.SampleDesc.Count = 1;
        desc.SampleDesc.Quality = 0;
        let mut tex_opt: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&desc, None, Some(&mut tex_opt)).expect("CreateTexture2D staging");
        let staging = tex_opt.unwrap();
        let staging_res: ID3D11Resource = staging.cast().unwrap();
        let tex_res: ID3D11Resource = tex.cast().unwrap();
        ctx.CopyResource(&staging_res, &tex_res);

        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        ctx.Map(&staging_res, 0, D3D11_MAP_READ, 0, Some(&mut map))
            .expect("Map");
        let (w, h) = (desc.Width as u32, desc.Height as u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for row in 0..h as usize {
            let src = map.pData as *const u8;
            let src_row = std::slice::from_raw_parts(src.add(row * map.RowPitch as usize), (w * 4) as usize);
            rgba[row * (w as usize) * 4..(row + 1) * (w as usize) * 4].copy_from_slice(src_row);
        }
        ctx.Unmap(&staging_res, 0);
        let _ = dup.ReleaseFrame();

        if let Some(pm) = tiny_skia::Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(w, h).unwrap()) {
            std::fs::write(&out, pm.encode_png().unwrap()).expect("write png");
            println!("saved {out} ({}x{})", w, h);
        }
    }
}
