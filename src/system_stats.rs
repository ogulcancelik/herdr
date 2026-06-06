//! Background sampler for the global status line: cpu, memory, disk space,
//! battery, network throughput, and best-effort GPU utilization. Metrics
//! that cannot be read on the current platform stay `None` and the status
//! line simply omits them.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemStats {
    /// Global CPU utilization, 0..=100.
    pub cpu_percent: Option<f32>,
    /// Used / total memory in bytes.
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    /// Free space on the volume holding $HOME, in bytes.
    pub disk_free: Option<u64>,
    /// Battery charge 0..=100 and whether we are on AC.
    pub battery_percent: Option<u8>,
    pub battery_charging: Option<bool>,
    /// Network throughput since the previous sample, bytes/sec.
    pub net_rx_per_sec: Option<u64>,
    pub net_tx_per_sec: Option<u64>,
    /// Best-effort GPU utilization, 0..=100 (macOS IOAccelerator).
    pub gpu_percent: Option<u8>,
}

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn the sampler thread; it sends a snapshot through `notify` every
/// interval until the receiver disappears.
pub fn spawn_sampler(
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("system-stats".into())
        .spawn(move || {
            let mut system = sysinfo::System::new();
            let mut networks = sysinfo::Networks::new_with_refreshed_list();
            let disks = sysinfo::Disks::new_with_refreshed_list();
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            // First CPU sample needs a baseline.
            system.refresh_cpu_usage();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);

            let mut disks = disks;
            loop {
                system.refresh_cpu_usage();
                system.refresh_memory();
                disks.refresh(true);
                let elapsed = SAMPLE_INTERVAL.as_secs_f64();
                networks.refresh(true);

                let cpu_percent = Some(system.global_cpu_usage());
                let mem_total = Some(system.total_memory());
                let mem_used = Some(system.used_memory());

                let disk_free = home.as_deref().and_then(|home| {
                    disks
                        .iter()
                        .filter(|disk| home.starts_with(disk.mount_point()))
                        .max_by_key(|disk| disk.mount_point().as_os_str().len())
                        .map(|disk| disk.available_space())
                });

                let (mut rx, mut tx) = (0u64, 0u64);
                for (_, data) in networks.iter() {
                    rx = rx.saturating_add(data.received());
                    tx = tx.saturating_add(data.transmitted());
                }
                let net_rx_per_sec = Some((rx as f64 / elapsed) as u64);
                let net_tx_per_sec = Some((tx as f64 / elapsed) as u64);

                let (battery_percent, battery_charging) = read_battery();
                let gpu_percent = read_gpu_percent();

                let stats = SystemStats {
                    cpu_percent,
                    mem_used,
                    mem_total,
                    disk_free,
                    battery_percent,
                    battery_charging,
                    net_rx_per_sec,
                    net_tx_per_sec,
                    gpu_percent,
                };
                if event_tx
                    .blocking_send(crate::events::AppEvent::SystemStatsUpdated(stats))
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        })
        .expect("system stats sampler thread should spawn")
}

/// macOS: `pmset -g batt`. Other platforms: None (omitted from the line).
fn read_battery() -> (Option<u8>, Option<bool>) {
    if !cfg!(target_os = "macos") {
        return (None, None);
    }
    let Ok(output) = std::process::Command::new("pmset")
        .args(["-g", "batt"])
        .output()
    else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let percent = text
        .split_whitespace()
        .find_map(|token| token.strip_suffix("%;").or_else(|| token.strip_suffix('%')))
        .and_then(|token| token.parse::<u8>().ok());
    let charging = if text.contains("AC Power") {
        Some(true)
    } else if text.contains("Battery Power") {
        Some(false)
    } else {
        None
    };
    (percent, charging)
}

/// macOS best effort: IOAccelerator "Device Utilization %" via ioreg. Needs
/// no privileges on Apple Silicon; anything unparseable yields None.
fn read_gpu_percent() -> Option<u8> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = std::process::Command::new("ioreg")
        .args(["-r", "-d", "1", "-w", "0", "-c", "IOAccelerator"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_gpu_utilization(&text)
}

fn parse_gpu_utilization(ioreg_text: &str) -> Option<u8> {
    let idx = ioreg_text.find("\"Device Utilization %\"")?;
    let rest = &ioreg_text[idx..];
    let eq = rest.find('=')?;
    let value: String = rest[eq + 1..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    value.parse::<u8>().ok().filter(|v| *v <= 100)
}

/// Compact human formatting for the status line: bytes -> "312G", "1.4M".
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 || unit == 0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gpu_utilization_from_ioreg_block() {
        let text = r#"
    "PerformanceStatistics" = {"Device Utilization %"=37,"Renderer Utilization %"=35}
"#;
        assert_eq!(parse_gpu_utilization(text), Some(37));
    }

    #[test]
    fn gpu_parse_rejects_garbage() {
        assert_eq!(parse_gpu_utilization("no gpu here"), None);
        assert_eq!(parse_gpu_utilization("\"Device Utilization %\"=x"), None);
    }

    #[test]
    fn human_bytes_formats_compactly() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(1500), "1.5K");
        assert_eq!(human_bytes(18 * 1024 * 1024 * 1024), "18G");
    }
}
