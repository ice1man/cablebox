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

/// An audio output mpv can use ("alsa/hdmi:CARD=Generic,DEV=0", "pipewire/…").
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

/// The current audio, for the debug overlay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioInfo {
    /// "ac3", "aac".
    pub codec: Option<String>,
    /// The broadcast's channel layout: "5.1(side)", "stereo".
    pub channels: Option<String>,
    /// Channels sent to the output after downmixing: "stereo", "5.1".
    pub out_channels: Option<String>,
    /// Audio output in use: "pipewire", "alsa".
    pub output: Option<String>,
    /// Sent as a bitstream to the TV or receiver (passthrough).
    pub passthrough: bool,
}

/// What's playing and how it's arriving, for the debug overlay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoInfo {
    pub width: i64,
    pub height: i64,
    /// The stream's frame rate.
    pub fps: Option<f64>,
    /// Frame rate after filters (deinterlacing can double it).
    pub vf_fps: Option<f64>,
    /// Current download rate in bytes per second.
    pub cache_speed: Option<i64>,
    /// Seconds of stream buffered ahead.
    pub buffered: Option<f64>,
    /// Codec name, e.g. "h264" or "mpeg2video".
    pub codec: Option<String>,
    /// Hardware decoder in use ("vaapi"), `None` when decoding in software.
    pub hwdec: Option<String>,
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

/// Scaler options "High" picture quality changes (mpv's `profile=high-quality` subset).
const HIGH_QUALITY: &[(&str, &str)] = &[
    ("scale", "ewa_lanczossharp"),
    ("cscale", "ewa_lanczossharp"),
    ("dscale", "mitchell"),
    ("deband", "yes"),
    ("sigmoid-upscaling", "yes"),
    ("correct-downscaling", "yes"),
    ("linear-downscaling", "yes"),
];

