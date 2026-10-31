#!/usr/bin/env bash
# One-time system setup for sidecarforlinux on Arch Linux.
#
#   scripts/setup-system.sh                       # packages (+ uinput access if missing)
#   scripts/setup-system.sh --with-uinput-group   # also add the uinput group/udev rule
#
# Prints every command it will run (several need sudo) and asks before
# doing anything. Safe to re-run. Review it first:  less scripts/setup-system.sh
set -euo pipefail

GROUP=uinput
RULE=/etc/udev/rules.d/60-sidecarforlinux-uinput.rules
MODLOAD=/etc/modules-load.d/sidecarforlinux.conf
USER_NAME=${SUDO_USER:-$(id -un)}

if [ "$(id -u)" -eq 0 ]; then
  echo "Run this as your normal user; it calls sudo itself for the steps that need it." >&2
  exit 1
fi

FORCE_GROUP=0
case "${1:-}" in
  --with-uinput-group) FORCE_GROUP=1 ;;
  "") ;;
  *) echo "usage: $0 [--with-uinput-group]" >&2; exit 2 ;;
esac

# usbmuxd = USB transport to the iPad; the others are diagnostics.
# On Arch the `libinput` command lives in libinput-tools.
PKGS=(usbmuxd libimobiledevice evtest libinput-tools)
command -v cargo >/dev/null 2>&1 || PKGS+=(rust)
# Building sidecard's native video backend (the default build): clang for
# bindgen, PipeWire and libva headers (docs/dependencies.md). --needed skips
# what is already installed.
BUILD_PKGS=(clang libpipewire libva pkgconf)

# Some packages (Steam, game-controller rules, ...) already give the logged-in
# user access to /dev/uinput through a udev "uaccess" ACL. Then the group and
# rule below are not needed.
UINPUT_STEPS=1
if [ -w /dev/uinput ] && [ "$FORCE_GROUP" -eq 0 ]; then
  UINPUT_STEPS=0
fi

echo "This will run, in order:"
echo
echo "  1. sudo pacman -S --needed ${PKGS[*]} ${BUILD_PKGS[*]}"
echo "       (${BUILD_PKGS[*]}: to build sidecard's native video backend. clang with its"
echo "        dependencies is about 400 MB. Without them install-daemon.sh builds without it.)"
if [ "$UINPUT_STEPS" -eq 1 ]; then
  cat <<PLAN
  2. sudo groupadd --system --force $GROUP
       (a dedicated group that may create virtual input devices)
  3. sudo tee $RULE
       KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="$GROUP", MODE="0660", OPTIONS+="static_node=uinput"
  4. sudo tee $MODLOAD   (load the uinput module at every boot)
       uinput
  5. sudo modprobe uinput && sudo udevadm control --reload && sudo udevadm trigger --name-match=uinput
  6. sudo usermod -aG $GROUP $USER_NAME
       (takes effect after you log out and back in)

Note: members of '$GROUP' can create virtual keyboards/mice/tablets, i.e.
inject input into your session. Only add your own desktop user.

To undo later:
  sudo rm $RULE $MODLOAD && sudo gpasswd -d $USER_NAME $GROUP && sudo udevadm control --reload
PLAN
else
  granted_by=$(grep -ls 'uinput' /usr/lib/udev/rules.d/*.rules /etc/udev/rules.d/*.rules 2>/dev/null | tr '\n' ' ' || true)
  cat <<NOTE

/dev/uinput is already writable by $(id -un) (udev ACL from: ${granted_by:-unknown}),
so the uinput group and udev rule are skipped and no re-login is needed.
If you ever remove the package that grants it, re-run with --with-uinput-group.
NOTE
fi
echo
echo "Nothing else is changed."

read -r -p "Proceed? [y/N] " ans
case "$ans" in y|Y|yes|YES) ;; *) echo "Nothing done."; exit 0 ;; esac

run() { echo "+ $*"; "$@"; }

run sudo pacman -S --needed "${PKGS[@]}" "${BUILD_PKGS[@]}"
if [ "$UINPUT_STEPS" -eq 1 ]; then
  run sudo groupadd --system --force "$GROUP"
  echo "+ sudo tee $RULE"
  printf '# sidecarforlinux: let the %s group create uinput devices\nKERNEL=="uinput", SUBSYSTEM=="misc", GROUP="%s", MODE="0660", OPTIONS+="static_node=uinput"\n' "$GROUP" "$GROUP" | sudo tee "$RULE" >/dev/null
  echo "+ sudo tee $MODLOAD"
  printf 'uinput\n' | sudo tee "$MODLOAD" >/dev/null
  run sudo modprobe uinput
  run sudo udevadm control --reload
  run sudo udevadm trigger --name-match=uinput
  run sudo usermod -aG "$GROUP" "$USER_NAME"
  echo
  ls -l /dev/uinput
  echo "Done. Log out and back in (or reboot) so '$USER_NAME' is in '$GROUP', then run scripts/doctor.sh."
else
  echo
  echo "Done. No re-login needed. Next: scripts/install-daemon.sh"
fi
