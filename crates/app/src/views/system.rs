//! System actions: the on-screen update prompt, Wi-Fi, server test, power.

use std::time::{Duration, Instant};

use sysctl::updates::{Restart, Update};

use crate::app::{post, App};
use crate::views::dialog::{Action, Dialog};

/// After "Later", ask again after this long.
const SNOOZE: Duration = Duration::from_secs(4 * 3600);

pub struct UpdateState {
    pub next_check: Instant,
    pub checking: bool,
    pub installing: bool,
    pub available: Vec<Update>,
    /// Updates were found and the prompt is waiting for a good moment.
    pub prompt_pending: bool,
}

impl UpdateState {
    pub fn new(first_check: Instant) -> Self {
        Self { next_check: first_check, checking: false, installing: false, available: Vec::new(), prompt_pending: false }
    }

    pub fn summary(&self) -> String {
        if self.installing {
            "Installing…".into()
        } else if self.checking {
            "Checking…".into()
        } else if !self.available.is_empty() {
            format!("{} available", self.available.len())
        } else {
            String::new()
        }
    }
}

impl App {
    pub fn tick_updates(&mut self, t: Instant) {
        let u = &self.updates;
        if self.cfg.updates.auto_check && !u.checking && !u.installing && t >= u.next_check {
            self.check_updates(false);
        }
        // Don't interrupt: wait until no dialog/keyboard is up and no recording is playing.
        if self.updates.prompt_pending && self.dialog.is_none() && self.osk.is_none() && self.playback.is_none() {
            self.updates.prompt_pending = false;
            self.update_prompt();
            self.render();
        }
    }

    /// `interactive`: the user asked from Settings, so report "up to date" and errors.
    pub fn check_updates(&mut self, interactive: bool) {
        if self.updates.checking || self.updates.installing {
            return;
        }
        self.updates.checking = true;
        if interactive {
            self.show_dialog(Dialog::busy("Checking for updates", "Contacting the update server…"));
        }
        self.spawn(sysctl::updates::check(true), move |a, r| {
            a.updates.checking = false;
            a.updates.next_check = Instant::now() + Duration::from_secs(a.cfg.updates.check_interval_hours.max(1) * 3600);
            if interactive {
                a.dialog = None;
            }
            match r {
                Ok(list) if list.is_empty() => {
                    a.updates.available.clear();
                    if interactive {
                        a.show_dialog(Dialog::new("System is up to date", "No updates are available.", vec![("OK", Action::Close)]));
                    }
                }
                Ok(list) => {
                    tracing::info!("{} updates available", list.len());
                    a.updates.available = list;
                    if interactive {
                        a.update_prompt();
                    } else {
                        a.updates.prompt_pending = true;
                    }
                }
                Err(e) => {
                    tracing::warn!("update check failed: {e}");
                    if interactive {
                        a.show_dialog(Dialog::new("Update check failed", e.to_string(), vec![("OK", Action::Close)]));
                    }
                }
            }
            a.render();
        });
    }

    fn update_prompt(&mut self) {
        let list = &self.updates.available;
        if list.is_empty() {
            return;
        }
        let important = list.iter().filter(|u| u.important).count();
        let mut names: Vec<&str> = list.iter().map(|u| u.name.as_str()).take(6).collect();
        if list.len() > names.len() {
            names.push("…");
        }
        let mut body = format!(
            "{} update{} ready to install",
            list.len(),
            if list.len() == 1 { " is" } else { "s are" }
        );
        if important > 0 {
            body.push_str(&format!(", including {important} security update{}", if important == 1 { "" } else { "s" }));
        }
        body.push_str(&format!(".\n\n{}\n\nTV may pause briefly while updating.", names.join(", ")));
        self.show_dialog(Dialog::new(
            "System Update Available",
            body,
            vec![("Install Now", Action::InstallUpdates), ("Later", Action::UpdateLater)],
        ));
    }

    pub fn postpone_updates(&mut self) {
        self.updates.prompt_pending = false;
        self.updates.next_check = Instant::now() + SNOOZE;
    }

