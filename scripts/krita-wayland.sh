#!/usr/bin/env bash
# Make Krita start in native Wayland mode from the app launcher.
#
#   scripts/krita-wayland.sh          # show the plan, ask, install the override
#   scripts/krita-wayland.sh --undo   # remove it again
#
# Why: Krita 6 forces its X11 mode (XWayland) unless QT_QPA_PLATFORM is set
# (krita/main.cc). In X11 mode it only sees tablets that existed when it
# started, and the iPad's virtual pen appears when the iPad connects. In
# Wayland mode it picks the pen up whenever it appears.
#
# It writes one file in your home directory (no sudo): a copy of Krita's
# launcher with QT_QPA_PLATFORM=wayland added to its Exec lines. That copy
# takes precedence over the system launcher. Re-run after a Krita update if
# the system launcher changed.
set -euo pipefail

NAME=org.kde.krita.desktop
MARK="# sidecarforlinux: Krita in native Wayland mode (scripts/krita-wayland.sh)"
DEST_DIR=${XDG_DATA_HOME:-$HOME/.local/share}/applications
DEST=$DEST_DIR/$NAME

refresh_menu() {
  if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
  fi
}

if [ "${1:-}" = "--undo" ]; then
  if [ -f "$DEST" ] && grep -qF "$MARK" "$DEST"; then
    rm -- "$DEST"
    refresh_menu
    echo "Removed $DEST. Krita starts in its default (X11) mode again."
  else
    echo "Nothing to undo: $DEST is not ours (or doesn't exist)."
  fi
  exit 0
elif [ -n "${1:-}" ]; then
  echo "usage: $0 [--undo]" >&2
  exit 2
fi

SRC=
IFS=: read -r -a dirs <<<"${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
for d in "${dirs[@]}"; do
  # Skip our own override in case XDG_DATA_DIRS lists the user data dir.
  if [ -f "$d/applications/$NAME" ] && [ "$d/applications/$NAME" -ef "$DEST" ]; then continue; fi
  if [ -f "$d/applications/$NAME" ]; then SRC=$d/applications/$NAME; break; fi
done
if [ -z "$SRC" ]; then
  echo "Krita's launcher ($NAME) was not found in XDG_DATA_DIRS. Is Krita installed (pacman -S krita)?" >&2
  exit 1
fi

BACKUP=
if [ -e "$DEST" ] && ! grep -qF "$MARK" "$DEST"; then
  BACKUP=$DEST.bak-$(date +%Y%m%d%H%M%S)
fi

cat <<PLAN
This will:
  1. copy  $SRC
     to    $DEST
     with  every "Exec=..." line changed to "Exec=env QT_QPA_PLATFORM=wayland ..."
PLAN
[ -n "$BACKUP" ] && echo "     (your existing $DEST is moved to $BACKUP first)"
cat <<PLAN
  2. run kbuildsycoca6 so the app launcher picks it up

No sudo, nothing outside $DEST_DIR. Undo: $0 --undo
Starting Krita from a terminal is not affected: use  QT_QPA_PLATFORM=wayland krita
PLAN

read -r -p "Proceed? [y/N] " ans
case "$ans" in y|Y|yes|YES) ;; *) echo "Nothing done."; exit 0 ;; esac

mkdir -p "$DEST_DIR"
[ -n "$BACKUP" ] && mv -- "$DEST" "$BACKUP"
tmp=$(mktemp "$DEST_DIR/.$NAME.XXXXXX")
{
  echo "$MARK"
  sed -E 's/^(Exec=)(env QT_QPA_PLATFORM=[^ ]+ )?/\1env QT_QPA_PLATFORM=wayland /' "$SRC"
} >"$tmp"
mv -- "$tmp" "$DEST"
refresh_menu

echo
grep -E '^Exec=' "$DEST" | sed 's/^/  /'
echo "Done. Quit Krita completely and start it again from the app launcher."
