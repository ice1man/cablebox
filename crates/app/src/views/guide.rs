//! Grid guide screen.

use std::time::Instant;

use input::RemoteKey;

use crate::app::{fmt_day, fmt_range, fmt_time, model, now, App};
use crate::entry::Entry;
use crate::guide::{place, spans, GuideNav, SLOT, SPAN};
use crate::{AppWindow, GuideCell, GuideDetail, GuideRow, Screen};

impl App {
    pub fn open_guide(&mut self) {
        let rows = self.lineup.visible_len();
        let row = self.tuned.as_deref().and_then(|u| self.lineup.visible_position(u)).unwrap_or(0);
        let visible = self.guide.visible.max(1);
        self.guide = GuideNav::open(row, rows, now());
        self.guide.set_visible(visible, rows);
        self.entry.cancel();
        self.screen = Screen::Guide;
        self.guide_changed();
        self.apply_video_rect();
    }

    /// Selected channel uuid.
    fn guide_channel(&self) -> Option<String> {
        self.lineup.visible_at(self.guide.row).map(|c| c.uuid.clone())
    }

    /// After any move: snap the selection and make sure EPG for the window is loading.
    fn guide_changed(&mut self) {
        if let Some(uuid) = self.guide_channel() {
            let evs = self.epg.events(&uuid).to_vec();
            self.guide.snap(&evs);
        }
        let (from, to) = (self.guide.start, self.guide.end());
        // Prefetch one window ahead so paging right is instant.
        self.load_epg(from, to + SPAN);
    }

    pub fn guide_jump_to(&mut self, uuid: &str) {
        if self.lineup.visible_position(uuid).is_none() {
            self.lineup.set_filter(crate::lineup::Filter::All);
        }
        if let Some(pos) = self.lineup.visible_position(uuid) {
            let rows = self.lineup.visible_len();
            let delta = pos as i64 - self.guide.row as i64;
            self.guide.move_rows(delta, rows);
            self.guide_changed();
        }
    }

    pub fn guide_key(&mut self, k: RemoteKey) {
        let rows = self.lineup.visible_len();
        let t = Instant::now();
        let events = |a: &App| a.guide_channel().map(|u| a.epg.events(&u).to_vec()).unwrap_or_default();
        match k {
            RemoteKey::Digit(d) => {
                if let Entry::Commit(n) = self.entry.digit(d, t) {
                    self.guide_commit(n);
                }
                return;
            }
            RemoteKey::Dot => {
                self.entry.dot(t);
                return;
            }
            RemoteKey::Ok if self.entry.is_active() => {
                if let Some(n) = self.entry.commit() {
                    self.guide_commit(n);
                }
                return;
            }
            RemoteKey::Back if self.entry.is_active() => {
                self.entry.backspace(t);
                return;
            }
            RemoteKey::Up => self.guide.move_rows(-1, rows),
            RemoteKey::Down => self.guide.move_rows(1, rows),
            RemoteKey::ChannelUp | RemoteKey::PageUp => self.guide.page(-1, rows),
            RemoteKey::ChannelDown | RemoteKey::PageDown => self.guide.page(1, rows),
            RemoteKey::Right => {
                let evs = events(self);
                self.guide.right(&evs);
            }
            RemoteKey::Left => {
                let evs = events(self);
                self.guide.left(&evs, now());
            }
            RemoteKey::Red => self.guide.days(-1, now()),
            RemoteKey::Green => self.guide.days(1, now()),
            RemoteKey::FastForward => {
                self.guide.start += SPAN;
                self.guide.anchor = self.guide.start;
            }
            RemoteKey::Rewind => {
                let t = now();
                self.guide.start = crate::guide::align(t);
                self.guide.anchor = t;
            }
            RemoteKey::Yellow => {
                if let Some(uuid) = self.guide_channel() {
                    let fav = self.lineup.toggle_favorite(&uuid);
                    self.save_config_soon();
                    self.toast(if fav { "Added to Favorites" } else { "Removed from Favorites" });
                    // The favorites view may have shrunk.
                    self.guide.move_rows(0, self.lineup.visible_len());
                }
            }
            RemoteKey::Blue => {
                let cur = self.guide_channel();
                self.cycle_filter();
                let rows = self.lineup.visible_len();
                let row = cur.and_then(|u| self.lineup.visible_position(&u)).unwrap_or(0);
                self.guide = GuideNav { row, top: 0, ..self.guide.clone() };
                self.guide.move_rows(0, rows);
            }
            RemoteKey::Ok => self.guide_select(),
            RemoteKey::Record => {
                let evs = events(self);
                if let Some(ev) = self.guide.selected(&evs).and_then(|s| s.event).cloned() {
                    self.record_dialog(&ev);
                }
            }
            RemoteKey::Back => self.go_live(),
            _ => return,
        }
        self.guide_changed();
    }

    fn guide_commit(&mut self, n: tvh::ChannelNumber) {
        match self.lineup.by_number(n).map(|c| c.uuid.clone()) {
            Some(uuid) => self.guide_jump_to(&uuid),
            None => self.toast(format!("Channel {n} not available")),
        }
    }

