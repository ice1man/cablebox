//! CableBox: a retro cable-box shell for Tvheadend.
//!
//! Runs as the device's only session straight on KMS/DRM (`--features kms`), or in a
//! window when started from a desktop (`--features winit`). See `display::init_session`.

mod app;
mod clock;
mod config;
mod debug;
mod display;
mod entry;
mod guide;
mod learn;
mod lineup;
mod pin;
mod sound;
mod views;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use player::{GlRenderer, Player};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, GraphicsAPI, PhysicalSize, RenderingState, Timer, TimerMode};

use crate::app::App;
use crate::config::Config;
use crate::display::Session;

slint::include_modules!();

/// Redraw every refresh instead of only when mpv has a new frame (Smooth motion while
/// video plays: mpv's frame timing needs a render and `report_swap` per vsync).
pub static EVERY_FRAME: AtomicBool = AtomicBool::new(false);

/// Frames drawn so far (the debug overlay's output frame rate).
pub static FRAMES_DRAWN: AtomicU64 = AtomicU64::new(0);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,cablebox=debug".into()),
        )
        .init();

    let (cfg, config_problem) = match Config::load() {
        Ok(cfg) => (cfg, None),
        Err(problem) => {
            tracing::error!("{}: {}; using defaults and leaving the file alone", problem.path.display(), problem.message);
            (Config::default(), Some(problem))
        }
    };

    // Must happen before the Slint backend initializes.
    let session = display::init_session();
    if session == Session::Tv {
        display::configure_backend(&cfg.display.output, &cfg.display.mode, cfg.display.scale);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("cablebox-io")
        .enable_all()
        .build()
        .context("starting async runtime")?;

    let ui = AppWindow::new().context("creating window")?;
    let player = Arc::new(Player::new(|ev| app::post(move |a| a.on_player(ev))).context("starting mpv")?);
    setup_video_underlay(&ui, player.clone(), cfg.display.scale);

    if std::env::args().any(|a| a == "--fullscreen") {
        ui.window().set_fullscreen(true);
    }

    let weak = ui.as_weak();
    ui.on_key(move |text| {
        if session == Session::Window && text == slint::SharedString::from(slint::platform::Key::F11) {
            if let Some(ui) = weak.upgrade() {
                let w = ui.window();
                w.set_fullscreen(!w.is_fullscreen());
            }
            return true;
        }
        app::with_app(|a| a.on_key_text(&text)).unwrap_or(false)
    });

    // In a window the keyboard arrives through the window; don't grab the desktop's devices.
    // Development only: a window can serve a receiver at a given path, to work on its Settings.
    let pretend_pico = std::env::var_os("CABLEBOX_PICO_DEVICE").filter(|_| std::env::var_os("CABLEBOX_PRETEND_BOX").is_some());
    let on_event = |e| app::post(move |a| a.on_remote_event(e));
    let bindings = learn::parse_table(&cfg.remote.buttons);
    let remote = match (cfg.remote.cec, session, pretend_pico) {
        (true, Session::Tv, _) => Some(input::remote::spawn(bindings, on_event)),
        (true, Session::Window, Some(path)) => Some(input::remote::spawn_pico_only(path.into(), bindings, on_event)),
        _ => None,
    };
    if let Some(path) = std::env::var_os("CABLEBOX_CONTROL") {
        input::control::spawn(path.into(), |k| app::post(move |a| a.on_key(k)));
    }

    app::install(App::new(&ui, rt.handle().clone(), player, cfg, config_problem, remote));

    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(200), || {
        app::with_app(|a| a.tick());
    });
    // Start once the window exists so its size (and our scale) is known.
    let weak = ui.as_weak();
    Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(ui) = weak.upgrade() {
            window_refresh_rate(&ui);
        }
        // Applies Smooth motion with whatever rate is known by now.
        app::with_app(|a| a.start());
    });

    ui.run().context("running UI")?;
    app::with_app(|a| a.save_config_now());
    Ok(())
}

