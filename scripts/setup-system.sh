#!/usr/bin/env bash
# One-time system setup for sidecarforlinux on Arch Linux.
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

cat <<PLAN
This will run, in order:

  1. sudo pacman -S --needed usbmuxd libimobiledevice evtest libinput
       (usbmuxd = USB transport to the iPad; the others are diagnostics)
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

Nothing else is changed. To undo later:
  sudo rm $RULE $MODLOAD && sudo gpasswd -d $USER_NAME $GROUP && sudo udevadm control --reload
PLAN

read -r -p "Proceed? [y/N] " ans
case "$ans" in y|Y|yes|YES) ;; *) echo "Nothing done."; exit 0 ;; esac

run() { echo "+ $*"; "$@"; }

run sudo pacman -S --needed usbmuxd libimobiledevice evtest libinput
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
