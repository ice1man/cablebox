//! Modal dialogs: confirmations, record options, update prompts.

use input::RemoteKey;
use tvh::EpgEvent;

use std::time::Instant;

use crate::app::{fmt_range, model, App};
use crate::pin::{self, Check};
use crate::{AppWindow, Screen};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Close,
    RecordOnce(u64),
    RecordSeries(u64),
    CancelDvr(String),
    RemoveRecording(String),
    DeleteAutorec(String),
    PlayRecording(String),
    InstallUpdates,
    UpdateLater,
    Reboot,
    PowerOff,
    RestartApp,
    OpenSettings,
    SetPin,
    RemovePin,
    /// Start (true) or stop the SSH server.
    SetSsh(bool),
    /// Stop CableBox and show a login prompt on the TV.
    ExitToConsole,
}

/// What a PIN prompt is for.
#[derive(Debug, Clone, PartialEq)]
pub enum PinPurpose {
    /// Check the PIN, then run this action.
    Unlock(Box<Action>),
    /// Choosing a new PIN: first entry…
    New,
    /// …then typing it again.
    Confirm(String),
    /// Not a PIN: typing the date and time ("YYYYMMDDHHMM") to set the system clock.
    DateTime,
}

impl PinPurpose {
    /// Digits to collect before acting.
    fn len(&self) -> usize {
        match self {
            PinPurpose::DateTime => crate::clock::DATETIME_DIGITS,
            _ => pin::LEN,
        }
    }
}

pub struct PinPrompt {
    pub entered: String,
    pub purpose: PinPurpose,
    pub error: String,
}

pub struct Dialog {
    pub title: String,
    pub body: String,
    pub buttons: Vec<(String, Action)>,
    pub selected: usize,
    /// 0–1 while something runs; buttons are hidden and Back is ignored.
    pub progress: Option<f32>,
    /// Set when this dialog is a PIN prompt (digits typed with the remote).
    pub pin: Option<PinPrompt>,
}

impl Dialog {
    pub fn new(title: impl Into<String>, body: impl Into<String>, buttons: Vec<(&str, Action)>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            buttons: buttons.into_iter().map(|(l, a)| (l.to_string(), a)).collect(),
            selected: 0,
            progress: None,
            pin: None,
        }
    }

    fn pin(purpose: PinPurpose) -> Self {
        let mut d = Self::new("", "", vec![]);
        d.pin = Some(PinPrompt { entered: String::new(), purpose, error: String::new() });
        d.sync_pin();
        d
    }

    /// Updates the title and body from the PIN prompt's state.
    fn sync_pin(&mut self) {
        let Some(p) = &self.pin else { return };
        self.title = match p.purpose {
            PinPurpose::Unlock(_) => "Enter PIN",
            PinPurpose::New => "Choose a 4-digit PIN",
            PinPurpose::Confirm(_) => "Enter the new PIN again",
            PinPurpose::DateTime => "Set date & time (24-hour)",
        }
        .into();
        self.body = match p.purpose {
            PinPurpose::DateTime => {
                let now = crate::app::now();
                let reference = chrono::TimeZone::timestamp_opt(&chrono::Local, now, 0)
                    .single()
                    .map(|d| d.format("Currently %Y-%m-%d  %H:%M").to_string())
                    .unwrap_or_default();
                format!("{}\n\n{}\n{}", crate::clock::datetime_mask(&p.entered), reference, p.error)
            }
            _ => format!("{}\n\n{}", pin::mask(&p.entered), p.error),
        };
    }

    pub fn busy(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self { progress: Some(0.0), ..Self::new(title, body, vec![]) }
    }
}

impl App {
    pub fn show_dialog(&mut self, d: Dialog) {
        self.dialog = Some(d);
    }

    pub fn dialog_key(&mut self, k: RemoteKey) {
        if self.dialog.as_ref().is_some_and(|d| d.pin.is_some()) {
            self.pin_key(k);
            return;
        }
        let Some(d) = &mut self.dialog else { return };
        if d.progress.is_some() && d.buttons.is_empty() {
            return; // busy: wait for completion
        }
        let n = d.buttons.len().max(1);
        match k {
            RemoteKey::Left | RemoteKey::Up => d.selected = (d.selected + n - 1) % n,
            RemoteKey::Right | RemoteKey::Down => d.selected = (d.selected + 1) % n,
            RemoteKey::Back | RemoteKey::Exit => {
                let last = d.buttons.last().map(|b| b.1.clone());
                self.dialog = None;
                // Back means "the safe choice", which we always put last.
                if let Some(a @ (Action::UpdateLater | Action::Close)) = last {
                    self.run_action(a);
                }
            }
            RemoteKey::Ok => {
                let action = d.buttons.get(d.selected).map(|b| b.1.clone()).unwrap_or(Action::Close);
                self.dialog = None;
                self.run_action(action);
            }
            _ => {}
        }
    }

