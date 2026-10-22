//! Modal dialogs: confirmations, record options, update prompts.

use input::RemoteKey;
use tvh::EpgEvent;

use crate::app::{fmt_range, model, App};
use crate::AppWindow;

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
}

pub struct Dialog {
    pub title: String,
    pub body: String,
    pub buttons: Vec<(String, Action)>,
    pub selected: usize,
    /// 0–1 while something runs; buttons are hidden and Back is ignored.
    pub progress: Option<f32>,
}

impl Dialog {
    pub fn new(title: impl Into<String>, body: impl Into<String>, buttons: Vec<(&str, Action)>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            buttons: buttons.into_iter().map(|(l, a)| (l.to_string(), a)).collect(),
            selected: 0,
            progress: None,
        }
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

    pub fn run_action(&mut self, action: Action) {
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
