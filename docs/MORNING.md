# Morning checklist

Things only you can do, in priority order. Every block can be copied and pasted.
Status of everything is in [REPORT.md](REPORT.md). Read its "what to do first" section (2 minutes) before this one.

Each step says what to send back. Paste outputs into the chat, or open an issue.

---

## 0. Get the code (1 min)

```bash
git clone https://github.com/no-fy/sidecarforlinux.git ~/src/sidecarforlinux
cd ~/src/sidecarforlinux
```

## 1. Run the doctor (2 min) → **paste the output back**

It changes nothing and never uses sudo.

```bash
scripts/doctor.sh | tee ~/sidecar-doctor.txt
```

Run it from a terminal inside your Plasma Wayland session, ideally with the iPad plugged in and unlocked.

## 2. One-time system changes (5 min, + re-login only if uinput needs a group)

These are the system changes your machine needs. Nothing was applied remotely.

| Change | Why | Undo |
|---|---|---|
| `pacman -S --needed usbmuxd libimobiledevice evtest libinput-tools` (+ `rust` if `cargo` is missing) | USB transport to the iPad; diagnostics (on Arch the `libinput` command is in `libinput-tools`); building the daemon | `pacman -R ...` |
| New system group `uinput` | dedicated permission to create virtual input devices | `groupdel uinput` |
| `/etc/udev/rules.d/60-sidecarforlinux-uinput.rules`: `KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="uinput", MODE="0660", OPTIONS+="static_node=uinput"` | lets that group open `/dev/uinput` | delete the file |
| `/etc/modules-load.d/sidecarforlinux.conf` containing `uinput` | load the in-tree `uinput` module at boot (no DKMS involved) | delete the file |
| `usermod -aG uinput $USER` | your user may create the virtual pen | `gpasswd -d $USER uinput` |
| `usbmuxd.service` | **no change needed**: Arch's usbmuxd package starts it from udev when an Apple device is plugged in | |

**Security note:** members of `uinput` can inject input into your session. Only add your own desktop user.

**If `doctor.sh` already says `/dev/uinput is writable`**, the group, udev rule and module rows are skipped. Some other package (Steam, game-controller udev rules, ...) already gives your logged-in user access through a udev `uaccess` ACL (the `+` in `crw-rw----+`). In that case only the packages are installed and no re-login is needed. `--with-uinput-group` adds the group anyway, so access survives removing that package.

The script below shows exactly the commands it will run and asks before it runs anything:

```bash
scripts/setup-system.sh
# only if it added the uinput group: log out of Plasma and back in, then re-check:
scripts/doctor.sh | grep -A8 'uinput (virtual'
```

**t2linux notes:**
- Phase 1 needs **no DKMS module**: `uinput` is in the mainline kernel.
- DKMS (and therefore `linux-t2-headers` matching your running t2 kernel) only matters later for `v4l2loopback` (phase 3 webcam).
- `evdi` is not used (see DECISIONS D16).
- **Assumption:** the MacBook Pro's Thunderbolt 3 (USB-C) ports enumerate an iPad like any xHCI port under the t2linux kernel. If `idevice_id -l` shows nothing, try another port (left vs right side are different controllers) or another cable, and tell me which one works.

## 3. Build and install the daemon (5 min)

```bash
scripts/install-daemon.sh         # builds, installs ~/.local/bin/sidecard, asks before adding a user service
sidecard --version && sidecard info
```

## 4. Test the virtual tablet **without the iPad** (5 min) → **paste the output back**

This proves uinput, libinput, KWin and Krita work before the iPad is involved. CI already proved the uinput and libinput parts on Ubuntu.

```bash
sidecard gen-demo > /tmp/demo.jsonl
# Terminal 1: watch libinput (Ctrl-C when the replay ends)
sudo libinput debug-events --show-keycodes 2>&1 | grep -E 'TABLET|Sidecar' | tee ~/sidecar-libinput.txt
# Terminal 2: create the tablet and replay a stroke with pressure/tilt/roll/hover, a double-tap and a squeeze
sidecard replay /tmp/demo.jsonl --settle 5
```

