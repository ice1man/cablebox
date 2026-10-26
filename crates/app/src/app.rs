//! The controller: owns all state, handles remote keys, and pushes view state to Slint.
//!
//! Everything here runs on the UI thread. Async work runs on a Tokio runtime and posts
//! its result back with [`post`], which re-enters through [`with_app`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use input::RemoteKey;
use player::{Player, PlayerEvent};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use tvh::{Channel, ChannelTag, DvrEntry, EpgEvent};

use crate::config::Config;
use crate::entry::{ChannelEntry, Entry};
use crate::guide::{EpgCache, GuideNav, BLOCK};
use crate::lineup::{Filter, Lineup};
use crate::views::dialog::Dialog;
use crate::views::dvr::DvrState;
use crate::views::menu::{MenuState, Osk};
use crate::views::system::UpdateState;
use crate::{AppWindow, BannerData, Screen};

const BANNER_TIME: Duration = Duration::from_secs(5);
/// The audio watchdog judges playback speed over windows of this length.
const STALL_WINDOW: Duration = Duration::from_secs(3);
const TOAST_TIME: Duration = Duration::from_secs(3);
const DVR_REFRESH: Duration = Duration::from_secs(60);
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const RETUNE_DELAY: Duration = Duration::from_secs(5);
/// How long "Tuning…" may show before we say "No Signal" (the stream stays open and
/// recovers by itself if the signal comes back).
const TUNE_TIMEOUT: Duration = Duration::from_secs(15);
const CONFIG_SAVE_DELAY: Duration = Duration::from_secs(3);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

/// Runs `f` against the controller. Returns `None` if it isn't installed or is already
/// borrowed (a re-entrant call, which is dropped rather than panicking).
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => {
            tracing::warn!("re-entrant controller access dropped");
            None
        }
    })
}

