//! Reaching the iPad: USB (usbmuxd tunnel), Wi-Fi (Bonjour/mDNS), or a
//! direct TCP address (fake iPad, tests). Then TLS on top.

use crate::identity::Identity;
use crate::tls;
use crate::usbmux;
use anyhow::{anyhow, bail, Context, Result};
use sidecar_protocol::pairing::Fingerprint;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// USB first, then Wi-Fi.
    Auto,
    Usb {
        udid: Option<String>,
    },
    Wifi,
    Tcp {
        addr: String,
    },
}

impl Transport {
    pub fn parse(kind: &str, udid: Option<String>, addr: Option<String>) -> Result<Transport> {
        Ok(match kind {
            "auto" => Transport::Auto,
            "usb" => Transport::Usb { udid },
            "wifi" => Transport::Wifi,
            "tcp" => Transport::Tcp {
                addr: addr.ok_or_else(|| anyhow!("--connect HOST:PORT required for tcp"))?,
            },
            _ => bail!("unknown transport {kind:?} (auto|usb|wifi|tcp)"),
        })
    }
}

pub struct Connected {
    pub stream: Box<dyn Io>,
    pub description: String,
    pub via_usb: bool,
}

pub async fn connect_usb(udid: Option<&str>, port: u16) -> Result<Connected> {
    let path = usbmux::socket_path();
    let devices = usbmux::list_devices(&path).await?;
    let dev = usbmux::choose(&devices, udid).ok_or_else(|| {
        if devices.is_empty() {
            anyhow!("no iOS device attached over USB (is it plugged in, unlocked and trusted?)")
        } else {
            anyhow!(
                "no matching USB device among {:?}",
                devices.iter().map(|d| &d.udid).collect::<Vec<_>>()
            )
        }
    })?;
    let s = usbmux::connect(&path, dev.device_id, port).await?;
    Ok(Connected {
        stream: Box::new(s),
        description: format!("USB {} (usbmux device {})", dev.udid, dev.device_id),
        via_usb: true,
    })
}

#[derive(Debug, Clone)]
pub struct Discovered {
    pub name: String,
    pub addrs: Vec<SocketAddr>,
    /// Certificate fingerprint advertised in the TXT record (`fp=`), used
    /// only to prefer a known iPad; trust is still checked after TLS.
    pub fp: Option<String>,
}

/// Browse `_sidecarlinux._tcp` for `wait`.
pub async fn discover(wait: Duration) -> Result<Vec<Discovered>> {
    tokio::task::spawn_blocking(move || -> Result<Vec<Discovered>> {
        let daemon = mdns_sd::ServiceDaemon::new().context("starting mDNS")?;
        let ty = format!("{}.local.", sidecar_protocol::BONJOUR_SERVICE_TYPE);
        let rx = daemon.browse(&ty)?;
        let deadline = std::time::Instant::now() + wait;
        let mut out: Vec<Discovered> = Vec::new();
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                    let mut addrs: Vec<SocketAddr> = info
                        .get_addresses()
                        .iter()
                        .map(|a| SocketAddr::new(a.to_ip_addr(), info.get_port()))
                        .collect();
                    addrs.sort_by_key(|a| !a.is_ipv4()); // IPv4 first: link-local v6 needs scope ids
                    let name = info
                        .get_fullname()
                        .split('.')
                        .next()
                        .unwrap_or("iPad")
                        .to_string();
                    if !out.iter().any(|d| d.name == name) {
                        out.push(Discovered {
                            name,
                            addrs,
                            fp: info.get_property_val_str("fp").map(str::to_string),
                        });
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = daemon.shutdown();
        Ok(out)
    })
    .await?
}

pub async fn connect_tcp(addr: &str) -> Result<Connected> {
    let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| anyhow!("timed out connecting to {addr}"))??;
    s.set_nodelay(true)?;
    Ok(Connected {
        stream: Box::new(s),
        description: format!("TCP {addr}"),
        via_usb: false,
    })
}

pub async fn connect_wifi(prefer_fp: &[String]) -> Result<Connected> {
    let found = discover(Duration::from_secs(3)).await?;
    if found.is_empty() {
        bail!("no iPad found on the network (is the app open with Wi-Fi enabled?)");
    }
    let mut ordered = found.clone();
    ordered.sort_by_key(|d| {
        !d.fp
            .as_ref()
            .is_some_and(|f| prefer_fp.iter().any(|p| p == f))
    });
    for d in &ordered {
        for a in &d.addrs {
            if let Ok(s) = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(a)).await
            {
                if let Ok(s) = s {
                    s.set_nodelay(true)?;
                    return Ok(Connected {
                        stream: Box::new(s),
                        description: format!("Wi-Fi {} ({a})", d.name),
                        via_usb: false,
                    });
                }
            }
        }
    }
    bail!(
        "found {} iPad(s) on the network but could not connect",
        found.len()
    )
}

pub async fn connect(t: &Transport, port: u16, prefer_fp: &[String]) -> Result<Connected> {
    match t {
        Transport::Usb { udid } => connect_usb(udid.as_deref(), port).await,
        Transport::Wifi => connect_wifi(prefer_fp).await,
        Transport::Tcp { addr } => connect_tcp(addr).await,
        Transport::Auto => match connect_usb(None, port).await {
            Ok(c) => Ok(c),
            Err(usb_err) => {
                tracing::debug!("USB: {usb_err:#}");
                connect_wifi(prefer_fp)
                    .await
                    .map_err(|wifi_err| anyhow!("USB: {usb_err:#}; Wi-Fi: {wifi_err:#}"))
            }
        },
    }
}

/// TLS client handshake; returns the stream and the iPad's certificate fingerprint.
pub async fn tls_connect(
    c: Connected,
    id: &Identity,
) -> Result<(tokio_rustls::client::TlsStream<Box<dyn Io>>, Fingerprint)> {
    let connector = tokio_rustls::TlsConnector::from(tls::client_config(id)?);
    let s = tokio::time::timeout(
        Duration::from_secs(10),
        connector.connect(tls::server_name(), c.stream),
    )
    .await
    .map_err(|_| anyhow!("TLS handshake timed out"))?
    .context("TLS handshake")?;
    let fp = tls::peer_fingerprint(s.get_ref().1.peer_certificates())?;
    Ok((s, fp))
}