    /// Actions that need the settings PIN (when one is set).
    fn needs_pin(&self, action: &Action) -> bool {
        let gated = matches!(
            action,
            Action::OpenSettings
                | Action::InstallUpdates
                | Action::CancelDvr(_)
                | Action::RemoveRecording(_)
                | Action::DeleteAutorec(_)
                | Action::Reboot
                | Action::PowerOff
                | Action::SetSsh(_)
                | Action::ExitToConsole
        );
        // Inside Settings the PIN was already entered once.
        let unlocked = self.settings_unlocked && self.screen == Screen::Menu;
        gated && self.cfg.ui.pin.is_some() && !unlocked
    }

    /// Runs an action, asking for the PIN first if it's protected.
    pub fn run_action(&mut self, action: Action) {
        if self.needs_pin(&action) {
            self.ask_pin(PinPurpose::Unlock(Box::new(action)));
        } else {
            self.perform(action);
        }
    }

    pub fn ask_datetime(&mut self) {
        self.show_dialog(Dialog::pin(PinPurpose::DateTime));
    }

    pub fn ask_pin(&mut self, purpose: PinPurpose) {
        let mut d = Dialog::pin(purpose);
        if let Some(left) = self.pin_gate.locked(Instant::now()) {
            d.pin.as_mut().unwrap().error = format!("Too many attempts. Try again in {} s.", left.as_secs() + 1);
            d.sync_pin();
        }
        self.show_dialog(d);
    }