/// Schedules `f` on the UI thread from any thread.
pub fn post(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Time of day in the user's chosen 12/24-hour format.
pub fn fmt_time(t: i64) -> String {
    crate::clock::format_time(t, crate::clock::hour24())
}

pub fn fmt_range(start: i64, stop: i64) -> String {
    format!("{} – {}", fmt_time(start), fmt_time(stop))
}

pub fn fmt_day(t: i64) -> String {
    Local.timestamp_opt(t, 0).single().map(|d| d.format("%a %b %-d").to_string()).unwrap_or_default()
}

/// A recording being played back instead of live TV.
pub struct Playback {
    pub entry: DvrEntry,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
}

pub struct App {
    pub ui: slint::Weak<AppWindow>,
    pub rt: tokio::runtime::Handle,
    pub player: Arc<Player>,
    pub cfg: Config,
    config_dirty: Option<Instant>,

    pub tvh: Option<tvh::Client>,
    /// Bumped on reconnect so stale async results are dropped.
    pub gen: u64,
    reconnect_at: Option<Instant>,

    pub lineup: Lineup,
    pub epg: EpgCache,
    epg_inflight: HashSet<i64>,
    pub dvr_upcoming: Vec<DvrEntry>,
    pub dvr_finished: Vec<DvrEntry>,
    pub autorecs: Vec<tvh::AutorecRule>,
    dvr_loaded_at: Option<Instant>,
    pub logos: HashMap<String, Image>,

    pub screen: Screen,
    pub tuned: Option<String>,
    previous: Option<String>,
    pub playback: Option<Playback>,
    pub video_status: String,
    retune_at: Option<Instant>,
    tune_deadline: Option<Instant>,

    pub entry: ChannelEntry,
    banner_until: Option<Instant>,
    toast: String,
    toast_until: Option<Instant>,
    clock: String,
    last_prune: i64,

    pub guide: GuideNav,
    pub dvr: DvrState,
    pub menu: MenuState,
    pub osk: Option<Osk>,
    pub dialog: Option<Dialog>,
    pub updates: UpdateState,
    /// The settings PIN was entered; cleared when leaving Settings.
    pub settings_unlocked: bool,
    pub pin_gate: crate::pin::PinGate,
    /// Running as the `cablebox.service` session on a box (not a dev window).
    pub under_service: bool,
    /// mpv is showing video (between its first frame and stop/end).
    video_active: bool,
    /// Next debug log of mpv's frame timing while Smooth motion is on.
    next_sync_log: Option<Instant>,
    /// When to next read mpv's measured refresh rate, if the screen didn't report one.
    measure_refresh_at: Option<Instant>,
    refresh_probe: crate::display::RefreshProbe,
    /// Debug overlay: when it last updated, the frame counter then, and the network status.
    debug_at: Option<(Instant, u64)>,
    debug_net: Option<sysctl::network::Status>,
    debug_net_at: Option<Instant>,
    /// Codec of the last video and its hardware decoder (`None`: decoded in software).
    pub last_decoder: Option<(String, Option<String>)>,
    /// The audio output actually in use ("auto" when the saved one is missing or failed).
    pub audio_in_use: String,
    /// The saved output stalled playback this session; Auto is used until Sound changes.
    pub audio_failed: bool,
    /// Watchdog: position at the start of the current window, when it started, and how many
    /// windows in a row played too slowly.
    stall_check: Option<(f64, Instant, u32)>,
}

impl App {
    pub fn new(ui: &AppWindow, rt: tokio::runtime::Handle, player: Arc<Player>, cfg: Config) -> Self {
        let first_check = Instant::now() + Duration::from_secs(60);
        Self {
            ui: ui.as_weak(),
            rt,
            player,
            cfg,
            config_dirty: None,
            tvh: None,
            gen: 0,
            reconnect_at: None,
            lineup: Lineup::default(),
            epg: EpgCache::default(),
            epg_inflight: HashSet::new(),
            dvr_upcoming: Vec::new(),
            dvr_finished: Vec::new(),
            autorecs: Vec::new(),
            dvr_loaded_at: None,
            logos: HashMap::new(),
            screen: Screen::Live,
            tuned: None,
            previous: None,
            playback: None,
            video_status: String::new(),
            retune_at: None,
            tune_deadline: None,
            entry: ChannelEntry::default(),
            banner_until: None,
            toast: String::new(),
            toast_until: None,
            clock: String::new(),
            last_prune: 0,
            guide: GuideNav::default(),
            dvr: DvrState::default(),
            menu: MenuState::default(),
            osk: None,
            dialog: None,
            updates: UpdateState::new(first_check),
            settings_unlocked: false,
            pin_gate: Default::default(),
            under_service: false,
            video_active: false,
            next_sync_log: None,
            measure_refresh_at: None,
            refresh_probe: Default::default(),
            debug_at: None,
            debug_net: None,
            debug_net_at: None,
            last_decoder: None,
            audio_in_use: "auto".into(),
            audio_failed: false,
            stall_check: None,
        }
    }

    /// First call after install: connect, or run first-time setup.
    pub fn start(&mut self) {
        crate::clock::set_hour24(self.cfg.clock.hour24);
        if self.cfg.ui.pin.as_deref().is_some_and(|p| !crate::pin::is_valid(p)) {
            tracing::warn!("ignoring invalid settings PIN in config (must be 4 digits)");
            self.cfg.ui.pin = None;
        }
        self.apply_sound();
        if let Some(v) = self.cfg.ui.volume {
            let _ = self.player.set_volume(v);
        }
        self.apply_video_settings();
        // Development only: show the box-only Settings rows in a window to work on their UI.
        // Using them would change this machine (polkit asks for your password first).
        let pretend = std::env::var_os("CABLEBOX_PRETEND_BOX").is_some();
        self.spawn(sysctl::services::is_main_process_of("cablebox.service"), move |a, yes| {
            if pretend && !yes {
                tracing::warn!("CABLEBOX_PRETEND_BOX: box-only settings are enabled in this window");
                a.under_service = true;
                a.render();
                return;
            }
            a.under_service = yes;
            tracing::info!("running as cablebox.service: {yes}");
            if yes {
                a.apply_ssh_preference();
            }
            a.render();
        });
        if self.cfg.is_configured() {
            self.connect();
        } else {
            self.open_setup();
        }
        self.render();
    }

    /// Runs `fut` on the runtime and hands the result to `done` on the UI thread.
    pub fn spawn<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
        done: impl FnOnce(&mut App, T) + Send + 'static,
    ) {
        self.rt.spawn(async move {
            let v = fut.await;
            post(move |a| done(a, v));
        });
    }

    /// Like [`spawn`](Self::spawn) but drops the result if we reconnected meanwhile.
    pub fn spawn_tvh<T: Send + 'static>(
        &self,
        f: impl FnOnce(tvh::Client) -> std::pin::Pin<Box<dyn Future<Output = T> + Send>>,
        done: impl FnOnce(&mut App, T) + Send + 'static,
    ) {
        let Some(c) = self.tvh.clone() else { return };
        let gen = self.gen;
        self.spawn(f(c), move |a, v| {
            if a.gen == gen {
                done(a, v);
            }
        });
    }

    pub fn save_config_soon(&mut self) {
        self.config_dirty = Some(Instant::now() + CONFIG_SAVE_DELAY);
    }

    pub fn save_config_now(&mut self) {
        self.config_dirty = None;
        self.cfg.ui.favorites = self.lineup.favorites();
        if let Err(e) = self.cfg.save() {
            tracing::error!("saving config: {e:#}");
            self.toast("Could not save settings");
        }
    }

    // ---------------------------------------------------------------- connection

    pub fn connect(&mut self) {
        self.gen += 1;
        self.reconnect_at = None;
        self.epg = EpgCache::default();
        self.epg_inflight.clear();
        self.tvh = match tvh::Client::new(&self.cfg.server.url, self.cfg.server.tvh_auth()) {
            Ok(c) => Some(c),
            Err(e) => {
                self.video_status = format!("Invalid server address: {e}");
                return;
            }
        };
        if self.tuned.is_none() {
            self.video_status = "Connecting…".into();
        }
        self.spawn_tvh(
            |c| {
                Box::pin(async move {
                    let channels = c.channels().await?;
                    let tags = c.tags().await.unwrap_or_default();
                    Ok::<_, tvh::Error>((channels, tags))
                })
            },
            |a, r| a.on_lineup(r),
        );
    }

    fn on_lineup(&mut self, r: Result<(Vec<Channel>, Vec<ChannelTag>), tvh::Error>) {
        let (channels, tags) = match r {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("loading channels: {e}");
                self.video_status = match e {
                    tvh::Error::Unauthorized => "Tvheadend login failed — check Settings".into(),
                    _ => format!("Cannot reach Tvheadend ({e})"),
                };
                self.reconnect_at = Some(Instant::now() + RECONNECT_DELAY);
                self.render();
                return;
            }
        };
        tracing::info!("{} channels, {} tags", channels.len(), tags.len());
        self.lineup = Lineup::new(channels, tags, self.cfg.ui.favorites.clone());
        self.lineup.set_filter(Filter::parse(&self.cfg.ui.filter));
        let (major, minor) = self.lineup.number_widths();
        self.entry.set_limits(major, minor);

        let t = now();
        self.load_epg(t, t + BLOCK);
        self.refresh_dvr();
        self.load_logos();

        if self.lineup.is_empty() {
            self.video_status = "No channels found on the server".into();
        } else if self.playback.is_none() {
            let start = self
                .tuned
                .clone()
                .or_else(|| self.cfg.ui.last_channel.clone())
                .filter(|u| self.lineup.get(u).is_some())
                .or_else(|| self.lineup.step(None, 1).map(|c| c.uuid.clone()));
            if let Some(uuid) = start {
                self.tuned = None;
                self.tune(&uuid);
            }
        }
        self.render();
    }

    pub fn load_epg(&mut self, from: i64, to: i64) {
        for block in self.epg.missing_blocks(from, to) {
            if !self.epg_inflight.insert(block) {
                continue;
            }
            self.spawn_tvh(
                move |c| Box::pin(async move { c.epg(None, block, block + BLOCK).await }),
                move |a, r| {
                    a.epg_inflight.remove(&block);
                    match r {
                        Ok(events) => {
                            a.epg.insert_block(block, events);
                            a.render();
                        }
                        Err(e) => tracing::warn!("loading EPG: {e}"),
                    }
                },
            );
        }
    }

    pub fn refresh_dvr(&mut self) {
        self.dvr_loaded_at = Some(Instant::now());
        self.spawn_tvh(
            |c| {
                Box::pin(async move {
                    let up = c.dvr_upcoming().await?;
                    let fin = c.dvr_finished().await?;
                    let auto = c.autorecs().await.unwrap_or_default();
                    Ok::<_, tvh::Error>((up, fin, auto))
                })
            },
            |a, r| match r {
                Ok((up, fin, auto)) => {
                    a.dvr_upcoming = up;
                    a.dvr_finished = fin;
                    a.autorecs = auto;
                    a.dvr.clamp(a.dvr_items_len());
                    a.render();
                }
                Err(e) => tracing::warn!("loading recordings: {e}"),
            },
        );
    }

    /// Fetches channel logos one at a time in the background.
    fn load_logos(&mut self) {
        let Some(c) = self.tvh.clone() else { return };
        let wanted: Vec<(String, String)> = self
            .lineup
            .channels()
            .iter()
            .filter(|ch| !self.logos.contains_key(&ch.uuid))
            .filter_map(|ch| ch.icon_public_url.clone().map(|u| (ch.uuid.clone(), u)))
            .collect();
        let gen = self.gen;
        self.rt.spawn(async move {
            for (uuid, icon) in wanted {
                let Some(url) = c.icon_url(&icon) else { continue };
                let Ok(bytes) = c.fetch(url).await else { continue };
                let Some(buf) = decode_logo(&bytes) else { continue };
                post(move |a| {
                    if a.gen == gen {
                        a.logos.insert(uuid, Image::from_rgba8(buf));
                        a.render();
                    }
                });
            }
        });
    }

    // ---------------------------------------------------------------- tuning

    pub fn tune(&mut self, uuid: &str) {
        let Some(tvh) = &self.tvh else { return };
        if self.lineup.get(uuid).is_none() {
            return;
        }
        let url = tvh.stream_url(uuid, &self.cfg.server.profile);
        if self.tuned.as_deref() != Some(uuid) {
            self.previous = self.tuned.replace(uuid.to_string());
        }
        self.playback = None;
        self.retune_at = None;
        self.video_status = "Tuning…".into();
        self.tune_deadline = Some(Instant::now() + TUNE_TIMEOUT);
        if let Err(e) = self.player.load(url.as_str()) {
            self.video_status = format!("Playback error: {e}");
        }
        self.cfg.ui.last_channel = Some(uuid.to_string());
        self.save_config_soon();
        self.show_banner();
    }

    fn step_channel(&mut self, delta: i64) {
        let next = self.lineup.step(self.tuned.as_deref(), delta).map(|c| c.uuid.clone());
        if let Some(uuid) = next {
            self.tune(&uuid);
        }
    }

    fn last_channel(&mut self) {
        if let Some(prev) = self.previous.clone() {
            self.tune(&prev);
        }
    }

    pub fn play_recording(&mut self, entry: DvrEntry) {
        let Some(tvh) = &self.tvh else { return };
        let url = tvh.recording_url(&entry.uuid);
        self.video_status = "Loading…".into();
        if let Err(e) = self.player.load(url.as_str()) {
            self.video_status = format!("Playback error: {e}");
        }
        self.playback = Some(Playback { entry, position: 0.0, duration: 0.0, paused: false });
        self.banner_until = None;
        self.screen = Screen::Live;
        self.apply_video_rect();
    }

    fn stop_playback(&mut self) {
        self.playback = None;
        match self.tuned.clone() {
            Some(uuid) => {
                self.tuned = None;
                self.tune(&uuid);
            }
            None => {
                let _ = self.player.stop();
                self.set_video_active(false);
            }
        }
    }

    /// Smooth motion and picture quality from Settings → Display.
    pub fn apply_video_settings(&mut self) {
        let v = self.video_mode();
        crate::display::set_frame_divisor(self.cfg.display.refresh_divisor);
        let hz = crate::display::refresh_hz().unwrap_or(60.0);
        self.player.set_hwdec(v.hwdec);
        self.player.set_smooth_motion(v.smooth_motion, hz);
        self.apply_frame_cap();
        self.player.set_high_quality(v.quality == crate::config::Quality::High);
        tracing::info!(
            "video: hardware acceleration {}, smooth motion {}, refresh target {:.2} Hz, quality {:?}",
            match (self.cfg.display.hw_accel, crate::display::gpu(), crate::display::hw_available()) {
                (false, ..) => "off",
                (true, None, _) => "waiting for the GPU",
                (true, _, true) => "on",
                (true, _, false) => "not available",
            },
            if v.smooth_motion { "on" } else { "off" },
            crate::display::target_hz(),
            v.quality
        );
        self.set_video_active(self.video_active);
    }

    /// Settings → Sound. A saved output that isn't connected falls back to Auto for this run
    /// (the choice stays saved for when it's back).
    pub fn apply_sound(&mut self) {
        let s = &self.cfg.sound;
        let device = if s.device.is_empty() || self.audio_failed {
            "auto"
        } else if self.player.audio_devices().iter().any(|d| d.name == s.device) {
            s.device.as_str()
        } else {
            tracing::warn!("audio output {} is not connected; using Auto", s.device);
            "auto"
        };
        self.player.set_audio(&crate::sound::mpv_options(s, device));
        self.audio_in_use = device.to_string();
        self.stall_check = None;
        tracing::info!("sound: {device}, {:?}, night mode {}, boost {}", s.format, s.night_mode, s.volume_boost);
        // Turning boost off brings a louder setting back to 100%.
        if self.player.volume() > 100 && !s.volume_boost {
            let _ = self.player.set_volume(100);
            self.cfg.ui.volume = Some(100);
            self.save_config_soon();
        }
    }

    /// An audio output that accepts sound but never plays it (an HDMI port with nothing
    /// attached, a device that takes a bitstream it can't use) stops mpv's clock, and with it
    /// the picture. If playback stops moving while the stream keeps arriving, fall back to Auto
    /// for this session and re-tune to get back to live.
    fn watch_audio_stall(&mut self, t: Instant) {
        // Auto is the system's own choice; only a chosen output can be the culprit.
        if !self.video_active || self.audio_in_use == "auto" || self.audio_failed {
            self.stall_check = None;
            return;
        }
        let Some((pos, buffered, paused)) = self.player.progress() else { return };
        let Some((last_pos, since, slow)) = self.stall_check else {
            self.stall_check = Some((pos, t, 0));
            return;
        };
        let elapsed = t.duration_since(since);
        if elapsed < STALL_WINDOW {
            return;
        }
        // Live TV plays in real time: less than half speed with data waiting means the
        // output isn't consuming audio.
        let rate = (pos - last_pos) / elapsed.as_secs_f64();
        let slow = if !paused && rate < 0.5 && buffered >= 2.0 { slow + 1 } else { 0 };
        tracing::debug!("audio watchdog: {:.2}× real time, {buffered:.1} s buffered, slow windows {slow}", rate);
        self.stall_check = Some((pos, t, slow));
        if slow < 2 {
            return;
        }
        let name = self.audio_in_use.clone();
        tracing::warn!("audio output {name} stalled playback ({rate:.2}× real time, {buffered:.1} s buffered); using Auto");
        self.audio_failed = true;
        self.apply_sound();
        self.toast("No sound from that output; using Auto. Check the connection or pick another in Settings → Sound");
        if self.playback.is_none() {
            if let Some(uuid) = self.tuned.clone() {
                self.tune(&uuid);
            }
        }
    }

    /// Refresh target without Smooth motion: thin the video to the target when it's slower
    /// than the video. (With Smooth motion mpv already draws at the target.) Re-checked when
    /// a video starts, since that's when its frame rate is known.
    fn apply_frame_cap(&mut self) {
        let cap = if self.video_mode().smooth_motion { None } else { crate::display::frame_cap(self.player.video_fps()) };
        self.player.set_frame_cap(cap);
        if let Some(fps) = cap {
            tracing::debug!("refresh target: video thinned to {fps:.2} fps");
        }
    }

    /// What the player does with the current settings and GPU.
    pub fn video_mode(&self) -> crate::config::VideoMode {
        self.cfg.display.effective(crate::display::hw_available())
    }

    /// Re-reads which decoder the current video uses (after a switch, mpv takes a moment).
    pub fn refresh_decoder(&mut self) {
        if self.video_active {
            if let Some(codec) = self.player.video_codec() {
                self.last_decoder = Some((codec, self.player.hwdec_current()));
            }
        }
    }

    /// Some screens don't report their refresh rate (Wayland windows); then mpv measures it
    /// while video is drawn every refresh. Its estimate is noisy, so a rate is only taken
    /// once it holds steady, and checked again for as long as video plays.
    fn measure_refresh(&mut self, t: Instant) {
        if crate::display::refresh_reported() {
            return;
        }
        let due = *self.measure_refresh_at.get_or_insert(t + Duration::from_secs(2));
        if t < due {
            return;
        }
        self.measure_refresh_at = Some(t + Duration::from_secs(2));
        let Some(hz) = self.refresh_probe.feed(self.player.estimated_display_fps()) else { return };
        if crate::display::set_measured_refresh_hz(hz) {
            tracing::info!("measured refresh: {hz:.2} Hz");
            self.apply_video_settings();
        }
    }

    /// Refreshes the debug overlay once a second while it's on (network every 5 s).
    pub fn update_debug(&mut self, t: Instant) {
        let Some(ui) = self.ui.upgrade() else { return };
        if !self.cfg.ui.debug_overlay {
            if self.debug_at.take().is_some() {
                ui.set_debug_text(Default::default());
                self.debug_net = None;
                self.debug_net_at = None;
            }
            return;
        }
        if self.debug_net_at.is_none_or(|at| t >= at) {
            self.debug_net_at = Some(t + Duration::from_secs(5));
            self.spawn(sysctl::network::status(), |a, r| a.debug_net = r.ok().or(Some(Default::default())));
        }
        let frames = crate::FRAMES_DRAWN.load(std::sync::atomic::Ordering::Relaxed);
        let drawn_fps = match self.debug_at {
            Some((at, _)) if t < at + Duration::from_secs(1) => return,
            Some((at, n)) => (frames - n) as f64 / (t - at).as_secs_f64(),
            None => 0.0,
        };
        self.debug_at = Some((t, frames));
        let size = ui.window().size();
        let text = crate::debug::text(&crate::debug::Snapshot {
            video: self.player.video_info(),
            audio: self.player.audio_info(),
            out: (size.width, size.height),
            refresh_hz: crate::display::refresh_hz(),
            divisor: crate::display::frame_divisor(),
            drawn_fps,
            net: self.debug_net.as_ref(),
            server: &self.cfg.server.url,
        });
        ui.set_debug_text(text.into());
    }

    fn set_video_active(&mut self, on: bool) {
        self.video_active = on;
        let every = on && self.video_mode().smooth_motion;
        if crate::EVERY_FRAME.swap(every, std::sync::atomic::Ordering::Relaxed) != every && every {
            if let Some(ui) = self.ui.upgrade() {
                ui.window().request_redraw();
            }
        }
    }

    pub fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Playing => {
                self.set_video_active(true);
                self.video_status.clear();
                self.tune_deadline = None;
                self.refresh_decoder();
                self.apply_frame_cap();
                if let Some((codec, hw)) = &self.last_decoder {
                    tracing::debug!("decoding {codec}: {}", hw.as_deref().unwrap_or("software"));
                }
            }
            PlayerEvent::Ended { error } => {
                self.set_video_active(false);
                if self.playback.is_some() {
                    self.toast("End of recording");
                    self.stop_playback();
                } else if self.tuned.is_some() {
                    self.video_status = error.unwrap_or_else(|| "No Signal".into());
                    self.retune_at = Some(Instant::now() + RETUNE_DELAY);
                }
            }
            PlayerEvent::Paused(p) => {
                if let Some(pb) = &mut self.playback {
                    pb.paused = p;
                }
            }
            PlayerEvent::Position(t) => {
                if let Some(pb) = &mut self.playback {
                    pb.position = t;
                }
            }
            PlayerEvent::Duration(d) => {
                if let Some(pb) = &mut self.playback {
                    pb.duration = d;
                }
            }
        }
        self.render();
    }

    // ---------------------------------------------------------------- overlays

    pub fn show_banner(&mut self) {
        self.banner_until = Some(Instant::now() + BANNER_TIME);
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = msg.into();
        self.toast_until = Some(Instant::now() + TOAST_TIME);
    }

    // ---------------------------------------------------------------- timers

    /// Called a few times per second.
    pub fn tick(&mut self) {
        let t = Instant::now();
        let mut dirty = false;

        self.update_debug(t);
        self.watch_audio_stall(t);

        if self.video_active && self.video_mode().smooth_motion {
            let due = *self.next_sync_log.get_or_insert(t + Duration::from_secs(10));
            if t >= due {
                tracing::debug!("smooth motion: {}", self.player.sync_stats());
                self.next_sync_log = Some(t + Duration::from_secs(60));
            }
            self.measure_refresh(t);
        } else {
            self.next_sync_log = None;
            self.measure_refresh_at = None;
            self.refresh_probe = Default::default();
        }

        if let Some(n) = self.entry.tick(t) {
            self.commit_number(n);
            dirty = true;
        }
        if self.banner_until.is_some_and(|u| t >= u) {
            self.banner_until = None;
            dirty = true;
        }
        if self.toast_until.is_some_and(|u| t >= u) {
            self.toast_until = None;
            self.toast.clear();
            dirty = true;
        }
        if self.reconnect_at.is_some_and(|u| t >= u) {
            self.connect();
            dirty = true;
        }
        if self.retune_at.is_some_and(|u| t >= u) {
            self.retune_at = None;
            if let Some(uuid) = self.tuned.clone() {
                self.tuned = None;
                self.tune(&uuid);
                self.banner_until = None;
                dirty = true;
            }
        }
        if self.tune_deadline.is_some_and(|u| t >= u) {
            self.tune_deadline = None;
            if self.playback.is_none() && self.video_status == "Tuning…" {
                tracing::info!("no picture after {TUNE_TIMEOUT:?}; showing No Signal");
                self.video_status = "No Signal".into();
                dirty = true;
            }
        }
        if self.config_dirty.is_some_and(|u| t >= u) {
            self.save_config_now();
        }
        if self.tvh.is_some() && self.dvr_loaded_at.is_some_and(|l| t.duration_since(l) >= DVR_REFRESH) {
            self.refresh_dvr();
        }

        // Keep now/next data around and memory bounded.
        let wall = now();
        if wall - self.last_prune >= 600 && !self.lineup.is_empty() {
            self.last_prune = wall;
            self.epg.prune(wall - 3600);
            self.load_epg(wall, wall + BLOCK / 2);
        }

        self.tick_updates(t);

        let clock = fmt_time(wall);
        if clock != self.clock {
            self.clock = clock;
            dirty = true;
        }
        // Banner progress and the playback bar move continuously.
        if dirty || self.banner_until.is_some() || self.playback.is_some() || self.screen == Screen::Guide {
            self.render();
        }
    }

    // ---------------------------------------------------------------- input

    /// Raw key text from Slint. Returns whether it was handled.
    pub fn on_key_text(&mut self, text: &str) -> bool {
        if self.osk.is_some() && self.dialog.is_none() && self.osk_text(text) {
            self.render();
            return true;
        }
        match input::from_key_text(text) {
            Some(k) => {
                self.on_key(k);
                true
            }
            None => false,
        }
    }

    pub fn on_key(&mut self, k: RemoteKey) {
        tracing::debug!("key {k:?} on {:?}", self.screen);
        if self.dialog.is_some() {
            self.dialog_key(k);
        } else if self.osk.is_some() {
            self.osk_key(k);
        } else if !self.global_key(k) {
            match self.screen {
                Screen::Live => self.live_key(k),
                Screen::Guide => self.guide_key(k),
                Screen::Dvr => self.dvr_key(k),
                Screen::Menu => self.menu_key(k),
            }
        }
        self.render();
    }

    /// Keys that work the same from any screen.
    fn global_key(&mut self, k: RemoteKey) -> bool {
        let configured = self.tvh.is_some();
        match k {
            RemoteKey::Guide if configured => {
                if self.screen == Screen::Guide {
                    self.go_live();
                } else {
                    self.open_guide();
                }
            }
            RemoteKey::Dvr if configured => {
                if self.screen == Screen::Dvr {
                    self.go_live();
                } else {
                    self.open_dvr();
                }
            }
            RemoteKey::Menu if self.screen != Screen::Menu => self.run_action(crate::views::dialog::Action::OpenSettings),
            RemoteKey::Exit if configured => self.go_live(),
            RemoteKey::VolumeUp | RemoteKey::VolumeDown if self.player.audio_info().is_some_and(|a| a.passthrough) => {
                // A bitstream can't be made louder here; the receiver decodes it.
                self.toast("Volume is set on your TV or receiver (passthrough)");
            }
            RemoteKey::VolumeUp | RemoteKey::VolumeDown => {
                let v = self.player.volume() + if k == RemoteKey::VolumeUp { 5 } else { -5 };
                let _ = self.player.set_volume(v);
                self.cfg.ui.volume = Some(self.player.volume());
                self.save_config_soon();
                let shown = self.player.volume();
                self.toast(format!("Volume {shown}"));
            }
            RemoteKey::Mute => {
                let _ = self.player.toggle_mute();
                self.toast("Mute");
            }
            _ => return false,
        }
        true
    }

    /// Re-formats the on-screen clock now (e.g. after switching 12/24-hour).
    pub fn refresh_clock_text(&mut self) {
        self.clock = fmt_time(now());
    }

    pub fn go_live(&mut self) {
        self.entry.cancel();
        self.settings_unlocked = false;
        self.screen = Screen::Live;
        self.apply_video_rect();
    }

    fn live_key(&mut self, k: RemoteKey) {
        let t = Instant::now();
        if self.playback.is_some() {
            match k {
                RemoteKey::PlayPause | RemoteKey::Ok => {
                    let _ = self.player.toggle_pause();
                }
                RemoteKey::FastForward | RemoteKey::Right => {
                    let _ = self.player.seek(30.0);
                }
                RemoteKey::Rewind | RemoteKey::Left => {
                    let _ = self.player.seek(-10.0);
                }
                RemoteKey::Stop | RemoteKey::Back => self.stop_playback(),
                _ => {}
            }
            return;
        }
        match k {
            RemoteKey::Digit(d) => {
                if let Entry::Commit(n) = self.entry.digit(d, t) {
                    self.commit_number(n);
                }
            }
            RemoteKey::Dot => {
                self.entry.dot(t);
            }
            RemoteKey::Ok if self.entry.is_active() => {
                if let Some(n) = self.entry.commit() {
                    self.commit_number(n);
                }
            }
            RemoteKey::Back if self.entry.is_active() => {
                self.entry.backspace(t);
            }
            RemoteKey::Ok | RemoteKey::Info => {
                if self.banner_until.is_some() {
                    self.banner_until = None;
                } else {
                    self.show_banner();
                }
            }
            RemoteKey::Back => self.banner_until = None,
            RemoteKey::ChannelUp | RemoteKey::Up => self.step_channel(1),
            RemoteKey::ChannelDown | RemoteKey::Down => self.step_channel(-1),
            RemoteKey::Last => self.last_channel(),
            RemoteKey::Record => {
                let t = now();
                let ev = self.tuned.as_deref().and_then(|u| self.epg.at(u, t)).cloned();
                match ev {
                    Some(ev) => self.record_dialog(&ev),
                    None => self.toast("No program information to record"),
                }
            }
            RemoteKey::Yellow => {
                if let Some(uuid) = self.tuned.clone() {
                    let fav = self.lineup.toggle_favorite(&uuid);
                    self.save_config_soon();
                    self.toast(if fav { "Added to Favorites" } else { "Removed from Favorites" });
                    self.show_banner();
                }
            }
            RemoteKey::Blue => self.cycle_filter(),
            _ => {}
        }
    }

    pub fn cycle_filter(&mut self) {
        self.lineup.cycle_filter();
        self.cfg.ui.filter = self.lineup.filter().to_config();
        self.save_config_soon();
        let label = self.lineup.filter_label();
        self.toast(format!("Showing: {label}"));
    }

    /// A typed number finished. Tunes on live TV; moves the selection in the guide.
    fn commit_number(&mut self, n: tvh::ChannelNumber) {
        let Some(ch) = self.lineup.by_number(n).map(|c| c.uuid.clone()) else {
            self.toast(format!("Channel {n} not available"));
            return;
        };
        match self.screen {
            Screen::Guide => self.guide_jump_to(&ch),
            _ => self.tune(&ch),
        }
    }

    // ---------------------------------------------------------------- rendering

    /// Window-relative video placement: full screen, or the guide's preview hole.
    pub fn apply_video_rect(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let rect = (self.screen == Screen::Guide).then(|| {
            let w = ui.window().size().to_logical(ui.window().scale_factor());
            let (ww, wh) = (w.width as f64, w.height as f64);
            let (x, y) = (ui.get_preview_x() as f64, ui.get_preview_y() as f64);
            let (pw, ph) = (ui.get_preview_w() as f64, ui.get_preview_h() as f64);
            player::VideoRect { left: x / ww, top: y / wh, right: (ww - x - pw) / ww, bottom: (wh - y - ph) / wh }
        });
        let _ = self.player.set_video_rect(rect);
    }

    pub fn render(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        // How many list rows fit depends on the screen; the UI measures it.
        let lineup_len = self.lineup.visible_len();
        self.guide.set_visible(ui.get_guide_rows_fit().max(1) as usize, lineup_len);
        self.dvr.visible = ui.get_dvr_rows_fit().max(1) as usize;
        self.menu.visible = ui.get_menu_rows_fit().max(1) as usize;
        ui.set_screen(self.screen);
        ui.set_clock(self.clock.clone().into());
        ui.set_toast(self.toast.clone().into());
        ui.set_video_status(self.video_status.clone().into());

        let entry = match self.screen {
            Screen::Live | Screen::Guide => self.entry_display(),
            _ => String::new(),
        };
        ui.set_entry_text(entry.into());

        let banner = self.banner_until.is_some() && self.playback.is_none();
        ui.set_banner_visible(banner);
        if banner {
            ui.set_banner(self.banner_data());
        }
        self.render_playback(&ui);

        match self.screen {
            Screen::Guide => self.render_guide(&ui),
            Screen::Dvr => self.render_dvr(&ui),
            Screen::Menu => self.render_menu(&ui),
            Screen::Live => {}
        }
        self.render_osk(&ui);
        self.render_dialog(&ui);
    }

    fn entry_display(&self) -> String {
        self.entry.display_text()
    }

    fn banner_data(&self) -> BannerData {
        let t = now();
        let Some(ch) = self.tuned.as_deref().and_then(|u| self.lineup.get(u)) else {
            return BannerData::default();
        };
        let (cur, next) = self.epg.now_next(&ch.uuid, t);
        let progress = cur.map(|e| (t - e.start) as f32 / (e.stop - e.start).max(1) as f32).unwrap_or(0.0);
        BannerData {
            number: ch.number.to_string().into(),
            name: ch.name.clone().into(),
            logo: self.logos.get(&ch.uuid).cloned().unwrap_or_default(),
            now_title: cur.map(|e| e.title.clone()).unwrap_or_default().into(),
            now_time: cur.map(|e| fmt_range(e.start, e.stop)).unwrap_or_default().into(),
            next_title: next.map(|e| e.title.clone()).unwrap_or_default().into(),
            next_time: next.map(|e| fmt_time(e.start)).unwrap_or_default().into(),
            progress,
            recording: self.channel_recording(&ch.uuid),
            favorite: self.lineup.is_favorite(&ch.uuid),
            filter: match self.lineup.filter() {
                Filter::All => String::new(),
                _ => self.lineup.filter_label(),
            }
            .into(),
        }
    }

    fn render_playback(&self, ui: &AppWindow) {
        ui.set_playback_visible(self.playback.is_some() && self.screen == Screen::Live);
        if let Some(pb) = &self.playback {
            let fmt = |s: f64| {
                let s = s.max(0.0) as u64;
                format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            };
            ui.set_playback_title(pb.entry.disp_title.clone().into());
            ui.set_playback_position(format!("{} / {}", fmt(pb.position), fmt(pb.duration)).into());
            ui.set_playback_progress(if pb.duration > 0.0 { (pb.position / pb.duration) as f32 } else { 0.0 });
            ui.set_playback_paused(pb.paused);
        }
    }

    /// Whether a recording is in progress on this channel.
    pub fn channel_recording(&self, uuid: &str) -> bool {
        self.dvr_upcoming.iter().any(|d| d.is_recording() && d.channel.as_deref() == Some(uuid))
    }

    /// DVR entry scheduled for this EPG event, if any.
    pub fn dvr_for_event(&self, ev: &EpgEvent) -> Option<&DvrEntry> {
        self.dvr_upcoming.iter().find(|d| d.broadcast == Some(ev.event_id)).or_else(|| {
            ev.dvr_uuid.as_deref().and_then(|u| self.dvr_upcoming.iter().find(|d| d.uuid == u))
        })
    }
}

/// Decodes and shrinks a channel logo to at most 128x64. Oversized images are rejected
/// before allocation so a hostile server can't exhaust memory.
fn decode_logo(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 << 20);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let img = img.resize(128, 64, image::imageops::FilterType::Triangle).to_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height()))
}

/// Shorthand for building Slint list models.
pub fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

#[cfg(test)]
mod tests {
    use super::decode_logo;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn logos_are_decoded_and_shrunk() {
        let buf = decode_logo(&png(256, 128)).unwrap();
        assert_eq!((buf.width(), buf.height()), (128, 64));
    }

    #[test]
    fn huge_logos_are_rejected() {
        assert!(decode_logo(&png(5000, 10)).is_none());
        assert!(decode_logo(b"not an image").is_none());
    }
}
