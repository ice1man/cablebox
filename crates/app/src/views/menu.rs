//! Settings screens, first-run setup, and the on-screen keyboard.

use input::RemoteKey;
use sysctl::network::{AccessPointInfo, Status};
use sysctl::services::State;

use crate::app::{model, App};
use crate::config::{AuthMode, Quality};
use crate::views::dialog::{Action, Dialog};
use crate::{AppWindow, MenuItem, Screen};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Setup,
    Root,
    Server,
    Network,
    Display,
    Remote,
    System,
    Clock,
    TzRegion,
    TzCity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Url,
    User,
    Pass,
    Auth,
    Profile,
    Cec,
    CecName,
    Scale,
    Mode,
    AutoUpdate,
    Hour24,
    Ntp,
    Output,
    SmoothMotion,
    Quality,
    DebugOverlay,
}

#[derive(Debug, Clone, PartialEq)]
enum Act {
    Open(Page),
    Edit(Field),
    Cycle(Field),
    Connect,
    TestServer,
    ScanWifi,
    JoinWifi(usize),
    CheckUpdates,
    Pin,
    Ssh,
    Console,
    Region(String),
    Zone(String),
    SetDateTime,
    Do(Action),
    None,
}

struct Item {
    label: String,
    value: String,
    act: Act,
}

impl Item {
    fn new(label: impl Into<String>, value: impl Into<String>, act: Act) -> Self {
        Self { label: label.into(), value: value.into(), act }
    }

    fn info(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self::new(label, value, Act::None)
    }
}

#[derive(Debug)]
pub struct MenuState {
    /// Page stack with the selected row on each.
    stack: Vec<(Page, usize)>,
    pub wifi: Vec<AccessPointInfo>,
    pub scanning: bool,
    pub net: Option<Status>,
    pub server_info: String,
    /// SSH server state; `None` while checking.
    pub ssh: Option<sysctl::services::State>,
    /// Time zone / NTP state from timedated; `None` while checking.
    pub clock: Option<sysctl::clock::Status>,
    /// All time zones, loaded when the zone picker first opens.
    pub zones: Vec<String>,
    /// Region chosen on the zone picker's first page.
    pub tz_region: String,
    /// Rows that fit on screen (from the UI).
    pub visible: usize,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            wifi: Vec::new(),
            scanning: false,
            net: None,
            server_info: String::new(),
            ssh: None,
            clock: None,
            zones: Vec::new(),
            tz_region: String::new(),
            visible: DEFAULT_VISIBLE_ROWS,
        }
    }
}

/// Rows the settings list shows at the 720-high design size; the UI reports the real number.
pub const DEFAULT_VISIBLE_ROWS: usize = 10;

/// Row to select after paging one screen up or down: the selectable row nearest the
/// target, preferring rows further in the paging direction.
fn page_target(sel: usize, n: usize, rows: usize, down: bool, selectable: impl Fn(usize) -> bool) -> Option<usize> {
    if down {
        let target = (sel + rows).min(n - 1);
        (target..n).chain((sel + 1..target).rev()).find(|&i| selectable(i))
    } else {
        let target = sel.saturating_sub(rows);
        (0..=target).rev().chain(target + 1..sel).find(|&i| selectable(i))
    }
}

/// First visible row so that `sel` stays in a `rows`-tall window over `len` rows.
pub fn scroll_window(sel: usize, len: usize, rows: usize) -> usize {
    if len <= rows {
        0
    } else {
        sel.saturating_sub(rows / 2).min(len - rows)
    }
}

impl MenuState {
    fn page(&self) -> Page {
        self.stack.last().map(|p| p.0).unwrap_or(Page::Root)
    }
}

pub enum OskTarget {
    Field(Field),
    WifiPassword(String),
}

pub struct Osk {
    pub label: String,
    pub value: String,
    pub secret: bool,
    pub sel: usize,
    pub shift: bool,
    pub target: OskTarget,
}