(`sudo` is only for *reading* all input devices in libinput debug-events. It's optional; `scripts/ci-uinput-check.sh` does the same checks.)

**Heads-up:** while the replay runs, the virtual pen *is* your mouse cursor. It hovers, clicks and drags across the middle of the screen and right-clicks once (the squeeze). A click can take keyboard focus away from the terminal, so Ctrl-C may not reach it. That's why a replay always stops by itself: `--loop` repeats every 3 s for 30 s (`--pause`, `--for`). `pkill -x sidecard` from another terminal also stops it.

Then:
1. **Drawing Tablet settings.** This keeps the device alive for 60 s without moving the cursor:

   ```bash
   sidecard replay /tmp/demo.jsonl --settle 60
   ```

   Meanwhile open **System Settings → Drawing Tablet**. Is **"Sidecar Apple Pencil"** listed? The yellow note "This device is managed by a user-space driver…" is expected. KWin flags every `/dev/uinput` device as virtual, and the settings page shows that note for them. KWin's input handling ignores the flag. Map it to the screen you want, or put `[kde] output = "eDP-2"` in `~/.config/sidecarforlinux/config.toml` (output names come from `kscreen-doctor -o`). After 60 s it plays the stroke once and exits.
2. **Krita.** Open Krita, make it full screen with a new canvas and a pressure + tilt brush. Then run:

   ```bash
   sidecard replay /tmp/demo.jsonl --loop --settle 5
   ```

   Switch to Krita within the 5 s. You should see an S-stroke that thickens and thins, then a short eraser stroke, repeated every few seconds for 30 s.

   Krita "Settings → Configure Krita → Tablet settings → Tablet tester" shows pressure, tilt and rotation live: open it, start the same command, and draw nothing yourself.

Send back: whether the device shows up in Drawing Tablet, a screenshot of the Krita stroke or the tablet tester, and `~/sidecar-libinput.txt`.

## 5. Install the iPad app (15–30 min the first time)

The CI builds an **unsigned** `.ipa` on every push to `main`, published as the rolling **`nightly`** pre-release: <https://github.com/no-fy/sidecarforlinux/releases/tag/nightly>.

Pick **one** path per Apple ID. xtool and SideStore revoke each other's development certificate, which can make apps stop launching. If you want both, use two Apple IDs.

### Path A: xtool on Arch (re-sign from the PC every 7 days)

```bash
# 1. xtool (AppImage; needs FUSE: sudo pacman -S --needed fuse2)
mkdir -p ~/.local/bin && cd ~/.local/bin
curl -fLO https://github.com/xtool-org/xtool/releases/download/1.24.0/xtool-x86_64.AppImage
chmod +x xtool-x86_64.AppImage && ln -sf xtool-x86_64.AppImage xtool
xtool --version
# 2. Log in once with your Apple ID: normal password + 2FA code (a secondary Apple ID is recommended)
xtool auth login --mode password
```

- The AppImage name was checked against the 1.24.0 release assets on 2026-10-30.
- You do **not** need Xcode.xip or `xtool setup` just to sign and install a prebuilt `.ipa`.

On the iPad, once:
1. Plug it in, unlock it, tap **Trust**.
2. **Settings → Privacy & Security → Developer Mode → On** (it reboots).

Then:

```bash
cd ~/src/sidecarforlinux
scripts/resign-ipad.sh     # downloads the nightly .ipa, gets a 7-day profile, signs, installs
```

The first time, on the iPad: **Settings → General → VPN & Device Management →** your Apple ID → **Trust**.

**Every 7 days:** run `scripts/resign-ipad.sh` again with the iPad plugged in. It reinstalls over the existing app and keeps its data and pairing.

### Path B: SideStore (refreshes on the iPad itself)

1. Install **iloader** on Arch (<https://github.com/nab138/iloader>, 2.4.1+). It installs SideStore *and* writes the pairing file. Use **SideStore ≥ 0.7.0-alpha**; older builds can't sign in since Apple's September 2026 change.
2. On the iPad, install **LocalDevVPN** from the App Store.
3. In SideStore → **+** → paste `https://github.com/no-fy/sidecarforlinux/releases/download/nightly/SidecarPad.ipa`. SideStore signs it with your Apple ID.
4. Refresh happens on the iPad (Settings in SideStore → refresh, or automatically) while LocalDevVPN is connected. After some iPadOS updates you need the PC once more to place a new pairing file.

| | Path A: xtool | Path B: SideStore |
|---|---|---|
| Refresh every 7 days | from the PC, iPad plugged in | on the iPad, needs LocalDevVPN on |
| Slots (of 3 per free Apple ID) | 1 | 2 (SideStore + the app) |
| Apple ID password stored | on the PC (xtool token) | on the iPad (SideStore) |
| Moving parts | xtool AppImage | iloader, SideStore, LocalDevVPN, pairing file |
| Best for | the iPad is often plugged into the laptop anyway | the iPad leaves the desk for > 7 days |

Don't install the app into **LiveContainer**: it blocks the app's Bonjour service (Wi-Fi fallback).

## 6. Pair the iPad with the PC (2 min)

```bash
# iPad: open "Sidecar", keep it in the foreground, USB plugged in, unlocked.
systemctl --user stop sidecard 2>/dev/null   # only one sidecard may talk to the iPad
sidecard pair          # shows a 6-digit code; the iPad shows one too
```

If the codes match, accept on **both** (type `y` on the PC, tap "Codes match, pair" on the iPad). From then on `sidecard run` (or the user service) connects automatically whenever the app is open.

If it fails, send back the output of `sidecard -v pair` and a screenshot of the iPad. The status text is at the top left of the app.

## 7. Test with the real pencil in Krita (10 min) → **paste back**

```bash
systemctl --user stop sidecard 2>/dev/null
sidecard run --record ~/pencil-$(date +%F).jsonl     # Ctrl-C to stop
# in another terminal, while drawing/hovering:
sudo evtest  # pick "Sidecar Apple Pencil"; Ctrl-C after a few strokes (sudo only to read input devices)
```

Check each signal and note ✅/❌:
- [ ] pressure: light → heavy strokes (Krita tablet tester)
- [ ] tilt: tilt the pencil left/right/towards you (tester shows X/Y tilt)
- [ ] barrel roll: rotate the Pencil Pro (tester's "rotation"; brush with the Rotation sensor)
- [ ] hover: cursor follows above the glass, with no stroke
- [ ] double-tap: switches to Krita's eraser and back, and the pencil buzzes (haptic)
- [ ] squeeze: opens Krita's pop-up palette (right button)
- [ ] KDE System Settings → Drawing Tablet lists the device; mapping to your chosen screen works

Send back the `~/pencil-*.jsonl` recording too. It becomes a regression test fixture.

## 8. Measure USB latency (2 min) → **paste back**

```bash
systemctl --user stop sidecard 2>/dev/null; sidecard latency --duration 30 --json ~/sidecar-latency.json
# draw continuously during the 30 s
```

Paste the printed report. It goes into [latency.md](latency.md).

## 9. Optional: Wi-Fi fallback

1. Unplug USB. In the app's ⚙︎ menu, tap **Allow pairing over Wi-Fi (2 min)**. iPadOS asks for Local Network permission the first time.
2. Run `sidecard pair --transport wifi`.
3. Then `sidecard latency --transport wifi --duration 30` and paste it.

## 10. Optional: phase 2, virtual display (experimental, 5 min) → **paste back**

This needs the GStreamer bits:

```bash
sudo pacman -S --needed gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-plugin-va gst-plugin-pipewire libva-utils
```

1. Check that KWin can create the monitor (no iPad needed):

   ```bash
   sidecard display-probe --seconds 20 &
   sleep 3; kscreen-doctor -o | grep -A4 Virtual-ipad
   ```

   Expected: `virtual output Virtual-ipad: 2360x1640 @ scale 2 ... PipeWire node N`, and the output appears in System Settings → Display.
   - For the 13" model: `--width 2732 --height 2048`.
   - If KWin didn't offer the protocol, check that `~/.local/share/applications/sidecard.desktop` exists (`scripts/install-daemon.sh` writes it), then log out and back in.
2. **With the iPad (app open, paired):** first the test pattern, which proves the video path:

   ```bash
   systemctl --user stop sidecard 2>/dev/null
   sidecard run --video test-pattern      # a bouncing ball should fill the iPad; the pencil still works
   ```

3. Then the real thing: a virtual monitor sized like the iPad, captured, VA-API encoded, streamed, with the pen pinned to it.

   ```bash
   sidecard run --video virtual                         # VA-API
   # if that fails: echo -e '[video]\nencoder = "x264"' >> ~/.config/sidecarforlinux/config.toml
   ```

   Drag a window onto "Virtual-ipad" and draw on it with the pencil.

Paste the terminal output either way, especially any GStreamer error. If VA-API fails, also paste `vainfo`.

## 11. Optional: collaboration

InkLink (<https://github.com/RakeRaven/inklink>, MIT, 2 days old) has the same goal; see DECISIONS D1. If you want to propose joining forces, that's your call; I didn't contact anyone.

---

### If something is stuck

- `sidecard -vv run`: verbose logs.
- `journalctl --user -u sidecard -f`: logs of the user service.
- "connection refused" from usbmuxd means the app isn't open in the foreground on the iPad.
- "not paired" means run `sidecard pair`.
- The tablet works but lands on the wrong screen: set `[kde] output` or use System Settings → Drawing Tablet.
