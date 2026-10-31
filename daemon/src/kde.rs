//! KDE Plasma integration: pin the virtual pen and touchscreen to an output
//! through KWin's D-Bus input-device API
//! (`org.kde.KWin.InputDevice.outputName`).
//!
//! We shell out to `busctl` (part of systemd) instead of linking a D-Bus
//! library; it's only called once per device creation.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const SERVICE: &str = "org.kde.KWin";
const IFACE: &str = "org.kde.KWin.InputDevice";

/// `/dev/input/event12` → `/org/kde/KWin/InputDevice/event12`
pub fn object_path(node: &std::path::Path) -> Option<String> {
    let name = node.file_name()?.to_str()?;
    name.starts_with("event")
        .then(|| format!("/org/kde/KWin/InputDevice/{name}"))
}

fn busctl(args: &[&str]) -> Result<String> {
    let out = Command::new("busctl")
        .arg("--user")
        .args(args)
        .output()
        .context("running busctl")?;
    if !out.status.success() {
        bail!(
            "busctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn is_true(p: &str, property: &str) -> Result<bool> {
    Ok(busctl(&["get-property", SERVICE, p, IFACE, property])?.ends_with("true"))
}

/// Sets `outputName` on our tablet and touchscreen devices (not the
/// keyboard). KWin registers new devices asynchronously, so retry for a
/// couple of seconds.
pub fn map_to_output(nodes: &[PathBuf], output: &str) -> Result<()> {
    let paths: Vec<String> = nodes.iter().filter_map(|n| object_path(n)).collect();
    if paths.is_empty() {
        bail!("no event node known for the pen device");
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for p in &paths {
        loop {
            // tabletTool answers once KWin knows the device; then ask about
            // touch (an error there just means "not a touchscreen").
            let kind = is_true(p, "tabletTool").map(|tablet| {
                if tablet {
                    Some("pen")
                } else if is_true(p, "touch").unwrap_or(false) {
                    Some("touchscreen")
                } else {
                    None
                }
            });
            match kind {
                Ok(Some(kind)) => {
                    busctl(&["set-property", SERVICE, p, IFACE, "outputName", "s", output])?;
                    tracing::info!("KWin: mapped {p} ({kind}) to output {output}");
                    break;
                }
                Ok(None) => break, // the keyboard device
                Err(e) if Instant::now() < deadline => {
                    tracing::debug!("waiting for KWin to see {p}: {e:#}");
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths() {
        assert_eq!(
            object_path(std::path::Path::new("/dev/input/event7")).as_deref(),
            Some("/org/kde/KWin/InputDevice/event7")
        );
        assert_eq!(object_path(std::path::Path::new("/dev/input/js0")), None);
    }
}