/// One mpv instance for the lifetime of the app.
pub struct Player {
    // Leaked on purpose: the render context borrows it and both live until process exit.
    mpv: &'static Mpv,
    /// mpv's own values for the `HIGH_QUALITY` options, restored for "Standard".
    standard: Vec<(&'static str, String)>,
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
        // Broadcast TV is often interlaced (1080i). "auto" deinterlaces only interlaced
        // frames; older mpv only knows yes/no, so this is best-effort.
        if let Err(e) = mpv.set_property("deinterlace", "auto") {
            tracing::warn!("mpv: deinterlace=auto not supported ({e}); interlaced video may show combing");
        }

        // No name: libmpv2 6.0's named path passes mpv a pointer to an already-freed
        // CString (use-after-free). With `None` it passes NULL and mpv picks a name.
        let events = mpv.create_client(None)?;
        events.disable_deprecated_events()?;
        // mpv's own warnings and errors go to our log (the journal on a box).
        // SAFETY: a valid handle owned by `events`; mpv copies the level string.
        unsafe { libmpv2_sys::mpv_request_log_messages(events.ctx.as_ptr(), c"warn".as_ptr()) };
        events.observe_property("pause", Format::Flag, PROP_PAUSE)?;
        events.observe_property("time-pos", Format::Double, PROP_TIME_POS)?;
        events.observe_property("duration", Format::Double, PROP_DURATION)?;
        thread::Builder::new()
            .name("mpv-events".into())
            .spawn(move || event_loop(events, on_event))
            .expect("spawn mpv event thread");

        let standard = HIGH_QUALITY
            .iter()
            .filter_map(|(name, _)| Some((*name, mpv.get_property::<String>(name).ok()?)))
            .collect();
        Ok(Self { mpv, standard })
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

    /// Clamped to 0..`volume-max` (100, or 130 with Volume boost).
    pub fn set_volume(&self, percent: i64) -> Result<()> {
        let max = self.mpv.get_property::<f64>("volume-max").unwrap_or(130.0) as i64;
        Ok(self.mpv.set_property("volume", percent.clamp(0, max))?)
    }

    /// Audio outputs mpv knows about (unfiltered).
    pub fn audio_devices(&self) -> Vec<AudioDevice> {
        let json = self.mpv.get_property::<String>("audio-device-list").unwrap_or_default();
        serde_json::from_str(&json).unwrap_or_default()
    }

    /// Applies audio options (device, channels, passthrough, filters) and reopens the audio
    /// output so they take effect on what's playing.
    pub fn set_audio(&self, opts: &[(&str, String)]) {
        for (name, value) in opts {
            if let Err(e) = self.mpv.set_property(name, value.as_str()) {
                tracing::warn!("mpv: {name}={value} not supported ({e})");
            }
        }
        if let Err(e) = self.mpv.command("ao-reload", &[]) {
            tracing::debug!("mpv: ao-reload: {e}");
        }
        tracing::debug!("mpv audio: af={:?}", self.mpv.get_property::<String>("af").unwrap_or_default());
    }

    /// Playback position and seconds buffered ahead (for the stalled-output watchdog), and
    /// whether mpv is paused for any reason (user, or waiting for the network).
    pub fn progress(&self) -> Option<(f64, f64, bool)> {
        let m = self.mpv;
        let pos = m.get_property::<f64>("time-pos").ok()?;
        let buffered = m.get_property::<f64>("demuxer-cache-duration").unwrap_or(0.0);
        let paused = m.get_property::<bool>("pause").unwrap_or(false) || m.get_property::<bool>("paused-for-cache").unwrap_or(false);
        Some((pos, buffered, paused))
    }

    /// `None` when nothing with audio is playing.
    pub fn audio_info(&self) -> Option<AudioInfo> {
        let m = self.mpv;
        let get = |p: &str| m.get_property::<String>(p).ok().filter(|s| !s.is_empty());
        let codec = get("audio-codec-name")?;
        Some(AudioInfo {
            codec: Some(codec),
            channels: get("current-tracks/audio/demux-channels"),
            out_channels: get("audio-out-params/hr-channels"),
            output: get("current-ao"),
            passthrough: get("audio-out-params/format").is_some_and(|f| f.starts_with("spdif")),
        })
    }

    pub fn volume(&self) -> i64 {
        self.mpv.get_property("volume").unwrap_or(100)
    }

    pub fn muted(&self) -> bool {
        self.mpv.get_property::<bool>("mute").unwrap_or(false)
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

    /// Smooth motion: mpv times frames to the display's refresh (`display_fps`) and blends
    /// neighbouring frames, so e.g. 24 fps films don't judder on a 60 Hz screen. The renderer
    /// must then draw every refresh and call `report_swap`. Best-effort, like deinterlacing.
    pub fn set_smooth_motion(&self, on: bool, display_fps: f64) {
        let props: &[(&str, String)] = if on {
            &[
                ("display-fps-override", format!("{display_fps:.3}")),
                ("video-sync", "display-resample".into()),
                ("interpolation", "yes".into()),
                ("tscale", "oversample".into()),
            ]
        } else {
            &[("interpolation", "no".into()), ("video-sync", "audio".into())]
        };
        for (name, value) in props {
            if let Err(e) = self.mpv.set_property(name, value.as_str()) {
                tracing::warn!("mpv: {name}={value} not supported ({e})");
            }
        }
    }

    /// Hardware video decoding: mpv's safe automatic choice, or always software. Applies to
    /// the current video right away (mpv re-creates the decoder).
    pub fn set_hwdec(&self, on: bool) {
        if let Err(e) = self.mpv.set_property("hwdec", if on { "auto-safe" } else { "no" }) {
            tracing::warn!("mpv: setting hwdec failed ({e})");
        }
    }

    /// Codec of the current video, e.g. "h264".
    pub fn video_codec(&self) -> Option<String> {
        self.mpv.get_property::<String>("video-format").ok().filter(|c| !c.is_empty())
    }

    /// "High" picture quality (sharper scaling, debanding; needs a strong GPU) or mpv's defaults.
    pub fn set_high_quality(&self, on: bool) {
        let values: Vec<(&str, &str)> = if on {
            HIGH_QUALITY.to_vec()
        } else {
            self.standard.iter().map(|(n, v)| (*n, v.as_str())).collect()
        };
        for (name, value) in values {
            if let Err(e) = self.mpv.set_property(name, value) {
                tracing::warn!("mpv: {name}={value} not supported ({e})");
            }
        }
    }

    /// The display's refresh rate as mpv measures it from `report_swap` timing (only while
    /// video plays and the renderer draws every refresh).
    pub fn estimated_display_fps(&self) -> Option<f64> {
        self.mpv.get_property::<f64>("estimated-display-fps").ok().filter(|f| *f > 0.0)
    }

    /// `None` when no video is playing.
    pub fn video_info(&self) -> Option<VideoInfo> {
        let m = self.mpv;
        let width = m.get_property::<i64>("video-params/w").ok().filter(|w| *w > 0)?;
        Some(VideoInfo {
            width,
            height: m.get_property::<i64>("video-params/h").unwrap_or(0),
            fps: m.get_property::<f64>("container-fps").ok().filter(|f| *f > 0.0),
            vf_fps: m.get_property::<f64>("estimated-vf-fps").ok().filter(|f| *f > 0.0),
            cache_speed: m.get_property::<i64>("cache-speed").ok(),
            buffered: m.get_property::<f64>("demuxer-cache-duration").ok(),
            codec: m.get_property::<String>("video-format").ok().filter(|c| !c.is_empty()),
            hwdec: self.hwdec_current(),
        })
    }

    /// Frame timing summary for logs: sync mode, measured display rate, late and dropped frames.
    pub fn sync_stats(&self) -> String {
        let get = |name: &str| self.mpv.get_property::<String>(name).unwrap_or_else(|_| "?".into());
        format!(
            "video-sync {}, interpolation {}, display {} Hz (estimated {}), mistimed {}, dropped {}",
            get("video-sync"),
            get("interpolation"),
            get("display-fps"),
            get("estimated-display-fps"),
            get("mistimed-frame-count"),
            get("frame-drop-count"),
        )
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
            Event::LogMessage { prefix, level, text, .. } => {
                let text = text.trim_end();
                match level {
                    // ffmpeg complains about every tune-in until the first full frame.
                    _ if prefix.starts_with("ffmpeg") => tracing::debug!("mpv {prefix}: {text}"),
                    "fatal" | "error" => tracing::error!("mpv {prefix}: {text}"),
                    _ => tracing::warn!("mpv {prefix}: {text}"),
                }
            }
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
