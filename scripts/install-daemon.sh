#!/usr/bin/env bash
# Build sidecard and install it for the current user (no sudo):
#   ~/.local/bin/sidecard
#   ~/.config/systemd/user/sidecard.service   (optional, asks first)
set -euo pipefail
cd "$(dirname "$0")/.."

echo "+ cargo build --release -p sidecard"
cargo build --release -p sidecard
install -Dm755 target/release/sidecard "$HOME/.local/bin/sidecard"
echo "installed $HOME/.local/bin/sidecard ($("$HOME/.local/bin/sidecard" --version))"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "note: add ~/.local/bin to your PATH";; esac

# Phase 2: on Plasma <= 6.7, KWin only lets programs use its screencast
# protocol (virtual monitors) if a .desktop file with this key points at
# their binary. Harmless on newer Plasma. No sudo, user-local only.
DESKTOP="$HOME/.local/share/applications/sidecard.desktop"
mkdir -p "$(dirname "$DESKTOP")"
cat > "$DESKTOP" <<DESKEOF
[Desktop Entry]
Type=Application
Name=sidecard (iPad pen tablet daemon)
Exec=$HOME/.local/bin/sidecard run
NoDisplay=true
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
DESKEOF
echo "installed $DESKTOP (KWin screencast permission for phase 2)"

UNIT="$HOME/.config/systemd/user/sidecard.service"
cat <<PLAN

Optionally install a systemd *user* service so the tablet starts with your
session and reconnects whenever the iPad app is open:

  write $UNIT
  systemctl --user daemon-reload
  systemctl --user enable --now sidecard.service

(Pair first: run 'sidecard pair' in a terminal once, with the iPad app open.)
PLAN
read -r -p "Install and enable the user service? [y/N] " ans
case "$ans" in y|Y|yes|YES) ;; *) echo "Skipped the service."; exit 0 ;; esac

mkdir -p "$(dirname "$UNIT")"
cat > "$UNIT" <<UNITEOF
[Unit]
Description=sidecarforlinux: iPad + Apple Pencil as a pen tablet
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.local/bin/sidecard run
Restart=always
RestartSec=2

[Install]
WantedBy=graphical-session.target
UNITEOF
systemctl --user daemon-reload
systemctl --user enable --now sidecard.service
systemctl --user --no-pager status sidecard.service | head -5
echo "Logs: journalctl --user -u sidecard -f"
