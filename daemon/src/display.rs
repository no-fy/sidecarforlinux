//! Phase 2, milestone 1: create a virtual monitor on KDE Plasma through
//! KWin's `zkde_screencast_unstable_v1.stream_virtual_output_with_description`
//! (v4, Plasma ≥ 6.3; falls back to `stream_virtual_output`, v2). KWin
//! creates an output named `Virtual-<name>` and a PipeWire node that streams
//! it; both disappear when the stream is closed or the daemon exits.
//!
//! Capture (PipeWire), encoding (VA-API) and the video channel come next,
//! see docs/phase2-virtual-display.md.

use anyhow::{anyhow, bail, Context, Result};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_stream_unstable_v1::{
    self, ZkdeScreencastStreamUnstableV1,
};
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_unstable_v1::{
    self, ZkdeScreencastUnstableV1,
};

/// Cursor handling of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Hidden,
    /// Cursor drawn into the frames (simplest for a first version).
    Embedded,
    /// Cursor sent as PipeWire metadata (lets the iPad draw it locally).
    Metadata,
}

impl CursorMode {
    fn wire(self) -> u32 {
        match self {
            CursorMode::Hidden => zkde_screencast_unstable_v1::Pointer::Hidden as u32,
            CursorMode::Embedded => zkde_screencast_unstable_v1::Pointer::Embedded as u32,
            CursorMode::Metadata => zkde_screencast_unstable_v1::Pointer::Metadata as u32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VirtualOutputSpec {
    /// Output becomes `Virtual-<name>` in KWin.
    pub name: String,
    pub description: String,
    /// Pixel size (KWin uses these as the mode size despite the protocol
    /// calling them "logical").
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub cursor: CursorMode,
}

impl VirtualOutputSpec {
    /// iPad Air 11" (2360x1640 @2x) by default.
    pub fn ipad_air_11() -> Self {
        VirtualOutputSpec {
            name: "ipad".into(),
            description: "iPad (sidecarforlinux)".into(),
            width: 2360,
            height: 1640,
            scale: 2.0,
            cursor: CursorMode::Embedded,
        }
    }

    /// KWin's name for the output.
    pub fn output_name(&self) -> String {
        format!("Virtual-{}", self.name)
    }
}

#[derive(Default)]
struct State {
    node: Option<u32>,
    failed: Option<String>,
    closed: bool,
    /// wl_output names (v4 `name` event), indexed like `outputs`.
    output_names: Vec<(wl_output::WlOutput, String)>,
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names.push((output.clone(), name));
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZkdeScreencastUnstableV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZkdeScreencastUnstableV1,
        _: zkde_screencast_unstable_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZkdeScreencastStreamUnstableV1,
        event: zkde_screencast_stream_unstable_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zkde_screencast_stream_unstable_v1::Event::Created { node } => state.node = Some(node),
            zkde_screencast_stream_unstable_v1::Event::Failed { error } => {
                state.failed = Some(error)
            }
            zkde_screencast_stream_unstable_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

/// A live virtual output. Dropping it closes the stream and KWin removes
/// the output.
pub struct VirtualOutput {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    stream: ZkdeScreencastStreamUnstableV1,
    pub spec: VirtualOutputSpec,
    /// PipeWire node id streaming the output.
    pub node_id: u32,
    /// Protocol version KWin granted (≥ 4 means a description was sent).
    pub protocol_version: u32,
}

const NOT_AVAILABLE_HINT: &str = "KWin did not offer zkde_screencast_unstable_v1 to this program. \
On Plasma <= 6.7 KWin only exposes it to programs whose .desktop file lists \
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1 (scripts/install-daemon.sh installs one). \
Is this a KDE Plasma Wayland session?";

impl VirtualOutput {
    /// Stream an existing output (see `Source::Existing`).
    pub fn stream_existing(
        output: &str,
        cursor: CursorMode,
        timeout: Duration,
    ) -> Result<VirtualOutput> {
        let conn = Connection::connect_to_env()
            .context("connecting to the Wayland compositor (WAYLAND_DISPLAY)")?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).context("reading Wayland globals")?;
        let qh = queue.handle();
        let screencast: ZkdeScreencastUnstableV1 = globals
            .bind(&qh, 1..=4, ())
            .map_err(|e| anyhow!("{NOT_AVAILABLE_HINT} ({e})"))?;
        let mut state = State::default();
        for g in globals.contents().clone_list() {
            if g.interface == "wl_output" && g.version >= 4 {
                let _o: wl_output::WlOutput = globals.registry().bind(g.name, 4, &qh, ());
            }
        }
        queue.roundtrip(&mut state)?;
        let names: Vec<String> = state.output_names.iter().map(|(_, n)| n.clone()).collect();
        let wl = state
            .output_names
            .iter()
            .find(|(_, n)| n == output)
            .map(|(o, _)| o.clone())
            .ok_or_else(|| anyhow!("no output named {output:?} (have {names:?})"))?;
        let stream = screencast.stream_output(&wl, cursor.wire(), &qh, ());
        let version = screencast.version();
        let deadline = Instant::now() + timeout;
        while state.node.is_none() {
            if let Some(err) = &state.failed {
                bail!("KWin refused to stream {output}: {err}");
            }
            if Instant::now() > deadline {
                bail!("KWin did not start streaming {output} within {timeout:?}");
            }
            queue.roundtrip(&mut state)?;
            if state.node.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let node_id = state.node.unwrap();
        let spec = VirtualOutputSpec {
            name: output.to_string(),
            cursor,
            ..VirtualOutputSpec::ipad_air_11()
        };
        Ok(VirtualOutput {
            conn,
            queue,
            state,
            stream,
            spec,
            node_id,
            protocol_version: version,
        })
    }

    pub fn create(spec: VirtualOutputSpec, timeout: Duration) -> Result<VirtualOutput> {
        let conn = Connection::connect_to_env()
            .context("connecting to the Wayland compositor (WAYLAND_DISPLAY)")?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).context("reading Wayland globals")?;
        let qh = queue.handle();
        let screencast: ZkdeScreencastUnstableV1 = globals
            .bind(&qh, 2..=4, ())
            .map_err(|e| anyhow!("{NOT_AVAILABLE_HINT} ({e})"))?;
        let version = screencast.version();
        let stream = if version >= 4 {
            screencast.stream_virtual_output_with_description(
                spec.name.clone(),
                spec.description.clone(),
                spec.width,
                spec.height,
                spec.scale,
                spec.cursor.wire(),
                &qh,
                (),
            )
        } else {
            screencast.stream_virtual_output(
                spec.name.clone(),
                spec.width,
                spec.height,
                spec.scale,
                spec.cursor.wire(),
                &qh,
                (),
            )
        };
        let mut state = State::default();
        let deadline = Instant::now() + timeout;
        while state.node.is_none() {
            if let Some(err) = &state.failed {
                bail!("KWin refused the virtual output: {err}");
            }
            if state.closed {
                bail!("KWin closed the virtual output stream immediately");
            }
            if Instant::now() > deadline {
                bail!("KWin did not create the virtual output within {timeout:?}");
            }
            queue.roundtrip(&mut state)?;
            if state.node.is_none() && state.failed.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let node_id = state.node.unwrap();
        Ok(VirtualOutput {
            conn,
            queue,
            state,
            stream,
            spec,
            node_id,
            protocol_version: version,
        })
    }

    /// Process pending compositor events; returns false once KWin closed the stream.
    pub fn poll(&mut self) -> Result<bool> {
        self.queue.roundtrip(&mut self.state)?;
        if let Some(e) = &self.state.failed {
            bail!("virtual output failed: {e}");
        }
        Ok(!self.state.closed)
    }
}

impl Drop for VirtualOutput {
    fn drop(&mut self) {
        self.stream.close();
        let _ = self.conn.flush();
    }
}
