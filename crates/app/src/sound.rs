//! Settings → Sound: which outputs to offer, and the mpv options for each setting.

use player::AudioDevice;

use crate::config::{AudioFormat, Sound};

/// Outputs worth offering, as (mpv name, label), after "Auto". mpv lists every ALSA plugin
/// and alias; keep the real outputs: sound-server sinks (PipeWire, else PulseAudio, which
/// list the same sinks) and ALSA's HDMI, S/PDIF and per-card default devices.
pub fn choices(devices: &[AudioDevice]) -> Vec<(String, String)> {
    let server = if devices.iter().any(|d| d.name.starts_with("pipewire/")) { "pipewire/" } else { "pulse/" };
    devices
        .iter()
        .filter(|d| {
            d.name.starts_with(server)
                || ["alsa/hdmi:", "alsa/iec958:", "alsa/sysdefault:"].iter().any(|p| d.name.starts_with(p))
        })
        .map(|d| (d.name.clone(), label(d)))
        .collect()
}

/// "HD-Audio Generic, HDMI 0/HDMI Audio Output" → "HDMI 0 · HD-Audio Generic".
pub fn label(d: &AudioDevice) -> String {
    if !d.name.starts_with("alsa/") {
        return d.description.clone();
    }
    let head = d.description.split('/').next().unwrap_or(&d.description);
    match head.split_once(", ") {
        Some((card, dev)) => format!("{dev} · {card}"),
        None => head.to_string(),
    }
}

/// Night mode: a gentle compressor so loud ads and effects come down and dialogue comes up.
const NIGHT_FILTER: &str = "lavfi=[acompressor=threshold=-24dB:ratio=4:attack=5:release=250:makeup=8dB]";

/// A bitstream can only go straight to HDMI or S/PDIF hardware. Sound servers (PipeWire,
/// PulseAudio) and analog outputs accept it and then stall playback.
pub fn passthrough_possible(device: &str) -> bool {
    device.starts_with("alsa/hdmi:") || device.starts_with("alsa/iec958:")
}

/// mpv properties for the Sound settings. `device` is the mpv name to use ("auto" when the
/// saved one isn't connected). Passthrough on an output that can't take it decodes as
/// Surround instead.
pub fn mpv_options(s: &Sound, device: &str) -> Vec<(&'static str, String)> {
    let passthrough = s.format == AudioFormat::Passthrough && passthrough_possible(device);
    let night = s.night_mode && !passthrough;
    vec![
        ("audio-device", device.to_string()),
        ("audio-channels", if s.format == AudioFormat::Stereo { "stereo" } else { "auto-safe" }.into()),
        ("audio-spdif", if passthrough { "ac3,eac3,dts" } else { "" }.into()),
        ("af", if night { NIGHT_FILTER } else { "" }.into()),
        // AC-3 carries the broadcaster's own compression; apply it in night mode.
        ("ad-lavc-ac3drc", if night { "1" } else { "0" }.into()),
        ("volume-max", if s.volume_boost { "130" } else { "100" }.into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, description: &str) -> AudioDevice {
        AudioDevice { name: name.into(), description: description.into() }
    }

    #[test]
    fn offers_real_outputs_only() {
        let list = [
            dev("auto", "Autoselect device"),
            dev("pipewire", "Default (pipewire)"),
            dev("pipewire/alsa_output.pci-0000_c1_00.6.analog-stereo", "Ryzen HD Audio Controller Analog Stereo"),
            dev("pulse/alsa_output.pci-0000_c1_00.6.analog-stereo", "Ryzen HD Audio Controller Analog Stereo"),
            dev("alsa", "Default (alsa)"),
            dev("alsa/speexrate", "Rate Converter Plugin Using Speex Resampler"),
            dev("alsa/plughw:CARD=Generic,DEV=3", "HD-Audio Generic, HDMI 0/Hardware device with all software conversions"),
            dev("alsa/hdmi:CARD=Generic,DEV=0", "HD-Audio Generic, HDMI 0/HDMI Audio Output"),
            dev("alsa/dmix:CARD=Generic,DEV=3", "HD-Audio Generic, HDMI 0/Direct sample mixing device"),
            dev("alsa/sysdefault:CARD=Generic_1", "HD-Audio Generic, ALC295 Analog/Default Audio Device"),
            dev("alsa/surround51:CARD=Generic_1,DEV=0", "HD-Audio Generic, ALC295 Analog/5.1 Surround output"),
            dev("jack", "Default (jack)"),
        ];
        let labels: Vec<_> = choices(&list).into_iter().map(|(_, l)| l).collect();
        assert_eq!(
            labels,
            ["Ryzen HD Audio Controller Analog Stereo", "HDMI 0 · HD-Audio Generic", "ALC295 Analog · HD-Audio Generic"]
        );
        // Without PipeWire, PulseAudio's sinks are offered instead.
        let pulse = [dev("pulse/x", "Speakers"), dev("alsa/hdmi:CARD=vc4hdmi0,DEV=0", "vc4-hdmi-0, MAI PCM i2s-hifi-0/HDMI Audio Output")];
        assert_eq!(choices(&pulse)[1].1, "MAI PCM i2s-hifi-0 · vc4-hdmi-0");
        assert_eq!(choices(&pulse)[0].1, "Speakers");
    }

    #[test]
    fn options_per_setting() {
        let get = |s: &Sound, k: &str| mpv_options(s, "auto").into_iter().find(|(n, _)| *n == k).unwrap().1;
        let mut s = Sound::default();
        assert_eq!((get(&s, "audio-channels"), get(&s, "audio-spdif")), ("stereo".into(), String::new()));
        assert_eq!(get(&s, "volume-max"), "100");
        s.format = AudioFormat::Surround;
        s.night_mode = true;
        s.volume_boost = true;
        assert_eq!(get(&s, "audio-channels"), "auto-safe");
        assert!(get(&s, "af").contains("acompressor"));
        assert_eq!((get(&s, "ad-lavc-ac3drc"), get(&s, "volume-max")), ("1".into(), "130".into()));
        s.format = AudioFormat::Passthrough;
        // Not possible on Auto or a sound server: decoded like Surround, night mode applies.
        assert_eq!((get(&s, "audio-spdif"), get(&s, "audio-channels")), (String::new(), "auto-safe".into()));
        assert!(get(&s, "af").contains("acompressor"));
        // On HDMI the receiver decodes: no filters.
        let hdmi = "alsa/hdmi:CARD=Generic,DEV=0";
        let opt = |k: &str| mpv_options(&s, hdmi).into_iter().find(|(n, _)| *n == k).unwrap().1;
        assert_eq!((opt("audio-spdif"), opt("af"), opt("ad-lavc-ac3drc")), ("ac3,eac3,dts".into(), String::new(), "0".into()));
        assert_eq!(opt("audio-device"), hdmi);
        assert!(passthrough_possible("alsa/iec958:CARD=PCH,DEV=0"));
        assert!(!passthrough_possible("pipewire/alsa_output.pci-0000_c1_00.1.hdmi-stereo"));
        assert!(!passthrough_possible("auto"));
    }
}
