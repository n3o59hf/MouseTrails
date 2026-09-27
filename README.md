# MouseTrails

Windows Plus!-style cursor effects that live in your system tray. A little
rainbow icon sits near the clock; your cursor leaves a rainbow ribbon, soap
bubbles drift up from it (click to pop them), and rings ripple out when you
click.

![demo](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)

## Features

- **Mouse Trail** — a ribbon that flows behind the cursor. Three styles:
  *Rainbow ribbon*, *Neon comet* (single color with glow), and *Ghost fade*.
- **Bubbles** — soap bubbles spawn as you move, drift upward with buoyancy,
  drag, wobble, and are gently pushed around by the cursor. Left-click pops
  nearby bubbles.
- **Sparkles** — twinkling four-point stars scatter as the cursor moves,
  with configurable gravity (negative floats up).
- **Click ripples** — expanding rings from every left click.
- **Settings window** — every effect can be toggled and tuned live (rates,
  sizes, lifetimes, colors, trail style). Changes apply instantly and save
  automatically.
- **Start with Windows** — checkbox in the settings window (or the tray
  menu); writes/removes `HKCU\...\CurrentVersion\Run`.
- Single instance, click-through overlay (never intercepts input), settings
  apply across all monitors.

## Building

```
cargo build --release
```

The result is a **single self-contained `MouseTrails.exe`** (~5 MB): the C
runtime is statically linked (`.cargo/config.toml` sets
`target-feature=+crt-static`) and the binary is LTO-optimized and stripped, so
it needs no Visual C++ Redistributable — just copy the exe anywhere and run it
(Windows 10/11; the only imports are system DLLs like `opengl32` and `dwmapi`).
Settings stay per-user in `%APPDATA%\MouseTrails\config.json`. A convenience
copy is kept at the repo root (gitignored).

Optional flag:

- `--settings` — open the settings window at startup (if another instance is
  already running, it just brings its settings window to the front).

## Using it

The app starts minimized to the tray (it may be inside the `^` overflow next
to the clock — drag it onto the taskbar to pin it there):

- **Left-click** the tray icon — open Settings.
- **Right-click** — menu: *Open Settings…*, *Start with Windows*, *Exit*.

Settings are stored in `%APPDATA%\MouseTrails\config.json`.

## How it works

- A fullscreen, transparent, click-through **layered window** (`WS_EX_LAYERED
  | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW`) covers the virtual
  screen.
- A background **cursor sampler thread** polls `GetCursorPos` ~every
  millisecond (`timeBeginPeriod(1)` + `Sleep(1)`), so the trail follows the
  real cursor path — including direction changes that happen between render
  frames. Samples are interpolated to ≤4 px spacing and the ribbon is drawn
  with quadratic midpoint smoothing, which keeps it continuous and rounded
  even during fast, jerky movement.
- Effects are simulated and rendered on the main thread at a configurable FPS
  (`WM_TIMER`) into a 32-bit DIB via **tiny-skia** (software rendering, no GPU
  dependency). The DIB memory is handed to tiny-skia directly — zero copies.
- Colors are created with R/B pre-swapped so tiny-skia's RGBA output matches
  the BGRA layout `UpdateLayeredWindow` expects.
- Each rendered frame is pushed to the screen with
  `UpdateLayeredWindowIndirect` (full-window update — see note below), and
  only when something is actually animating; idle costs one timer tick.
- The tray icon is generated procedurally at startup (rainbow ribbon arc).
- Settings UI is **egui/eframe** on a worker thread (`winit`'s
  `any_thread(true)` via eframe's `event_loop_builder` hook — supported on
  Windows).

### Note on dirty rectangles

`UpdateLayeredWindowIndirect`'s `prcDirty` partial updates silently fail to
composite on some Windows builds (observed on Windows 11 26200), so this app
pushes the full window each rendered frame instead. The buffer is only
touched inside the effects' bounding box, so the CPU cost stays modest.

## Diagnostics

- `examples/screen_dump.rs` — captures the true composited screen (DXGI
  desktop duplication, includes layered windows that GDI captures miss):
  `cargo run --release --example screen_dump out.png [wait_ms]`
- `MOUSETRAILS_DEBUG=1` — log to `%TEMP%\mousetrails_debug.log`
- `MOUSETRAILS_DUMP=1` — dump the overlay's framebuffer to
  `%TEMP%\mousetrails_dump.png` (every 20 rendered frames)

## Project layout

```
src/main.rs         entry, single-instance guard, DPI awareness
src/overlay.rs      fullscreen layered window, tray icon + menu, render loop
src/effects.rs      effect simulation + tiny-skia rendering (trail, bubbles,
                    sparkles, ripples)
src/settings.rs     settings model + JSON persistence
src/settings_ui.rs  egui settings window + thread management
src/startup.rs      HKCU Run registry helpers
src/icon.rs         procedural tray/window icon
```
