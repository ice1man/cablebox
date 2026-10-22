//! CableBox: a retro cable-box shell for Tvheadend.
//!
//! Run windowed for development (`SLINT_BACKEND=winit`), or as the device's only
//! session straight on KMS/DRM (`--features kms`, `SLINT_BACKEND=linuxkms`).

mod app;
mod config;
mod entry;
mod guide;
mod lineup;
mod views;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use player::{GlRenderer, Player};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, GraphicsAPI, PhysicalSize, RenderingState, Timer, TimerMode};

use crate::app::App;
use crate::config::Config;

slint::include_modules!();

/// The UI is laid out for a 1280x720 logical canvas.
const DESIGN_HEIGHT: f32 = 720.0;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,cablebox=debug".into()),
        )
        .init();

    let cfg = Config::load().unwrap_or_else(|e| {
        tracing::error!("{e:#}; using defaults");
        Config::default()
    });

    // Must be set before the Slint backend initializes.
    if !cfg.display.mode.is_empty() && std::env::var_os("SLINT_DRM_MODE").is_none() {
        std::env::set_var("SLINT_DRM_MODE", &cfg.display.mode);
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

    ui.on_key(|text| app::with_app(|a| a.on_key_text(&text)).unwrap_or(false));

    if cfg.remote.cec {
        input::remote::spawn(|k| app::post(move |a| a.on_key(k)));
    }
    if let Some(path) = std::env::var_os("CABLEBOX_CONTROL") {
        input::control::spawn(path.into(), |k| app::post(move |a| a.on_key(k)));
    }

    app::install(App::new(&ui, rt.handle().clone(), player, cfg));

    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_millis(200), || {
        app::with_app(|a| a.tick());
    });
    // Start once the window exists so its size (and our scale) is known.
    Timer::single_shot(Duration::from_millis(50), || {
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
                    app::with_app(|a| a.apply_video_rect());
                });
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

/// Scales the 1280x720 design to the window (or a fixed factor from settings).
pub fn apply_scale(ui: &slint::Weak<AppWindow>, scale: Option<f32>) {
    let Some(ui) = ui.upgrade() else { return };
    let w = ui.window();
    let phys = w.size();
    if phys.height == 0 {
        return;
    }
    let s = scale.unwrap_or(phys.height as f32 / DESIGN_HEIGHT).max(0.25);
    if (w.scale_factor() - s).abs() > 0.001 {
        w.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: s });
        w.dispatch_event(WindowEvent::Resized { size: phys.to_logical(s) });
    }
}
