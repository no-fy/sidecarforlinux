#!/usr/bin/env bash
# Re-sign and (re)install the iPad app with xtool and your free Apple ID.
# Free provisioning profiles expire after 7 days: run this at least weekly,
# with the iPad plugged in and unlocked. App data is kept.
#
#   scripts/resign-ipad.sh            # latest CI build ("nightly" release)
#   scripts/resign-ipad.sh path.ipa   # a specific .ipa
#
# Don't use xtool and SideStore with the same Apple ID (each revokes the
# other's certificate). See docs/MORNING.md.
set -euo pipefail
REPO="${REPO:-no-fy/sidecarforlinux}"
APP="${APP:-SidecarPad}"
UDID="${UDID:-}"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

command -v xtool >/dev/null || { echo "xtool not found: see docs/MORNING.md (install path A)" >&2; exit 1; }

# 1. usbmuxd is normally started by udev when the iPad is plugged in.
if ! systemctl is-active --quiet usbmuxd; then
  echo "usbmuxd is not running. This needs: sudo systemctl start usbmuxd"
  read -r -p "Run it now? [y/N] " a
  case "$a" in y|Y) echo "+ sudo systemctl start usbmuxd"; sudo systemctl start usbmuxd ;; *) exit 1 ;; esac
fi

# 2. Apple ID session (asks for password + 2FA only when needed).
if ! xtool auth status 2>/dev/null | grep -qi 'logged in'; then
  echo "+ xtool auth login --mode password"
  xtool auth login --mode password
fi

# 3. The .ipa.
if [ $# -ge 1 ]; then
  cp "$1" "$work/$APP.ipa"
else
  url="https://github.com/$REPO/releases/download/nightly/$APP.ipa"
  echo "+ downloading $url"
  curl -fSL -o "$work/$APP.ipa" "$url"
  if curl -fsSL -o "$work/SHA256SUMS.txt" "https://github.com/$REPO/releases/download/nightly/SHA256SUMS.txt"; then
    (cd "$work" && sha256sum -c SHA256SUMS.txt)
  fi
fi

# 4. New 7-day profile, re-sign, install over the existing app.
echo "+ xtool install ${UDID:+--udid $UDID} --usb $work/$APP.ipa"
xtool install ${UDID:+--udid "$UDID"} --usb "$work/$APP.ipa"
echo "Installed. Expires in 7 days: run this script again before $(date -d '+7 days' '+%a %d %b')."
