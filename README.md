# CableBox

A retro cable-box shell for Linux set-top boxes. The device boots straight into a
full-screen, remote-controlled TV interface backed by a [Tvheadend](https://tvheadend.org)
server. There's no desktop underneath. CableBox *is* the session: live TV, guide, DVR,
Wi-Fi and system updates all happen on screen.

- **Live TV**: type channel numbers like a real box (`1 2 _`, `5.1`, auto-tune after a pause),
  CH+/CH−, Last, and an info banner with now/next and a progress bar.
- **Guide page**: a classic grid with 30-minute columns, live video in a preview window,
  record from any program, ±24h jumps, and a "now" marker.
- **DVR**: Upcoming / Recorded / Series tabs, playback with seek, cancel and delete.
- **Favorites & filters**: favorites plus Tvheadend channel tags, followed by CH+/− and the guide.
- **Settings**: Tvheadend server, Wi-Fi (NetworkManager), display mode and scale, remote,
  clock (12/24-hour, time zone, automatic network time or set by hand), system.
- **Updates**: a daily check through PackageKit/apt with a cable-box-style
  *"System Update Available — Install Now / Later"* prompt and a progress bar. It offers a reboot
  when one is needed, and a restart when CableBox updated itself.
- **Input**: keyboard, IR receivers, and HDMI-CEC (your TV's remote), all through one key map.

Runs on x86_64, Raspberry Pi 4/5 (arm64) and RISC-V boards (riscv64), on Debian 13 or
trixie-based Raspberry Pi OS.

## Architecture

```
crates/
  tvh/      Tvheadend HTTP API client (channels, tags, EPG, DVR; basic/digest auth)
  player/   libmpv wrapper; renders video through mpv's OpenGL render API
  input/    RemoteKey; keyboard map, evdev reader for IR/CEC remotes, control FIFO
  sysctl/   D-Bus: PackageKit (updates), NetworkManager (Wi-Fi), logind (power)
  app/      the `cablebox` binary: Slint UI (ui/*.slint) + controller (src/)
packaging/  systemd units, polkit rule, Debian maintainer scripts
tools/      mock_tvh.py, a fake Tvheadend server for development
```

Video is drawn by libmpv into the window's GL framebuffer *before* Slint paints the UI
(`set_rendering_notifier` → `BeforeRendering`). The UI overlays it, and the guide leaves a
transparent hole where the preview shows through (mpv's `video-margin-ratio-*` place the picture).
The UI is laid out on a 1280×720 canvas and scaled to the display.

All state lives in `app::App` on the UI thread. Every key goes through `App::on_key`, and
async work (HTTP, D-Bus) runs on Tokio and posts results back with `app::post`.

**Remotes**: IR receivers (rc-core) and HDMI-CEC adapters both appear as kernel input
devices. CableBox grabs "remote-like" devices exclusively (they have keys such as `KEY_OK`/`KEY_RED`
and no letter keys) and maps them in `input/src/remote.rs`. For CEC, `cablebox-cec.service` runs
`cec-ctl --playback` so the TV forwards its remote's buttons. That covers the Pi's built-in HDMI-CEC
and Pulse-Eight USB adapters (via `inputattach --pulse8-cec`). libcec isn't needed.

## Development

Build dependencies (Debian):

```sh
sudo apt install build-essential pkg-config libmpv-dev libfontconfig-dev libxkbcommon-dev \
                 libinput-dev libgbm-dev libudev-dev libdrm-dev ffmpeg
```

Rust 1.89+ is required (zbus 6). Slint is pinned to 1.14.1, and the workspace uses the
MSRV-aware resolver. The windowed `winit` backend is a default, development-only feature.
Packages are built with `--no-default-features --features kms`.

Run against the mock server in a window:

```sh
python3 tools/mock_tvh.py --port 19981 &          # 12 channels, 24h EPG, test-pattern streams
cat > /tmp/cb.toml <<'EOF'
[server]
url = "http://127.0.0.1:19981"
auth = "none"
[remote]
cec = false
EOF
CABLEBOX_CONFIG=/tmp/cb.toml SLINT_BACKEND=winit cargo run -p cablebox
```

Keyboard: `0-9` `.` channels · arrows / Enter / Esc · `G` guide · `I` info · `M` menu ·
`D` recordings · `R` record · `F` favorite · PgUp/PgDn channel · Backspace last channel ·
Space play/pause · `[` `]` rewind/ff · `-` `=` volume · F1–F4 red/green/yellow/blue.

Automation: set `CABLEBOX_CONTROL=/path/fifo`, then `echo "guide down down ok" > /path/fifo`.
Key names are in `RemoteKey::from_name`. This also works over SSH on a device.

Tests: `cargo test --workspace`. The read-only D-Bus probe is `cargo run -p sysctl --example probe`.

## Install from apt

Prebuilt packages for Debian 13 / trixie-based Raspberry Pi OS (amd64, arm64, riscv64) are published
in the ICLX apt repository, https://apt.icecubelabs.xyz/:

```sh
sudo curl -fsSLo /usr/share/keyrings/iclx-archive-keyring.gpg https://apt.icecubelabs.xyz/iclx-archive-keyring.gpg
printf 'Types: deb\nURIs: https://apt.icecubelabs.xyz/\nSuites: trixie\nComponents: main\nSigned-By: /usr/share/keyrings/iclx-archive-keyring.gpg\nArchitectures: amd64 arm64 riscv64\n' \
  | sudo tee /etc/apt/sources.list.d/iclx.sources
sudo apt update && sudo apt install cablebox
sudo systemctl enable --now cablebox cablebox-cec
```

Signing key fingerprint: `F46D BCC4 C6F9 98CC AD48  55B8 07F1 F103 2119 CCCD`.
New versions published there appear in CableBox's on-screen update prompt.

## Deploying to a box

To build the packages yourself:

```sh
cargo install cargo-deb
cargo deb -p cablebox                       # amd64, builds with --features kms
tools/build-cross.sh arm64                  # Raspberry Pi, cross-compiled (see below)
tools/build-cross.sh riscv64                # RISC-V boards
sudo apt install ./target/debian/cablebox_*.deb
sudo systemctl enable --now cablebox cablebox-cec
```

Cross builds need the target's compiler (`gcc-aarch64-linux-gnu` or `gcc-riscv64-linux-gnu`) and
its Rust target (`rustup target add aarch64-unknown-linux-gnu` or `riscv64gc-unknown-linux-gnu`).
They don't need root or a multiarch setup: `tools/cross-sysroot.sh ARCH` downloads the five libraries
CableBox links against from Debian trixie, verifying the archive signature and every hash, into
`target/ARCH-sysroot`. The result needs glibc 2.39+, so the target must run Debian 13 (or **Raspberry
Pi OS based on trixie**). Bookworm-based images won't work.

On RISC-V, GPU and video-decoder drivers vary a lot by board. CableBox needs working OpenGL (ES)
through Mesa for its UI, and mpv falls back to software decoding when there's no hardware decoder.
That can struggle above 720p on current RISC-V CPUs. Tvheadend's `webtv-h264-aac-mpegts` profile,
or a lower-resolution transcoding profile, helps.

The package creates a `cablebox` system user (groups video, render, input, audio), installs
the units and a polkit rule that lets that user install updates, manage Wi-Fi and reboot
without a password. The service takes over tty1. Its config lives in
`/var/lib/cablebox/.config/cablebox/config.toml`, and the first boot shows the setup screen.

Display: `Settings → Display`:
- **Screen** picks which output to use. Auto prefers a TV or monitor (HDMI, DisplayPort, …) over a
  laptop's built-in panel and falls back to the panel when nothing else is connected.
- **Resolution** picks one of that screen's modes.

Both apply on restart (through `SLINT_DRM_OUTPUT` and `SLINT_DRM_MODE`). If the chosen screen isn't
connected at startup, CableBox falls back to Auto instead of failing to start. Interface size
defaults to the screen height ÷ 720.

## Security

- **Tvheadend login:** prefer **Digest**. Basic login over `http://` sends the password readable
  on the network, and Settings marks it "Basic (unencrypted!)". `https://` servers are supported
  (rustls with the system CA store, certificates verified). Credentials are only ever sent to the
  configured server, never to external channel-logo hosts.
- **Settings PIN** (Settings → System): protects Settings, installing updates, reboot/power off,
  and cancelling or deleting recordings. Five wrong tries lock it for 60 s. Forgot it? Delete the
  `pin = ...` line from `/var/lib/cablebox/.config/cablebox/config.toml` over SSH.
- **Getting out of the TV interface** (Settings → System, behind the PIN):
  - **Remote access (SSH):** starts or stops `ssh.service` (install `openssh-server` first) and
    remembers the choice across reboots. The row shows the address to `ssh` to. Anyone on the
    network can try to log in, so use SSH keys or strong passwords.
  - **Exit to console:** stops CableBox and shows a login prompt on the TV. Log in with a USB
    keyboard; `sudo systemctl start cablebox` or a reboot brings the TV back.
  - **Locked out entirely** (no network, no remote): add `systemd.mask=cablebox.service` to the
    kernel command line (Pi: end of `cmdline.txt` on the SD card's boot partition; x86: press `e`
    in the boot menu) to boot to a normal login. Remove it afterwards.
  The polkit rule allows the `cablebox` user exactly these: start/stop `ssh.service` and start
  `getty@tty1.service`.
- **Clock** (Settings → Clock & time zone): time zone, network time and setting the clock go through
  `systemd-timedated` (polkit `org.freedesktop.timedate1.set-timezone`, `set-ntp`, `set-time` for
  the `cablebox` user). They're only offered when running as `cablebox.service`.
  `CABLEBOX_PRETEND_BOX=1` shows them in a dev window for UI work, but using them would change
  that machine.
- **Sandbox:** `cablebox.service` runs with systemd hardening (`systemd-analyze security`
  exposure 1.8). The `cablebox` user still needs the `input` group so it can read remotes.
- **Untrusted input limits:** API replies are capped at 64 MiB, logos at 2 MiB and 4096×4096
  pixels. mpv loads no user config or scripts, and ffmpeg may only open http/https/tcp/tls.
- **Updates:** only signed packages from configured repos (PackageKit `ONLY_TRUSTED`). The
  `cablebox` package depends on `iclx-archive-keyring`, so repo key rotations arrive as normal updates.
- **Dependencies:** `cargo audit` is clean. Accepted "unmaintained" notices are documented
  in `.cargo/audit.toml`.

## Status

Verified on an x86_64 desktop (winit backend, VAAPI decoding) against the mock server:
- live TV, channel entry, guide with preview, recording, DVR playback, settings, and the on-screen keyboard;
- a real PackageKit update check and prompt (install was not exercised).

Still to verify on hardware:
- The KMS/DRM session (`--features kms`) on a Pi 5 and an x86 box.
- mpv GL interop with hwdec on the Pi (`hwdec-current` is logged at debug level).
- CEC key mapping with real TVs.
- Whether `/sys/class/drm/*/modes` order matches `SLINT_DRM_MODE` indices.
- An end-to-end update install and reboot.

- The systemd sandbox with real DRM, audio and remotes. It was tested locally with the
  winit backend under the same seccomp/namespace settings. If something breaks on a box,
  `systemctl edit cablebox` can relax a single setting.

Not yet implemented:
- Tvheadend comet push updates (DVR state is polled every 60s).
- Reminders.
- Per-channel parental controls (the settings PIN exists).