    /// OK: watch what's on now, or offer to record something later.
    fn guide_select(&mut self) {
        let Some(uuid) = self.guide_channel() else { return };
        let evs = self.epg.events(&uuid).to_vec();
        let t = now();
        let sel = self.guide.selected(&evs);
        match sel.and_then(|s| s.event) {
            Some(ev) if ev.start > t => {
                let ev = ev.clone();
                self.record_dialog(&ev);
            }
            _ if sel.is_some_and(|s| s.start > t) => self.toast("No program information"),
            _ => {
                self.go_live();
                if self.tuned.as_deref() != Some(&uuid) {
                    self.tune(&uuid);
                }
            }
        }
    }

    pub fn render_guide(&self, ui: &AppWindow) {
        let t = now();
        let nav = &self.guide;
        let from = nav.start;
        let end = nav.end();

        let rows: Vec<GuideRow> = (nav.top..(nav.top + nav.visible).min(self.lineup.visible_len()))
            .filter_map(|pos| self.lineup.visible_at(pos).map(|c| (pos, c)))
            .map(|(pos, ch)| {
                let evs = self.epg.events(&ch.uuid);
                let selected = (pos == nav.row).then(|| nav.selected(evs)).flatten();
                let cells: Vec<GuideCell> = spans(evs, from, end)
                    .iter()
                    .map(|sp| {
                        let (x, w) = place(sp, from);
                        let is_sel = selected.is_some_and(|s| s.start == sp.start && s.stop == sp.stop);
                        GuideCell {
                            title: match sp.event {
                                Some(e) => e.title.clone(),
                                // The channel sends no guide data for this time.
                                None if self.epg.missing_blocks(sp.start, sp.stop).is_empty() => "No info".into(),
                                None => "Loading…".into(),
                            }
                            .into(),
                            x,
                            w,
                            selected: is_sel,
                            recording: sp.event.is_some_and(|e| e.dvr_uuid.is_some() || self.dvr_for_event(e).is_some()),
                            airing: sp.start <= t && t < sp.stop,
                            more_left: sp.start < from,
                            more_right: sp.stop > end,
                        }
                    })
                    .collect();
                GuideRow {
                    number: ch.number.to_string().into(),
                    name: ch.name.clone().into(),
                    logo: self.logos.get(&ch.uuid).cloned().unwrap_or_default(),
                    favorite: self.lineup.is_favorite(&ch.uuid),
                    tuned: self.tuned.as_deref() == Some(ch.uuid.as_str()),
                    cells: model(cells),
                }
            })
            .collect();
        ui.set_guide_rows(model(rows));

        let times: Vec<slint::SharedString> = (0..SPAN / SLOT).map(|i| fmt_time(from + i * SLOT).into()).collect();
        ui.set_guide_times(model(times));
        ui.set_guide_date(if from <= t && t < end { "Today".into() } else { fmt_day(from).into() });
        ui.set_guide_filter(self.lineup.filter_label().into());
        ui.set_guide_now_x(if (from..end).contains(&t) { (t - from) as f32 / SPAN as f32 } else { -1.0 });

        let detail = self
            .lineup
            .visible_at(nav.row)
            .map(|ch| {
                let evs = self.epg.events(&ch.uuid);
                let channel = format!("{} {}", ch.number, ch.name);
                match nav.selected(evs) {
                    Some(sp) => match sp.event {
                        Some(e) => GuideDetail {
                            title: e.title.clone().into(),
                            episode: [e.episode_onscreen.as_deref(), e.subtitle.as_deref()]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join("  ·  ")
                                .into(),
                            when: format!("{}  {}", fmt_day(e.start), fmt_range(e.start, e.stop)).into(),
                            channel: channel.into(),
                            synopsis: e.synopsis().to_string().into(),
                            recording: match self.dvr_for_event(e) {
                                Some(d) if d.is_recording() => "● RECORDING".into(),
                                Some(_) => "● SCHEDULED".into(),
                                None if e.dvr_uuid.is_some() => "● SCHEDULED".into(),
                                None => Default::default(),
                            },
                            // What OK does; mirrors guide_select() and record_dialog().
                            hint: if e.start <= t {
                                "Watch  ·  ● Record".into()
                            } else if self.dvr_for_event(e).is_some() {
                                "Cancel recording…".into()
                            } else {
                                "Record…".into()
                            },
                        },
                        None => GuideDetail {
                            title: "No program information".into(),
                            when: fmt_range(sp.start, sp.stop).into(),
                            channel: channel.into(),
                            synopsis: "This channel isn't sending guide data for this time.".into(),
                            hint: if sp.start <= t { "Watch".into() } else { Default::default() },
                            ..Default::default()
                        },
                    },
                    None => GuideDetail { channel: channel.into(), ..Default::default() },
                }
            })
            .unwrap_or_else(|| GuideDetail {
                title: "No channels".into(),
                synopsis: "Press Blue to change the channel filter.".into(),
                ..Default::default()
            });
        ui.set_guide_detail(detail);
    }
}
