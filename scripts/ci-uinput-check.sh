#!/usr/bin/env bash
# Replays the synthetic pencil recording into a real uinput tablet and
# captures what evtest, libinput and udev make of it. Used in CI (as root on
# an ubuntu runner); also handy on your own machine:
#
#   scripts/ci-uinput-check.sh ./target/release/sidecard out/
#
# Needs: evtest, libinput-tools (libinput debug-events/list-devices), udevadm,
# and write access to /dev/uinput.
set -euo pipefail

BIN=${1:-./target/release/sidecard}
OUT=${2:-out}
mkdir -p "$OUT"
REC="$OUT/demo.jsonl"
"$BIN" gen-demo > "$REC"

# Start the replay with a settle delay so we can attach readers first.
"$BIN" replay "$REC" --speed 1 --settle 3 > "$OUT/replay.log" 2>&1 &
REPLAY=$!

NODE=""
for _ in $(seq 1 50); do
  NODE=$(grep -o '/dev/input/event[0-9]*' "$OUT/replay.log" | head -1 || true)
  [ -n "$NODE" ] && break
  sleep 0.1
done
if [ -z "$NODE" ]; then
  echo "no event node reported by sidecard replay" >&2
  cat "$OUT/replay.log" >&2
  exit 1
fi
echo "tablet node: $NODE"

udevadm settle || true
udevadm info --query=property --name="$NODE" > "$OUT/udev.txt" || true
timeout 10 evtest "$NODE" > "$OUT/evtest.txt" 2>&1 &
EVTEST=$!
timeout 10 libinput debug-events --device "$NODE" --show-keycodes > "$OUT/libinput-debug-events.txt" 2>&1 &
LIBINPUT=$!
sleep 0.5
libinput list-devices > "$OUT/libinput-list-devices.txt" 2>&1 || true

wait "$REPLAY"
sleep 0.5
kill "$EVTEST" "$LIBINPUT" 2>/dev/null || true
wait "$EVTEST" "$LIBINPUT" 2>/dev/null || true

fail=0
check() { # file pattern description
  if grep -qE "$2" "$1"; then echo "ok   $3"; else echo "FAIL $3"; fail=1; fi
}
echo "--- checks"
check "$OUT/udev.txt" '^ID_INPUT_TABLET=1' "udev classifies the device as a tablet"
check "$OUT/evtest.txt" 'Input device name: "Sidecar Apple Pencil"' "evtest sees the device"
check "$OUT/evtest.txt" 'Property type 1 \(INPUT_PROP_DIRECT\)' "INPUT_PROP_DIRECT set"
check "$OUT/evtest.txt" 'code 24 \(ABS_PRESSURE\), value [1-9]' "evtest: pressure"
check "$OUT/evtest.txt" 'code 26 \(ABS_TILT_X\), value -?[1-9]' "evtest: tilt X"
check "$OUT/evtest.txt" 'code 27 \(ABS_TILT_Y\), value -?[1-9]' "evtest: tilt Y"
check "$OUT/evtest.txt" 'code 2 \(ABS_Z\), value [1-9]' "evtest: barrel roll (ABS_Z)"
check "$OUT/evtest.txt" 'code 25 \(ABS_DISTANCE\), value [1-9]' "evtest: hover distance"
check "$OUT/evtest.txt" 'code 320 \(BTN_TOOL_PEN\), value 1' "evtest: pen proximity in"
check "$OUT/evtest.txt" 'code 330 \(BTN_TOUCH\), value 1' "evtest: tip down"
check "$OUT/evtest.txt" 'code 321 \(BTN_TOOL_RUBBER\), value 1' "evtest: eraser (double-tap)"
check "$OUT/evtest.txt" 'code 332 \(BTN_STYLUS2\), value 1' "evtest: BTN_STYLUS2 (squeeze)"
check "$OUT/libinput-list-devices.txt" 'Capabilities:.*tablet' "libinput: tablet capability"
check "$OUT/libinput-debug-events.txt" 'TABLET_TOOL_PROXIMITY' "libinput: proximity events"
check "$OUT/libinput-debug-events.txt" 'TABLET_TOOL_TIP.*down' "libinput: tip down"
check "$OUT/libinput-debug-events.txt" 'pressure: 0\.[1-9]|pressure: 1\.0' "libinput: pressure"
check "$OUT/libinput-debug-events.txt" 'tilt: -?[1-9][0-9]*\.[0-9]+' "libinput: tilt"
check "$OUT/libinput-debug-events.txt" 'rotation: [0-9]+\.[0-9]+' "libinput: rotation (roll)"
check "$OUT/libinput-debug-events.txt" 'distance: 0\.[0-9]*[1-9]' "libinput: distance (hover)"
check "$OUT/libinput-debug-events.txt" 'TABLET_TOOL_BUTTON.*(BTN_STYLUS2|332)' "libinput: stylus button 2"
check "$OUT/libinput-debug-events.txt" 'type eraser|eraser' "libinput: eraser tool"

# --- The finger touchscreen: a two-finger pinch, then a touch cancelled as
# a palm (the pencil came near). libinput must see a touchscreen, and the
# palm must arrive as a cancel, not a lift.
"$BIN" touch-demo --settle 3 > "$OUT/touch-demo.log" 2>&1 &
TDEMO=$!
TNODE=""
for _ in $(seq 1 50); do
  TNODE=$(grep -o '/dev/input/event[0-9]*' "$OUT/touch-demo.log" | head -1 || true)
  [ -n "$TNODE" ] && break
  sleep 0.1
done
if [ -z "$TNODE" ]; then
  echo "no event node reported by sidecard touch-demo" >&2
  cat "$OUT/touch-demo.log" >&2
  exit 1
fi
echo "touch node: $TNODE"
udevadm settle || true
udevadm info --query=property --name="$TNODE" > "$OUT/touch-udev.txt" || true
timeout 10 evtest "$TNODE" > "$OUT/touch-evtest.txt" 2>&1 &
TEVTEST=$!
timeout 10 libinput debug-events --device "$TNODE" > "$OUT/touch-libinput-debug-events.txt" 2>&1 &
TLIBINPUT=$!
sleep 0.5
libinput list-devices > "$OUT/touch-libinput-list-devices.txt" 2>&1 || true
grep -A14 'Device:.*Sidecar iPad Touch' "$OUT/touch-libinput-list-devices.txt" > "$OUT/touch-libinput-device.txt" || true
wait "$TDEMO"
sleep 0.5
kill "$TEVTEST" "$TLIBINPUT" 2>/dev/null || true
wait "$TEVTEST" "$TLIBINPUT" 2>/dev/null || true
echo "--- touch checks"
check "$OUT/touch-udev.txt" '^ID_INPUT_TOUCHSCREEN=1' "udev classifies the touch device as a touchscreen"
check "$OUT/touch-evtest.txt" 'Input device name: "Sidecar iPad Touch"' "evtest sees the touch device"
check "$OUT/touch-evtest.txt" 'code 53 \(ABS_MT_POSITION_X\), value [1-9]' "evtest: finger positions"
check "$OUT/touch-libinput-device.txt" 'Capabilities:.*touch' "libinput: touch capability"
check "$OUT/touch-libinput-debug-events.txt" 'TOUCH_DOWN' "libinput: touch down"
check "$OUT/touch-libinput-debug-events.txt" 'TOUCH_MOTION' "libinput: touch motion"
check "$OUT/touch-libinput-debug-events.txt" 'TOUCH_UP' "libinput: touch up"
check "$OUT/touch-libinput-debug-events.txt" 'TOUCH_CANCEL' "libinput: palm -> touch cancel"
exit $fail
