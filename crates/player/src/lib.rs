//! Thin wrapper around libmpv: playback control, an event thread, and an OpenGL
//! renderer that a UI toolkit can drive from its own GL context.

use std::ffi::{c_void, CString};
use std::thread;

use libmpv2::events::{Event, PropertyData};
use libmpv2::render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType};
use libmpv2::{Format, Mpv};

pub use libmpv2::Error as MpvError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mpv: {0}")]
    Mpv(#[from] MpvError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Things the UI cares about, delivered from mpv's event thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// A stream started decoding (first frame on the way).
    Playing,
    /// Playback ended; `error` is set when it failed (no signal, tuner busy, ...).
    Ended { error: Option<String> },
    Paused(bool),
    /// Playback position in seconds (recordings only; live streams report it too but it's meaningless).
    Position(f64),
    Duration(f64),
}

/// Margins around the video as fractions of the output size.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VideoRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

const PROP_PAUSE: u64 = 1;
const PROP_TIME_POS: u64 = 2;
const PROP_DURATION: u64 = 3;

/// One mpv instance for the lifetime of the app.
pub struct Player {
    // Leaked on purpose: the render context borrows it and both live until process exit.
    mpv: &'static Mpv,
}

impl Player {
    /// Creates the player. `on_event` is called on a background thread.
    pub fn new(on_event: impl Fn(PlayerEvent) + Send + 'static) -> Result<Self> {
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("vo", "libmpv")?;
            init.set_property("hwdec", "auto-safe")?;
            // Live TV: start fast, tolerate jitter, never show mpv's own UI.
            init.set_property("cache", "yes")?;
            init.set_property("demuxer-max-bytes", "64MiB")?;
            init.set_property("cache-secs", "10")?;
            init.set_property("osd-level", 0)?;
            init.set_property("keep-open", "no")?;
            init.set_property("input-default-bindings", false)?;
            init.set_property("ytdl", false)?;
            // Lockdown: no user config or scripts, and ffmpeg may only open network
            // protocols (a hostile stream can't redirect playback to local files).
            init.set_property("config", false)?;
            init.set_property("load-scripts", false)?;
            let protocols = "http,https,tcp,tls,httpproxy";
            // `%N%` quotes a value containing commas in mpv's key=value list syntax.
            init.set_property("stream-lavf-o", format!("protocol_whitelist=%{}%{protocols}", protocols.len()))?;
            Ok(())
        })?;
        let mpv: &'static Mpv = Box::leak(Box::new(mpv));

        // No name: libmpv2 6.0's named path passes mpv a pointer to an already-freed
        // CString (use-after-free). With `None` it passes NULL and mpv picks a name.
        let events = mpv.create_client(None)?;
        events.disable_deprecated_events()?;
        events.observe_property("pause", Format::Flag, PROP_PAUSE)?;
        events.observe_property("time-pos", Format::Double, PROP_TIME_POS)?;
        events.observe_property("duration", Format::Double, PROP_DURATION)?;
        thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || event_loop(events, on_event))
            .expect("spawn mpv event thread");

        Ok(Self { mpv })
    }

    /// Replaces whatever is playing with `url`.
    pub fn load(&self, url: &str) -> Result<()> {
        self.mpv.command("loadfile", &[url, "replace"])?;
        self.mpv.set_property("pause", false)?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        Ok(self.mpv.command("stop", &[])?)
    }

    pub fn toggle_pause(&self) -> Result<()> {
        Ok(self.mpv.command("cycle", &["pause"])?)
    }

    pub fn set_paused(&self, paused: bool) -> Result<()> {
        Ok(self.mpv.set_property("pause", paused)?)
    }

    /// Relative seek in seconds (negative rewinds).
    pub fn seek(&self, secs: f64) -> Result<()> {
        Ok(self.mpv.command("seek", &[&secs.to_string(), "relative"])?)
    }

    pub fn set_volume(&self, percent: i64) -> Result<()> {
        Ok(self.mpv.set_property("volume", percent.clamp(0, 130))?)
    }

    pub fn volume(&self) -> i64 {
        self.mpv.get_property("volume").unwrap_or(100)
    }

    pub fn toggle_mute(&self) -> Result<()> {
        Ok(self.mpv.command("cycle", &["mute"])?)
    }

    /// Confines video to a sub-rectangle of the output, as fractions of the full size
    /// (`None` = full screen). Used for the guide's preview window.
    pub fn set_video_rect(&self, rect: Option<VideoRect>) -> Result<()> {
        let r = rect.unwrap_or_default();
        self.mpv.set_property("video-margin-ratio-left", r.left)?;
        self.mpv.set_property("video-margin-ratio-top", r.top)?;
        self.mpv.set_property("video-margin-ratio-right", r.right)?;
        self.mpv.set_property("video-margin-ratio-bottom", r.bottom)?;
        Ok(())
    }

    /// Name of the active hardware decoder, or `None` for software decoding.
    pub fn hwdec_current(&self) -> Option<String> {
        self.mpv.get_property::<String>("hwdec-current").ok().filter(|s| s != "no" && !s.is_empty())
    }

    /// Creates a renderer bound to the *current* OpenGL context.
    ///
    /// # Safety
    /// Must be called with the target GL context current on this thread. `get_proc_address`
    /// is only used during this call (mpv resolves all GL functions up front).
    pub unsafe fn create_gl_renderer(
        &self,
        get_proc_address: &dyn Fn(&std::ffi::CStr) -> *const c_void,
    ) -> Result<GlRenderer> {
        // Erase the borrow's lifetime; see `resolve` for why this is sound.
        let lookup = ProcLookup(std::mem::transmute::<
            *const (dyn Fn(&std::ffi::CStr) -> *const c_void + '_),
            *const (dyn Fn(&std::ffi::CStr) -> *const c_void + 'static),
        >(get_proc_address));
        let ctx = self.mpv.create_render_context(vec![
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams { get_proc_address: resolve, ctx: lookup }),
        ])?;

        let name = c"glGetIntegerv";
        let get_integerv = get_proc_address(name);
        let get_integerv = (!get_integerv.is_null())
            .then(|| std::mem::transmute::<*const c_void, GlGetIntegerv>(get_integerv));

        Ok(GlRenderer { ctx, get_integerv })
    }
}

