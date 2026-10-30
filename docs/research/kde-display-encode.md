# Sidecar-for-Linux: KDE virtual display, capture, encode, and tablet mapping research

*Researched 2026-10-30 against live sources. Target: Arch Linux, KDE Plasma 6.8.x on Wayland. The newest tags are KWin `v6.8.2` (2026-10-27) and libinput `1.32.0` (2026-09-17). Hardware: AMD GPU with amdgpu and Mesa radeonsi VA-API. The daemon is written in Rust.*

Every code excerpt below is copied verbatim from the URL shown next to it. The `master`/`main` branches were current on 2026-10-30. Anything I could not check against a source is marked **UNVERIFIED** or **TO TEST**.

---

## 0. TL;DR

### Recommended Phase 2 architecture

```
            ┌──────────────────────────── Linux (Rust daemon "sidecard") ─────────────────────────────┐
 KWin       │ 1. Wayland client: zkde_screencast_unstable_v1 (v4+)                                     │
 (6.8.x)    │      stream_virtual_output_with_description(name="ipad", description="iPad Air 13″",     │
            │        width=2732, height=2048, scale=2.0, pointer=metadata(4))                          │
            │    → KWin creates output "Virtual-ipad" (60 Hz), it appears in Display Settings and in   │
            │      the Drawing Tablet KCM; it is destroyed when the stream is closed                   │
            │    → events: serial(object_serial) then created(node_id)                                 │
            │    Fallback: xdg portal ScreenCast types=VIRTUAL(4), cursor_mode=4, persist_mode=2,       │
            │      then negotiate SPA_FORMAT_VIDEO_size=2732x2048 (KWin ≥ 6.8 resizes the output)       │
            │ 2. PipeWire consumer (pipewire-rs 0.10): PW_KEY_TARGET_OBJECT=<serial>, DMA-BUF +        │
            │    modifiers (MANDATORY|DONT_FIXATE), SHM fallback, metas Header/Cursor/VideoDamage       │
            │    (+SyncTimeline). Drop buffers with SPA_CHUNK_FLAG_CORRUPTED (cursor-only updates) and  │
            │    send the cursor position/bitmap to the iPad on a side channel                          │
            │ 3. VA-API encode with zero copy: DMA-BUF → VAAPI surface → RGB→NV12 on the GPU (VPP) →    │
            │    H.264 (HEVC optional), no B-frames, CBR/low VBV, IDR on request, AUD + SPS/PPS on IDR  │
            │    (GStreamer vapostproc!vah264enc via gstreamer-rs, or libavcodec h264_vaapi like KPipeWire)│
            │ 4. Transport: TCP_NODELAY/QUIC/UDP over USB-C networking or Wi-Fi; one AU per message     │
            │ 5. Input: uinput pen device (see §E.5) + DBus Set outputName="Virtual-ipad"               │
            └──────────────────────────────────────────────────────────────────────────────────────────┘
 iPad:   Annex-B → AVCC, CMVideoFormatDescription from SPS/PPS, then AVSampleBufferDisplayLayer with
         DisplayImmediately (or VTDecompressionSession + Metal). Cursor is drawn locally as an overlay.
```

Why the `zkde_screencast` path is primary and the portal is the fallback:
- The daemon must open `/dev/uinput`, so it cannot be sandboxed anyway. With `zkde_screencast` it chooses the size, the **scale**, and a deterministic output name, and no dialog appears.
- KRdp uses the same path for `--virtual-monitor` (`--plasma`).
- KWin ≥ 6.8 hides the protocol only from sandboxed clients. KWin ≤ 6.7 requires `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1` in a `.desktop` file whose `Exec` resolves to the daemon binary.
- The portal path is standard and also works on GNOME (Mutter implements virtual monitors too). Its limits: the scale is fixed at 1, the output name contains the app id, and a dialog appears on first use.

### uinput pen device for libinput/KWin (details in §E.5)

| Item | Setting |
|---|---|
| Name / IDs | Stable name such as `"Sidecar iPad Pencil"`. `BUS_VIRTUAL` (or `BUS_USB`), with a fixed vendor/product. **Do not use Wacom VID 0x056a.** KWin keys its config on `[Libinput][vid][pid][name]`. |
| Props | `INPUT_PROP_DIRECT`. Do not set `INPUT_PROP_POINTER`. Do not use `BTN_0`/`BTN_1` (a separate pad device would be needed for those). |
| Keys | `BTN_TOOL_PEN` (required for proximity), `BTN_TOUCH`, `BTN_STYLUS` (optional `BTN_STYLUS2`, `BTN_TOOL_RUBBER`). Do not set `BTN_TOOL_FINGER` or `BTN_TOOL_MOUSE`. |
| `ABS_X` | min 0, max `W_px*S-1` (for example S=8 → 21855 on the 13″), **resolution > 0** in units/mm (`S*264/25.4`, so 83 for S=8). |
| `ABS_Y` | min 0, max `H_px*S-1` (16383), **same resolution** as X. With resolution 0, libinput drops the device. |
| `ABS_PRESSURE` | 0..4095 (or 0..65535), resolution 0. On contact send ≥ 6 % of the range: libinput's tip-down threshold is 5 % and tip-up is 1 %. Send 0 on lift. |
| `ABS_TILT_X/Y` | −9000..9000 with **resolution 5730** (units per radian, 0.01° units). libinput then converts with `rad2deg(value/resolution)`. Without a resolution, libinput maps the range linearly to ±64°. |
| `ABS_DISTANCE` | 0..1023 if you forward hover (Pencil hover is supported on the iPad Air M4). Having this axis also turns off libinput's pressure-offset heuristic. |
| `ABS_Z` | 0..359 for barrel roll (Apple Pencil Pro). libinput exposes it as rotation even without libwacom: `deg = ((v-min)/(max-min+1)*360 + 90) mod 360`, so send `v = (θ − 90) mod 360`. |
| Mapping | After both the device and the virtual output exist, call `org.freedesktop.DBus.Properties.Set("org.kde.KWin.InputDevice","outputName", <"Virtual-ipad">)` on `/org/kde/KWin/InputDevice/eventN`. Otherwise KWin maps the tablet to the **active output**. |

---

## A. A virtual monitor on KDE Plasma Wayland

### A.1 xdg-desktop-portal ScreenCast with source type VIRTUAL (4)

**Spec.** Source: https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.ScreenCast.xml (interface version 6)

```
        AvailableSourceTypes:
        - ``1``: MONITOR: Share existing monitors
        - ``2``: WINDOW: Share application windows
        - ``4``: VIRTUAL: Extend with new virtual monitor
...
        * ``types`` (``u``)  Bitmask of what types of content to record. Default is MONITOR.
        * ``cursor_mode`` (``u``) ... The default cursor mode is 'Hidden'.
        * ``persist_mode`` (``u``)
          - ``0``: Do not persist (default)
          - ``1``: Permissions persist as long as the application is running
          - ``2``: Permissions persist until explicitly revoked
        * ``restore_token`` (``s``) ... The restore token is invalidated after using it once.
...
      Since version 6 of this interface, the node ID in the stream tuple
      is deprecated for stream targeting. Consumers should prefer the
      ``pipewire-serial`` property and ``PW_KEY_TARGET_OBJECT``
```

Cursor modes are 1 = Hidden, 2 = Embedded, 4 = Metadata ("sent as PipeWire stream metadata").

**KDE support timeline.** Taken from the commits and first tags of https://invent.kde.org/plasma/xdg-desktop-portal-kde:

| Commit | Change | First release |
|---|---|---|
| `9ee83def` 2022-05-04 "wayland: Support virtual outputs when screensharing" | Adds a "virtual" choice to the dialog. The commit message says it creates a *FullHD output at 1x scale* that the user can place. | 5.25 |
| `7f134117` 2024-11-19 "Allow restoring virtual outputs" | Restore tokens now work for virtual outputs. | 6.3 |
| `d9a55b3b` 2025-07-22 "screencast: Wire up virtual output" | **Advertises VIRTUAL (4) in `AvailableSourceTypes`.** The message: "We can do virtual outputs since a long time but have never advertised it through the portal." | **6.5** |
| `5900c11d` 2026-08-14 "Allow the user to set a custom size for virtual screens" (FEATURE 485850) | Adds a resolution combobox to the dialog. | **6.8** |

Current implementation (master), from https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/screencast.h:

```cpp
    uint AvailableSourceTypes() const { return Monitor | Window | Virtual; };
    uint AvailableCursorModes() const { return Hidden | Embedded | Metadata; };
```

From `src/screencast.cpp`: the default size is 1920×1080 and the scale is hard-coded to 1 in `waylandintegration.cpp`.

```cpp
    QSize virtualScreenSize{1920, 1080};
...
        case Output::Virtual: {
            const QString outputName = session->appId().isEmpty()
                ? i18n("Virtual Output")
                : i18nc("%1 is the application name", "Virtual Output (shared with %1)", Utils::applicationName(session->appId()));
            stream =
                WaylandIntegration::startStreamingVirtual(OutputsModel::virtualScreenIdForApp(session->appId()), outputName, virtualScreenSize, cursorMode);
```

From `src/waylandintegration.cpp`:

```cpp
    return startStreaming(m_screencasting->createVirtualOutputStream(name, description, size, 1, mode));
```

From `src/outputsmodel.cpp`, the output name is derived from the app id:

```cpp
    const QString baseId = QStringLiteral("virtual-xdp-kde-") + appId;
```

KWin then prefixes it with `Virtual-`, giving `Virtual-virtual-xdp-kde-<appid>`.

**Resolution choices in the 6.8 dialog** (`src/PipeWireDelegate.qml`): 1600×1200, 1280×1024, 1024×768, 2560×1600, 1920×1200, 1280×800, 5120×2880, 3840×2160, 3200×1800, 2880×1620, 2560×1440, 1920×1080 (default), 1600×900, 1368×768, 1280×720. **Neither iPad native resolution is in the list.**

**The client can choose the size anyway on Plasma ≥ 6.8, through PipeWire negotiation.** KWin commit `452707eb` (2026-07-29, first tag v6.7.90, so 6.8) is titled "screencast: Resizable Virtual Monitors":

> When streaming a virtual monitor to a remote server, we want the client to be able to be in charge of the virtual monitor size. Pipewire has a path built in that allows the client and server to negotiate a size … Gnome uses this … It also allows the virtual monitor path via the xdg-desktop-portal to not need additional sidechannels.

Here is the KWin code that implements it. From https://invent.kde.org/plasma/kwin/-/raw/master/src/plugins/screencast/screencaststream.cpp:

```cpp
    constexpr spa_rectangle streamMinSize = SPA_RECTANGLE(200, 200);
    constexpr spa_rectangle streamMaxSize = SPA_RECTANGLE(10000, 10000);
    spa_rectangle minSize = m_source->followsStreamSize() ? streamMinSize : defaultSize;
    spa_rectangle maxSize = m_source->followsStreamSize() ? streamMaxSize : defaultSize;
...
    if (m_source && m_source->followsStreamSize()) {
        m_source->resize(negotiatedSize);
    }
```

