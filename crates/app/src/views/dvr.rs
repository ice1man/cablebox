//! Recordings library: upcoming, recorded, and series rules.

use input::RemoteKey;

use crate::app::{fmt_day, fmt_range, model, App};
use crate::views::dialog::{Action, Dialog};
use crate::{AppWindow, DvrItem, Screen};

const PAGE: usize = 9;
const TABS: usize = 3;

#[derive(Debug, Default)]
pub struct DvrState {
    pub tab: usize,
    pub sel: usize,
}

impl DvrState {
    pub fn clamp(&mut self, len: usize) {
        self.sel = self.sel.min(len.saturating_sub(1));
    }
}

impl App {
    pub fn open_dvr(&mut self) {
        self.dvr = DvrState { tab: 1, sel: 0 };
        self.screen = Screen::Dvr;
        self.refresh_dvr();
    }

    pub fn dvr_items_len(&self) -> usize {
        match self.dvr.tab {
            0 => self.dvr_upcoming.len(),
            1 => self.dvr_finished.len(),
            _ => self.autorecs.len(),
        }
    }

    /// Newest recordings first; upcoming soonest first.
    fn finished_sorted(&self) -> Vec<&tvh::DvrEntry> {
        let mut v: Vec<_> = self.dvr_finished.iter().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.start));
        v
    }

    pub fn dvr_key(&mut self, k: RemoteKey) {
        let len = self.dvr_items_len();
        match k {
            RemoteKey::Left => {
                self.dvr.tab = (self.dvr.tab + TABS - 1) % TABS;
                self.dvr.sel = 0;
            }
            RemoteKey::Right => {
                self.dvr.tab = (self.dvr.tab + 1) % TABS;
                self.dvr.sel = 0;
            }
            RemoteKey::Up if len > 0 => self.dvr.sel = (self.dvr.sel + len - 1) % len,
            RemoteKey::Down if len > 0 => self.dvr.sel = (self.dvr.sel + 1) % len,
            RemoteKey::ChannelUp | RemoteKey::PageUp => self.dvr.sel = self.dvr.sel.saturating_sub(PAGE),
            RemoteKey::ChannelDown | RemoteKey::PageDown => {
                self.dvr.sel = (self.dvr.sel + PAGE).min(len.saturating_sub(1))
            }
            RemoteKey::Ok | RemoteKey::PlayPause => self.dvr_activate(),
            RemoteKey::Red | RemoteKey::Stop => self.dvr_delete(),
            RemoteKey::Back => self.go_live(),
            _ => {}
        }
    }

    fn dvr_activate(&mut self) {
        match self.dvr.tab {
            0 => self.dvr_delete(),
            1 => {
                let Some(e) = self.finished_sorted().get(self.dvr.sel).map(|e| (*e).clone()) else { return };
                self.show_dialog(Dialog::new(
                    e.disp_title.clone(),
                    e.disp_description.clone().unwrap_or_default(),
                    vec![
                        ("Play", Action::PlayRecording(e.uuid.clone())),
                        ("Delete", Action::RemoveRecording(e.uuid.clone())),
                        ("Close", Action::Close),
                    ],
                ));
            }
            _ => self.dvr_delete(),
        }
    }

    fn dvr_delete(&mut self) {
        let sel = self.dvr.sel;
        let dialog = match self.dvr.tab {
            0 => self.dvr_upcoming.get(sel).map(|e| {
                let (verb, label) = if e.is_recording() { ("Stop", "Stop Recording") } else { ("Cancel", "Cancel Recording") };
                Dialog::new(
                    format!("{verb} recording?"),
                    format!("{}\n{}", e.disp_title, fmt_range(e.start, e.stop)),
                    vec![(label, Action::CancelDvr(e.uuid.clone())), ("Keep", Action::Close)],
                )
            }),
            1 => self.finished_sorted().get(sel).map(|e| {
                Dialog::new(
                    "Delete recording?",
                    format!("{}\nThis cannot be undone.", e.disp_title),
                    vec![("Delete", Action::RemoveRecording(e.uuid.clone())), ("Keep", Action::Close)],
                )
            }),
            _ => self.autorecs.get(sel).map(|r| {
                Dialog::new(
                    "Remove series recording?",
                    format!("{}\nAlready-scheduled episodes will be cancelled.", rule_name(r)),
                    vec![("Remove", Action::DeleteAutorec(r.uuid.clone())), ("Keep", Action::Close)],
                )
            }),
        };
        if let Some(d) = dialog {
            self.show_dialog(d);
        }
    }

    pub fn render_dvr(&self, ui: &AppWindow) {
        let sel = self.dvr.sel;
        let first = sel.saturating_sub(PAGE - 1).min(self.dvr_items_len().saturating_sub(PAGE));
        let item = |i: usize, e: &tvh::DvrEntry| DvrItem {
            title: e.disp_title.clone().into(),
            subtitle: e.disp_subtitle.clone().unwrap_or_default().into(),
            when: format!("{}  {}", fmt_day(e.start), crate::app::fmt_time(e.start)).into(),
            channel: e.channelname.clone().unwrap_or_default().into(),
            status: match e.sched_status.as_deref() {
                Some("recording") => "Recording".into(),
                Some("scheduled") => "Scheduled".into(),
                Some("completed") => size_label(e.filesize).into(),
                _ => e.status.clone().unwrap_or_default().into(),
            },
            selected: i == sel,
            recording: e.is_recording(),
        };

        let (items, detail, empty): (Vec<DvrItem>, String, &str) = match self.dvr.tab {
            0 => (
                self.dvr_upcoming.iter().enumerate().skip(first).take(PAGE).map(|(i, e)| item(i, e)).collect(),
                self.dvr_upcoming.get(sel).map(entry_detail).unwrap_or_default(),
                "Nothing scheduled. Press Record on a program in the Guide.",
            ),
            1 => {
                let fin = self.finished_sorted();
                (
                    fin.iter().enumerate().skip(first).take(PAGE).map(|(i, e)| item(i, e)).collect(),
                    fin.get(sel).map(|e| entry_detail(e)).unwrap_or_default(),
                    "No recordings yet.",
                )
            }
            _ => (
                self.autorecs
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(PAGE)
                    .map(|(i, r)| DvrItem {
                        title: rule_name(r).into(),
                        subtitle: if r.enabled { "Enabled".into() } else { "Disabled".into() },
                        channel: r
                            .channel
                            .as_deref()
                            .and_then(|u| self.lineup.get(u))
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "Any channel".into())
                            .into(),
                        selected: i == sel,
                        ..Default::default()
                    })
                    .collect(),
                String::new(),
                "No series recordings.",
            ),
        };
        ui.set_dvr_tab(self.dvr.tab as i32);
        ui.set_dvr_items(model(items));
        ui.set_dvr_detail(detail.into());
        ui.set_dvr_empty(empty.into());
    }
}

fn rule_name(r: &tvh::AutorecRule) -> String {
    if !r.name.is_empty() {
        r.name.clone()
    } else {
        r.title.clone()
    }
}

fn entry_detail(e: &tvh::DvrEntry) -> String {
    let mut s = format!("{}  {}", fmt_day(e.start), fmt_range(e.start, e.stop));
    if let Some(d) = e.disp_description.as_deref().filter(|d| !d.is_empty()) {
        s.push_str(" — ");
        s.push_str(d);
    }
    s
}

fn size_label(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        Some(b) if b > 0 => format!("{} MB", b >> 20),
        _ => "Recorded".into(),
    }
}