/// Raw pointer to the toolkit's lookup closure; only dereferenced inside `create_gl_renderer`.
struct ProcLookup(*const dyn Fn(&std::ffi::CStr) -> *const c_void);

fn resolve(lookup: &ProcLookup, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else { return std::ptr::null_mut() };
    // SAFETY: mpv only calls this synchronously from mpv_render_context_create, while the
    // closure borrowed in create_gl_renderer is still alive.
    unsafe { (*lookup.0)(&name) as *mut c_void }
}

type GlGetIntegerv = unsafe extern "system" fn(pname: u32, data: *mut i32);
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

/// Draws mpv's current frame into a GL framebuffer. Not `Send`: keep it on the render thread.
pub struct GlRenderer {
    ctx: RenderContext<'static>,
    get_integerv: Option<GlGetIntegerv>,
}

impl GlRenderer {
    /// Called from any thread whenever a new frame is ready and the UI should redraw.
    pub fn set_update_callback(&mut self, cb: impl Fn() + Send + 'static) {
        self.ctx.set_update_callback(cb);
    }

    /// Renders into the currently bound draw framebuffer at the given pixel size.
    pub fn render(&self, width: u32, height: u32) {
        let mut fbo = 0;
        if let Some(get) = self.get_integerv {
            // SAFETY: valid GL function from the current context; writes one GLint.
            unsafe { get(GL_DRAW_FRAMEBUFFER_BINDING, &mut fbo) };
        }
        if let Err(e) = self.ctx.render::<ProcLookup>(fbo, width as i32, height as i32, true) {
            tracing::warn!("mpv render failed: {e}");
        }
    }

    pub fn report_swap(&self) {
        self.ctx.report_swap();
    }
}

fn event_loop(events: Mpv, on_event: impl Fn(PlayerEvent)) {
    loop {
        let Some(ev) = events.wait_event(60.0) else { continue };
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                tracing::debug!("mpv event error: {e}");
                continue;
            }
        };
        match ev {
            Event::Shutdown => break,
            Event::PlaybackRestart => on_event(PlayerEvent::Playing),
            // Reasons: 0 EOF, 2 stop/replaced (every channel change), 3 quit, 4 error.
            Event::EndFile(0) => on_event(PlayerEvent::Ended { error: None }),
            Event::EndFile(4) => on_event(PlayerEvent::Ended { error: Some("No signal".into()) }),
            Event::PropertyChange { reply_userdata, change, .. } => match (reply_userdata, change) {
                (PROP_PAUSE, PropertyData::Flag(p)) => on_event(PlayerEvent::Paused(p)),
                (PROP_TIME_POS, PropertyData::Double(t)) => on_event(PlayerEvent::Position(t)),
                (PROP_DURATION, PropertyData::Double(d)) => on_event(PlayerEvent::Duration(d)),
                _ => {}
            },
            _ => {}
        }
    }
}