/// Draws mpv's video into the window's GL framebuffer before Slint paints the UI on top.
fn setup_video_underlay(ui: &AppWindow, player: Arc<Player>, scale: Option<f32>) {
    let renderer: RefCell<Option<GlRenderer>> = RefCell::new(None);
    let last_size = Rc::new(Cell::new(PhysicalSize::default()));
    let weak = ui.as_weak();

    let result = ui.window().set_rendering_notifier(move |state, api| match state {
        RenderingState::RenderingSetup => {
            let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                tracing::error!("video needs an OpenGL renderer (use the femtovg renderer)");
                return;
            };
            // SAFETY: Slint calls this with its GL context current.
            if let Some(name) = unsafe { gl_renderer_name(get_proc_address) } {
                display::set_gpu(name);
                // Hardware acceleration depends on it; apply once the app exists.
                Timer::single_shot(Duration::ZERO, || {
                    app::with_app(|a| a.apply_video_settings());
                });
            }
            // SAFETY: as above.
            match unsafe { player.create_gl_renderer(get_proc_address) } {
                Ok(mut r) => {
                    let w = weak.clone();
                    r.set_update_callback(move || {
                        let w = w.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = w.upgrade() {
                                ui.window().request_redraw();
                            }
                        });
                    });
                    *renderer.borrow_mut() = Some(r);
                }
                Err(e) => tracing::error!("creating video renderer: {e}"),
            }
        }
        RenderingState::BeforeRendering => {
            let Some(ui) = weak.upgrade() else { return };
            let size = ui.window().size();
            if let Some(r) = renderer.borrow().as_ref() {
                r.render(size.width, size.height);
            }
            if size != last_size.get() {
                last_size.set(size);
                // Can't change the scale mid-frame; do it right after.
                let w = weak.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    apply_scale(&w, scale);
                    let new_rate = w.upgrade().is_some_and(|ui| window_refresh_rate(&ui));
                    app::with_app(|a| {
                        a.apply_video_rect();
                        if new_rate {
                            a.apply_video_settings();
                        }
                    });
                });
            }
        }
        RenderingState::AfterRendering => {
            FRAMES_DRAWN.fetch_add(1, Ordering::Relaxed);
            if let Some(r) = renderer.borrow().as_ref() {
                r.report_swap();
            }
            if EVERY_FRAME.load(Ordering::Relaxed) {
                if let Some(ui) = weak.upgrade() {
                    ui.window().request_redraw();
                }
            }
        }
        RenderingState::RenderingTeardown => {
            renderer.borrow_mut().take();
        }
        _ => {}
    });
    if let Err(e) = result {
        tracing::error!("cannot attach video renderer: {e}");
    }
}

/// `GL_RENDERER` of the current context: the GPU and driver, or e.g. "llvmpipe" when Mesa
/// renders on the CPU.
///
/// # Safety
/// A GL context must be current on this thread.
unsafe fn gl_renderer_name(get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void) -> Option<String> {
    const GL_RENDERER: u32 = 0x1F01;
    let f = get_proc_address(c"glGetString");
    if f.is_null() {
        return None;
    }
    let get_string = std::mem::transmute::<*const std::ffi::c_void, unsafe extern "system" fn(u32) -> *const u8>(f);
    let p = get_string(GL_RENDERER);
    (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p.cast()).to_string_lossy().into_owned())
}

/// In a window, the refresh rate comes from the monitor it's on (KMS reads it at startup).
/// Checked again on every resize: Wayland only reports the monitor once the window is
/// shown, and the window can move. True when it changed.
#[cfg(feature = "winit")]
fn window_refresh_rate(ui: &AppWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    if display::session() != Session::Window {
        return false;
    }
    let mhz = ui.window().with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz()));
    let Some(mhz) = mhz.flatten() else { return false };
    let changed = display::set_refresh_hz(mhz as f64 / 1000.0);
    if changed {
        tracing::info!("monitor refresh: {:.2} Hz", mhz as f64 / 1000.0);
    }
    changed
}

#[cfg(not(feature = "winit"))]
fn window_refresh_rate(_: &AppWindow) -> bool {
    false
}

/// Scales the 1280x720 design to the window (or a fixed factor from settings).
pub fn apply_scale(ui: &slint::Weak<AppWindow>, scale: Option<f32>) {
    let Some(ui) = ui.upgrade() else { return };
    let w = ui.window();
    let phys = w.size();
    if phys.height == 0 {
        return;
    }
    let s = display::ui_scale(phys.height, scale);
    if (w.scale_factor() - s).abs() > 0.001 {
        w.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: s });
        w.dispatch_event(WindowEvent::Resized { size: phys.to_logical(s) });
    }
    let logical = phys.to_logical(w.scale_factor());
    tracing::info!(
        "layout: {}x{} px, scale {:.2} → {:.0}x{:.0}",
        phys.width,
        phys.height,
        w.scale_factor(),
        logical.width,
        logical.height
    );
}
