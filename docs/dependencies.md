# Dependency inventory for the Arch + t2linux target

Machine, from `scripts/doctor.sh` on 2026-10-30: **MacBook Pro 16" 2019 (MacBookPro16,1)**, Arch Linux with the t2linux kernel 7.2.11, KDE Plasma 6.8.2 on Wayland, libinput 1.32. GPUs: Intel UHD 630 (`i915`, VA-API through the iHD driver) and Radeon Pro 5300M/5500M (Navi 14, `amdgpu`). iPad Air with Apple Pencil Pro. (Earlier docs said "Mac Pro 2019"; that was a mix-up.)

## Phase 1 (pen): needed

| Package (Arch) | Why | Notes |
|---|---|---|
| `usbmuxd` | USB transport to the iPad | started by udev on plug-in; no unit to enable |
| `rust` (or rustup) | build `sidecard` | about 1.5 GB |
| *(kernel)* `uinput` module | virtual tablet | **in-tree**, no DKMS; autoloaded via `/etc/modules-load.d/` |
| udev rule + group `uinput` | non-root access to `/dev/uinput` | `scripts/setup-system.sh` |
| `libinput`, `libwacom` | already part of Plasma | libwacom doesn't know our device, which is intended |
| `kwin`, `plasma-desktop` | Drawing Tablet KCM, D-Bus `outputName` mapping | Plasma 6.x |
| `systemd` (`busctl`) | KWin D-Bus calls | always present |

## Phase 1: diagnostics (recommended)

| Package | Why |
|---|---|
| `libimobiledevice` | `idevice_id`, `ideviceinfo`, `idevicepair` (pairing check in doctor.sh) |
| `evtest` | raw evdev check |
| `wayland-utils` | `wayland-info`: is `zkde_screencast_unstable_v1` exposed? |
| `libva-utils` | `vainfo` (phase 2 readiness) |
| `avahi` | `avahi-browse` for the Wi-Fi fallback (optional: sidecard has its own mDNS client) |
| `krita` | the reference application for testing |

## iPad app install

| Path | Packages |
|---|---|
| A. xtool | xtool **AppImage** (1.24.0), `fuse2`; Apple ID login on the PC. **No** Xcode.xip needed to sign or install a prebuilt `.ipa`. |
| B. SideStore | `iloader` (2.4.1+) on the PC once; SideStore ≥ 0.7.0-alpha and LocalDevVPN on the iPad |

## Phase 2 (virtual display): later

| Package | Why |
|---|---|
| `pipewire`, `wireplumber` | screen capture (already in Plasma) |
| `gstreamer`, `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`, **`gst-plugin-va`** (VA-API encoders, separate package on Arch), `gst-plugin-pipewire`, `gst-plugins-ugly` (x264 fallback and test pattern) | the encoder subprocess (`gst-launch-1.0`, DECISIONS D18). Package names checked against archlinux.org on 2026-10-30. |
| `ffmpeg` (alternative to GStreamer) | `h264_vaapi` |
| `libva-mesa-driver` / `mesa` | VA-API encode on radeonsi |

## t2linux specifics

- **No DKMS module is needed for phases 1–2** (uinput is in-tree; the virtual display uses KWin, not evdi or vkms).
- DKMS appears only in **phase 3** (`v4l2loopback` for the webcam mode). It must be built against **`linux-t2-headers` matching the running `linux-t2` kernel**, and rebuilt on every kernel update (`dkms autoinstall`). `doctor.sh` reports whether the headers are installed.
- **Assumption:** the T2 chip (apple-bce) only handles the internal keyboard/trackpad/audio paths. The MacBook Pro's Thunderbolt 3 ports are ordinary xHCI controllers, so the iPad over USB doesn't involve apple-bce.
- If an iPad isn't seen on one Thunderbolt port, try a port on the other side (separate controller) or another cable.