const COLS: usize = 10;
const KEYS: [&str; 50] = [
    "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", //
    "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", //
    "a", "s", "d", "f", "g", "h", "j", "k", "l", ":", //
    "z", "x", "c", "v", "b", "n", "m", ".", "/", "-", //
    "SHIFT", "@", "_", "#", "?", "!", "&", "SPACE", "DEL", "DONE",
];

const SCALES: [Option<f32>; 6] = [None, Some(1.0), Some(1.25), Some(1.5), Some(2.0), Some(3.0)];
const PROFILES: [&str; 4] = ["pass", "matroska", "webtv-h264-aac-mpegts", "htsp"];

fn on_off(b: bool) -> &'static str {
    if b {
        "On"
    } else {
        "Off"
    }
}

/// Modes of the screen the Display settings refer to: the saved one, or what Auto picks.
fn screen_modes(saved_output: &str) -> Vec<String> {
    let outs = crate::display::outputs();
    let name = crate::display::pick(&outs, saved_output);
    outs.into_iter().find(|o| Some(&o.name) == name.as_ref()).map(|o| o.modes).unwrap_or_default()
}

impl App {
    pub fn open_setup(&mut self) {
        self.menu.stack = vec![(Page::Setup, 0)];
        self.screen = Screen::Menu;
    }

    pub fn open_settings(&mut self) {
        self.entry.cancel();
        self.menu.stack = vec![(Page::Root, 0)];
        self.screen = Screen::Menu;
    }

    fn open_page(&mut self, page: Page) {
        self.menu.stack.push((page, 0));
        match page {
            Page::Network => self.refresh_network(),
            Page::System => self.refresh_ssh(),
            Page::Clock => self.refresh_clock(),
            Page::TzRegion => {
                self.load_zones();
                self.select_current_zone();
            }
            Page::TzCity => self.select_current_zone(),
            Page::Server | Page::Setup => self.menu.server_info.clear(),
            _ => {}
        }
    }