    fn pin_key(&mut self, k: RemoteKey) {
        let Some(d) = &mut self.dialog else { return };
        let Some(p) = &mut d.pin else { return };
        match k {
            RemoteKey::Digit(n) if p.entered.len() < p.purpose.len() => {
                p.entered.push(char::from(b'0' + n));
                p.error.clear();
            }
            RemoteKey::Back if !p.entered.is_empty() => {
                p.entered.pop();
            }
            RemoteKey::Back | RemoteKey::Exit => {
                self.dialog = None;
                return;
            }
            _ => return,
        }
        if p.entered.len() < p.purpose.len() {
            d.sync_pin();
            return;
        }

        let entered = std::mem::take(&mut p.entered);
        match p.purpose.clone() {
            PinPurpose::Unlock(action) => {
                let expected = self.cfg.ui.pin.clone().unwrap_or_default();
                match self.pin_gate.check(&expected, &entered, Instant::now()) {
                    Check::Ok => {
                        self.dialog = None;
                        self.perform(*action);
                        return;
                    }
                    Check::Wrong { remaining } => {
                        let tries = if remaining == 1 { "try" } else { "tries" };
                        p.error = format!("Wrong PIN. {remaining} {tries} left.");
                    }
                    Check::Locked(left) => {
                        p.error = format!("Too many attempts. Try again in {} s.", left.as_secs());
                    }
                }
            }
            PinPurpose::DateTime => {
                match crate::clock::parse_datetime(&entered).and_then(crate::clock::local_to_unix_usec) {
                    Some(usec) => {
                        self.dialog = None;
                        self.set_system_time(usec);
                        return;
                    }
                    None => p.error = "That isn't a valid date and time. Try again.".into(),
                }
            }
            PinPurpose::New => p.purpose = PinPurpose::Confirm(entered),
            PinPurpose::Confirm(first) if first == entered => {
                self.dialog = None;
                self.cfg.ui.pin = Some(first);
                self.save_config_now();
                self.toast("Settings PIN set");
                return;
            }
            PinPurpose::Confirm(_) => {
                p.purpose = PinPurpose::New;
                p.error = "The PINs didn't match. Start again.".into();
            }
        }
        d.sync_pin();
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Close => {}
            Action::RecordOnce(id) => self.record(id, false),
            Action::RecordSeries(id) => self.record(id, true),
            Action::CancelDvr(uuid) => self.dvr_op(uuid, "Recording cancelled", |c, u| {
                Box::pin(async move { c.dvr_cancel(&u).await })
            }),
            Action::RemoveRecording(uuid) => self.dvr_op(uuid, "Recording deleted", |c, u| {
                Box::pin(async move { c.dvr_remove(&u).await })
            }),
            Action::DeleteAutorec(uuid) => self.dvr_op(uuid, "Series recording removed", |c, u| {
                Box::pin(async move { c.idnode_delete(&u).await })
            }),
            Action::PlayRecording(uuid) => {
                if let Some(e) = self.dvr_finished.iter().find(|e| e.uuid == uuid).cloned() {
                    self.play_recording(e);
                }
            }
            Action::InstallUpdates => self.install_updates(),
            Action::UpdateLater => self.postpone_updates(),
            Action::Reboot => self.power(true),
            Action::PowerOff => self.power(false),
            Action::RestartApp => {
                self.save_config_now();
                // systemd (Restart=always) brings us back up.
                std::process::exit(0);
            }
            Action::OpenSettings => {
                self.settings_unlocked = true;
                self.open_settings();
            }
            Action::SetPin => self.ask_pin(PinPurpose::New),
            Action::RemovePin => {
                self.cfg.ui.pin = None;
                self.save_config_now();
                self.toast("Settings PIN removed");
            }
            Action::SetSsh(on) => self.set_ssh(on),
            Action::ExitToConsole => self.exit_to_console(),
        }
    }

    /// Offers once/series recording, or cancelling if already scheduled.
    pub fn record_dialog(&mut self, ev: &EpgEvent) {
        let when = fmt_range(ev.start, ev.stop);
        if let Some(d) = self.dvr_for_event(ev) {
            let uuid = d.uuid.clone();
            self.show_dialog(Dialog::new(
                ev.title.clone(),
                format!("{when}\nThis program is set to record."),
                vec![("Cancel Recording", Action::CancelDvr(uuid)), ("Keep", Action::Close)],
            ));
            return;
        }
        let mut buttons = vec![("Record", Action::RecordOnce(ev.event_id))];
        if ev.serieslink_uri.is_some() || ev.episode_onscreen.is_some() {
            buttons.push(("Record Series", Action::RecordSeries(ev.event_id)));
        }
        buttons.push(("Close", Action::Close));
        self.show_dialog(Dialog::new(ev.title.clone(), when, buttons));
    }

    fn record(&mut self, event_id: u64, series: bool) {
        self.spawn_tvh(
            move |c| {
                Box::pin(async move {
                    if series {
                        c.record_series(event_id).await
                    } else {
                        c.record_event(event_id).await
                    }
                })
            },
            move |a, r| {
                match r {
                    Ok(()) => {
                        a.epg.set_dvr(event_id, Some(String::new()));
                        a.toast(if series { "Series recording set" } else { "Recording scheduled" });
                    }
                    Err(e) => a.toast(format!("Could not schedule: {e}")),
                }
                a.refresh_dvr();
                a.render();
            },
        );
    }

    /// Runs a DVR mutation, then refreshes lists and guide markers.
    fn dvr_op(
        &mut self,
        uuid: String,
        ok_msg: &'static str,
        op: impl FnOnce(tvh::Client, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = tvh::Result<()>> + Send>>
            + 'static,
    ) {
        let cleared: Vec<u64> = self
            .epg_events_for_dvr(&uuid)
            .into_iter()
            .collect();
        self.spawn_tvh(
            move |c| op(c, uuid),
            move |a, r| {
                match r {
                    Ok(()) => {
                        for id in cleared {
                            a.epg.set_dvr(id, None);
                        }
                        a.toast(ok_msg);
                    }
                    Err(e) => a.toast(format!("Failed: {e}")),
                }
                a.refresh_dvr();
                a.render();
            },
        );
    }

    fn epg_events_for_dvr(&self, dvr_uuid: &str) -> Vec<u64> {
        self.dvr_upcoming
            .iter()
            .filter(|d| d.uuid == dvr_uuid)
            .filter_map(|d| d.broadcast)
            .collect()
    }

    pub fn render_dialog(&self, ui: &AppWindow) {
        ui.set_dialog_visible(self.dialog.is_some());
        let Some(d) = &self.dialog else { return };
        ui.set_dialog_title(d.title.clone().into());
        ui.set_dialog_body(d.body.clone().into());
        ui.set_dialog_buttons(model(d.buttons.iter().map(|b| b.0.clone().into()).collect()));
        ui.set_dialog_selected(d.selected as i32);
        ui.set_dialog_progress(d.progress.unwrap_or(-1.0));
    }
}
