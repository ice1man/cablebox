//! Settings screens, first-run setup, and the on-screen keyboard.

use input::RemoteKey;
use sysctl::network::{AccessPointInfo, Status};

use crate::app::{model, App};
use crate::config::AuthMode;
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

#[derive(Debug, Default)]
pub struct MenuState {
    /// Page stack with the selected row on each.
    stack: Vec<(Page, usize)>,
    pub wifi: Vec<AccessPointInfo>,
    pub scanning: bool,
    pub net: Option<Status>,
    pub server_info: String,
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

/// Display modes of the first connected DRM connector, in the kernel's order
/// (which is what `SLINT_DRM_MODE` indexes).
pub fn drm_modes() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let mut conns: Vec<_> = dir.flatten().map(|e| e.path()).filter(|p| p.join("modes").exists()).collect();
    conns.sort();
    for c in conns {
        if std::fs::read_to_string(c.join("status")).is_ok_and(|s| s.trim() == "connected") {
            let modes = std::fs::read_to_string(c.join("modes")).unwrap_or_default();
            return modes.lines().map(str::to_string).collect();
        }
    }
    Vec::new()
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
                Item::new("System & updates", "", Act::Open(Page::System)),
            ],
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
                let modes = drm_modes();
                let mode = c
                    .display
                    .mode
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| modes.get(i).map(|m| format!("{m} (#{i})")))
                    .unwrap_or_else(|| "Auto".into());
                vec![
                    Item::new("Resolution", mode, Act::Cycle(Field::Mode)),
                    Item::new(
                        "Interface size",
                        c.display.scale.map(|s| format!("{:.0}%", s * 100.0)).unwrap_or_else(|| "Auto".into()),
                        Act::Cycle(Field::Scale),
                    ),
                    Item::new("Restart to apply", "", Act::Do(Action::RestartApp)),
                ]
            }
            Page::Remote => vec![
                Item::new("Remote & HDMI-CEC input", on_off(c.remote.cec), Act::Cycle(Field::Cec)),
                Item::new("CEC device name", c.remote.cec_name.clone(), Act::Edit(Field::CecName)),
                Item::info("Keyboard", "0-9 . channels · G guide · I info · M menu · D recordings"),
                Item::info("", "R record · F favorite · F1-F4 colour keys · Esc back"),
            ],
            Page::System => vec![
                Item::info("Version", env!("CARGO_PKG_VERSION")),
                Item::new("Check for updates", self.updates.summary(), Act::CheckUpdates),
                Item::new("Automatic update checks", on_off(c.updates.auto_check), Act::Cycle(Field::AutoUpdate)),
                Item::new("Restart CableBox", "", Act::Do(Action::RestartApp)),
                Item::new("Reboot", "", Act::Do(Action::Reboot)),
                Item::new("Power off", "", Act::Do(Action::PowerOff)),
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
            Field::Scale => {
                let i = SCALES.iter().position(|s| *s == c.display.scale).unwrap_or(0);
                c.display.scale = SCALES[step(SCALES.len(), i)];
            }
            Field::Mode => {
                // "" (auto) followed by each connector mode index.
                let n = drm_modes().len() + 1;
                let cur = c.display.mode.parse::<usize>().map(|i| i + 1).unwrap_or(0);
                let next = step(n, cur);
                c.display.mode = if next == 0 { String::new() } else { (next - 1).to_string() };
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
            Page::Display => ("DISPLAY", "Resolution changes apply after CableBox restarts."),
            Page::Remote => ("REMOTE CONTROL", "Most TVs pass their remote's buttons to CableBox over HDMI-CEC."),
            Page::System => ("SYSTEM", "Updates are installed from your distribution's package repositories."),
        };
        ui.set_menu_title(title.into());
        ui.set_menu_hint(hint.into());
        let rows: Vec<MenuItem> = items
            .into_iter()
            .enumerate()
            .map(|(i, it)| MenuItem {
                disabled: it.act == Act::None,
                label: it.label.into(),
                value: it.value.into(),
                selected: i == sel,
            })
            .collect();
        ui.set_menu_items(model(rows));
    }
}