    fn items(&self, page: Page) -> Vec<Item> {
        let c = &self.cfg;
        let server = |connect_label: &str| {
            let mut v = vec![
                Item::new("Server address", if c.server.url.is_empty() { "http://…:9981" } else { &c.server.url }, Act::Edit(Field::Url)),
                Item::new("Login", match c.server.auth {
                    AuthMode::None => "None",
                    AuthMode::Basic if c.server.url.starts_with("http://") => "Basic (unencrypted!)",
                    AuthMode::Basic => "Basic",
                    AuthMode::Digest => "Digest",
                }, Act::Cycle(Field::Auth)),
            ];
            if c.server.auth != AuthMode::None {
                v.push(Item::new("Username", c.server.user.clone(), Act::Edit(Field::User)));
                v.push(Item::new("Password", "•".repeat(c.server.pass.chars().count()), Act::Edit(Field::Pass)));
            }
            v.push(Item::new("Stream profile", c.server.profile.clone(), Act::Cycle(Field::Profile)));
            v.push(Item::new("Test connection", self.menu.server_info.clone(), Act::TestServer));
            v.push(Item::new(connect_label, "", Act::Connect));
            v
        };
        match page {
            Page::Setup => server("Connect"),
            Page::Server => server("Save & reconnect"),
            Page::Root => vec![
                Item::new("Tvheadend server", c.server.url.clone(), Act::Open(Page::Server)),
                Item::new("Network", "", Act::Open(Page::Network)),
                Item::new("Display", "", Act::Open(Page::Display)),
                Item::new("Remote control", "", Act::Open(Page::Remote)),
                Item::new(
                    "Clock & time zone",
                    self.menu.clock.as_ref().map(|c| c.timezone.clone()).unwrap_or_default(),
                    Act::Open(Page::Clock),
                ),
                Item::new("System & updates", "", Act::Open(Page::System)),
            ],
            Page::Clock => self.clock_items(),
            Page::TzRegion => {
                if self.menu.zones.is_empty() {
                    vec![Item::info("Loading time zones…", "")]
                } else {
                    crate::clock::regions(&self.menu.zones).into_iter().map(|r| Item::new(r.clone(), "", Act::Region(r))).collect()
                }
            }
            Page::TzCity => {
                let current = self.menu.clock.as_ref().map(|c| c.timezone.as_str()).unwrap_or("");
                crate::clock::cities(&self.menu.zones, &self.menu.tz_region)
                    .into_iter()
                    .map(|(label, zone)| {
                        let mark = if zone == current { "Current" } else { "" };
                        Item::new(label, mark, Act::Zone(zone))
                    })
                    .collect()
            }
            Page::Network => {
                let mut v = match &self.menu.net {
                    Some(st) => vec![
                        Item::info("Status", if st.connected { "Connected" } else { "Not connected" }),
                        Item::info("Connection", st.name.clone()),
                        Item::info("IP address", st.address.clone()),
                    ],
                    None => vec![Item::info("Status", "Checking…")],
                };
                v.push(Item::new(
                    "Scan for Wi-Fi networks",
                    if self.menu.scanning { "Scanning…" } else { "" },
                    Act::ScanWifi,
                ));
                for (i, ap) in self.menu.wifi.iter().enumerate() {
                    let bars = match ap.strength {
                        75.. => "▂▄▆█",
                        50.. => "▂▄▆",
                        25.. => "▂▄",
                        _ => "▂",
                    };
                    let value = format!("{}{} {bars}", if ap.active { "Connected  " } else { "" }, if ap.secured { "🔒" } else { "" });
                    v.push(Item::new(format!("   {}", ap.ssid), value, Act::JoinWifi(i)));
                }
                v
            }
            Page::Display => {
                let outs = crate::display::outputs();
                let auto = crate::display::pick(&outs, "").map(|n| crate::display::label(&n));
                let screen = if c.display.output.is_empty() {
                    match auto {
                        Some(a) => format!("Auto ({a})"),
                        None => "Auto".into(),
                    }
                } else {
                    let connected = outs.iter().any(|o| o.connected && o.name == c.display.output);
                    let name = crate::display::label(&c.display.output);
                    if connected { name } else { format!("{name} (not connected)") }
                };
                let modes = screen_modes(&c.display.output);
                let mode = c
                    .display
                    .mode
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| modes.get(i).map(|m| format!("{m} (#{i})")))
                    .unwrap_or_else(|| "Auto".into());
                let mut v = if crate::display::session() == crate::display::Session::Window {
                    vec![Item::info("Screen", "Window (F11 for full screen)")]
                } else {
                    vec![
                        Item::new("Screen", screen, Act::Cycle(Field::Output)),
                        Item::info("Showing on", crate::display::in_use().map(crate::display::label).unwrap_or_else(|| "Default".into())),
                        Item::new("Resolution", mode, Act::Cycle(Field::Mode)),
                    ]
                };
                v.extend([
                    Item::new(
                        "Interface size",
                        c.display.scale.map(|s| format!("{:.0}%", s * 100.0)).unwrap_or_else(|| "Auto".into()),
                        Act::Cycle(Field::Scale),
                    ),
                    Item::new("Smooth motion", on_off(c.display.smooth_motion), Act::Cycle(Field::SmoothMotion)),
                    Item::new(
                        "Picture quality",
                        match c.display.quality {
                            Quality::Standard => "Standard",
                            Quality::High => "High",
                        },
                        Act::Cycle(Field::Quality),
                    ),
                    Item::new("Restart to apply", "", Act::Do(Action::RestartApp)),
                ]);
                v
            }
            Page::Remote => vec![
                Item::new("Remote & HDMI-CEC input", on_off(c.remote.cec), Act::Cycle(Field::Cec)),
                Item::new("CEC device name", c.remote.cec_name.clone(), Act::Edit(Field::CecName)),
                Item::info("Keyboard", "0-9 . channels · G guide · I info · M menu · D recordings"),
                Item::info("", "R record · F favorite · F1-F4 colour keys · Esc back"),
                Item::info("", "F11 full screen (in a window)"),
            ],
            Page::System => vec![
                Item::info("Version", env!("CARGO_PKG_VERSION")),
                Item::new("Check for updates", self.updates.summary(), Act::CheckUpdates),
                Item::new("Automatic update checks", on_off(c.updates.auto_check), Act::Cycle(Field::AutoUpdate)),
                Item::new("Settings PIN", on_off(c.ui.pin.is_some()), Act::Pin),
                Item::new("Debug info", on_off(c.ui.debug_overlay), Act::Cycle(Field::DebugOverlay)),
                match self.menu.ssh {
                    Some(State::NotInstalled) => Item::info("Remote access (SSH)", "Not installed"),
                    Some(State::Running) => Item::new("Remote access (SSH)", self.ssh_address(), Act::Ssh),
                    Some(State::Stopped) => Item::new("Remote access (SSH)", "Off", Act::Ssh),
                    None => Item::info("Remote access (SSH)", "Checking…"),
                },
                Item::new("Restart CableBox", "", Act::Do(Action::RestartApp)),
                Item::new("Reboot", "", Act::Do(Action::Reboot)),
                Item::new("Power off", "", Act::Do(Action::PowerOff)),
                if self.under_service {
                    Item::new("Exit to console", "", Act::Console)
                } else {
                    Item::info("Exit to console", "Only on a box")
                },
            ],
        }
    }

    pub fn menu_key(&mut self, k: RemoteKey) {
        let page = self.menu.page();
        let items = self.items(page);
        let n = items.len();
        if n == 0 {
            return;
        }
        let Some((_, sel)) = self.menu.stack.last_mut() else { return };
        *sel = (*sel).min(n.saturating_sub(1));
        let selectable = |i: usize| items[i].act != Act::None;
        if n > 0 && !selectable(*sel) {
            *sel = (0..n).find(|&i| selectable(i)).unwrap_or(0);
        }
        match k {
            RemoteKey::ChannelUp | RemoteKey::ChannelDown | RemoteKey::PageUp | RemoteKey::PageDown => {
                let down = matches!(k, RemoteKey::ChannelDown | RemoteKey::PageDown);
                if let Some(i) = page_target(*sel, n, self.menu.visible, down, selectable) {
                    *sel = i;
                }
            }
            RemoteKey::Up | RemoteKey::Down => {
                let step = if k == RemoteKey::Up { n - 1 } else { 1 };
                let mut i = *sel;
                for _ in 0..n {
                    i = (i + step) % n;
                    if selectable(i) {
                        *sel = i;
                        break;
                    }
                }
            }
            RemoteKey::Left | RemoteKey::Right => {
                if let Act::Cycle(f) = items[*sel].act {
                    self.cycle(f, if k == RemoteKey::Right { 1 } else { -1 });
                }
            }
            RemoteKey::Ok => {
                let act = items[*sel].act.clone();
                self.menu_act(act);
            }
            RemoteKey::Back | RemoteKey::Menu => {
                if page == Page::Setup {
                    return;
                }
                if k == RemoteKey::Menu || self.menu.stack.len() <= 1 {
                    self.go_live();
                } else {
                    self.menu.stack.pop();
                }
            }
            _ => {}
        }
    }

    fn menu_act(&mut self, act: Act) {
        match act {
            Act::Open(p) => self.open_page(p),
            Act::Edit(f) => self.edit(f),
            Act::Cycle(f) => self.cycle(f, 1),
            Act::Connect => {
                if self.cfg.server.url.is_empty() {
                    self.toast("Enter the server address first");
                    return;
                }
                self.save_config_now();
                self.tuned = None;
                self.connect();
                self.go_live();
            }
            Act::TestServer => self.test_server(),
            Act::ScanWifi => self.scan_wifi(),
            Act::JoinWifi(i) => {
                if let Some(ap) = self.menu.wifi.get(i).cloned() {
                    if ap.secured && !ap.active {
                        self.osk = Some(Osk {
                            label: format!("Password for {}", ap.ssid),
                            value: String::new(),
                            secret: true,
                            sel: 0,
                            shift: false,
                            target: OskTarget::WifiPassword(ap.ssid),
                        });
                    } else if !ap.active {
                        self.connect_wifi(ap.ssid, None);
                    }
                }
            }
            Act::CheckUpdates => self.check_updates(true),
            Act::Region(r) => {
                self.menu.tz_region = r;
                self.open_page(Page::TzCity);
            }
            Act::Zone(z) => self.set_timezone(z),
            Act::SetDateTime => self.ask_datetime(),
            Act::Ssh => {
                let on = self.menu.ssh != Some(State::Running);
                let (title, body, label) = if on {
                    (
                        "Turn on remote access?",
                        "Starts the SSH server so you can log in to this box from another computer. Anyone on your network can try to log in, so use strong passwords or SSH keys.",
                        "Turn On",
                    )
                } else {
                    ("Turn off remote access?", "Stops the SSH server. Open sessions may be disconnected.", "Turn Off")
                };
                self.show_dialog(Dialog::new(title, body, vec![(label, Action::SetSsh(on)), ("Cancel", Action::Close)]));
            }
            Act::Console => self.show_dialog(Dialog::new(
                "Exit to console?",
                "TV stops and a login prompt appears. Log in with a keyboard. To come back, run 'sudo systemctl start cablebox' or reboot.",
                vec![("Exit to Console", Action::ExitToConsole), ("Cancel", Action::Close)],
            )),
            Act::Pin if self.cfg.ui.pin.is_none() => self.run_action(Action::SetPin),
            Act::Pin => self.show_dialog(Dialog::new(
                "Settings PIN",
                "The PIN protects Settings, system updates, rebooting and deleting recordings.",
                vec![("Change PIN", Action::SetPin), ("Remove PIN", Action::RemovePin), ("Cancel", Action::Close)],
            )),
            Act::Do(a @ (Action::Reboot | Action::PowerOff | Action::RestartApp)) => {
                let (title, label) = match a {
                    Action::Reboot => ("Reboot the box?", "Reboot"),
                    Action::PowerOff => ("Power off the box?", "Power Off"),
                    _ => ("Restart CableBox?", "Restart"),
                };
                self.show_dialog(Dialog::new(title, "", vec![(label, a), ("Cancel", Action::Close)]));
            }
            Act::Do(a) => self.run_action(a),
            Act::None => {}
        }
    }

    fn cycle(&mut self, f: Field, dir: i32) {
        let step = |len: usize, cur: usize| ((cur as i32 + dir).rem_euclid(len as i32)) as usize;
        let c = &mut self.cfg;
        match f {
            Field::Auth => {
                let all = [AuthMode::None, AuthMode::Basic, AuthMode::Digest];
                let i = all.iter().position(|a| *a == c.server.auth).unwrap_or(0);
                c.server.auth = all[step(all.len(), i)].clone();
            }
            Field::Profile => {
                let i = PROFILES.iter().position(|p| *p == c.server.profile).unwrap_or(0);
                c.server.profile = PROFILES[step(PROFILES.len(), i)].into();
            }
            Field::Cec => c.remote.cec = !c.remote.cec,
            Field::AutoUpdate => c.updates.auto_check = !c.updates.auto_check,
            Field::DebugOverlay => c.ui.debug_overlay = !c.ui.debug_overlay,
            Field::Scale => {
                let i = SCALES.iter().position(|s| *s == c.display.scale).unwrap_or(0);
                c.display.scale = SCALES[step(SCALES.len(), i)];
            }
            Field::Mode => {
                // "" (auto) followed by each connector mode index.
                let n = screen_modes(&c.display.output).len() + 1;
                let cur = c.display.mode.parse::<usize>().map(|i| i + 1).unwrap_or(0);
                let next = step(n, cur);
                c.display.mode = if next == 0 { String::new() } else { (next - 1).to_string() };
            }
            Field::Output => {
                // "" (Auto) followed by each connected screen.
                let mut names = vec![String::new()];
                names.extend(crate::display::outputs().into_iter().filter(|o| o.connected).map(|o| o.name));
                let cur = names.iter().position(|n| *n == c.display.output).unwrap_or(0);
                c.display.output = names[step(names.len(), cur)].clone();
                // A resolution only makes sense for the screen it was picked on.
                c.display.mode.clear();
            }
            Field::SmoothMotion => c.display.smooth_motion = !c.display.smooth_motion,
            Field::Quality => {
                c.display.quality = match c.display.quality {
                    Quality::Standard => Quality::High,
                    Quality::High => Quality::Standard,
                }
            }
            Field::Hour24 => {
                c.clock.hour24 = !c.clock.hour24;
                crate::clock::set_hour24(c.clock.hour24);
                self.refresh_clock_text();
            }
            Field::Ntp => {
                let on = !self.menu.clock.as_ref().is_some_and(|c| c.ntp);
                self.set_ntp(on);
                return;
            }
            Field::Url | Field::User | Field::Pass | Field::CecName => return,
        }
        // Server fields are saved on Connect; everything else right away.
        if !matches!(f, Field::Auth | Field::Profile) {
            self.save_config_now();
        }
        if f == Field::Scale {
            crate::apply_scale(&self.ui, self.cfg.display.scale);
        }
        if matches!(f, Field::SmoothMotion | Field::Quality) {
            self.apply_video_settings();
        }
        if f == Field::DebugOverlay {
            self.update_debug(std::time::Instant::now());
        }
        if f == Field::Cec {
            self.toast("HDMI-CEC change applies after restart");
        }
    }

    fn edit(&mut self, f: Field) {
        let (label, value, secret) = match f {
            Field::Url => ("Server address (e.g. http://192.168.1.10:9981)", self.cfg.server.url.clone(), false),
            Field::User => ("Username", self.cfg.server.user.clone(), false),
            Field::Pass => ("Password", self.cfg.server.pass.clone(), true),
            Field::CecName => ("CEC device name", self.cfg.remote.cec_name.clone(), false),
            _ => return,
        };
        let value = if f == Field::Url && value.is_empty() { "http://".into() } else { value };
        self.osk = Some(Osk { label: label.into(), value, secret, sel: 0, shift: false, target: OskTarget::Field(f) });
    }

    // ------------------------------------------------------------ on-screen keyboard

    fn osk_done(&mut self) {
        let Some(osk) = self.osk.take() else { return };
        match osk.target {
            OskTarget::Field(f) => {
                let v = osk.value.trim().to_string();
                match f {
                    Field::Url => self.cfg.server.url = v.trim_end_matches('/').to_string(),
                    Field::User => self.cfg.server.user = v,
                    Field::Pass => self.cfg.server.pass = osk.value,
                    Field::CecName => {
                        self.cfg.remote.cec_name = v;
                        self.save_config_now();
                    }
                    _ => {}
                }
            }
            OskTarget::WifiPassword(ssid) => self.connect_wifi(ssid, Some(osk.value)),
        }
    }

    fn osk_press(&mut self, key: &str) {
        let Some(osk) = &mut self.osk else { return };
        match key {
            "SHIFT" => osk.shift = !osk.shift,
            "SPACE" => osk.value.push(' '),
            "DEL" => {
                osk.value.pop();
            }
            "DONE" => self.osk_done(),
            k if osk.shift => {
                osk.value.push_str(&k.to_uppercase());
                osk.shift = false;
            }
            k => osk.value.push_str(k),
        }
    }

    pub fn osk_key(&mut self, k: RemoteKey) {
        let Some(osk) = &mut self.osk else { return };
        let rows = KEYS.len() / COLS;
        let (r, c) = (osk.sel / COLS, osk.sel % COLS);
        match k {
            RemoteKey::Up => osk.sel = ((r + rows - 1) % rows) * COLS + c,
            RemoteKey::Down => osk.sel = ((r + 1) % rows) * COLS + c,
            RemoteKey::Left => osk.sel = r * COLS + (c + COLS - 1) % COLS,
            RemoteKey::Right => osk.sel = r * COLS + (c + 1) % COLS,
            RemoteKey::Digit(d) => osk.value.push(char::from(b'0' + d)),
            RemoteKey::Dot => osk.value.push('.'),
            RemoteKey::Ok => {
                let key = KEYS[osk.sel];
                self.osk_press(key);
            }
            RemoteKey::Red => self.osk_press("DEL"),
            RemoteKey::Green => self.osk_done(),
            RemoteKey::Back if !osk.value.is_empty() => {
                osk.value.pop();
            }
            RemoteKey::Back | RemoteKey::Exit => self.osk = None,
            _ => {}
        }
    }

    /// Physical keyboard typing while the OSK is open. Returns false for keys that
    /// should be handled as remote keys (arrows etc.).
    pub fn osk_text(&mut self, text: &str) -> bool {
        let Some(osk) = &mut self.osk else { return false };
        let mut chars = text.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else { return false };
        match ch {
            '\u{8}' | '\u{7f}' => {
                osk.value.pop();
            }
            '\n' | '\r' => self.osk_done(),
            '\u{1b}' => self.osk = None,
            '\u{F700}'..='\u{F8FF}' => return false,
            c if !c.is_control() => osk.value.push(c),
            _ => return false,
        }
        true
    }

    pub fn render_osk(&self, ui: &AppWindow) {
        ui.set_osk_visible(self.osk.is_some());
        let Some(osk) = &self.osk else { return };
        ui.set_osk_label(osk.label.clone().into());
        let shown = if osk.secret { "•".repeat(osk.value.chars().count()) } else { osk.value.clone() };
        ui.set_osk_value(shown.into());
        let keys: Vec<slint::SharedString> = KEYS
            .iter()
            .map(|k| if osk.shift && k.len() == 1 { k.to_uppercase().into() } else { (*k).into() })
            .collect();
        ui.set_osk_keys(model(keys));
        ui.set_osk_columns(COLS as i32);
        ui.set_osk_selected(osk.sel as i32);
    }

    pub fn render_menu(&self, ui: &AppWindow) {
        let page = self.menu.page();
        let sel = self.menu.stack.last().map(|p| p.1).unwrap_or(0);
        let items = self.items(page);
        let sel = if items.get(sel).is_some_and(|i| i.act == Act::None) {
            items.iter().position(|i| i.act != Act::None).unwrap_or(0)
        } else {
            sel
        };
        let (title, hint) = match page {
            Page::Setup => (
                "WELCOME TO CABLEBOX",
                "Enter your Tvheadend server address, then choose Connect. Use the arrow keys and OK; ◀ ▶ change options.",
            ),
            Page::Root => ("SETTINGS", "Back or Menu returns to TV."),
            Page::Server => ("TVHEADEND SERVER", "◀ ▶ change options. Changes take effect when you choose Save & reconnect."),
            Page::Network => ("NETWORK", "Choose a Wi-Fi network to connect. Wired connections are used automatically."),
            Page::Display => ("DISPLAY", "◀ ▶ change options. Screen and resolution apply after a restart. Smooth motion blends frames to the screen's refresh rate (helps 24 fps films). High picture quality needs a strong GPU; keep Standard on a Raspberry Pi."),
            Page::Remote => ("REMOTE CONTROL", "Most TVs pass their remote's buttons to CableBox over HDMI-CEC."),
            Page::System => ("SYSTEM", "Updates are installed from your distribution's package repositories."),
            Page::Clock => ("CLOCK", "◀ ▶ change options. With automatic time off, you can set the date and time yourself."),
            Page::TzRegion => ("TIME ZONE", "Choose a region. CH+/CH− page through long lists."),
            Page::TzCity => ("TIME ZONE", "Choose the city closest to you. CH+/CH− page through long lists."),
        };
        ui.set_menu_title(title.into());
        ui.set_menu_hint(hint.into());
        let visible = self.menu.visible.max(1);
        let first = scroll_window(sel, items.len(), visible);
        ui.set_menu_more_above(first > 0);
        ui.set_menu_more_below(first + visible < items.len());
        let rows: Vec<MenuItem> = items
            .into_iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, it)| MenuItem {
                disabled: it.act == Act::None,
                label: it.label.into(),
                value: it.value.into(),
                selected: i == sel,
            })
            .collect();
        ui.set_menu_items(model(rows));
    }

    fn clock_items(&self) -> Vec<Item> {
        let now = crate::app::now();
        let mut v = vec![
            Item::info("Now", format!("{}  {}", crate::app::fmt_day(now), crate::app::fmt_time(now))),
            Item::new(
                "Time format",
                if self.cfg.clock.hour24 { "24-hour" } else { "12-hour" },
                Act::Cycle(Field::Hour24),
            ),
        ];
        let Some(c) = &self.menu.clock else {
            v.push(Item::info("Time zone", "Checking…"));
            return v;
        };
        // Changing the system clock is only offered on a box, never from a dev window.
        let on_box = self.under_service;
        v.push(if on_box {
            Item::new("Time zone", c.timezone.clone(), Act::Open(Page::TzRegion))
        } else {
            Item::info("Time zone", format!("{} · only on a box", c.timezone))
        });
        let ntp = match (c.ntp, c.synced) {
            (true, true) => "On · synchronized",
            (true, false) => "On · waiting for sync",
            (false, _) => "Off",
        };
        v.push(if on_box && c.can_ntp {
            Item::new("Automatic time (network)", ntp, Act::Cycle(Field::Ntp))
        } else if !c.can_ntp {
            Item::info("Automatic time (network)", "Not available")
        } else {
            Item::info("Automatic time (network)", format!("{ntp} · only on a box"))
        });
        if on_box && !c.ntp {
            v.push(Item::new("Set date & time", "", Act::SetDateTime));
        }
        v
    }

    /// After zones load asynchronously, re-select the current one if a picker is open.
    pub fn select_current_zone_if_open(&mut self) {
        if matches!(self.menu.page(), Page::TzRegion | Page::TzCity) {
            self.select_current_zone();
        }
    }

    /// Leaves the zone picker after a choice.
    pub fn menu_back_to_clock(&mut self) {
        while matches!(self.menu.page(), Page::TzRegion | Page::TzCity) && self.menu.stack.len() > 1 {
            self.menu.stack.pop();
        }
    }

    /// Puts the selection on the current time zone's region or city.
    fn select_current_zone(&mut self) {
        let Some(tz) = self.menu.clock.as_ref().map(|c| c.timezone.clone()) else { return };
        let idx = match self.menu.page() {
            Page::TzRegion => {
                let region = tz.split_once('/').map(|(r, _)| r).unwrap_or(crate::clock::OTHER_REGION);
                crate::clock::regions(&self.menu.zones).iter().position(|r| r == region)
            }
            Page::TzCity => crate::clock::cities(&self.menu.zones, &self.menu.tz_region).iter().position(|(_, z)| *z == tz),
            _ => None,
        };
        if let (Some(i), Some(top)) = (idx, self.menu.stack.last_mut()) {
            top.1 = i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{page_target, scroll_window};

    #[test]
    fn paging_moves_one_screen() {
        let all = |_: usize| true;
        assert_eq!(page_target(20, 100, 10, true, all), Some(30));
        assert_eq!(page_target(95, 100, 10, true, all), Some(99));
        assert_eq!(page_target(20, 100, 10, false, all), Some(10));
        assert_eq!(page_target(3, 100, 10, false, all), Some(0));
        // Skips non-selectable rows, preferring further in the paging direction.
        let not_30 = |i: usize| i != 30;
        assert_eq!(page_target(20, 100, 10, true, not_30), Some(31));
        let only_low = |i: usize| i < 25;
        assert_eq!(page_target(20, 100, 10, true, only_low), Some(24));
        let not_0 = |i: usize| i != 0;
        assert_eq!(page_target(5, 100, 10, false, not_0), Some(1));
    }

    #[test]
    fn scroll_window_keeps_selection_visible() {
        assert_eq!(scroll_window(3, 8, 10), 0); // short list: no scrolling
        assert_eq!(scroll_window(0, 50, 10), 0);
        assert_eq!(scroll_window(5, 50, 10), 0);
        assert_eq!(scroll_window(6, 50, 10), 1);
        assert_eq!(scroll_window(25, 50, 10), 20);
        assert_eq!(scroll_window(49, 50, 10), 40); // clamps at the end
        for sel in 0..50 {
            let first = scroll_window(sel, 50, 10);
            assert!((first..first + 10).contains(&sel));
        }
    }
}
