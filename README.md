# CableBox

A retro cable-box shell for Linux set-top boxes. The device boots straight into a
full-screen, remote-controlled TV interface backed by a [Tvheadend](https://tvheadend.org)
server. There's no desktop underneath. CableBox *is* the session: live TV, guide, DVR,
Wi-Fi and system updates all happen on screen.

- **Live TV**: type channel numbers like a real box (`1 2 _`, `5.1`, auto-tune after a pause),
  CH+/CH−, Last, and an info banner with now/next and a progress bar.
- **Guide page**: a classic grid with 30-minute columns, live video in a preview window,
  a "now" marker and ±24h jumps. OK on a later program offers Record / Record Series (the info
  panel shows what OK will do), and time a channel sends no guide data for shows as "No info".
- **DVR**: Upcoming / Recorded / Series tabs, playback with seek, cancel and delete.
- **Favorites & filters**: favorites plus Tvheadend channel tags, followed by CH+/− and the guide.
- **Settings**: Tvheadend server, Wi-Fi (NetworkManager), screen and resolution, remote,
  clock (12/24-hour, time zone, automatic network time or set by hand), system, settings PIN.
- **Picture & sound**: hardware acceleration (GPU decoding, with a clear "Hardware not available"
  when there's no GPU), Smooth motion (frame interpolation), High picture quality, a refresh
  target to save power, and sound output, Stereo / Surround / Dolby Digital passthrough,
  night mode and volume boost.
- **Debug info**: an optional corner overlay with input/output resolution and frame rates,
  decoder, audio, network and stream health.
- **Updates**: a daily check through PackageKit/apt with a cable-box-style
  *"System Update Available — Install Now / Later"* prompt and a progress bar. It offers a reboot
  when one is needed, and a restart when CableBox updated itself.
- **Input**: keyboard, IR receivers, and HDMI-CEC (your TV's remote), all through one key map.
  Settings can learn any remote an IR receiver can decode.
- **Also a desktop app**: started from a desktop it opens in a window (F11 for full screen).

Runs on x86_64, Raspberry Pi 4/5 (arm64) and RISC-V boards (riscv64), on Debian 13 or
trixie-based Raspberry Pi OS.

## Architecture

```
crates/
  tvh/      Tvheadend HTTP API client (channels, tags, EPG, DVR; basic/digest auth)
  player/   libmpv wrapper; renders video through mpv's OpenGL render API
  input/    RemoteKey; keyboard map, evdev reader for IR/CEC remotes, control FIFO
  sysctl/   D-Bus: PackageKit (updates), NetworkManager (Wi-Fi), logind (power),
            systemd (SSH, console), timedated (clock)
  app/      the `cablebox` binary: Slint UI (ui/*.slint) + controller (src/)
packaging/  systemd units, polkit rule, Debian maintainer scripts and copyright, desktop
            launcher and icon, THIRD-PARTY-LICENSES
tools/      mock_tvh.py (a fake Tvheadend server for development), cross-build and
            license-notice scripts
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

Learned buttons are identified by the scancode rc-core reports (`MSC_SCAN`), which it sends even
for buttons with no keymap entry, and live in CableBox's config rather than the kernel keymap, so
the sandboxed service needs no extra privileges. `input/src/learn.rs` turns raw events into
presses. `70-cablebox-ir.rules` turns on all IR protocols, since a receiver otherwise decodes only
its stock remote's.

## Development

Build dependencies (Debian):

```sh
sudo apt install build-essential pkg-config libmpv-dev libfontconfig-dev libxkbcommon-dev \
                 libinput-dev libgbm-dev libudev-dev libdrm-dev ffmpeg
```

Rust 1.89+ is required (zbus 6). Slint is pinned to 1.14.1, and the workspace uses the
MSRV-aware resolver. Packages are built with `--no-default-features --features kms,winit`: full
screen on KMS as a box, a window when started from a desktop. Plain `cargo run` builds only `winit`.

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

To work on the receiver's Settings pages with no hardware, run a simulated Pico and point a window
at it. Set `auto_check = false` under `[updates]` first: `CABLEBOX_PRETEND_BOX` enables the box-only
actions, so the update prompt there acts on this machine.

```sh
cargo run -p input --example fake_pico -- /tmp/pico    # then type: ir nec 1 10, fail hash, quit
CABLEBOX_PRETEND_BOX=1 CABLEBOX_PICO_DEVICE=/tmp/pico CABLEBOX_CONFIG=/tmp/cb.toml \
    SLINT_BACKEND=winit cargo run -p cablebox
```

Keyboard: `0-9` `.` channels · arrows / Enter / Esc · `G` guide · `I` info · `M` menu ·
`D` recordings · `R` record · `F` favorite · PgUp/PgDn channel · Backspace last channel ·
Space play/pause · `[` `]` rewind/ff · `-` `=` volume · F1–F4 red/green/yellow/blue ·
F11 full screen (in a window).

Automation: set `CABLEBOX_CONTROL=/path/fifo`, then `echo "guide down down ok" > /path/fifo`.
Key names are in `RemoteKey::from_name`. This also works over SSH on a device.

Tests: `cargo test --workspace`. The read-only D-Bus probe is `cargo run -p sysctl --example probe`.

## Install from apt

Prebuilt packages for Debian 13 / trixie-based Raspberry Pi OS (amd64, arm64, riscv64) are published
in the ICLX apt repository, https://apt.icecubelabs.xyz/:

```sh
sudo apt install curl
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
tools/third-party-licenses.sh               # refresh crate notices (cargo install cargo-about --features cli)
cargo deb -p cablebox                       # amd64, builds with --features kms,winit
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

## Settings

If `config.toml` has an error, CableBox says where, runs on default settings, and never writes to the
file. Fix it and restart, or choose one of:
- **Repair File** comments out the bad lines, listing them, and keeps the rest of your settings. The
  original is copied to `config.toml.bad`, which is the lasting copy of those lines: the comments go
  the next time settings are saved. A bad section header has to be fixed by hand.
- **Start Fresh** moves the file to `config.toml.bad` and begins again.

Everything below is on screen; values are saved to the config file right away.

### Display

`Settings → Display`:
- **Screen** picks which output to use. Auto prefers a TV or monitor (HDMI, DisplayPort, …) over a
  laptop's built-in panel and falls back to the panel when nothing else is connected.
- **Resolution** picks one of that screen's modes.

Both apply on restart (through `SLINT_DRM_OUTPUT` and `SLINT_DRM_MODE`). If the chosen screen isn't
connected at startup, CableBox falls back to Auto instead of failing to start. Interface size
defaults to the screen height ÷ 720. On a box it's set before the KMS backend starts
(`SLINT_SCALE_FACTOR`, from the chosen screen's resolution), and the guide, recordings and Settings
lists show as many rows as fit. `journalctl -u cablebox | grep -E "screen:|layout:"` shows what
was chosen.

### Picture

These are also in `Settings → Display` and apply immediately:
- **Hardware acceleration** (on by default) lets the GPU decode video (mpv `hwdec=auto-safe`),
  scale it and generate frames. The row shows the decoder in use (`On · vaapi`), or that the
  channel's codec is decoded in software. Many GPUs can't decode MPEG-2, which most over-the-air
  ATSC channels use. **Graphics** shows the OpenGL renderer. When it's a software rasterizer
  (Mesa llvmpipe, common on boards without GPU drivers), the row says *Hardware not available*.
  Smooth motion and High quality then stay off, since they'd run on the CPU. Switching it off
  does the same, and keeps your choices for when it's back on.
- **Smooth motion** (off by default) turns on mpv's interpolation with `video-sync=display-resample`.
  It blends frames to the screen's refresh rate, so 24/30 fps content doesn't judder. The rate is
  read from the KMS mode (or, in a window, from the monitor), and when a screen doesn't report it
  mpv's own measurement is used. With it on, CableBox redraws every refresh while video plays, which
  costs GPU time; run with `RUST_LOG=info,cablebox=debug` to log mistimed/dropped frames. A
  measured rate well below the screen's (say 100 Hz on a 120 Hz screen) means the GPU can't keep
  up: use Standard picture quality, or turn Smooth motion off.
- **Refresh target** draws less often to save GPU and power: Full, Half, Third or Quarter of the
  screen's refresh rate (whole fractions, so frames stay evenly timed). Below Full, video faster
  than the target is thinned to it (mpv's `fps` filter; e.g. 59.94 → 29.99 at Quarter on a 120 Hz
  screen) and Smooth motion is off, since it needs every refresh. In a Wayland window, where the
  screen doesn't report its rate, 60 Hz is assumed.
- **Picture quality**: Standard keeps mpv's defaults. High uses `ewa_lanczossharp` scaling and
  debanding, which looks sharper on big screens but needs a strong GPU. Keep Standard on a Pi.

### Sound

`Settings → Sound`:
- **Output device:** Auto, or a specific output (sound-server sinks, HDMI, S/PDIF, analog). An
  output that isn't connected falls back to Auto without losing the choice.
- **Audio format:**
  - **Stereo** (default) downmixes, which is right for TV speakers.
  - **Surround (5.1)** decodes all channels.
  - **Passthrough** sends Dolby Digital untouched to a soundbar or receiver. It only works on a
    direct HDMI or S/PDIF output (ALSA `hdmi:`/`iec958:`). On any other output it decodes like
    Surround and the row says so, because sound servers accept the bitstream and then stall
    playback. In passthrough, volume is set on the receiver.
- If a chosen output takes sound but never plays it (an HDMI port with nothing attached, or a
  device that accepts a bitstream it can't use), the picture would freeze waiting on it. CableBox
  notices playback running at under half speed with data waiting, switches to Auto for the
  session, re-tunes, and shows "No sound · using Auto". Picking an output or format again
  retries it.
- **Night mode:** a gentle compressor, plus the broadcaster's own AC-3 range compression.
- **Volume boost:** allows up to 130%.

mpv's warnings and errors appear in CableBox's log (`journalctl -u cablebox`), prefixed `mpv`.

### Debug info

`Settings → System → Debug info` shows a box in the top-left corner of every screen:
- **IN:** the video's resolution and frame rate (plus the rate after deinterlacing), codec, and
  decoder (`hw:vaapi` or `sw`).
- **AUD:** the audio codec and channels, what's sent to the output, and passthrough.
- **OUT:** the screen's resolution, refresh rate (and Refresh target), and frames CableBox
  actually drew in the last second.
- **NET:** wired or Wi-Fi (with signal), connection name, and IP address.
- **STR:** the Tvheadend server, the stream's current download rate, and how many seconds are
  buffered. Live TV plays close to the live edge, so a buffer near 0 s is normal; it matters
  when it stays at 0 and the picture stutters.

### Remote

`Settings → Remote control → Learn a remote` walks through the buttons on any remote your IR
receiver can decode: OK, the arrows and Back are required, and pressing the new OK skips the rest.
Nothing changes until you press the new OK to save at the end, so a mistake can't lock you out;
the new Back discards it. Learned buttons add to the built-in key map and take priority over it,
so several remotes can work together.

- **Buttons** relearns a single key, replacing that key's earlier learned buttons.
- **Reset learned buttons** forgets them all.
- **Receiver** shows what was detected. It needs "Remote & HDMI-CEC input" on, and a box rather
  than a desktop window.
- A USB keyboard never goes through the learned buttons, so it always works, and Esc cancels
  learning. Learning also gives up after 20 seconds with no button, which usually means the
  receiver can't decode that remote.

They're saved as `[remote.buttons]` in `config.toml`, e.g. `"scan:800f0422" = "ok"`. Deleting that
section over SSH has the same effect as resetting.

With a Pico IR receiver connected, this page also shows its firmware version and signal level, and
offers an update when the package ships newer firmware (with one prompt per version). It has no
built-in key map, so its remote must be learned using a USB keyboard or another working remote. An
interrupted or failed update leaves the receiver on its previous firmware.

Each hardware design has a model number, reported by its firmware. IR1 is the Pico with a bare
photodiode. Firmware is built for one model and ships in its own folder
(`/usr/share/cablebox/firmware/ir1/`). CableBox only offers a receiver its own model's firmware, and
the receiver also refuses any other model's before erasing anything, so a later design can't be
overwritten with IR1 firmware. A new design takes the next number, and numbers are never reused.

## Running in a window

Started from a desktop (the **CableBox** menu entry, or `cablebox` in a terminal), CableBox opens in
a window instead of taking over the screen. F11 toggles full screen. It uses that user's own config
(`~/.config/cablebox/config.toml`), and the Screen/Resolution settings don't apply. Exit to console
and the clock settings are only offered on a box. Updates and SSH work, but ask for your password
through the desktop, since the passwordless polkit rule only covers the `cablebox` user. The remote
reader is off in a window: keys come from the window. The service always runs full screen (it sets
`SLINT_BACKEND=linuxkms`), and so does `cablebox` started from a text console.

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

Verified:
- On a real box: the full-screen KMS session and on-screen updates installed end to end, through
  every release since 0.1.5.
- Against a real Tvheadend 4.3 server with ATSC channels: live TV (1080i MPEG-2 with
  deinterlacing, 720p), channel entry, guide, recordings playback, and Dolby Digital 5.1.
- In windows on an x86_64 laptop (Wayland and X11, AMD GPU): every setting, including hardware
  decoding (VA-API on H.264 from the mock server), Smooth motion, refresh targets, sound outputs,
  the stalled-output fallback and a software-rendering GPU (`LIBGL_ALWAYS_SOFTWARE=1`).

Still to verify on hardware:
- A Raspberry Pi 5, including mpv GL interop with hardware decoding (see the debug overlay or
  `Settings → Display → Hardware acceleration`).
- Dolby Digital passthrough into a real soundbar or receiver.
- CEC key mapping with real TVs.
- Learning a remote end to end. The logic has unit tests, but it hasn't yet run with a real IR
  receiver or `rc-loopback`, and neither has the udev rule. In particular: whether receivers
  accept `protocols=all`, and whether the `ir-keytable` rule from `v4l-utils` resets it afterwards.
- The Pico receiver. CableBox's side (`input/src/pico.rs`, `ir-proto`) is tested only against a
  simulated receiver, and is inactive in the package: no firmware ships, and there's no udev rule
  for the receiver until it has a real USB ID. The firmware in `firmware/rp2040` runs on a Pico
  only without its bootloader and doesn't yet recognise remotes reliably.
- Whether `/sys/class/drm/*/modes` order matches `SLINT_DRM_MODE` indices.
- The systemd sandbox with real DRM, audio and remotes. It was tested locally with the
  winit backend under the same seccomp/namespace settings. If something breaks on a box,
  `systemctl edit cablebox` can relax a single setting.

Not yet implemented:
- Tvheadend comet push updates (DVR state is polled every 60s).
- Reminders.
- Per-channel parental controls (the settings PIN exists).

## License

CableBox is free software: you can redistribute it and/or modify it under the terms of the GNU
General Public License as published by the Free Software Foundation, either version 2 of the
License, or (at your option) any later version. See [LICENSE](LICENSE).

The packaged program links Slint, which is used under GPL-3.0, and Apache-2.0 crates, so the
binaries as a whole are distributed under GPL-3.0 (permitted by "or any later version").
The licenses and notices of every crate compiled in are in
[packaging/THIRD-PARTY-LICENSES](packaging/THIRD-PARTY-LICENSES), installed with the package
in `/usr/share/doc/cablebox/`. Release `X.Y.Z` is built from git tag `vX.Y.Z`.