    pub fn install_updates(&mut self) {
        let updates = self.updates.available.clone();
        if updates.is_empty() || self.updates.installing {
            return;
        }
        self.updates.installing = true;
        self.show_dialog(Dialog::busy("Installing updates", format!("Installing {} packages. Please don't unplug the box.", updates.len())));
        self.spawn(
            async move {
                let progress = |p: u32| {
                    post(move |a| {
                        if let Some(d) = a.dialog.as_mut().filter(|d| d.buttons.is_empty()) {
                            d.progress = Some(p as f32 / 100.0);
                            a.render();
                        }
                    })
                };
                sysctl::updates::install(&updates, &progress).await
            },
            |a, r| {
                a.updates.installing = false;
                a.dialog = None;
                match r {
                    Ok(restart) => {
                        a.updates.available.clear();
                        match restart {
                            Restart::System => a.show_dialog(Dialog::new(
                                "Updates installed",
                                "A restart is needed to finish updating.",
                                vec![("Restart Now", Action::Reboot), ("Later", Action::Close)],
                            )),
                            Restart::Session => a.show_dialog(Dialog::new(
                                "Updates installed",
                                "CableBox was updated and needs to restart.",
                                vec![("Restart CableBox", Action::RestartApp), ("Later", Action::Close)],
                            )),
                            Restart::None => a.toast("Updates installed"),
                        }
                    }
                    Err(e) => a.show_dialog(Dialog::new("Update failed", e.to_string(), vec![("OK", Action::Close)])),
                }
                a.render();
            },
        );
    }

    pub fn power(&mut self, reboot: bool) {
        self.save_config_now();
        self.show_dialog(Dialog::busy(if reboot { "Restarting…" } else { "Powering off…" }, ""));
        let fut = async move {
            if reboot {
                sysctl::power::reboot().await
            } else {
                sysctl::power::power_off().await
            }
        };
        self.spawn(fut, |a, r| {
            if let Err(e) = r {
                a.dialog = None;
                a.show_dialog(Dialog::new("Could not power off", e.to_string(), vec![("OK", Action::Close)]));
                a.render();
            }
        });
    }

    pub fn refresh_network(&mut self) {
        self.spawn(sysctl::network::status(), |a, r| {
            match r {
                Ok(st) => a.menu.net = Some(st),
                Err(e) => tracing::warn!("network status: {e}"),
            }
            a.render();
        });
    }

    pub fn scan_wifi(&mut self) {
        if self.menu.scanning {
            return;
        }
        self.menu.scanning = true;
        self.spawn(sysctl::network::scan(), |a, r| {
            a.menu.scanning = false;
            match r {
                Ok(list) => a.menu.wifi = list,
                Err(e) => a.toast(e.to_string()),
            }
            a.render();
        });
    }

    pub fn connect_wifi(&mut self, ssid: String, password: Option<String>) {
        self.show_dialog(Dialog::busy("Connecting", format!("Joining {ssid}…")));
        let fut = async move {
            sysctl::network::connect(&ssid, password.as_deref()).await?;
            // Give DHCP a moment before reporting status.
            tokio::time::sleep(Duration::from_secs(4)).await;
            sysctl::network::status().await
        };
        self.spawn(fut, |a, r| {
            a.dialog = None;
            match r {
                Ok(st) => {
                    a.toast(if st.connected { format!("Connected to {}", st.name) } else { "Connecting…".into() });
                    a.menu.net = Some(st);
                    a.scan_wifi();
                    if a.tvh.is_some() && a.lineup.is_empty() {
                        a.connect();
                    }
                }
                Err(e) => a.show_dialog(Dialog::new("Could not connect", e.to_string(), vec![("OK", Action::Close)])),
            }
            a.render();
        });
    }

    pub fn test_server(&mut self) {
        let client = match tvh::Client::new(&self.cfg.server.url, self.cfg.server.tvh_auth()) {
            Ok(c) => c,
            Err(e) => {
                self.menu.server_info = e.to_string();
                return;
            }
        };
        self.menu.server_info = "Testing…".into();
        let fut = async move {
            let info = client.server_info().await?;
            let n = client.channels().await?.len();
            Ok::<_, tvh::Error>((info, n))
        };
        self.spawn(fut, |a, r| {
            a.menu.server_info = match r {
                Ok((info, n)) => format!("OK · Tvheadend {} · {n} channels", info.sw_version),
                Err(tvh::Error::Unauthorized) => "Login failed".into(),
                Err(e) => format!("Failed: {e}"),
            };
            a.render();
        });
    }
}
