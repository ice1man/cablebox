//! Settings → System → Debug info: a corner overlay with what's playing, how it's shown,
//! and how it's arriving.

use player::VideoInfo;
use sysctl::network::Status;

/// Everything the overlay shows, gathered once a second.
#[derive(Debug, Clone, Default)]
pub struct Snapshot<'a> {
    pub video: Option<VideoInfo>,
    /// Output size in physical pixels.
    pub out: (u32, u32),
    pub refresh_hz: Option<f64>,
    /// Frames CableBox drew in the last second.
    pub drawn_fps: f64,
    /// `None` until NetworkManager has answered.
    pub net: Option<&'a Status>,
    /// Tvheadend server URL from the config.
    pub server: &'a str,
}

pub fn text(s: &Snapshot<'_>) -> String {
    let mut lines = Vec::new();
    lines.push(match &s.video {
        Some(v) => {
            let mut l = format!("IN   {}×{}", v.width, v.height);
            if let Some(f) = v.fps {
                l += &format!("  {f:.2} fps");
                // Filters (deinterlacing) changed the rate: 29.97 → 59.94.
                if let Some(vf) = v.vf_fps.filter(|vf| (vf / f - 1.0).abs() > 0.01) {
                    let why = if (vf / f - 2.0).abs() < 0.05 { " (deinterlaced)" } else { "" };
                    l += &format!(" → {vf:.2}{why}");
                }
            }
            l
        }
        None => "IN   no video".into(),
    });

    let mut out = format!("OUT  {}×{}", s.out.0, s.out.1);
    if let Some(hz) = s.refresh_hz {
        out += &format!(" @ {hz:.2} Hz");
    }
    out += &format!("  {:.1} fps drawn", s.drawn_fps);
    lines.push(out);

    lines.push(match s.net {
        None => "NET  checking…".into(),
        Some(n) if !n.connected && n.address.is_empty() => "NET  not connected".into(),
        Some(n) => {
            let mut l = format!("NET  {} \"{}\"", if n.wifi { "Wi-Fi" } else { "Wired" }, n.name);
            if let Some(sig) = n.signal {
                l += &format!(" {sig}%");
            }
            if !n.address.is_empty() {
                l += &format!("  {}", n.address);
            }
            l
        }
    });

    if let Some(v) = &s.video {
        let mut l = format!("STR  {}", server_host(s.server));
        if let Some(bps) = v.cache_speed {
            l += &format!("  {:.1} Mb/s", bps as f64 * 8.0 / 1e6);
        }
        if let Some(b) = v.buffered {
            l += &format!("  buffer {b:.1} s");
        }
        lines.push(l);
    }
    lines.join("\n")
}

/// "http://user@10.0.0.5:9981/tvh" → "10.0.0.5:9981".
fn server_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or(rest);
    host.rsplit_once('@').map_or(host, |(_, h)| h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(fps: f64, vf: f64) -> VideoInfo {
        VideoInfo {
            width: 1920,
            height: 1080,
            fps: Some(fps),
            vf_fps: Some(vf),
            cache_speed: Some(1_050_000),
            buffered: Some(3.24),
        }
    }

    fn wifi() -> Status {
        Status { connected: true, name: "home".into(), address: "192.168.1.20".into(), wifi: true, signal: Some(72) }
    }

    #[test]
    fn full_overlay() {
        let net = wifi();
        let s = Snapshot {
            video: Some(video(29.97, 59.94)),
            out: (1920, 1080),
            refresh_hz: Some(60.0),
            drawn_fps: 59.84,
            net: Some(&net),
            server: "http://10.50.50.235:9981",
        };
        assert_eq!(
            text(&s),
            "IN   1920×1080  29.97 fps → 59.94 (deinterlaced)\n\
             OUT  1920×1080 @ 60.00 Hz  59.8 fps drawn\n\
             NET  Wi-Fi \"home\" 72%  192.168.1.20\n\
             STR  10.50.50.235:9981  8.4 Mb/s  buffer 3.2 s"
        );
    }

    #[test]
    fn progressive_video_has_no_arrow() {
        let s = Snapshot { video: Some(video(59.94, 59.94)), out: (1280, 720), ..Default::default() };
        assert!(text(&s).starts_with("IN   1920×1080  59.94 fps\nOUT  1280×720  0.0 fps drawn\n"));
    }

    #[test]
    fn no_video_and_network_states() {
        let wired = Status { connected: true, name: "Wired connection 1".into(), address: "10.0.0.5".into(), ..Default::default() };
        let s = Snapshot { net: Some(&wired), ..Default::default() };
        let t = text(&s);
        assert!(t.starts_with("IN   no video\n"));
        assert!(t.ends_with("NET  Wired \"Wired connection 1\"  10.0.0.5"));
        assert!(!t.contains("STR"));
        let off = Status::default();
        assert!(text(&Snapshot { net: Some(&off), ..Default::default() }).ends_with("NET  not connected"));
        assert!(text(&Snapshot::default()).ends_with("NET  checking…"));
    }

    #[test]
    fn server_host_strips_scheme_user_and_path() {
        assert_eq!(server_host("http://10.0.0.5:9981"), "10.0.0.5:9981");
        assert_eq!(server_host("https://me@tv.example:9981/tvh"), "tv.example:9981");
        assert_eq!(server_host("tv:9981"), "tv:9981");
    }
}