From `outputscreencastsource.cpp`, `followsStreamSize()` returns `m_output->canResize()`. `DrmVirtualOutput::canResize()` returns `true`.

In practice, a portal consumer that offers `SPA_FORMAT_VIDEO_size = 2732×2048` (a fixed size or a narrow range) gets a 2732×2048 virtual output. The scale stays 1 unless the user changes it in Display Settings.

**Refresh rate.** The virtual output is created with a single 60 Hz mode (`drm_virtual_output.cpp`, quoted in §A.2). `resize()` recreates a 60 Hz mode as well.

**Lifetime.** The output exists only while the stream does. From `screencastmanager.cpp`:

```cpp
    auto output = kwinApp()->outputBackend()->createVirtualOutput(name, description, size, scale);
    streamOutput(stream, workspace()->findOutput(output), mode);
    connect(stream, &ScreencastStreamV1Interface::finished, output, [output] {
        kwinApp()->outputBackend()->removeVirtualOutput(output);
    });
```

When the portal session closes, the stream closes and the output is removed.

**It is a real output.** It appears in Display Settings and can be arranged there. `WaylandServer::handleOutputAdded` offers every non-placeholder, non-`NonDesktop` output through `kde_output_device_v2`, which KScreen uses:

```cpp
void WaylandServer::handleOutputAdded(BackendOutput *output)
{
    if (!output->isPlaceholder() && !output->isNonDesktop()) {
        m_outputDeviceRegistry->offer(output);
    }
}
```

The output is also a `wl_output`, so it shows up as a `QScreen` in the Drawing Tablet KCM (§D.3). KWin's `OutputConfigurationStore` persists output settings in `kwinoutputconfig.json`. For outputs without EDID it matches by connector name, so position and scale are *likely* remembered across sessions for the same name. **TO TEST.**

