#!/usr/bin/env bash
# sidecarforlinux doctor: read-only checks of this Linux machine.
#
#   scripts/doctor.sh            # print the report
#   scripts/doctor.sh > doctor.txt
#
# It changes nothing and never uses sudo. Paste the output back into the
# project chat / an issue. Each line is OK / WARN / FAIL / INFO.

set -u
LC_ALL=C

ok()   { printf '  OK    %s\n' "$*"; }
warn() { printf '  WARN  %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; }
info() { printf '  INFO  %s\n' "$*"; }
hdr()  { printf '\n== %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

printf 'sidecarforlinux doctor - %s\n' "$(date -Iseconds)"

hdr "System"
info "kernel: $(uname -srm)"
if uname -r | grep -qi t2; then ok "t2linux kernel detected ($(uname -r))"; else warn "kernel name has no 't2' - is this the t2linux kernel?"; fi
[ -r /etc/os-release ] && info "os: $(. /etc/os-release; echo "${PRETTY_NAME:-?}")"
if [ -r /sys/class/dmi/id/product_name ]; then info "machine: $(cat /sys/class/dmi/id/product_name 2>/dev/null)"; fi
if have lsmod && lsmod | grep -q '^apple_bce'; then ok "apple-bce (T2) module loaded"; else info "apple-bce module not listed (fine if built in)"; fi
if have pacman; then
  for p in linux-t2 linux-t2-headers apple-bcm-firmware; do
    v=$(pacman -Q "$p" 2>/dev/null) && info "package $v"
  done
  if pacman -Qq dkms >/dev/null 2>&1; then info "dkms installed: $(dkms status 2>/dev/null | tr '\n' ';')"; fi
  if ! pacman -Q "linux-t2-headers" >/dev/null 2>&1; then info "linux-t2-headers not installed (only needed for DKMS modules such as v4l2loopback/evdi - not needed for phase 1)"; fi
fi

hdr "Session"
info "XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-unset} XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-unset} WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-unset}"
[ "${XDG_SESSION_TYPE:-}" = wayland ] && ok "Wayland session" || warn "not a Wayland session (run this from your Plasma Wayland session)"
if have plasmashell; then info "$(plasmashell --version 2>/dev/null)"; else warn "plasmashell not found"; fi
if have kwin_wayland; then info "$(kwin_wayland --version 2>/dev/null | head -1)"; fi
if have pacman; then
  for p in kwin plasma-desktop xdg-desktop-portal xdg-desktop-portal-kde pipewire wireplumber libinput libwacom krdp kpipewire; do
    v=$(pacman -Q "$p" 2>/dev/null) && info "package $v" || info "package $p: not installed"
  done
fi
if have wayland-info; then
  if wayland-info 2>/dev/null | grep -q zkde_screencast_unstable_v1; then
    ok "KWin exposes zkde_screencast_unstable_v1 (virtual monitors for phase 2): $(wayland-info 2>/dev/null | grep -A0 zkde_screencast | head -1 | sed 's/^ *//')"
  else
    warn "zkde_screencast_unstable_v1 not visible to this client (normal on KWin < 6.8 for unprivileged apps)"
  fi
else
  info "wayland-info not installed (pacman -S wayland-utils) - skipping protocol check"
fi
if have busctl && busctl --user list 2>/dev/null | grep -q org.kde.KWin; then
  ok "KWin D-Bus reachable (needed for automatic pen-to-screen mapping)"
  n=$(busctl --user tree org.kde.KWin 2>/dev/null | grep -c '/org/kde/KWin/InputDevice/event' || true)
  info "KWin input devices on D-Bus: $n"
else
  warn "KWin D-Bus not reachable from this shell"
fi
if have kscreen-doctor; then
  info "outputs: $(kscreen-doctor -o 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -E '^Output:' | awk '{print $3}' | tr '\n' ' ')"
fi

hdr "uinput (virtual pen tablet)"
if [ -e /dev/uinput ]; then
  info "$(ls -l /dev/uinput)"
  if [ -w /dev/uinput ]; then ok "/dev/uinput is writable by $(id -un)"; else fail "/dev/uinput not writable by $(id -un) - see docs/MORNING.md step 2 (udev rule + group)"; fi
else
  if [ -e /sys/class/misc/uinput ]; then fail "/dev/uinput missing although the module is loaded"; else fail "uinput not loaded - 'modprobe uinput' (MORNING.md step 2 makes it permanent)"; fi
fi
grep -qs '^uinput' /proc/modules && info "uinput loaded as a module" || info "uinput not listed as module (built in, or not loaded)"
if [ -e /etc/udev/rules.d/60-sidecarforlinux-uinput.rules ]; then
  ok "udev rule installed: $(grep -v '^#' /etc/udev/rules.d/60-sidecarforlinux-uinput.rules | head -1)"
elif [ -w /dev/uinput ]; then
  info "sidecarforlinux udev rule not needed: /dev/uinput is already writable"
else
  warn "sidecarforlinux udev rule not installed"
fi
if have getfacl && [ -e /dev/uinput ]; then
  acl=$(getfacl -cp /dev/uinput 2>/dev/null | grep -E '^user:[^:]+:' | tr '\n' ' ')
  [ -n "$acl" ] && info "/dev/uinput ACL: $acl(granted by: $(grep -ls 'uinput' /usr/lib/udev/rules.d/*.rules /etc/udev/rules.d/*.rules 2>/dev/null | tr '\n' ' '))"
fi
info "groups of $(id -un): $(id -Gn)"
if have libinput; then info "libinput $(libinput --version 2>/dev/null)"; else warn "libinput tools missing (pacman -S libinput-tools)"; fi
have evtest && ok "evtest installed" || warn "evtest missing (pacman -S evtest)"
if have libinput; then
  if libinput list-devices 2>/dev/null | grep -q "Sidecar Apple Pencil"; then ok "a Sidecar Apple Pencil device is present right now"; else info "no Sidecar Apple Pencil device right now (only exists while sidecard runs)"; fi
fi

hdr "Video encode (phase 2)"
if have lspci; then lspci -nn 2>/dev/null | grep -Ei 'vga|3d|display' | sed 's/^/  INFO  gpu: /'; fi
for d in /sys/class/drm/card*/device/driver; do [ -e "$d" ] && info "$(basename "$(dirname "$(dirname "$d")")") driver: $(basename "$(readlink "$d")")"; done
if have vainfo; then
  out=$(vainfo 2>&1)
  echo "$out" | grep -E 'Driver version|vainfo: VA-API version' | sed 's/^/  INFO  /'
  echo "$out" | grep -qE 'VAProfileH264.*VAEntrypointEncSlice' && ok "VA-API H.264 encode available" || fail "no VA-API H.264 encode entrypoint"
  echo "$out" | grep -qE 'VAProfileHEVCMain.*VAEntrypointEncSlice' && ok "VA-API HEVC encode available" || warn "no VA-API HEVC encode entrypoint"
  echo "$out" | grep -E 'EncSlice' | sed 's/^/  INFO  /'
else
  warn "vainfo not installed (pacman -S libva-utils)"
fi
if have gst-inspect-1.0; then
  for e in vah264enc vah264lpenc vah265enc x264enc; do
    gst-inspect-1.0 "$e" >/dev/null 2>&1 && ok "gstreamer element $e" || info "gstreamer element $e not available"
  done
  info "(phase 2 only: va* elements come from gst-plugin-va, x264enc from gst-plugins-ugly)"
else
  info "gst-inspect-1.0 not installed (gstreamer, only needed for phase 2)"
fi
have ffmpeg && info "$(ffmpeg -hide_banner -version 2>/dev/null | head -1)"

hdr "USB / usbmuxd / iPad"
if have usbmuxd; then info "$(usbmuxd --version 2>/dev/null | head -1)"; else fail "usbmuxd not installed (pacman -S usbmuxd)"; fi
if have systemctl; then
  st=$(systemctl is-active usbmuxd 2>/dev/null); info "usbmuxd.service: $st (it is started by udev when an iOS device is plugged in)"
fi
if [ -S /var/run/usbmuxd ] || [ -S /run/usbmuxd ]; then ok "usbmuxd socket present: $(ls -l /run/usbmuxd 2>/dev/null || ls -l /var/run/usbmuxd)"; else warn "usbmuxd socket not present (plug in and unlock the iPad)"; fi
if have idevice_id; then
  ids=$(idevice_id -l 2>/dev/null)
  if [ -n "$ids" ]; then
    ok "iOS device(s) over USB: $(echo "$ids" | tr '\n' ' ')"
    for u in $ids; do
      if have ideviceinfo; then
        info "  $(ideviceinfo -u "$u" -k ProductType 2>/dev/null) iPadOS $(ideviceinfo -u "$u" -k ProductVersion 2>/dev/null) name=$(ideviceinfo -u "$u" -k DeviceName 2>/dev/null)"
      fi
      if have idevicepair; then
        idevicepair -u "$u" validate >/dev/null 2>&1 && ok "  paired with this computer (Trust accepted)" || fail "  not paired: unlock the iPad, tap 'Trust', then: idevicepair pair"
      fi
    done
  elif ! have usbmuxd; then
    warn "no iOS device visible over USB (expected: idevice_id needs usbmuxd, which is not installed)"
  else
    warn "no iOS device visible over USB (cable? unlocked? trusted?)"
  fi
else
  info "libimobiledevice tools not installed (pacman -S libimobiledevice) - optional, for diagnostics"
fi
if have lsusb; then
  lsusb 2>/dev/null | grep -i 'apple' | sed 's/^/  INFO  usb: /'
else
  info "lsusb not installed (pacman -S usbutils) - optional, lists Apple USB devices"
fi

hdr "Network (Wi-Fi fallback)"
if have avahi-browse; then
  r=$(timeout 4 avahi-browse -rpt _sidecarlinux._tcp 2>/dev/null | grep '^=' | head -3)
  [ -n "$r" ] && ok "iPad advertising _sidecarlinux._tcp: $r" || info "no _sidecarlinux._tcp on the LAN right now (app closed or Wi-Fi off)"
  systemctl is-active avahi-daemon >/dev/null 2>&1 && info "avahi-daemon active (not required: sidecard has its own mDNS client)"
else
  info "avahi-browse not installed (optional)"
fi

hdr "sidecarforlinux"
if have sidecard; then
  ok "$(sidecard --version)"
  sidecard info 2>/dev/null | sed 's/^/  INFO  /'
else
  info "sidecard not on PATH (build: cargo build --release -p sidecard)"
fi
if have cargo; then info "$(cargo --version)"; else info "cargo not installed (pacman -S rust) - needed to build sidecard"; fi
if have xtool; then info "xtool $(xtool --version 2>/dev/null | head -1)"; else info "xtool not installed (only for install path A in MORNING.md)"; fi

printf '\nDone. Nothing was changed on this system.\n'
