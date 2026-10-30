//! Minimal usbmuxd client: list attached iOS devices and open a TCP tunnel
//! to a port on the device over USB.
//!
//! Wire format (plist variant used by libimobiledevice since usbmuxd 1.0.8):
//! 16-byte header `length:u32le version:u32le(=1) message:u32le(=8 PLIST)
//! tag:u32le`, followed by an XML plist dictionary. After a successful
//! `Connect` the socket becomes a raw byte pipe to the device port.

use anyhow::{anyhow, bail, Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub const DEFAULT_SOCKET: &str = "/var/run/usbmuxd";
const VERSION_PLIST: u32 = 1;
const MESSAGE_PLIST: u32 = 8;

pub fn socket_path() -> PathBuf {
    // libimobiledevice honours this variable too (e.g. for usbmuxd over TCP);
    // we only support the unix-socket form.
    match std::env::var("USBMUXD_SOCKET_ADDRESS") {
        Ok(v) if v.starts_with("UNIX:") => PathBuf::from(&v[5..]),
        Ok(v) if v.starts_with('/') => PathBuf::from(v),
        _ => PathBuf::from(DEFAULT_SOCKET),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub device_id: u64,
    pub udid: String,
    /// "USB" or "Network".
    pub connection_type: String,
    pub product_id: Option<u64>,
}

fn request(msg_type: &str, extra: Vec<(&str, plist::Value)>) -> plist::Dictionary {
    let mut d = plist::Dictionary::new();
    d.insert("MessageType".into(), msg_type.into());
    d.insert(
        "ClientVersionString".into(),
        concat!("sidecard ", env!("CARGO_PKG_VERSION")).into(),
    );
    d.insert("ProgName".into(), "sidecard".into());
    d.insert("kLibUSBMuxVersion".into(), plist::Value::Integer(3.into()));
    for (k, v) in extra {
        d.insert(k.into(), v);
    }
    d
}

pub async fn send_plist<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    tag: u32,
    d: &plist::Dictionary,
) -> Result<()> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, d)?;
    let mut msg = Vec::with_capacity(16 + body.len());
    msg.extend_from_slice(&((16 + body.len()) as u32).to_le_bytes());
    msg.extend_from_slice(&VERSION_PLIST.to_le_bytes());
    msg.extend_from_slice(&MESSAGE_PLIST.to_le_bytes());
    msg.extend_from_slice(&tag.to_le_bytes());
    msg.extend_from_slice(&body);
    w.write_all(&msg).await?;
    Ok(())
}

pub async fn recv_plist<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<(u32, plist::Dictionary)> {
    let mut hdr = [0u8; 16];
    r.read_exact(&mut hdr)
        .await
        .context("usbmuxd closed the connection")?;
    let len = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
    let tag = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
    if !(16..=1 << 20).contains(&len) {
        bail!("bad usbmuxd message length {len}");
    }
    let mut body = vec![0u8; len - 16];
    r.read_exact(&mut body).await?;
    let v: plist::Value = plist::from_reader(Cursor::new(body))?;
    let d = v
        .into_dictionary()
        .ok_or_else(|| anyhow!("usbmuxd reply is not a dictionary"))?;
    Ok((tag, d))
}

async fn open(path: &Path) -> Result<UnixStream> {
    UnixStream::connect(path).await.with_context(|| {
        format!(
            "connecting to usbmuxd at {} (is usbmuxd installed and running?)",
            path.display()
        )
    })
}

fn int(d: &plist::Dictionary, k: &str) -> Option<u64> {
    d.get(k).and_then(|v| v.as_unsigned_integer())
}

pub async fn list_devices(path: &Path) -> Result<Vec<Device>> {
    let mut s = open(path).await?;
    send_plist(&mut s, 1, &request("ListDevices", vec![])).await?;
    let (_, reply) = recv_plist(&mut s).await?;
    let list = reply
        .get("DeviceList")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("usbmuxd: no DeviceList in reply"))?;
    let mut out = Vec::new();
    for entry in list {
        let Some(props) = entry
            .as_dictionary()
            .and_then(|e| e.get("Properties"))
            .and_then(|p| p.as_dictionary())
        else {
            continue;
        };
        let Some(device_id) = int(props, "DeviceID") else {
            continue;
        };
        out.push(Device {
            device_id,
            udid: props
                .get("SerialNumber")
                .and_then(|v| v.as_string())
                .unwrap_or_default()
                .to_string(),
            connection_type: props
                .get("ConnectionType")
                .and_then(|v| v.as_string())
                .unwrap_or("USB")
                .to_string(),
            product_id: int(props, "ProductID"),
        });
    }
    Ok(out)
}

/// usbmuxd wants the port in network byte order inside a host integer.
pub fn port_to_usbmux(port: u16) -> u64 {
    port.to_be() as u64
}

fn result_text(n: u64) -> &'static str {
    match n {
        0 => "ok",
        2 => "device not found (BADDEV)",
        3 => "connection refused: is the Sidecar app open in the foreground on the iPad?",
        6 => "bad usbmux protocol version",
        _ => "unknown error",
    }
}

/// Open a tunnel to `port` on the device. The returned stream carries raw
/// bytes to/from the app listening on that port.
pub async fn connect(path: &Path, device_id: u64, port: u16) -> Result<UnixStream> {
    let mut s = open(path).await?;
    let req = request(
        "Connect",
        vec![
            ("DeviceID", plist::Value::Integer(device_id.into())),
            (
                "PortNumber",
                plist::Value::Integer(port_to_usbmux(port).into()),
            ),
        ],
    );
    send_plist(&mut s, 2, &req).await?;
    let (_, reply) = recv_plist(&mut s).await?;
    let n = int(&reply, "Number").ok_or_else(|| anyhow!("usbmuxd: unexpected reply {reply:?}"))?;
    if n != 0 {
        bail!(
            "usbmuxd Connect to port {port} failed: {} ({n})",
            result_text(n)
        );
    }
    Ok(s)
}

/// Pick a device: the requested UDID, else the first USB-attached one.
pub fn choose(devices: &[Device], udid: Option<&str>) -> Option<Device> {
    match udid {
        Some(u) if !u.is_empty() => devices
            .iter()
            .find(|d| d.udid.eq_ignore_ascii_case(u))
            .cloned(),
        _ => devices.iter().find(|d| d.connection_type == "USB").cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_byte_order() {
        assert_eq!(port_to_usbmux(47800), 0xB8BA);
        assert_eq!(port_to_usbmux(0x1234), 0x3412);
    }

    #[test]
    fn choose_prefers_usb_and_udid() {
        let d = |id, udid: &str, ct: &str| Device {
            device_id: id,
            udid: udid.into(),
            connection_type: ct.into(),
            product_id: None,
        };
        let list = vec![d(1, "aaa", "Network"), d(2, "bbb", "USB")];
        assert_eq!(choose(&list, None).unwrap().device_id, 2);
        assert_eq!(choose(&list, Some("AAA")).unwrap().device_id, 1);
        assert!(choose(&list, Some("zzz")).is_none());
    }
}