**Permission dialog behaviour** (`ScreenChooserDialog.qml`, `screenchooserdialog.cpp`):
- When `types == Virtual` the dialog text is "Create a new virtual display to share with %1:" and only one virtual monitor is allowed. If an app requests MONITOR, the dialog still offers "Create a virtual screen, then share".
- The "Allow the application to do this without asking next time" checkbox is **checked by default**. With `persist_mode=2`, the response contains a `restore_token`. KDE's restore payload stores `outputs: ["Virtual"]` and `virtualSize`. On the next `Start` with that token, no dialog appears. Tokens are single-use, so the new token from each `Start` must be stored.
- Unsandboxed apps need an app id. Either run as a systemd scope `app-<id>-*.scope`, or call `org.freedesktop.host.portal.Registry.Register(app_id, {})` before any portal call (https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.host.portal.Registry.xml): "Registering must be done before any portal method call".
- Rust: `ashpd` 0.13.13 has `SourceType::{Monitor=1, Window=2, Virtual=4}` (https://docs.rs/ashpd/latest/ashpd/desktop/screencast/enum.SourceType.html).

### A.2 KWin's `zkde_screencast_unstable_v1`

Protocol XML: https://invent.kde.org/libraries/plasma-wayland-protocols/-/raw/master/src/protocols/zkde-screencast-unstable-v1.xml (interface version 6).

```xml
        <description summary="Protocol for managing PipeWire feeds of the different displays and windows">
            Warning! The protocol described in this file is a desktop environment
            implementation detail. Regular clients must not use this protocol.
            Backward incompatible changes may be added without bumping the major
            version of the extension.
        </description>
        <enum name="pointer">
            <entry name="hidden" value="1" summary="No cursor"/>
            <entry name="embedded" value="2" summary="Render the cursor on the stream"/>
            <entry name="metadata" value="4" summary="Send metadata about where the cursor is through PipeWire"/>
        </enum>
        <request name="stream_virtual_output" since="2">
            <arg name="stream" type="new_id" interface="zkde_screencast_stream_unstable_v1"/>
            <arg name="name" type="string" summary="name of the created output"/>
            <arg name="width" type="int" summary="Logical width resolution"/>
            <arg name="height" type="int" summary="Logical height resolution"/>
            <arg name="scale" type="fixed" summary="Scaling factor of the display where it's to be displayed"/>
            <arg name="pointer" type="uint" summary="Requested pointer mode"/>
        </request>
        <request name="stream_virtual_output_with_description" since="4">
            ... name, description ("user visible description of the created output"), width, height, scale, pointer
        </request>
    <interface name="zkde_screencast_stream_unstable_v1" version="6">
        <request name="close" type="destructor"/>
        <event name="closed"/>
        <event name="created" deprecated-since="6"> <arg name="node" type="uint"/> </event>
        <event name="failed"> <arg name="error" type="string"/> </event>
        <event name="serial" since="6"> object_serial_hi, object_serial_low  (sent before created)
```

**Version history.** These are KWin commits on `src/wayland/screencast_v1.cpp`, with the first tag that contains each:

| Version | Content | KWin commit | First Plasma |
|---|---|---|---|
| 2 | `stream_virtual_output` | `efc77310` 2021-10-22 | 5.25 (tag v5.24.90) |
| 3 | `stream_region` | `124c78cb` 2022-03-15 | 5.25 |
| 4 | `stream_virtual_output_with_description` | `ae04e36c` 2024-11-19 | 6.3 (v6.2.90) |
| 5 | region scale 0 means automatic | `24852040` 2025-03-18 | 6.4 (v6.3.90) |
| 6 | `serial` event (PipeWire object serial) | `526b0e4c` 2026-05-02 | 6.7 (v6.6.90) |

KWin master advertises `static int s_version = 6;`.

**What KWin does with the arguments.** `width`/`height` become the **pixel mode size**, even though the XML calls them "Logical". The scale becomes the output scale. KRdp passes `1920x1080@1`-style values. From https://invent.kde.org/plasma/kwin/-/raw/master/src/backends/drm/drm_virtual_output.cpp:

```cpp
    auto mode = std::make_shared<OutputMode>(OutputModeline(size, 60000, OutputModeline::Flag::Preferred));
    m_renderLoop->setRefreshRate(mode->refreshRate());
    setInformation(Information{
        .name = QStringLiteral("Virtual-") + name,
        .model = description,
        .physicalSize = size,
        .capabilities = capabilities,
    });
    setState(State{ .scale = scale, .modes = {mode}, .currentMode = mode, });
```

From `drm_backend.cpp`:

```cpp
    const auto ret = new DrmVirtualOutput(this, name, description, size, scale, BackendOutput::Capability::CustomModes);
```

The output therefore has a fixed 60 Hz mode, the name `Virtual-<name>`, and the description as its model, which is the user-visible name.

The output advertises the `CustomModes` capability (KWin commit `d918731b`, 2026-02-13, "add custom mode support to the virtual outputs", Plasma 6.7). `kscreen-doctor` in libkscreen supports `output.<name>.addCustomMode.<w>.<h>.<refresh_mHz>[.reduced|full]` (`src/doctor/doctor.cpp`). So a **120 Hz mode on the virtual output should be possible**. The screencast stream's `maxFramerate` comes from `m_source->refreshRate()`, so it would also allow 120 fps capture. Note that `resize()` resets the output to a single 60 Hz mode. **TO TEST.**

**Who may bind it.**
- **KWin ≥ 6.8 (master):** only *sandboxed* clients are denied. From https://invent.kde.org/plasma/kwin/-/raw/master/src/wayland_server.cpp:

  ```cpp
      const QSet<QByteArray> restrictedInterfaces = {
          QByteArrayLiteral("org_kde_plasma_window_management"),
          QByteArrayLiteral("org_kde_kwin_fake_input"),
          QByteArrayLiteral("zkde_screencast_unstable_v1"),
          ...
      };
  ...
          if (client->isSandboxed()) {
              return !restrictedInterfaces.contains(interfaceName);
          }
          return true;
  ```

  Commit `f9bf0ee6` (2026-06-18, first tag v6.7.90) is "Skip announcing restricted interfaces only to sandboxed clients": "The desktop files can be changed or overriden by the user, which negates all the benefits … a better approach … is to use the security context protocol." Commit `97b5274a` determines sandboxing from the cgroup (Flatpak/Snap).
- **KWin ≤ 6.7:** the Plasma/6.7 branch `wayland_server.cpp` has `interfacesBlackList` containing `zkde_screencast_unstable_v1`, plus a lookup of `X-KDE-Wayland-Interfaces` from the `.desktop` file whose `Exec` canonical path equals the client's executable (`utils/serviceutils.h`, `fetchProcessServiceField`). `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1` disables the check, which is useful for development. KRdp ships `server/org.kde.krdpserver.desktop.cmake` with `Exec=@CMAKE_INSTALL_PREFIX@/bin/krdpserver` and `X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1`.
- In both cases a Flatpak build cannot use it.

**How KRdp uses it** (https://invent.kde.org/plasma/krdp):
- `server/main.cpp` has the option `--virtual-monitor`: "Creates a new virtual output to connect to (WIDTHxHEIGHT@SCALE, e.g. 1920x1080@1). Incompatible with --monitor." It also has `--mode RemoteAccess|SharedAccess|AdditionalDisplay` and `--plasma` ("Use Plasma protocols instead of XDP").
- In `SessionController.cpp`, without explicit options, `AdditionalDisplay`/`RemoteAccess` create `VirtualMonitor{name=i18n("Remote Monitor"), dpr=1, size=1024x768}`.
- The XDP path is `PortalSession.cpp`: `if (virtualMonitor()) parameters = {{"types", 4u}}; // VIRTUAL`.
- The Plasma path is `PlasmaScreencastV1Session.cpp`:

  ```cpp
      if (auto vm = virtualMonitor()) {
          d->request.reset(d->m_screencasting.createVirtualMonitorStream(vm->name, vm->size, vm->dpr, Screencasting::Metadata));
          // KWin prefixes virtual output names with "Virtual-"
          d->mappingId = "Virtual-"_L1 + vm->name;
  ```

- For input, KRdp connects to KWin EIS over DBus with `org.kde.KWin.EIS.RemoteDesktop.connectToEIS(7)` and uses libei. **KWin's EIS devices do not support tablets**. From `src/plugins/eis/eisdevice.cpp`: `bool EisDevice::isTabletTool() const { return false; }`. Pen pressure and tilt therefore need uinput.

**KPipeWire** (https://invent.kde.org/plasma/kpipewire) is a Qt6/C++ library made of `KPipeWire`, `KPipeWireRecord`, and `KPipeWireDmaBuf`. Its license is `LGPL-2.1-only OR LGPL-3.0-only OR LicenseRef-KDE-Accepted-LGPL`.
- `PipeWireSourceStream` handles DMA-BUF negotiation, filtering modifiers through VA-API support (`vaapi->supportsModifier(drm_format, modifier)`), plus cursor and damage metas.
- `PipeWireEncodedStream` emits encoded packets. Its encoders are `VP8, VP9, H264Main, H264Baseline, WebP, Gif`, using libav (`h264_vaapi`, libx264, libopenh264, libvpx).
- The VA-API H.264 encoder is in `src/h264vaapiencoder.cpp`:

  ```cpp
      const auto filterGraph = std::format("hwmap=mode=direct:derive_device=vaapi,scale_vaapi=format=nv12:mode=fast:out_range={}", colorRange);
  ...
      auto codec = avcodec_find_encoder_by_name("h264_vaapi");
      m_avCodecContext->max_b_frames = 0;
      m_avCodecContext->gop_size = 100;
      m_avCodecContext->pix_fmt = AV_PIX_FMT_VAAPI;
      m_avCodecContext->time_base = AVRational{1, 1000};
      if (m_quality) { m_avCodecContext->global_quality = percentageToAbsoluteQuality(m_quality); } else { m_avCodecContext->global_quality = 35; }
      ... profile = AV_PROFILE_H264_CONSTRAINED_BASELINE / MAIN / HIGH
  AVDictionary *H264VAAPIEncoder::buildEncodingOptions()
      if (m_encodingPreference == PipeWireBaseEncodedStream::EncodingPreference::Speed) {
          // a depth of >1 can leave a pending frame queued indefinitely.
          av_dict_set_int(&options, "async_depth", 1, 0);
      }
      // Disable motion estimation, not great while dragging windows but speeds up encoding by an order of magnitude
      av_dict_set(&options, "flags", "+mv4", 0);
      // Disable in-loop filtering
      av_dict_set(&options, "-flags", "+loop", 0);
  ```

  So the low-latency settings are: input as DRM_PRIME, mapped zero-copy to VAAPI, converted to NV12 on the GPU; no B-frames; `async_depth=1`; quality-based rate control (`global_quality` gives CQP), not CBR; GOP 100. `libx264encoder.cpp` with `Speed` uses `preset=ultrafast` and `tune=zerolatency`. KRdp uses `H264Baseline`, `EncodingPreference::Speed`, `ColorRange::Full`, and `setMaxPendingFrames(fps)` (`src/VideoStreamSurface.cpp`).
- **Reuse from Rust is impractical.** The API is QObject-based (signals, a Qt event loop, QImage/QSize types), so you would need a C++ shim over cxx-qt and would bring in Qt6 and KF6. It is better to treat KPipeWire as a reference implementation and redo the roughly 300 lines involved with `pipewire-rs` plus libav or GStreamer. The LGPL would permit dynamic linking if needed.

### A.3 Other options and comparison

**iPad Air native resolutions.** The newest models are the iPad Air M4 (2026). From https://www.apple.com/ipad-air/specs/:
- 11″: **2360×1640 px at 264 ppi**, 500 nits.
- 13″: **2732×2048 px at 264 ppi**, 600 nits.
- Both support Apple Pencil Pro, Apple Pencil (USB-C), and **Apple Pencil hover**.
- Media engine: H.264, HEVC, and ProRes, plus AV1 decode.
- There is no ProMotion; the display runs at 60 Hz.

Logical size at scale 2: 1180×820 (11″) and 1366×1024 (13″).

**Other options:**

1. **KWin virtual output through `zkde_screencast`** (§A.2), recommended.
2. **Portal VIRTUAL** (§A.1), the fallback.
3. **DBus API in KWin to create virtual outputs.** None exists. I enumerated `src/` (1,692 files): `createVirtualOutput` is only called by the screencast plugin. KWin's `--virtual` backend is a whole headless *session* backend (`src/backends/virtual/`), and it cannot add outputs to a running DRM session. There is no "Virtual display" setting in KWin or System Settings. KRdp's "AdditionalDisplay" mode is the only UI-level feature, and it uses option 1 or 2.
4. **Forcing a connector with an EDID override** on an unused AMD port:
   - Kernel parameters: `video=<connector>:<mode>e` ("'e' will force the display to be enabled, i.e. it will override the detection" — https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/fb/modedb.rst). Also `drm.edid_firmware=[<connector>:]<file>`, which loads from `/lib/firmware`; see `Documentation/admin-guide/kernel-parameters.txt`. The EDID file must be in the initramfs if amdgpu loads early.
   - Needs root, a bootloader edit, and a reboot. It uses a real CRTC and connector.
   - On the Mac Pro MPX modules the ports are TB3/HDMI, so a free one may not exist. A forced DP connector without a sink can fail link training, so prefer HDMI. **TO TEST.**
   - Benefits: true refresh rates (120 Hz possible), standard behaviour everywhere. You still capture it through the portal or `zkde` `stream_output`.
5. **evdi (DisplayLink)** from AUR `evdi-dkms` 1.15.1:
   - "DisplayLink has verified the module compiles and works with Ubuntu variants of kernels up to 6.15 … newer kernels … may require extra development" (https://github.com/DisplayLink/evdi README).
   - DKMS must build against `linux-t2-headers` (t2linux Arch installs `linux-t2 linux-t2-headers`, per https://wiki.t2linux.org/distributions/arch/installation/), and it breaks on every t2 kernel bump.
   - Display-only DRM device: KWin uses its multi-GPU copy path, and a libevdi client must "plug" an EDID and pull the pixels. This adds a CPU copy, which is bad for latency at 2732×2048. It needs root to install. Not recommended.
6. **vkms** (in-tree virtual KMS) has similar drawbacks: a software DRM device and the multi-GPU copy path.
7. **GNOME-style headless outputs.** Mutter has `org.gnome.Mutter.ScreenCast` `RecordVirtual` (https://gitlab.gnome.org/GNOME/mutter/-/raw/main/data/dbus-interfaces/org.gnome.Mutter.ScreenCast.xml). It is GNOME-only, and the portal VIRTUAL type maps to it there.

| Option | No root | Robustness | Added latency | Exact iPad res + scale | Shows in Display Settings + Drawing Tablet KCM |
|---|---|---|---|---|---|
| zkde `stream_virtual_output_with_description` | ✅ (≤ 6.7 needs a `.desktop` entry) | Good. KRdp uses it; the protocol is unstable but versioned. | Software vsync at 60 Hz (120 possible with a custom mode) | ✅ size and scale | ✅ |
| Portal VIRTUAL | ✅ | Good and standard. One dialog, then restore token. | Same as above | Size ✅ (≥ 6.8 via PipeWire size negotiation; ≤ 6.7 preset list only). Scale = 1. | ✅ |
| EDID-forced connector | ❌ root + reboot | Fragile (needs a free port, link training) | Real CRTC, same capture path | ✅ via the EDID | ✅ |
| evdi DKMS | ❌ root, DKMS on the t2 kernel | Poor (kernel ≤ 6.15 verified) | Extra copy | ✅ via the EDID | ✅ |
| vkms | ❌ modprobe | Medium | Extra copy | Partly | ✅ |

**Recommendation:** use option 1 with option 2 as a fallback.
- It needs no root and no kernel changes, works with the t2 kernel, and gives exact size and scale.
- The output name is deterministic, which matters for tablet mapping and for KWin remembering its placement.
- It is destroyed automatically when the daemon dies.
- It is the same code path KDE's own KRdp uses.
- Latency is identical to the portal because both use the same KWin `ScreenCastStream`.

---

## B. Capturing the output with low latency

### B.1 What KWin offers (producer side)

From https://invent.kde.org/plasma/kwin/-/raw/master/src/plugins/screencast/screencaststream.cpp:
- KWin is the **driver**: `pw_stream_connect(..., PW_STREAM_FLAG_DRIVER | PW_STREAM_FLAG_ALLOC_BUFFERS)`. It produces a frame when the output repaints, so frames are damage-driven. Frames are paced by a per-stream `VsyncSource` at the negotiated `max_framerate`, which defaults to the output refresh rate.
- `VsyncSource::arm()` aligns to the next `1/refresh` tick (`alignTimestamp(...)` in `src/utils/vsyncsource.cpp`), which adds about half a frame of latency on average.
- **Formats:** one EnumFormat with the output's DRM format and modifiers (`SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE`), then an SHM fallback with `ARGB8888` (BGRA, with BGRx offered as an equivalent). The table also contains NV12 and RGB888, but those are only used when the source's format is one of them. Expect BGRx/BGRA DMA-BUFs.
- **Buffers:** 2..4 (3 by default). With explicit sync it negotiates `SPA_META_SyncTimeline` with `blocks = planes + 2`. Metas: `SPA_META_Cursor`, `SPA_META_VideoDamage` (≤ 16 rects), `SPA_META_Header`.
- **Cursor metadata mode.** Cursor-only updates are queued as buffers whose chunk is flagged CORRUPTED:

  ```cpp
      if (effectiveContents & Content::Video) {
          spa_data->chunk->flags = SPA_CHUNK_FLAG_NONE;
      } else {
          // in pipewire terms, corrupted means "do not look at the frame contents" and here they're empty.
          spa_data->chunk->flags = SPA_CHUNK_FLAG_CORRUPTED;
      }
  ```

  **Your consumer must not encode those buffers.** Only update the cursor from `spa_meta_cursor` (id, position, hotspot, and an optional `spa_meta_bitmap`; see `spa/include/spa/buffer/meta.h`). In Embedded mode KWin renders the cursor into the frame (`setRenderCursor`). In Hidden mode there is no cursor.

### B.2 Consumer-side DMA-BUF negotiation

The official procedure is in https://gitlab.freedesktop.org/pipewire/pipewire/-/raw/master/doc/dox/internals/dma-buf.dox:

> The stream parameters should contain two SPA_PARAM_EnumFormat objects for each format: one for DMA-BUFs, one for shared memory buffers as a fallback. Query the list of all supported modifiers from your graphics API of choice. Add a SPA_FORMAT_VIDEO_modifier property to the first stream parameter with the flags `SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE` … plus `DRM_FORMAT_MOD_INVALID` if the graphics API supports modifier-less buffers.
>
> When the `param_changed` hook is called for a SPA_PARAM_Format … Use `spa_pod_find_prop(param, NULL, SPA_FORMAT_VIDEO_modifier)` to check whether modifiers were negotiated. If they were negotiated, set the SPA_PARAM_BUFFERS_dataType property to `1 << SPA_DATA_DmaBuf`.
>
> Consumers should ignore the field `maxsize` of a `spa_data` and the field `size` of a `spa_chunk` … n_datas might also contain extra fds for things like sync_timelime metadata.

KPipeWire's consumer (`src/pipewiresourcestream.cpp`) is a good template:
- It offers `SPA_FORMAT_VIDEO_size` as a range around the default and `maxFramerate`.
- The modifiers are the VA-API-supported ones, with MANDATORY|DONT_FIXATE.
- On `param_changed` it sets `buffers = CHOICE_RANGE(3,2,16)` and `dataType = DmaBuf|MemFd|MemPtr`, and requests the Header meta, the Cursor meta (sized `CURSOR_META_SIZE(64,64)`, range 1×1..1024×1024), and optionally VideoDamage.
- It renegotiates and drops a modifier if importing it fails (`renegotiateModifierFailed`).

For the size: on KWin ≥ 6.8 virtual outputs, offering `SPA_FORMAT_VIDEO_size` fixed to the iPad resolution resizes the output (§A.1). Set `maxFramerate` to 60/1.

### B.3 Rust building blocks

| Need | Crate (crates.io, 2026-10-30) | Notes |
|---|---|---|
| PipeWire consumer | `pipewire` 0.10.1 and `libspa` 0.10.1 (https://gitlab.freedesktop.org/pipewire/pipewire-rs) | Full control: build pods for EnumFormat with modifiers, handle `param_changed`, read the cursor meta, skip CORRUPTED chunks, connect with `PW_KEY_TARGET_OBJECT=<serial>` to the default daemon (zkde path) or `pw_context_connect_fd` with the portal fd. |
| GStreamer | `gstreamer` / `gstreamer-video` 0.25.4 | `pipewiresrc` (in the PipeWire repo, `src/gst/gstpipewiresrc.c`) has properties `path`, `target-object`, `fd`, `always-copy`, `min-buffers`, `max-buffers`, `resend-last`, `keepalive-time`, `autoconnect`, `use-bufferpool`, `on-disconnect`, `provide-clock`. It supports DMA_DRM caps (`HAVE_GSTREAMER_DMA_DRM`). Caveats: **the cursor meta is exposed only as a `GstVideoRegionOfInterestMeta "cursor"` with the position and no bitmap**, and CORRUPTED chunks are pushed downstream as `GST_BUFFER_FLAG_CORRUPTED`, so add a pad probe that drops them or use Embedded cursor mode. |
| Portal | `ashpd` 0.13.13 | `SourceType::Virtual`, `CursorMode`, `PersistMode`, `open_pipe_wire_remote()`. |
| Wayland `zkde_screencast` | `wayland-client` 0.31.15 and `wayland-scanner` 0.31.11 | Generate bindings from the plasma-wayland-protocols XML. |
| DBus (KWin InputDevice, portal registry) | `zbus` 6.2.0 | |
| uinput | `evdev` 0.13.2 | `VirtualDeviceBuilder::{with_absolute_axis, with_properties, with_keys, with_msc, input_id, name}`; `AbsInfo` has `resolution`. |
| libav VA-API | `ffmpeg-next` 9.0.0 | Mirrors KPipeWire's filter graph. |

**Cursor strategy for a pen display.** Request the metadata mode (4). Draw the cursor **on the iPad** as a local overlay using the received position and bitmap. While the Pencil is in contact or hovering, place it at the Pencil's location immediately. This removes a whole round trip from perceived cursor latency. Never encode cursor-only (CORRUPTED) buffers.

---

## C. Encoding on AMD and decoding on iOS

*Checked against Mesa main, GStreamer main, FFmpeg master, kernel.org, and Apple's documentation JSON on 2026-10-30. A delegated research pass produced this section. I re-checked the claims marked ✔︎ myself against the source.*

### C.0 Summary

| Topic | Recommendation |
|---|---|
| Hardware API | VA-API on Mesa radeonsi. It exposes **only `VAEntrypointEncSlice`**, so `vah264lpenc`, `vah265lpenc`, and FFmpeg `-low_power 1` are not available on AMD. |
| Codec | H.264 High/Main with **0 B-frames** works on every Mac Pro GPU. HEVC is optional; no AMD block does HEVC B-frames. None of these GPUs can encode AV1 (that needs VCN 4.0+). |
| Rate control | CBR with a VBV/CPB of about 1–2 frame durations. radeonsi offers CQP, CBR, VBR, and QVBR (QVBR only on newer VCN firmware). |
| FFmpeg pitfall ✔︎ | `h264_vaapi` defaults to `bf=2`, `g=120`, `async_depth=2`. Use `-bf 0 -async_depth 1`. |
| GStreamer pitfall ✔︎ | VA elements have rank NONE, so name them explicitly. The default `target-usage=4` selects radeonsi's **QUALITY** preset; use 2 or 3 (BALANCE without pre-encode). A live upstream gives 0 frames of encoder delay. |
| Mesa knob ✔︎ | `AMD_DEBUG=lowlatencyenc` ("Enable low latency encoding"), Mesa ≥ 24.2, VCN only. |
| Intra refresh | Implemented in radeonsi through VA (`VAEncMiscParameterRIR`, Mesa ≥ 24.0). Neither FFmpeg's nor GStreamer's VA encoders send it, so using it needs direct libva code. |
| iOS | Annex-B → 4-byte AVCC, then `CMVideoFormatDescription`, then either `VTDecompressionSession` (without TemporalProcessing or 1xRealTime) or `AVSampleBufferDisplayLayer` with `DisplayImmediately`. The iPad Air runs at 60 Hz. |

### C.1 GStreamer `va` plugin

Sources:
- https://gitlab.freedesktop.org/gstreamer/gstreamer/-/raw/main/subprojects/gst-plugins-bad/sys/va/gstvah264enc.c, plus `gstvah265enc.c`, `gstvaencoder.c`, `gstvabaseenc.c`, `gstvacaps.c`, and `plugin.c`
- https://gstreamer.freedesktop.org/documentation/va/vah264enc.html

**`vah264enc` properties:**

| Property | Type / range | Default | Notes |
|---|---|---|---|
| `key-int-max` | 0–1024 | 0 | 0 means the encoder picks the GOP size. |
| `b-frames` | 0–31 | **0** | |
| `i-frames` | 0–1023 | 0 | |
| `ref-frames` | 0–16 | 3 | The driver clamps it; radeonsi uses 1 L0 reference. |
| `b-pyramid` | bool | FALSE | |
| `num-slices` | 1–200 | 1 | |
| `max-qp` / `min-qp` | 0–51 | 51 / 1 | |
| `qpi` / `qpp` / `qpb` | 0–51 | 26 | |
| `dct8x8` | bool | TRUE | radeonsi supports 8×8 transform only on VCN5. |
| `cabac` | bool | TRUE | |
| `trellis` | bool | FALSE | |
| `aud` | bool | FALSE | Needs packed raw-data support. |
| `cc-insert` | bool | TRUE | |
| `mbbrc` | enum auto/on/off | auto | |
| `bitrate` | kbps, 0–2048000 | 0 | Can be changed while PLAYING. |
| `target-percentage` | 50–100 | 66 | VBR only. |
| `target-usage` | 1–7 | 4 | |
| `cpb-size` | Kb | 0 | 0 means "caching 2 second coded data". |
| `rate-control` | enum | `cbr` | Lists only the modes the driver reports. |

`vah265enc` has a similar set (`ref-frames` 0–15) plus `num-tile-cols` and `num-tile-rows`. It has no `cabac`, `dct8x8`, or `cc-insert`.

**Rate-control enum** (`gstvaencoder.c`):

```c
  {VA_RC_CBR, "Constant Bitrate", "cbr"}, {VA_RC_VBR, "Variable Bitrate", "vbr"},
  {VA_RC_VCM, "Video Conferencing Mode (Non HRD compliant)", "vcm"}, {VA_RC_CQP, "Constant Quantizer", "cqp"},
  {VA_RC_ICQ, "Intelligent Constant Quality", "icq"}, {VA_RC_QVBR, "Quality defined VBR", "qvbr"},
```

**There is no "low-latency" property.** Latency follows from whether the upstream is live (`gstvabaseenc.c` and `gstvah264enc.c`):

```c
  /* in case live streaming, we should run on low-latency mode */
  ...
  if (base->is_live) { base->preferred_output_delay = 0; } else { base->preferred_output_delay = 4; }
  latency_num = base->preferred_output_delay + self->gop.ip_period - 1;
```

So with a live `pipewiresrc` and `b-frames=0`, the encoder adds 0 frames of delay. A `force-key-unit` event starts a new GOP with an IDR.

**What "lp" means.** It refers to `VAEntrypointEncSliceLP`, libva's "low power/high performance variant for slice level encode". Mesa only adds `EncSlice` (`src/gallium/frontends/va/config.c`):

```c
   if (p != PIPE_VIDEO_PROFILE_AV1_MAIN || check_av1enc_support == true)
      if (vl_codec_supported(pscreen, p, true))
         entrypoint_list[(*num_entrypoints)++] = VAEntrypointEncSlice;
```

As a result, radeonsi never registers `vah264lpenc` or `vah265lpenc`.

**Zero-copy input.**
- `gstvacaps.c` builds sink caps for `memory:VAMemory`, for `memory:DMABuf` (DMA_DRM, when the driver supports DRM PRIME import), and for raw buffers.
- `gstvabaseenc.c` imports DMA_DRM caps through `gst_va_dmabuf_allocator_new`.
- DMA_DRM caps arrived in GStreamer 1.24.
- The encoder's input format on radeonsi is **NV12** (P010 for HEVC Main10), so KWin's BGRx buffers need `vapostproc` (VA VPP) first.

Suggested pipeline. The values are engineering judgment and have not been run:

```
pipewiresrc fd=$FD target-object=$SERIAL do-timestamp=true ! video/x-raw(memory:DMABuf),format=DMA_DRM !
  vapostproc ! video/x-raw(memory:VAMemory),format=NV12 !
  vah264enc rate-control=cbr bitrate=20000 cpb-size=333 key-int-max=1024 b-frames=0 ref-frames=1
            target-usage=3 aud=false cc-insert=false !
  video/x-h264,stream-format=byte-stream,alignment=au ! appsink sync=false
```

Add a pad probe that drops `GST_BUFFER_FLAG_CORRUPTED` buffers (see §B).

### C.2 FFmpeg `h264_vaapi` / `hevc_vaapi`

Sources:
- https://ffmpeg.org/ffmpeg-codecs.html#VAAPI-encoders
- `libavcodec/vaapi_encode_h264.c` ✔︎, `hw_base_encode.h` ✔︎, `hw_base_encode.c`, `vaapi_encode.c`, `hw_base_encode_h264.c` (FFmpeg master)

Verified defaults:

```c
static const FFCodecDefault vaapi_encode_h264_defaults[] = {
    { "b",              "0"   },
    { "bf",             "2"   },
    { "g",              "120" },
...
    { "async_depth", "Maximum processing parallelism. " "Increase this to improve single channel performance.",
      OFFSET(common.base.async_depth), AV_OPT_TYPE_INT, { .i64 = 2 }, 1, MAX_ASYNC_DEPTH, FLAGS }
```

**Options:**

| Option | Values | Notes |
|---|---|---|
| `-bf` | | Default 2; set 0. |
| `-g` | | Default 120. |
| `-idr_interval` | | |
| `-b_depth` | | |
| `-async_depth` | 1–64 | Default 2. Mesa implements `vaSyncBuffer`, so FFmpeg uses async encode and holds output until `async_depth` frames are in flight. **Set 1.** |
| `-low_power` | | Not usable on radeonsi. |
| `-rc_mode` | auto/CQP/CBR/VBR/ICQ/QVBR/AVBR | |
| `-max_frame_size` | | |
| `-qp` | | Forces CQP. |
| `-quality` | | Becomes `VAEncMiscParameterBufferQualityLevel`; see C.3. `0` selects the SPEED preset. |
| `-profile` | constrained_baseline/main/high | |
| `-coder` | cavlc/cabac | |
| `-aud` | | |
| `-sei` | identifier/timing/recovery_point/a53_cc | |

**Rate-control auto-selection.** If bitrate equals maxrate, FFmpeg picks CBR. With a bitrate alone it picks VBR. With no bitrate it picks ICQ, then CQP; radeonsi lacks ICQ, so it ends up on CQP. When `bufsize` is not set, the HRD buffer defaults to about 1 second.

**Other behaviour:**
- `frame->pict_type = AV_PICTURE_TYPE_I` forces an IDR.
- With `bf=0` the SPS VUI is written with `bitstream_restriction_flag=1`, `max_num_reorder_frames=0`, and `max_dec_frame_buffering=1`.

Test command:

```sh
ffmpeg -init_hw_device vaapi=va:/dev/dri/renderD128 -filter_hw_device va \
  -re -f lavfi -i testsrc2=size=2360x1640:rate=60 -vf 'format=nv12,hwupload' \
  -c:v h264_vaapi -profile:v high -bf 0 -g 3600 -async_depth 1 \
  -rc_mode CBR -b:v 20M -maxrate 20M -bufsize 333k -quality 0 -aud 0 -sei 0 -f h264 pipe:1
```

In the daemon, follow KPipeWire's approach:
- Import the PipeWire DMA-BUF as `AV_PIX_FMT_DRM_PRIME`.
- Map it with `hwmap=mode=direct:derive_device=vaapi`.
- Convert with `scale_vaapi=format=nv12`.

`ffmpeg-next` 9.0.0 has safe setters (`set_max_b_frames`, `set_gop`, `set_bit_rate`, `set_max_bit_rate`, `open_with(Dictionary)`) but **no safe hwdevice/hwframes API**. Use `ffmpeg-sys-next` for `av_hwdevice_ctx_create`, `av_hwframe_ctx_init`, and `av_hwframe_map`.

### C.3 Mesa radeonsi encode capabilities

The code now lives in `src/amd/common/ac_video.c` ✔︎, `src/gallium/drivers/radeonsi/mm/*`, and `src/gallium/frontends/va/*` ✔︎.

**Profiles:**
- H.264 ConstrainedBaseline/Main/High.
- HEVC Main, Main10 (VCN ≥ 2.0), and MainStill.
- AV1 Main only on VCN ≥ 4.0.
- Packed headers are supported, so FFmpeg and GStreamer write their own SPS/PPS/VUI.

**Rate control** (`frontends/va/config.c`): the driver reports `VA_RC_CQP | VA_RC_CBR | VA_RC_VBR` ("Legacy behavior reports these three modes for all drivers"), plus QVBR when `qvbr_supported()` passes (VCN3 firmware ≥ 30, VCN4 ≥ 15, VCN5 ≥ 3). It has no ICQ, AVBR, or VCM.

**B-frames ✔︎** (`ac_video.c`):

```c
   cap->avc.b_l0_refs = info->vcn_ip_version >= VCN_3_0_0 ? 1 : 0;
   cap->avc.l1_refs = info->vcn_ip_version >= VCN_3_0_0 ? 1 : 0;
   ...
   cap->hevc.b_l0_refs = 0;
   cap->hevc.l1_refs = 0;
```

H.264 B-frames arrived in Mesa 23.3 ("radeonsi/vcn: Add encode support for H264 B-frames", MR 25565, without B-pyramid) and only on VCN ≥ 3. VCE supports P-frames only, and no AMD block does HEVC B-frames.

**Intra refresh.**
- `PIPE_VIDEO_CAP_ENC_INTRA_REFRESH` supports ROW, COLUMN, and P_FRAME modes. It is exposed as `VAConfigAttribEncIntraRefresh` and consumed from `VAEncMiscParameterRIR`.
- It is reset on every `vaBeginPicture`, so it must be re-sent each frame.
- Implemented for VCN (`radeon_vcn_enc.c`) and VCE (`radeon_vce.c`, bar-based). No implementation exists for UVD-ENC, i.e. HEVC on Polaris and Vega (**UNVERIFIED**).
- Added in Mesa 24.0.

**Quality level ✔︎** (`frontends/va/encode.c`, `va_private.h`). The value is a bitfield `valid:1 | preset_mode:2 | pre_encode:1 | vbaq:1`, with presets SPEED=0, BALANCE=1, QUALITY=2, HIGH_QUALITY=3:

```c
   if (!in->quality) { p->level = 0; p->preset_mode = PRESET_MODE_SPEED; p->pre_encode_mode = PREENCODING_MODE_DISABLE; p->vbaq_mode = VBAQ_DISABLE; return; }
   if (p->level != in->quality) {
      if (in->quality == 1) { p->preset_mode = PRESET_MODE_BALANCE; p->pre_encode_mode = PREENCODING_MODE_DEFAULT; p->vbaq_mode = VBAQ_AUTO; }
      else { p->preset_mode = in->preset_mode; p->pre_encode_mode = in->pre_encode_mode; p->vbaq_mode = in->vbaq_mode; }
```

What each setting does:
- FFmpeg `-quality 0` selects the **SPEED** preset.
- GStreamer `target-usage` 0 is never sent (`if (target_usage == 0) return TRUE;` ✔︎), and the range is 1–7:
  - 1 = BALANCE with pre-encode and VBAQ.
  - 2 or 3 = BALANCE.
  - **4 (the default) or 5 = QUALITY.**
  - 6 or 7 = HIGH_QUALITY.
  - SPEED cannot be selected from GStreamer.
- VCE ignores presets.

**Environment knobs** (https://docs.mesa3d.org/envvars.html ✔︎):
- `AMD_DEBUG=lowlatencyenc` enables low latency encoding (Mesa 24.2, VCN only).
- `AMD_DEBUG=noefc` turns off the encoder's built-in colour conversion. The VCN ≥ 2.0 encoder (except 2.2) can convert RGB to NV12 itself. Whether VA-API input triggers that path is **UNVERIFIED**, so plan on `vapostproc`/`scale_vaapi`.

**Mac Pro 2019 GPUs.** Kernel encode caps come from `amdgpu/{vi.c,soc15.c,nv.c}`; Mesa caps from `ac_video.c`.

| Mac Pro GPU | ASIC | Encode block | H.264 enc | HEVC enc | AV1 enc | H.264 B-frames (Mesa) | Kernel max encode res |
|---|---|---|---|---|---|---|---|
| Radeon Pro 580X | Polaris 20 (third-party ID; UNVERIFIED from AMD/Apple) | VCE 3.4 (AVC). HEVC uses the **UVD-ENC** ring, which needs UVD fw ≥ 1.130.16. | ✅ | ✅ | ❌ | ❌ | 4096×4096 |
| Vega II / Vega II Duo | Vega 20 | VCE 4.1 + 2× UVD 7.2 (HEVC through UVD-ENC) | ✅ | ✅ | ❌ | ❌ | 4096×4096 |
| W5500X / W5700X | Navi 14 / Navi 10 | VCN 2.0 | ✅ | ✅ Main and Main10 | ❌ | ❌ | 4096×4096 |
| W6800X / W6900X / W6800X Duo | Navi 21 | VCN 3.0 | ✅ | ✅ Main and Main10 | ❌ (VCN 4+) | ✅ since Mesa 23.3 (1 L0 + 1 L1, no pyramid) | AVC 4096×4096, HEVC 8192×4352 |

Every one of these GPUs can VA-encode the iPad resolutions (2360×1640 and 2732×2048) as H.264 without B-frames. That is the recommended common path, and it also gives the lowest latency.

### C.4 Software fallback

**x264** (`common/base.c`, https://code.videolan.org/videolan/x264/-/raw/master/common/base.c):

```c
        else if( len == 11 && !strncasecmp( tune, "zerolatency", 11 ) )
        {
            param->rc.i_lookahead = 0;
            param->i_sync_lookahead = 0;
            param->i_bframe = 0;
            param->b_sliced_threads = 1;
            param->b_vfr_input = 0;
            param->rc.b_mb_tree = 0;
        }
```

`--fullhelp` describes it as `--bframes 0 --force-cfr --no-mbtree --sync-lookahead 0 --sliced-threads --rc-lookahead 0`.
- **ultrafast** sets ref 1, scenecut 0, no deblock, no CABAC, `me=dia`, `subme=0`, `aq=0`, no trellis, no mbtree, no weightp, lookahead 0.
- **superfast** sets `me=dia`, `subme=1`, ref 1, `weightp=simple`, lookahead 0.
- `--intra-refresh` means "Use Periodic Intra Refresh instead of IDR frames".
- `--vbv-maxrate` / `--vbv-bufsize`: set bufsize ≈ maxrate/fps.

GStreamer `x264enc`: `tune=zerolatency speed-preset=ultrafast bitrate=20000 vbv-buf-capacity=17 key-int-max=0 intra-refresh=true byte-stream=true`. The default `vbv-buf-capacity` is 600 ms and the default `speed-preset` is medium.

**OpenH264** (Cisco, BSD-2):
- Constrained Baseline only (CAVLC, no B-frames), up to Level 5.2.
- GStreamer `openh264enc` has `usage-type=screen`.
- Rust crate `openh264` 0.9.9.
- Cisco covers the MPEG LA fees only if you use Cisco's prebuilt binary.

### C.5 iOS / iPadOS decoding

**Bitstream conversion.**
1. Split the stream on `00 00 01` / `00 00 00 01`.
2. Pass SPS(7)/PPS(8), or VPS(32)/SPS(33)/PPS(34) for HEVC, **without start codes** to `CMVideoFormatDescriptionCreateFromH264ParameterSets` (or `...FromHEVCParameterSets`, iOS 11+) with `nalUnitHeaderLength: 4`.
3. Replace every start code in the frame NALs with a 4-byte big-endian length.
4. Wrap the result with `CMBlockBuffer` and then `CMSampleBufferCreateReady`.

Moonlight-iOS (`Limelight/Stream/VideoDecoderRenderer.m`) does exactly this.

**(a) `VTDecompressionSession`.** Leave `kVTDecompressionPropertyKey_RealTime` at its default of true. Flag guidance, from Apple's documentation (https://developer.apple.com/documentation/videotoolbox/vtdecodeframeflags):
- Do **not** set `kVTDecodeFrame_EnableTemporalProcessing`: "With the bit set, it is legal for the decoder to delay frames indefinitely".
- Do **not** set `kVTDecodeFrame_1xRealTimePlayback`: it lets the decoder use "a low-power mode that can't decode faster than realtime".
- `kVTDecodeFrame_EnableAsynchronousDecompression` is optional. Clearing it makes the decoder emit each frame before `DecodeFrame` returns.

Render the output `CVPixelBuffer` through Metal or `AVSampleBufferDisplayLayer`.

**(b) Direct `AVSampleBufferDisplayLayer` enqueue.** Set `kCMSampleAttachmentKey_DisplayImmediately = kCFBooleanTrue` through `CMSampleBufferGetSampleAttachmentsArray(createIfNecessary:true)`. From Apple's docs: "the decoded image will be displayed as soon as possible, replacing all previously enqueued images regardless of their timestamps".

Deprecations:
- `AVSampleBufferDisplayLayer.enqueue(_:)` is discouraged from iOS 17 in favour of `sampleBufferRenderer` (`AVSampleBufferVideoRenderer`, iOS 17+, which can enqueue from a background thread).
- **✔︎ `AVSampleBufferVideoRenderer.enqueue(_:)` itself is deprecated in iOS/iPadOS 27.0** (https://developer.apple.com/tutorials/data/documentation/avfoundation/avsamplebuffervideorenderer/enqueue(_:).json): "Attach renderer to a render synchronizer with sampleBufferReceiver(adding:) and use the receiver's enqueue(_:) async or enqueueImmediately(_:) methods instead". The new method is `AVSampleBufferVideoRenderer.Receiver.enqueueImmediately(_:)`.
- Apple also warns against combining `DisplayImmediately` with a control timebase or synchronizer. How that interacts with the iOS 27 receiver path is **UNVERIFIED**.
- Deprecated APIs still work, but a `VTDecompressionSession` + Metal (`CAMetalLayer`) presenter avoids this API churn and gives explicit control of presentation.

**Low-latency pitfalls:**
- Never send B-frames, and always emit VUI with `bitstream_restriction_flag=1` and `max_num_reorder_frames=0` (FFmpeg `bf=0` and GStreamer `vah264enc` do this; x264 writes `num_reorder_frames=0` when bframes is 0).
- Without that VUI, decoders may buffer frames to reorder them. Mozilla's VideoToolbox note confirms VT reorders internally; whether VT or AVSBDL actually holds frames when the VUI is missing is **UNVERIFIED**, so measure it.
- After a flush or a packet-loss resync, send an IDR, which the decoder needs.

**Display.** The iPad Air M4 has no ProMotion and runs at 60 Hz. ProMotion (24–120 Hz) is iPad Pro only, per Apple's "Optimizing … ProMotion displays" article. Use `CADisplayLink.preferredFrameRateRange` (iOS 15+) to pace any custom presenter.

**HEVC decode.** It is hardware-accelerated on the iPad Air M1 (support.apple.com/111887) and M4 (H.264, HEVC, ProRes, AV1 decode, per apple.com/ipad-air/specs). For M2/M3, Apple's spec pages were not checked (**UNVERIFIED**, but expected). On the Linux side, H.264 is the safer default.

---

## D. Tablet mapping in KWin (Phase 1)

### D.1 DBus interface `org.kde.KWin.InputDevice`

Defined in https://invent.kde.org/plasma/kwin/-/raw/master/src/backends/libinput/device.h. Every `Q_PROPERTY` is exported (`ExportAllProperties`).

**Object path.** From `device.cpp`:

```cpp
    QDBusConnection::sessionBus().registerObject(QStringLiteral("/org/kde/KWin/InputDevice/") + m_sysName,
                                                 QStringLiteral("org.kde.KWin.InputDevice"),
                                                 this,
                                                 QDBusConnection::ExportAllProperties);
```

`m_sysName = libinput_device_get_sysname()` gives paths like **`/org/kde/KWin/InputDevice/event23`**. The manager object `/org/kde/KWin/InputDevice` with interface `org.kde.KWin.InputDeviceManager` has property `devicesSysNames` (`as`) and signals `deviceAdded(s)` / `deviceRemoved(s)` (`connection.cpp`).

Tablet-relevant properties, with names and types exactly as in the source:

| Property | Type | Access |
|---|---|---|
| `tabletTool`, `tabletPad` | `bool` | read-only |
| `name`, `sysName` | `QString` | read-only |
| `outputName` | `QString` | **read-write** (setter `setOutputName`) |
| `size` | `QSizeF` (mm, from `libinput_device_get_size`) | read-only |
| `product`, `vendor` | `quint32` | read-only |
| `enabled` | `bool` | read-write |
| `supportsCalibrationMatrix` | `bool` | read-only |
| `defaultCalibrationMatrix` | `QString` | read-only |
| `calibrationMatrix` | `QString` | read-write |
| `orientation` / `orientationDBus` | `Qt::ScreenOrientation` / `int` | read-write |
| `supportsLeftHanded`, `leftHanded` | `bool` | read-only / read-write |
| `supportsOutputArea` | `bool` (= `m_tabletTool`) | read-only |
| `defaultOutputArea` | `QRectF` | read-only |
| `outputArea` | `QRectF` (normalized; identity is `0,0,1,1`) | read-write |
| `defaultMapToWorkspace` | `bool` | read-only |
| `mapToWorkspace` | `bool` | read-write |
| `deviceGroupId` | `QString` | read-only |
| `defaultPressureCurve` | `QString` | read-only |
| **`pressureCurve`** | `QString`, serialized as two control points `"x1,y1;x2,y2"` of a cubic Bézier from (0,0) to (1,1) | read-write |
| `supportsInputArea`, `defaultInputArea` | `bool`, `QRectF` | read-only |
| `inputArea` | `QRectF` | read-write |
| `supportsPressureRange` | `bool` (notify) | read-only |
| `pressureRangeMin`, `pressureRangeMax` | `double` | read-write |
| `defaultPressureRangeMin`, `defaultPressureRangeMax` | `double` | read-only |
| `tabletToolIsRelative` | `bool` | read-write |
| `supportsRotation` | `bool` | read-only |
| `rotation`, `defaultRotation` | `quint32` ("rotation angle, as 0 to 360 degrees") | read-write / read-only |
| `tabletPadButtonCount`, `tabletPadDialCount`, `tabletPadRingCount`, `tabletPadStripCount` | `quint32` | read-only |
| `numModes`, `currentModes` | `QList<uint>` | read-only |
| `isVirtual` | `bool` | read-only |

There is no property called `tabletToolPressureCurve`. That name is only the **kcminputrc key** (`TabletToolPressureCurve`). The DBus property is `pressureCurve`.

`isVirtual` comes from this code:

```cpp
            // In UDev, all virtual uinput devices have a devpath start with /devices/virtual
            m_isVirtual = strstr(devPath, "/devices/virtual/") != nullptr;
```

**Persistence.** Settings live in `kcminputrc`, group `[Libinput][<vendor>][<product>][<name>]` (`Connection::applyDeviceConfig`). The keys are `OutputName`, `OutputUuid`, `OutputArea`, `MapToWorkspace`, `TabletToolPressureCurve`, `TabletToolPressureRangeMin/Max`, `InputArea`, `TabletToolRelativeMode`, `Rotation`, `Calibration`, `Orientation`, `LeftHanded`, `Enabled`, and others. `setOutputName()` writes **`OutputUuid`**, not `OutputName`.

### D.2 How KWin chooses the output and area for a tablet

From `src/backends/libinput/connection.cpp`:

```cpp
static QPointF tabletToolPosition(TabletToolEvent *event)
{
    if (event->device()->isMapToWorkspace()) {
        return workspace()->geometry().topLeft() + event->transformedPosition(workspace()->geometry().size());
    } else {
        BackendOutput *backendOutput = event->device()->output();
        if (backendOutput) {
            return devicePointToGlobalPosition(event->transformedPosition(backendOutput->modeSize()), backendOutput);
        }
        BackendOutput *output = workspace()->activeOutput()->backendOutput();
        return devicePointToGlobalPosition(event->transformedPosition(output->modeSize()), output);
    }
}
```

From `events.cpp`, where `outputArea` is applied:

```cpp
QPointF TabletToolEvent::transformedPosition(const QSize &size) const
{
    const QRectF outputArea = device()->outputArea();
    return {(size.width() - 1) * outputArea.x() + libinput_event_tablet_tool_get_x_transformed(m_tabletToolEvent, (size.width() - 1) * outputArea.width()),
            (size.height() - 1) * outputArea.y() + libinput_event_tablet_tool_get_y_transformed(m_tabletToolEvent, (size.height() - 1) * outputArea.height())};
}
```

`devicePointToGlobalPosition()` applies the output transform (rotations), the device offset, and `/scale`, then adds the logical output position and clamps.

Assigning the output (`Connection::applyScreenToDevice`):
- It runs when a device is added and on every `Workspace::outputsChanged` (`InputRedirection::updateScreens`).
- It only applies to touch and tablet-tool devices.
- Order: stored `OutputUuid` first, then `outputName`. For **touch** devices only, KWin then guesses by physical size, the internal panel, or the first output.
- **A tablet with neither falls back to `workspace()->activeOutput()`**, which is the "Follow the Current Screen" option in the KCM.

`Device::setOutputName(name)` searches `outputs` for `output->name() == name` that also has a logical output. It sets the output, sets `m_outputUuid = output->uuid()`, and writes `OutputUuid`.

**Practical recipe for the daemon.** After the virtual output `Virtual-ipad` exists and the uinput device has appeared:

```
busctl --user set-property org.kde.KWin /org/kde/KWin/InputDevice/eventN org.kde.KWin.InputDevice outputName s "Virtual-ipad"
```

Do this every time the virtual output is (re)created. Leave `mapToWorkspace=false` and `outputArea=0,0,1,1`. Match the device's aspect ratio to the output's so that "stretch" is exact.

Two pitfalls:
- **Do not rely on the udev `WL_OUTPUT` property.** libinput reads it (`udev-seat.c`: `output_name = udev_device_get_property_value(udev_device, "WL_OUTPUT");`) and KWin seeds `m_outputName` from `libinput_device_get_output_name()`. But `loadConfiguration()` then reads `OutputName` with default `""` through `setConfigOutputName()`, and `OutputUuid` with default `""` through `setOutputUuid()`. As I read the code, that clears the value. **UNVERIFIED at runtime.**
- For the same reason, pre-writing only `OutputName=` into kcminputrc is not enough. The runtime DBus `Set` is reliable.

**`INPUT_PROP_DIRECT` does not matter to KWin's mapping.** KWin never reads it. It only matters inside libinput:
- With DIRECT, the calibration matrix is enabled (`tablet_init_calibration`).
- With DIRECT, libinput's tablet **area** config is *not* provided (`tablet_init_area`: `if (!libevdev_has_property(device->evdev, INPUT_PROP_DIRECT)) { ... area.config ... }`).
- udev classifies `BTN_TOOL_PEN` or `BTN_STYLUS` with ABS_X/Y as a tablet regardless of DIRECT (§E.4).

### D.3 Drawing Tablet KCM (plasma-desktop `kcms/tablet`)

From `tabletsmodel.cpp`: the KCM lists devices from `org.kde.KWin.InputDeviceManager.devicesSysNames` and keeps every device whose `tabletTool` or `tabletPad` property is true, grouped by `deviceGroup`:

```cpp
    auto dev = new KWinDevices::InputDevice(sysName, this);
    const bool isTabletTool = dev->tabletTool();
    const bool isTabletPad = dev->tabletPad();
    if (!isTabletTool && !isTabletPad) {
```

libwacom is only used for the display name. `libwacom_new_from_path(..., WFALLBACK_NONE)` fails for unknown devices and the KCM falls back to `penDevice->name()`. There is no filter for virtual devices, so **a uinput tablet that libinput accepts will be listed**.

The output combobox (`kcmtablet.cpp` `OutputsModel`) contains "Follow the Current Screen", "All Screens", and every `QScreen`, labelled `model` (falling back to `name`). A virtual output created with a description is labelled with that description. Choosing an entry sets `outputName` and `mapToWorkspace`. The "area" combobox offers "Stretch and Fill" and "Keep Aspect Ratio and Fit", which write `outputArea`.

### D.4 Are rotation and distance forwarded to Wayland clients?

Yes, when libinput reports the capability on the *tool*. KWin builds tool capabilities from libinput (`device.cpp` `TabletTool::capabilities()`: `libinput_tablet_tool_has_pressure/_distance/_rotation/_tilt/_slider/_wheel`). In `input.cpp` it sends them on proximity, axis, and tip events:

```cpp
        if (tool->hasCapability(TabletToolV2Interface::Tilt))     { tool->sendTilt(event->xTilt, event->yTilt); }
        if (tool->hasCapability(TabletToolV2Interface::Rotation)) { tool->sendRotation(event->rotation); }
        if (tool->hasCapability(TabletToolV2Interface::Distance)) { tool->sendDistance(event->distance); }
```

`tablet_v2.cpp` sends pressure and distance as `65535 * value`, and tilt and rotation as `wl_fixed` degrees. The pressure KWin sends is `pressureCurve().valueForProgress(libinput pressure)`, after `libinput_tablet_tool_config_pressure_range_set(min,max)` from `pressureRangeMin/Max`.

---

## E. libinput tablet requirements (libinput main, 1.32)

### E.1 What makes a device a tablet

**Step 1 — udev must tag the device `ID_INPUT_TABLET=1`.** libinput then creates the tablet dispatch only if the device has no touchpad or touchscreen tag (`src/evdev.c`):

```c
	tablet_tags = EVDEV_UDEV_TAG_TABLET | EVDEV_UDEV_TAG_TOUCHPAD |
		      EVDEV_UDEV_TAG_TOUCHSCREEN;
	/* libwacom assigns tablet _and_ tablet_pad to the pad devices */
	if (udev_tags & EVDEV_UDEV_TAG_TABLET_PAD) { dispatch = evdev_tablet_pad_create(device); ... }
	if ((udev_tags & tablet_tags) == EVDEV_UDEV_TAG_TABLET) {
		dispatch = evdev_tablet_create(device);
```

**Step 2 — `tablet_reject_device()` must pass** (`src/evdev-tablet.c`):

```c
	has_xy = libevdev_has_event_code(evdev, EV_ABS, ABS_X) &&
		 libevdev_has_event_code(evdev, EV_ABS, ABS_Y);
	has_pen = libevdev_has_event_code(evdev, EV_KEY, BTN_TOOL_PEN);
	has_btn_stylus = libevdev_has_event_code(evdev, EV_KEY, BTN_STYLUS);
	has_size = evdev_device_get_size(device, &w, &h) == 0;

	if (has_xy && (has_pen || has_btn_stylus) && has_size)
		return false;

	evdev_log_bug_libinput(device,
			       "missing tablet capabilities:%s%s%s%s. "
			       "Ignoring this device.\n", ...  has_size ? "" : " resolution");
```

**Step 3 — `has_size` requires a real (non-fake) resolution and a range that is not 0..1** (`src/evdev.c`):

```c
	if ((x && x->minimum == 0 && x->maximum == 1) ||
	    (y && y->minimum == 0 && y->maximum == 1))
		return -1;
	if (!x || !y || device->abs.is_fake_resolution || !x->resolution ||
	    !y->resolution)
		return -1;
```

`evdev_fix_abs_resolution()` only fakes a resolution when the kernel reports 0, and a fake resolution then fails `has_size`. The resolution can also come from the udev properties `LIBINPUT_ATTR_RESOLUTION_HINT` / `LIBINPUT_ATTR_SIZE_HINT`, but **setting `absinfo.resolution` in `UI_ABS_SETUP` is the right way**.

**Other things `tablet_init()` does:**
- Enables `BTN_TOOL_PEN` if it is missing.
- Disables `BTN_TOOL_MOUSE`/`BTN_TOOL_LENS` for non-Wacom vendors ("Our rotation code only works with Wacoms").
- `tablet_fix_tilt()` disables tilt unless *both* `ABS_TILT_X` and `ABS_TILT_Y` exist. For resolution-0 tilt axes with an even range, it bumps the max by 1.
- **Turns smoothing off for uinput devices**:

  ```c
  	/* By default, always enable smoothing except on AES or uinput devices.
  	 * AttrTabletSmoothing can override this, if necessary.
  	 */
  	if (!q || !quirks_get_bool(q, QUIRK_ATTR_TABLET_SMOOTHING, &use_smoothing))
  		use_smoothing = !is_aes && !is_virtual;
  ```

  `is_virtual` comes from `udev_device_is_virtual()`, i.e. a devpath under `/devices/virtual`.
- libwacom lookup: a uinput device is "not known to libwacom". `tablet_is_display_tablet()` returns `!wacom`, which is true, so **calibration is available even without `INPUT_PROP_DIRECT`**. Left-handed mode is available (`!wacom`).

### E.2 Which axes a tool gets when libwacom does not know it

The stylus lookup is `libwacom_stylus_get_for_id(db, tool_id)`. `tool_id` comes from `ABS_MISC`, which is 0 for us. libwacom's `wacom.stylus` has no entry for id 0; the generic ids are `0xfffff`, `0xffffd`, and so on. So the stylus is NULL and the fallback in `tool_set_bits()` applies:

```c
	/* If we don't have libwacom, we simply copy any axis we have on the
	   tablet onto the tool. Except we know that mice only have rotation
	   anyway.
	 */
	switch (type) {
	case LIBINPUT_TABLET_TOOL_TYPE_PEN:
	...
		copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_PRESSURE);
		copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_DISTANCE);
		copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_TILT_X);
		copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_TILT_Y);
		copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_SLIDER);
		/* Rotation is special, it can be either ABS_Z or
		 * BTN_TOOL_MOUSE+ABS_TILT_X/Y. ... */
		if (libevdev_has_event_code(tablet->device->evdev, EV_ABS, ABS_Z))
			copy_axis_cap(tablet, tool, LIBINPUT_TABLET_TOOL_AXIS_ROTATION_Z);
```

**A plain pen with `ABS_Z` gets rotation without libwacom.** `ABS_WHEEL` becomes the slider, normalized to −1..1.

### E.3 Axis conversions (`src/evdev-tablet.c`, `src/util-input-event.h`)

**Tilt:**

```c
static inline double
adjust_tilt(const struct input_absinfo *absinfo)
{
	double value = absinfo_normalize(absinfo);
	const int WACOM_MAX_DEGREES = 64;

	/* If resolution is nonzero, it's in units/radian. But require
	 * a min/max less/greater than zero so we can assume 0 is the
	 * center */
	if (absinfo->resolution != 0 && absinfo->maximum > 0 && absinfo->minimum < 0) {
		value = rad2deg((double)absinfo->value / absinfo->resolution);
	} else {
		/* Wacom supports physical [-64, 64] degrees, so map to that by default. ... */
		value = (value * 2) - 1;
		value *= WACOM_MAX_DEGREES;
	}
	return value;
}
```

In left-handed mode both tilts are negated. The docs (`doc/user/tablet-support.rst`) say: "a positive tilt angle thus means that the stylus' top is tilted towards the logical right and/or bottom of the tablet."

**Rotation (`ABS_Z`):**

```c
static double
convert_to_degrees(const struct input_absinfo *absinfo, double offset)
{
	/* range is [0, 360[, i.e. range + 1 */
	double value = (absinfo->value - absinfo->minimum) / absinfo_range(absinfo);
	return fmod(value * 360.0 + offset, 360.0);
}
...
		/* artpen has 0 with buttons pointing east */
		tablet->axes.rotation = convert_to_degrees(absinfo, 90);
```

`absinfo_range` is `max − min + 1`. In left-handed mode the result becomes `fmod(180 + r, 360)`. The API (`libinput.h`) says: "the current z rotation of the tool in degrees, clockwise from the tool's logical neutral position."

**Distance:** `normalize_distance = absinfo_normalize()`, which is `(v−min)/(max−min)` clamped to [0,1]. Distance and pressure are kept mutually exclusive: when the tool is "in contact" (pressure > lower threshold), distance is forced to 0, and otherwise pressure is forced to 0.

**Pressure:**
- Thresholds are set in `update_pressure_range()`, which runs when the tool is configured:

  ```c
  		/* 5 and 1% of the pressure range */
  		hi = axis_range_percentage(&abs, 5);
  		lo = axis_range_percentage(&abs, 1);
  ```

  These can be overridden by the `AttrPressureRange` quirk (a root-owned quirks file).
- Contact is decided from pressure, not from `BTN_TOUCH` ("if we have pressure, always use that for contact, not BTN_TOUCH"): `pressure >= upper` gives tip down and `pressure <= lower` gives tip up.
- Normalization rescales from the lower threshold: `abs.minimum = threshold->threshold.lower; return absinfo_normalize_value(&abs, abs_value);`.
- **Offset heuristic:** if the device **has `ABS_DISTANCE`**, `heuristic_state = DONE` and the offset is 0, so nothing is detected. Without distance, libinput watches the first proximity-ins and may adopt the minimum observed pressure as an offset (≤ 50 % of the range).
- The heuristic is also disabled when a custom pressure range is set. KWin sets the range from `pressureRangeMin/Max`, defaults 0..1, which counts as "not configured".

### E.4 udev `ID_INPUT_TABLET` classification

From systemd `src/udev/udev-builtin-input_id.c` (https://github.com/systemd/systemd/blob/main/src/udev/udev-builtin-input_id.c):

```c
        has_keys = test_bit(EV_KEY, bitmask_ev);
        has_abs_coordinates = test_bit(ABS_X, bitmask_abs) && test_bit(ABS_Y, bitmask_abs);
        has_3d_coordinates = has_abs_coordinates && test_bit(ABS_Z, bitmask_abs);
        is_accelerometer = test_bit(INPUT_PROP_ACCELEROMETER, bitmask_props);
        if (!has_keys && has_3d_coordinates)
                is_accelerometer = true;
...
        has_stylus = test_bit(BTN_STYLUS, bitmask_key);
        has_pen = test_bit(BTN_TOOL_PEN, bitmask_key);
...
        has_pad_buttons = test_bit(BTN_0, bitmask_key) && test_bit(BTN_1, bitmask_key) && !has_pen;
        if (has_abs_coordinates) {
                if (has_stylus || has_pen)
                        is_tablet = true;
...
        if (is_tablet)
                udev_builtin_add_property(event, "ID_INPUT_TABLET", "1");
```

The tablet test only needs ABS_X plus ABS_Y plus (`BTN_STYLUS` or `BTN_TOOL_PEN`). `INPUT_PROP_DIRECT` and `INPUT_PROP_POINTER` are irrelevant here.

Watch out for two cases:
- `ABS_Z` on a device **without any EV_KEY** is classified as an accelerometer. Our pen has keys, so this is fine.
- Pad buttons (`BTN_0` and `BTN_1`) together with `BTN_TOOL_PEN` do *not* make a pad. Expose pad buttons through a separate uinput device that has no pen.

**Permissions.** systemd's `50-udev-default.rules` sets `SUBSYSTEM=="input", GROUP="input"` but contains no `/dev/uinput` rule, so `/dev/uinput` stays root-only. Ship a rule such as `KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"`, or use the input group. **TO TEST** on the target machine.

### E.5 Recommended uinput pen device setup (Phase 1)

Setup with `UI_DEV_SETUP` and `UI_ABS_SETUP`; the Rust `evdev` crate's `VirtualDeviceBuilder` + `AbsInfo` wrap these. Shown for the iPad Air 13″ (2732×2048, 264 ppi = 10.394 px/mm); the 11″ is 2360×1640.

```
name      = "Sidecar iPad Pencil"            # stable; KWin config keyed by vid/pid/name
input_id  = { bustype: BUS_VIRTUAL(0x06), vendor: <fixed non-Wacom>, product: <fixed>, version: 1 }
props     = INPUT_PROP_DIRECT
EV_KEY    = BTN_TOOL_PEN, BTN_TOUCH, BTN_STYLUS, [BTN_STYLUS2], [BTN_TOOL_RUBBER]
EV_ABS    (value, min, max, fuzz, flat, resolution)
  ABS_X        = (0, 0, 2732*8-1 = 21855, 0, 0, 83)     # 8 sub-pixel units/px; 83 ≈ 8*264/25.4 units/mm
  ABS_Y        = (0, 0, 2048*8-1 = 16383, 0, 0, 83)     # SAME resolution as X → correct mm size/aspect
  ABS_PRESSURE = (0, 0, 4095, 0, 0, 0)
  ABS_TILT_X   = (0, -9000, 9000, 0, 0, 5730)           # 0.01° units; 5730 ≈ 18000/π units per radian
  ABS_TILT_Y   = (0, -9000, 9000, 0, 0, 5730)
  ABS_DISTANCE = (0, 0, 1023, 0, 0, 0)                  # optional (hover); also disables pressure-offset guessing
  ABS_Z        = (0, 0, 359, 0, 0, 0)                   # optional barrel roll → libinput rotation
EV_MSC    = MSC_TIMESTAMP (optional)
```

Event sequences (one `SYN_REPORT` per frame):
- **Hover in:** `BTN_TOOL_PEN=1`, `ABS_X`, `ABS_Y`, `ABS_DISTANCE=d>0`, `ABS_PRESSURE=0`, tilt, `SYN`.
- **Touch down:** `ABS_DISTANCE=0`, `ABS_PRESSURE=p`, `BTN_TOUCH=1`, `SYN`. Map `p = round(4095*(0.06 + 0.94*force))` so that any contact clears the 5 % tip-down threshold.
- **Move:** X/Y/pressure/tilt, `SYN`. Send at the Pencil's sample rate (use `coalescedTouches`) or at least 120 Hz.
- **Up:** `ABS_PRESSURE=0`, `BTN_TOUCH=0`, `SYN`.
- **Out of range:** `BTN_TOOL_PEN=0`, `SYN`.

Converting iOS data:
- **Tilt.** Altitude α runs from 0 (flat) to π/2 (upright); azimuth φ is in view coordinates with y pointing down. Use `tilt_x = atan(cos φ · cot α)` and `tilt_y = atan(sin φ · cot α)` in degrees, then write `round(deg*100)`. Check the signs empirically: libinput expects positive values when the top of the stylus leans to the right or bottom.
- **Rotation.** For a roll angle θ in degrees, send `ABS_Z = (θ − 90) mod 360`.

KWin side:
- Call `Set outputName` on `/org/kde/KWin/InputDevice/<eventN>`. Find `eventN` by watching `InputDeviceManager.deviceAdded` and matching the `name`, or from `/sys/devices/virtual/input/inputX/eventN`.
- The pressure curve can be adjusted by the user in the KCM, or by the daemon through `pressureCurve`.

---

## F. Things to test on the real machine

1. Whether `zkde_screencast` v4 works from an unsandboxed Rust client on Plasma 6.8.2 without a `.desktop` file, and that a 6.7 machine needs it.
2. Whether `kscreen-doctor output.Virtual-ipad.addCustomMode.2732.2048.120000` works on a virtual output, and whether the screencast then delivers 120 fps.
3. Whether KWin remembers the virtual output's position and scale across sessions in `kwinoutputconfig.json`.
4. The `WL_OUTPUT` and kcminputrc pre-seeding behaviour (§D.2).
5. VA-API modifier compatibility with KWin's BGRx DMA-BUFs on Polaris, Vega 20, and Navi (see §C), and explicit sync (`SPA_META_SyncTimeline`).
6. End-to-end latency budget: app render, KWin composite on the 60 Hz virtual vsync (0–16.7 ms), stream tick (aligned, up to 1 frame), GPU blit, encode (a few ms), network, decode, and iPad vsync (0–16.7 ms). **Estimate** 30–60 ms glass-to-glass. Measure it with a timestamp overlay.

---

## Source index

**KDE**
- KWin (master):
  - https://invent.kde.org/plasma/kwin/-/raw/master/src/plugins/screencast/screencastmanager.cpp
  - …/src/plugins/screencast/screencaststream.cpp
  - …/src/plugins/screencast/outputscreencastsource.cpp
  - …/src/wayland/screencast_v1.cpp
  - …/src/wayland_server.cpp (and the Plasma/6.7 branch)
  - …/src/utils/serviceutils.h (6.7)
  - …/src/backends/drm/drm_virtual_output.cpp
  - …/src/backends/drm/drm_backend.cpp
  - …/src/backends/libinput/device.h, device.cpp, connection.cpp, events.cpp, events.h
  - …/src/input.cpp
  - …/src/wayland/tablet_v2.cpp
  - …/src/utils/vsyncsource.cpp
  - …/src/plugins/eis/eisdevice.cpp
  - …/src/outputconfigurationstore.cpp
- KWin commits via the GitLab API: `f9bf0ee6`, `97b5274a`, `452707eb`, `d918731b`, `efc77310`, `ae04e36c`, `24852040`, `526b0e4c`
- plasma-wayland-protocols: https://invent.kde.org/libraries/plasma-wayland-protocols/-/raw/master/src/protocols/zkde-screencast-unstable-v1.xml
- xdg-desktop-portal-kde: https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/raw/master/src/{screencast.cpp,screencast.h,waylandintegration.cpp,screencasting.cpp,outputsmodel.cpp,screenchooserdialog.cpp,ScreenChooserDialog.qml,PipeWireDelegate.qml,PipeWireLayout.qml}; commits `9ee83def`, `7f134117`, `d9a55b3b`, `5900c11d`
- KRdp: https://invent.kde.org/plasma/krdp/-/raw/master/{server/main.cpp,server/SessionController.cpp,server/org.kde.krdpserver.desktop.cmake,src/PlasmaScreencastV1Session.cpp,src/PortalSession.cpp,src/VideoStreamSurface.cpp}
- KPipeWire: https://invent.kde.org/plasma/kpipewire/-/raw/master/src/{h264vaapiencoder.cpp,encoder.cpp,libx264encoder.cpp,vaapiutils.cpp,pipewiresourcestream.cpp,pipewirebaseencodedstream.h,CMakeLists.txt}
- plasma-desktop Drawing Tablet KCM: https://invent.kde.org/plasma/plasma-desktop/-/raw/master/kcms/tablet/{tabletsmodel.cpp,kcmtablet.cpp,ui/DisplayTab.qml}
- libkscreen: https://invent.kde.org/plasma/libkscreen/-/raw/master/src/doctor/doctor.cpp

**freedesktop**
- Portal spec: https://github.com/flatpak/xdg-desktop-portal/blob/main/data/org.freedesktop.portal.ScreenCast.xml and org.freedesktop.host.portal.Registry.xml
- PipeWire:
  - https://gitlab.freedesktop.org/pipewire/pipewire/-/raw/master/doc/dox/internals/dma-buf.dox
  - …/src/gst/gstpipewiresrc.c
  - …/spa/include/spa/buffer/meta.h
- libinput: https://gitlab.freedesktop.org/libinput/libinput/-/raw/main/src/{evdev-tablet.c,evdev.c,evdev.h,libinput.c,libinput.h,udev-seat.c,util-input-event.h}, doc/user/tablet-support.rst
- libwacom: https://github.com/linuxwacom/libwacom (libwacom/libwacom.c, libwacomint.h, data/wacom.stylus)
- systemd: https://github.com/systemd/systemd/blob/main/src/udev/udev-builtin-input_id.c, rules.d/50-udev-default.rules.in

**Other**
- Linux kernel docs: https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/Documentation/admin-guide/kernel-parameters.txt, …/Documentation/fb/modedb.rst
- evdi: https://github.com/DisplayLink/evdi (README); AUR `evdi-dkms` 1.15.1
- t2linux: https://wiki.t2linux.org/distributions/arch/installation/
- Apple: https://www.apple.com/ipad-air/specs/
- Mutter: https://gitlab.gnome.org/GNOME/mutter/-/raw/main/data/dbus-interfaces/org.gnome.Mutter.ScreenCast.xml
- crates.io API for crate versions; docs.rs for ashpd and evdev
