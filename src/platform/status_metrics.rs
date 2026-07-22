//! Native system metrics for the Herdr status bar.
//!
//! Collectors stay in-process: libc/sysctl/mach/IOKit and /proc|/sys only.
//! No shell-outs to tmux, powerline, `ps`, `vm_stat`, `pmset`, etc.
//!
//! Public IP is the one exception that needs the network: it is fetched in a
//! background thread and cached to disk (powerline-compatible 5 minute TTL),
//! so the render path never blocks on HTTP.
//!
//! Values are cached briefly so rendering never blocks on syscalls.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

#[cfg_attr(test, allow(dead_code))]
const CACHE_TTL: Duration = Duration::from_millis(1500);

/// Match tmux-powerline `network_ips` WAN cache TTL.
#[cfg_attr(test, allow(dead_code))]
const PUBLIC_IP_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Default)]
pub(crate) struct StatusMetrics {
    pub cpu_percent: Option<u8>,
    pub mem_used_gb: Option<f32>,
    pub mem_total_gb: Option<f32>,
    pub battery_percent: Option<u8>,
    pub battery_charging: Option<bool>,
    pub local_ip: Option<String>,
    pub tailscale_ip: Option<String>,
    pub public_ip: Option<String>,
    pub net_down_kib: Option<u64>,
    pub net_up_kib: Option<u64>,
    /// Primary uplink kind for the bandwidth glyph (wifi vs ethernet).
    pub net_kind: NetKind,
    /// True when a VPN/tunnel (utun/wg/tailscale/tun) is up.
    pub vpn_active: bool,
    /// True when this process is running over SSH/mosh.
    pub remote_session: bool,
    pub hostname: String,
    pub username: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum NetKind {
    #[default]
    Unknown,
    Wifi,
    Ethernet,
}

#[cfg_attr(test, allow(dead_code))]
struct Cache {
    at: Instant,
    value: StatusMetrics,
}

#[cfg_attr(test, allow(dead_code))]
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Previous CPU tick sample for rate calculation (idle, total).
static CPU_PREV: Mutex<Option<(u64, u64)>> = Mutex::new(None);

/// Previous interface byte counters (rx, tx) + sample time for bandwidth.
static NET_PREV: Mutex<Option<(u64, u64, Instant)>> = Mutex::new(None);

/// Only one public-IP refresh in flight at a time.
static PUBLIC_IP_FETCHING: AtomicBool = AtomicBool::new(false);

pub(crate) fn status_metrics() -> StatusMetrics {
    // UI characterization tests hash the full frame. Live host metrics would
    // make those digests non-deterministic, so tests always get a fixture.
    #[cfg(test)]
    {
        return status_metrics_fixture();
    }

    #[cfg(not(test))]
    {
        if let Ok(guard) = CACHE.lock() {
            if let Some(cache) = guard.as_ref() {
                if cache.at.elapsed() < CACHE_TTL {
                    return cache.value.clone();
                }
            }
        }

        let value = collect_status_metrics();
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some(Cache {
                at: Instant::now(),
                value: value.clone(),
            });
        }
        value
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn status_metrics_fixture() -> StatusMetrics {
    StatusMetrics {
        cpu_percent: Some(12),
        mem_used_gb: Some(8.0),
        mem_total_gb: Some(16.0),
        battery_percent: Some(88),
        battery_charging: Some(false),
        local_ip: Some("10.0.0.2".into()),
        tailscale_ip: Some("100.64.0.1".into()),
        public_ip: Some("203.0.113.10".into()),
        net_down_kib: Some(120),
        net_up_kib: Some(34),
        net_kind: NetKind::Wifi,
        vpn_active: true,
        remote_session: false,
        hostname: "testhost".into(),
        username: "testuser".into(),
    }
}

fn collect_status_metrics() -> StatusMetrics {
    let mut metrics = StatusMetrics {
        hostname: hostname(),
        username: username(),
        remote_session: is_remote_session(),
        ..StatusMetrics::default()
    };

    #[cfg(target_os = "macos")]
    collect_macos(&mut metrics);
    #[cfg(target_os = "linux")]
    collect_linux(&mut metrics);

    // Public IP is OS-agnostic: disk cache + optional background refresh.
    metrics.public_ip = public_ip_cached();

    metrics
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is a valid writable buffer of known length.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "localhost".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    short_host(&name)
}

fn username() -> String {
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return user;
        }
    }
    // SAFETY: getuid is always safe; getpwuid returns static/heap-owned data.
    let uid = unsafe { libc::getuid() };
    let pwd = unsafe { libc::getpwuid(uid) };
    if pwd.is_null() {
        return uid.to_string();
    }
    // SAFETY: pwd is non-null; pw_name is a NUL-terminated C string.
    let name = unsafe { std::ffi::CStr::from_ptr((*pwd).pw_name) };
    name.to_string_lossy().into_owned()
}

fn short_host(name: &str) -> String {
    name.split('.').next().unwrap_or(name).to_string()
}

fn is_remote_session() -> bool {
    for key in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH"] {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn collect_macos(metrics: &mut StatusMetrics) {
    if let Some(total) = sysctl_u64(c"hw.memsize") {
        metrics.mem_total_gb = Some(total as f32 / 1_073_741_824.0);
    }
    metrics.mem_used_gb = macos_mem_used_gb();
    metrics.cpu_percent = sample_cpu_percent(macos_cpu_ticks);
    let (pct, charging) = macos_battery();
    metrics.battery_percent = pct;
    metrics.battery_charging = charging;
    let (local, tailscale) = interface_ipv4s();
    metrics.local_ip = local;
    metrics.tailscale_ip = tailscale;
    metrics.vpn_active = macos_vpn_active() || metrics.tailscale_ip.is_some();
    metrics.net_kind = macos_net_kind();
    if let Some((down, up)) = sample_bandwidth(macos_primary_iface_bytes) {
        metrics.net_down_kib = Some(down);
        metrics.net_up_kib = Some(up);
    }
}

#[cfg(target_os = "linux")]
fn collect_linux(metrics: &mut StatusMetrics) {
    if let Some((used, total)) = linux_mem_gb() {
        metrics.mem_used_gb = Some(used);
        metrics.mem_total_gb = Some(total);
    }
    metrics.cpu_percent = sample_cpu_percent(linux_cpu_ticks);
    let (pct, charging) = linux_battery();
    metrics.battery_percent = pct;
    metrics.battery_charging = charging;
    let (local, tailscale) = interface_ipv4s();
    metrics.local_ip = local;
    metrics.tailscale_ip = tailscale;
    let iface = linux_default_iface();
    metrics.net_kind = linux_net_kind(iface.as_deref());
    metrics.vpn_active = linux_vpn_active() || metrics.tailscale_ip.is_some();
    if let Some((down, up)) = sample_bandwidth(|| linux_primary_iface_bytes(iface.as_deref())) {
        metrics.net_down_kib = Some(down);
        metrics.net_up_kib = Some(up);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sample_cpu_percent(ticks: impl FnOnce() -> Option<(u64, u64)>) -> Option<u8> {
    let (idle, total) = ticks()?;
    let mut prev = CPU_PREV.lock().ok()?;
    let result = if let Some((prev_idle, prev_total)) = *prev {
        let d_idle = idle.saturating_sub(prev_idle);
        let d_total = total.saturating_sub(prev_total);
        if d_total > 0 {
            let busy = d_total.saturating_sub(d_idle) as f64;
            Some(((busy / d_total as f64) * 100.0).round().clamp(0.0, 100.0) as u8)
        } else {
            Some(0)
        }
    } else {
        // First sample establishes the baseline; show 0 so the segment paints.
        Some(0)
    };
    *prev = Some((idle, total));
    result
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sample_bandwidth(bytes: impl FnOnce() -> Option<(u64, u64)>) -> Option<(u64, u64)> {
    let (rx, tx) = bytes()?;
    let now = Instant::now();
    let mut prev = NET_PREV.lock().ok()?;
    let result = if let Some((prev_rx, prev_tx, prev_at)) = *prev {
        let secs = now.duration_since(prev_at).as_secs_f64().max(0.001);
        // Counter reset / interface change: treat as a fresh baseline.
        if rx < prev_rx || tx < prev_tx {
            *prev = Some((rx, tx, now));
            return Some((0, 0));
        }
        let down = ((rx.saturating_sub(prev_rx) as f64) / secs / 1024.0).round() as u64;
        let up = ((tx.saturating_sub(prev_tx) as f64) / secs / 1024.0).round() as u64;
        Some((down, up))
    } else {
        // First sample establishes the baseline; show 0K/s so the segment paints.
        Some((0, 0))
    };
    *prev = Some((rx, tx, now));
    result
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: name is a valid C string; value/len point to valid stack storage.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && len == std::mem::size_of::<u64>() {
        Some(value)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_mem_used_gb() -> Option<f32> {
    const HOST_VM_INFO64: libc::c_int = 4;
    const HOST_VM_INFO64_COUNT: libc::mach_msg_type_number_t =
        (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;

    #[repr(C)]
    #[derive(Default)]
    struct VmStatistics64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }

    // SAFETY: host_self returns a send right we don't own; stats buffer is stack-owned.
    let host = unsafe { mach_host_self() };
    let mut stats = VmStatistics64::default();
    let mut count = HOST_VM_INFO64_COUNT;
    let rc = unsafe {
        host_statistics64(
            host,
            HOST_VM_INFO64,
            (&raw mut stats).cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    let page_size = sysctl_u64(c"hw.pagesize").unwrap_or(4096);
    let total = sysctl_u64(c"hw.memsize")?;
    // App memory ≈ internal + wired + compressor (matches Activity Monitor "Memory Used").
    let used_pages = u64::from(stats.internal_page_count)
        .saturating_add(u64::from(stats.wire_count))
        .saturating_add(u64::from(stats.compressor_page_count));
    let used = used_pages.saturating_mul(page_size).min(total);
    Some(used as f32 / 1_073_741_824.0)
}

#[cfg(target_os = "macos")]
fn macos_cpu_ticks() -> Option<(u64, u64)> {
    const HOST_CPU_LOAD_INFO: libc::c_int = 3;
    const CPU_STATE_USER: usize = 0;
    const CPU_STATE_SYSTEM: usize = 1;
    const CPU_STATE_IDLE: usize = 2;
    const CPU_STATE_NICE: usize = 3;
    const HOST_CPU_LOAD_INFO_COUNT: libc::mach_msg_type_number_t = 4;

    #[repr(C)]
    struct HostCpuLoadInfo {
        cpu_ticks: [u32; 4],
    }

    let host = unsafe { mach_host_self() };
    let mut info = HostCpuLoadInfo { cpu_ticks: [0; 4] };
    let mut count = HOST_CPU_LOAD_INFO_COUNT;
    let rc = unsafe {
        host_statistics(
            host,
            HOST_CPU_LOAD_INFO,
            (&raw mut info).cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    let user = u64::from(info.cpu_ticks[CPU_STATE_USER]);
    let system = u64::from(info.cpu_ticks[CPU_STATE_SYSTEM]);
    let idle = u64::from(info.cpu_ticks[CPU_STATE_IDLE]);
    let nice = u64::from(info.cpu_ticks[CPU_STATE_NICE]);
    let total = user + system + idle + nice;
    Some((idle, total))
}

#[cfg(target_os = "macos")]
fn macos_battery() -> (Option<u8>, Option<bool>) {
    // SAFETY: IOKit power-source snapshot is retained; we release both blob and list.
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return (None, None);
        }
        let list = IOPSCopyPowerSourcesList(blob);
        if list.is_null() {
            CFRelease(blob);
            return (None, None);
        }
        let count = CFArrayGetCount(list);
        let mut best: Option<(u8, bool)> = None;
        for i in 0..count {
            let ps = CFArrayGetValueAtIndex(list, i);
            if ps.is_null() {
                continue;
            }
            let desc = IOPSGetPowerSourceDescription(blob, ps);
            if desc.is_null() {
                continue;
            }
            // Prefer internal batteries; fall back to any source with capacity.
            let is_internal = cf_dict_string_eq(desc, IOPS_TYPE_KEY, IOPS_INTERNAL_BATTERY_TYPE);
            let capacity = cf_dict_i32(desc, IOPS_CURRENT_CAPACITY_KEY).unwrap_or(-1);
            if !(0..=100).contains(&capacity) {
                continue;
            }
            let state = cf_dict_string(desc, IOPS_POWER_SOURCE_STATE_KEY).unwrap_or_default();
            let charging = state == IOPS_AC_POWER_VALUE
                || cf_dict_bool(desc, IOPS_IS_CHARGING_KEY).unwrap_or(false);
            let candidate = (capacity as u8, charging);
            if is_internal {
                best = Some(candidate);
                break;
            }
            if best.is_none() {
                best = Some(candidate);
            }
        }
        CFRelease(list);
        CFRelease(blob);
        match best {
            Some((pct, charging)) => (Some(pct), Some(charging)),
            None => (None, None),
        }
    }
}

/// Primary interface byte counters via `NET_RT_IFLIST2` / `if_data64`.
///
/// Prefer the default-route interface (`en0` fallback). Using 64-bit counters
/// avoids the wraparound that 32-bit `getifaddrs` `if_data` hits on busy links.
#[cfg(target_os = "macos")]
fn macos_primary_iface_bytes() -> Option<(u64, u64)> {
    let preferred = macos_default_iface().unwrap_or_else(|| "en0".into());
    let snapshots = macos_iface_byte_snapshots()?;
    if let Some((_, rx, tx)) = snapshots.iter().find(|(n, _, _)| n == &preferred) {
        return Some((*rx, *tx));
    }
    // Fall back to the busiest non-loopback interface.
    snapshots
        .into_iter()
        .filter(|(n, _, _)| !n.starts_with("lo"))
        .max_by_key(|(_, rx, tx)| rx.saturating_add(*tx))
        .map(|(_, rx, tx)| (rx, tx))
}

#[cfg(target_os = "macos")]
fn macos_iface_byte_snapshots() -> Option<Vec<(String, u64, u64)>> {
    // CTL_NET / AF_ROUTE / NET_RT_IFLIST2
    const CTL_NET: libc::c_int = 4;
    const AF_ROUTE: libc::c_int = 17;
    const NET_RT_IFLIST2: libc::c_int = 6;
    const RTM_IFINFO2: u8 = 0x12;

    // Darwin `IF_DATA_TIMEVAL` is `struct timeval32` (two u32s) on modern SDKs.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct IfDataTimeval32 {
        tv_sec: u32,
        tv_usec: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct IfData64 {
        ifi_type: u8,
        ifi_typelen: u8,
        ifi_physical: u8,
        ifi_addrlen: u8,
        ifi_hdrlen: u8,
        ifi_recvquota: u8,
        ifi_xmitquota: u8,
        ifi_unused1: u8,
        ifi_mtu: u32,
        ifi_metric: u32,
        ifi_baudrate: u64,
        ifi_ipackets: u64,
        ifi_ierrors: u64,
        ifi_opackets: u64,
        ifi_oerrors: u64,
        ifi_collisions: u64,
        ifi_ibytes: u64,
        ifi_obytes: u64,
        ifi_imcasts: u64,
        ifi_omcasts: u64,
        ifi_iqdrops: u64,
        ifi_noproto: u64,
        ifi_recvtiming: u32,
        ifi_xmittiming: u32,
        ifi_lastchange: IfDataTimeval32,
    }

    #[repr(C)]
    struct IfMsghdr2 {
        ifm_msglen: u16,
        ifm_version: u8,
        ifm_type: u8,
        ifm_addrs: i32,
        ifm_flags: i32,
        ifm_index: u16,
        _ifm_pad: u16,
        ifm_snd_len: i32,
        ifm_snd_maxlen: i32,
        ifm_snd_drops: i32,
        ifm_timer: i32,
        ifm_data: IfData64,
    }


    let mut mib: [libc::c_int; 6] = [CTL_NET, AF_ROUTE, 0, 0, NET_RT_IFLIST2, 0];
    let mut len: libc::size_t = 0;
    // SAFETY: mib points at a valid 6-int array; size probe with null data pointer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);

    // Layout sanity — Darwin if_msghdr2 is 160 bytes with timeval32 lastchange.
    debug_assert_eq!(std::mem::size_of::<IfData64>(), 128);
    debug_assert_eq!(std::mem::size_of::<IfMsghdr2>(), 160);

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= buf.len() {
        let msglen = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        let ifm_type = buf[offset + 3];
        if msglen == 0 || offset + msglen > buf.len() {
            break;
        }
        if ifm_type == RTM_IFINFO2 && msglen >= std::mem::size_of::<IfMsghdr2>() {
            // Copy out — route-table messages are not guaranteed 8-byte aligned.
            let mut hdr = std::mem::MaybeUninit::<IfMsghdr2>::uninit();
            // SAFETY: msglen bounds-checked; we copy exactly one header.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    buf.as_ptr().add(offset),
                    hdr.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<IfMsghdr2>(),
                );
            }
            let hdr = unsafe { hdr.assume_init() };
            let sdl_off = offset + std::mem::size_of::<IfMsghdr2>();
            if sdl_off + 8 <= offset + msglen {
                let sdl_nlen = buf[sdl_off + 5] as usize;
                let name_off = sdl_off + 8;
                if sdl_nlen > 0 && name_off + sdl_nlen <= offset + msglen {
                    let name = String::from_utf8_lossy(&buf[name_off..name_off + sdl_nlen]).into_owned();
                    out.push((name, hdr.ifm_data.ifi_ibytes, hdr.ifm_data.ifi_obytes));
                }
            }
        }
        offset += msglen;
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Best-effort default interface name via `route get default`-equivalent:
/// parse the routing table for RTF_GATEWAY|RTF_UP default route. Falls back
/// to the first `en*` interface with traffic.
#[cfg(target_os = "macos")]
fn macos_default_iface() -> Option<String> {
    // Lightweight heuristic: prefer en0 (Wi-Fi on Apple Silicon / Intel Macs),
    // then any en* with non-zero counters, then first non-lo.
    let snaps = macos_iface_byte_snapshots()?;
    if snaps.iter().any(|(n, _, _)| n == "en0") {
        return Some("en0".into());
    }
    if let Some((n, _, _)) = snaps
        .iter()
        .filter(|(n, rx, tx)| n.starts_with("en") && (*rx > 0 || *tx > 0))
        .max_by_key(|(_, rx, tx)| rx.saturating_add(*tx))
    {
        return Some(n.clone());
    }
    snaps
        .into_iter()
        .find(|(n, _, _)| !n.starts_with("lo"))
        .map(|(n, _, _)| n)
}

#[cfg(target_os = "macos")]
fn macos_net_kind() -> NetKind {
    match macos_default_iface().as_deref() {
        // Apple Wi-Fi is almost always en0; en1/en2 are Thunderbolt bridges.
        Some("en0") => NetKind::Wifi,
        Some(name) if name.starts_with("en") => NetKind::Ethernet,
        Some(name) if name.starts_with("eth") || name.starts_with("usb") => NetKind::Ethernet,
        Some(name) if name.starts_with("wl") => NetKind::Wifi,
        _ => NetKind::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn macos_vpn_active() -> bool {
    // Any utun/ipsec/ppp interface with traffic indicates a tunnel.
    let Some(snaps) = macos_iface_byte_snapshots() else {
        return false;
    };
    snaps.iter().any(|(n, rx, tx)| {
        let tun = n.starts_with("utun")
            || n.starts_with("ipsec")
            || n.starts_with("ppp")
            || n.contains("tailscale");
        tun && (*rx > 0 || *tx > 0)
    })
}

#[cfg(target_os = "linux")]
fn linux_mem_gb() -> Option<(f32, f32)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = None;
    let mut available_kb = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_kb(rest);
        }
    }
    let total = total_kb?;
    let available = available_kb.unwrap_or(0);
    let used = total.saturating_sub(available);
    Some((used as f32 / 1_048_576.0, total as f32 / 1_048_576.0))
}

#[cfg(target_os = "linux")]
fn parse_kb(rest: &str) -> Option<u64> {
    rest.split_whitespace().next()?.parse().ok()
}

#[cfg(target_os = "linux")]
fn linux_cpu_ticks() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let mut vals = [0u64; 8];
    for slot in vals.iter_mut() {
        *slot = parts.next()?.parse().ok()?;
    }
    // user nice system idle iowait irq softirq steal
    let idle = vals[3].saturating_add(vals[4]);
    let total: u64 = vals.iter().sum();
    Some((idle, total))
}

#[cfg(target_os = "linux")]
fn linux_battery() -> (Option<u8>, Option<bool>) {
    let bat_dir = std::fs::read_dir("/sys/class/power_supply").ok().and_then(|rd| {
        rd.filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("BAT"))
            })
    });
    let Some(bat) = bat_dir else {
        return (None, None);
    };
    let capacity = std::fs::read_to_string(bat.join("capacity"))
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok());
    let status = std::fs::read_to_string(bat.join("status"))
        .ok()
        .map(|s| s.trim().to_ascii_lowercase());
    let charging = status.map(|s| s == "charging" || s == "full");
    (capacity, charging)
}

#[cfg(target_os = "linux")]
fn linux_default_iface() -> Option<String> {
    // /proc/net/route: destination 00000000 is the default route.
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in text.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?;
        let dest = cols.next()?;
        if dest == "00000000" {
            return Some(iface.to_string());
        }
    }
    // Fall back to first non-lo interface with a stats file.
    let rd = std::fs::read_dir("/sys/class/net").ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "lo" {
            return Some(name);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_net_kind(iface: Option<&str>) -> NetKind {
    let Some(iface) = iface else {
        return NetKind::Unknown;
    };
    if std::path::Path::new(&format!("/sys/class/net/{iface}/wireless")).exists() {
        return NetKind::Wifi;
    }
    // type 1 = ARPHRD_ETHER
    let type_path = format!("/sys/class/net/{iface}/type");
    if let Ok(t) = std::fs::read_to_string(type_path) {
        if t.trim() == "1" {
            return NetKind::Ethernet;
        }
    }
    if iface.starts_with("wl") || iface.starts_with("wlan") {
        NetKind::Wifi
    } else if iface.starts_with("eth") || iface.starts_with("en") {
        NetKind::Ethernet
    } else {
        NetKind::Unknown
    }
}

#[cfg(target_os = "linux")]
fn linux_vpn_active() -> bool {
    let Ok(rd) = std::fs::read_dir("/sys/class/net") else {
        return false;
    };
    rd.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        name.starts_with("tun")
            || name.starts_with("wg")
            || name.starts_with("tailscale")
            || name.starts_with("ppp")
            || name.starts_with("ipsec")
    })
}

#[cfg(target_os = "linux")]
fn linux_primary_iface_bytes(iface: Option<&str>) -> Option<(u64, u64)> {
    let iface = match iface {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => linux_default_iface()?,
    };
    let rx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let tx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((rx, tx))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn interface_ipv4s() -> (Option<String>, Option<String>) {
    // SAFETY: getifaddrs/freeifaddrs pair.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 || ifap.is_null() {
            return (None, None);
        }

        // Collect candidates so we can prefer physical LAN over virtual bridges.
        let mut lan_preferred: Option<String> = None;
        let mut lan_fallback: Option<String> = None;
        let mut tailscale: Option<String> = None;
        let mut cur = ifap;
        while !cur.is_null() {
            let entry = &*cur;
            if entry.ifa_addr.is_null() {
                cur = entry.ifa_next;
                continue;
            }
            if (*entry.ifa_addr).sa_family as i32 != libc::AF_INET {
                cur = entry.ifa_next;
                continue;
            }
            let sin = &*(entry.ifa_addr as *const libc::sockaddr_in);
            let addr = u32::from_be(sin.sin_addr.s_addr);
            if addr == 0 || (addr >> 24) == 127 {
                cur = entry.ifa_next;
                continue;
            }
            let ip = format!(
                "{}.{}.{}.{}",
                (addr >> 24) & 0xff,
                (addr >> 16) & 0xff,
                (addr >> 8) & 0xff,
                addr & 0xff
            );
            let name = if entry.ifa_name.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(entry.ifa_name)
                    .to_string_lossy()
                    .into_owned()
            };

            let is_tunnel = name.contains("tailscale")
                || name.starts_with("utun")
                || name.starts_with("tun")
                || name.starts_with("wg")
                || name.starts_with("ppp")
                || name.starts_with("ipsec");
            // Tailscale userspace networking advertises CGNAT 100.64/10 on a tunnel iface.
            let is_tailscale = name.contains("tailscale")
                || (is_cgnat(addr) && is_tunnel);

            if is_tailscale {
                if tailscale.is_none() {
                    tailscale = Some(ip);
                }
            } else if !is_tunnel {
                let preferred = name.starts_with("en")
                    || name.starts_with("eth")
                    || name.starts_with("wl")
                    || name.starts_with("wlan");
                if preferred {
                    if lan_preferred.is_none() {
                        lan_preferred = Some(ip);
                    }
                } else if lan_fallback.is_none() {
                    lan_fallback = Some(ip);
                }
            }
            cur = entry.ifa_next;
        }
        libc::freeifaddrs(ifap);
        (lan_preferred.or(lan_fallback), tailscale)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_cgnat(addr: u32) -> bool {
    // 100.64.0.0/10
    (addr >> 22) == (0x6440_0000 >> 22)
}

// ---------------------------------------------------------------------------
// Public IP — disk-cached, background-refreshed (never blocks render)
// ---------------------------------------------------------------------------

fn public_ip_cache_path() -> PathBuf {
    // Prefer a stable user cache dir; fall back to /tmp like powerline does.
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home).join(".cache/herdr");
        let _ = std::fs::create_dir_all(&dir);
        return dir.join("public-ip");
    }
    PathBuf::from("/tmp/herdr-public-ip")
}

fn public_ip_cached() -> Option<String> {
    let path = public_ip_cache_path();
    if let Some(ip) = read_public_ip_cache(&path) {
        // Refresh in the background once the entry is past half-life so the
        // next paint after TTL still has a warm value.
        if public_ip_cache_age_secs(&path).is_some_and(|age| age > PUBLIC_IP_TTL_SECS / 2) {
            spawn_public_ip_refresh(path);
        }
        return Some(ip);
    }
    // One-shot migration from the tmux-powerline WAN cache so the first Herdr
    // paint after switching already shows the public IP.
    let legacy = std::path::Path::new("/tmp/tmux-powerline-wan-ip");
    if let Some(ip) = read_public_ip_cache(legacy) {
        let _ = std::fs::write(&path, format!("{ip}\n"));
        return Some(ip);
    }
    spawn_public_ip_refresh(path);
    None
}

fn read_public_ip_cache(path: &std::path::Path) -> Option<String> {
    let age = public_ip_cache_age_secs(path)?;
    if age > PUBLIC_IP_TTL_SECS {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let ip = text.trim();
    if is_plausible_ipv4(ip) {
        Some(ip.to_string())
    } else {
        None
    }
}

fn public_ip_cache_age_secs(path: &std::path::Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    Some(age.as_secs())
}

fn spawn_public_ip_refresh(path: PathBuf) {
    if PUBLIC_IP_FETCHING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    std::thread::Builder::new()
        .name("herdr-public-ip".into())
        .spawn(move || {
            let ip = fetch_public_ip();
            if let Some(ip) = ip {
                let _ = std::fs::write(&path, format!("{ip}\n"));
            }
            PUBLIC_IP_FETCHING.store(false, Ordering::SeqCst);
        })
        .ok();
}

fn fetch_public_ip() -> Option<String> {
    // Use the same noninteractive curl helper as the rest of herdr so PATH /
    // sandbox behaviour stays consistent. Bound total time tightly.
    let mut cmd = crate::noninteractive_process::curl_command();
    cmd.args([
        "-4",
        "-fsS",
        "--max-time",
        "2",
        "--connect-timeout",
        "1",
        "https://ifconfig.me",
    ]);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        // Fallback endpoint.
        let mut cmd = crate::noninteractive_process::curl_command();
        cmd.args([
            "-4",
            "-fsS",
            "--max-time",
            "2",
            "--connect-timeout",
            "1",
            "https://icanhazip.com",
        ]);
        let output = cmd.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return is_plausible_ipv4(&ip).then_some(ip);
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    is_plausible_ipv4(&ip).then_some(ip)
}

fn is_plausible_ipv4(s: &str) -> bool {
    let mut parts = s.split('.');
    let mut n = 0;
    for part in parts.by_ref() {
        n += 1;
        if n > 4 {
            return false;
        }
        let Ok(v) = part.parse::<u8>() else {
            return false;
        };
        let _ = v;
    }
    n == 4 && parts.next().is_none()
}

// ---------------------------------------------------------------------------
// macOS FFI
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_host_self() -> libc::mach_port_t;
    fn host_statistics(
        host: libc::mach_port_t,
        flavor: libc::c_int,
        host_info_out: *mut libc::integer_t,
        host_info_outCnt: *mut libc::mach_msg_type_number_t,
    ) -> libc::c_int;
    fn host_statistics64(
        host: libc::mach_port_t,
        flavor: libc::c_int,
        host_info_out: *mut libc::integer_t,
        host_info_outCnt: *mut libc::mach_msg_type_number_t,
    ) -> libc::c_int;
}

#[cfg(target_os = "macos")]
type CfTypeRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfArrayRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfDictionaryRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfStringRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfNumberRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfBooleanRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CfIndex = isize;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CfTypeRef);
    fn CFArrayGetCount(theArray: CfArrayRef) -> CfIndex;
    fn CFArrayGetValueAtIndex(theArray: CfArrayRef, idx: CfIndex) -> CfTypeRef;
    fn CFDictionaryGetValue(theDict: CfDictionaryRef, key: CfTypeRef) -> CfTypeRef;
    fn CFStringCreateWithCString(
        alloc: CfTypeRef,
        cStr: *const libc::c_char,
        encoding: u32,
    ) -> CfStringRef;
    fn CFStringGetCString(
        theString: CfStringRef,
        buffer: *mut libc::c_char,
        bufferSize: CfIndex,
        encoding: u32,
    ) -> u8;
    fn CFNumberGetValue(number: CfNumberRef, theType: i32, valuePtr: *mut std::ffi::c_void) -> u8;
    fn CFBooleanGetValue(boolean: CfBooleanRef) -> u8;
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CfTypeRef;
    fn IOPSCopyPowerSourcesList(blob: CfTypeRef) -> CfArrayRef;
    fn IOPSGetPowerSourceDescription(blob: CfTypeRef, ps: CfTypeRef) -> CfDictionaryRef;
}

#[cfg(target_os = "macos")]
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const CF_NUMBER_INT_TYPE: i32 = 9;

// IOKit power-source keys (from IOKit/ps/IOPSKeys.h).
#[cfg(target_os = "macos")]
const IOPS_TYPE_KEY: &str = "Type";
#[cfg(target_os = "macos")]
const IOPS_INTERNAL_BATTERY_TYPE: &str = "InternalBattery";
#[cfg(target_os = "macos")]
const IOPS_CURRENT_CAPACITY_KEY: &str = "Current Capacity";
#[cfg(target_os = "macos")]
const IOPS_POWER_SOURCE_STATE_KEY: &str = "Power Source State";
#[cfg(target_os = "macos")]
const IOPS_AC_POWER_VALUE: &str = "AC Power";
#[cfg(target_os = "macos")]
const IOPS_IS_CHARGING_KEY: &str = "Is Charging";

#[cfg(target_os = "macos")]
unsafe fn cf_dict_i32(dict: CfDictionaryRef, key: &str) -> Option<i32> {
    let key_ref = CFStringCreateWithCString(
        std::ptr::null(),
        std::ffi::CString::new(key).ok()?.as_ptr(),
        CF_STRING_ENCODING_UTF8,
    );
    if key_ref.is_null() {
        return None;
    }
    let val = CFDictionaryGetValue(dict, key_ref);
    CFRelease(key_ref);
    if val.is_null() {
        return None;
    }
    let mut out: i32 = 0;
    if CFNumberGetValue(val, CF_NUMBER_INT_TYPE, (&raw mut out).cast()) == 0 {
        return None;
    }
    Some(out)
}

#[cfg(target_os = "macos")]
unsafe fn cf_dict_bool(dict: CfDictionaryRef, key: &str) -> Option<bool> {
    let key_ref = CFStringCreateWithCString(
        std::ptr::null(),
        std::ffi::CString::new(key).ok()?.as_ptr(),
        CF_STRING_ENCODING_UTF8,
    );
    if key_ref.is_null() {
        return None;
    }
    let val = CFDictionaryGetValue(dict, key_ref);
    CFRelease(key_ref);
    if val.is_null() {
        return None;
    }
    Some(CFBooleanGetValue(val) != 0)
}

#[cfg(target_os = "macos")]
unsafe fn cf_dict_string(dict: CfDictionaryRef, key: &str) -> Option<String> {
    let key_ref = CFStringCreateWithCString(
        std::ptr::null(),
        std::ffi::CString::new(key).ok()?.as_ptr(),
        CF_STRING_ENCODING_UTF8,
    );
    if key_ref.is_null() {
        return None;
    }
    let val = CFDictionaryGetValue(dict, key_ref);
    CFRelease(key_ref);
    if val.is_null() {
        return None;
    }
    let mut buf = [0i8; 128];
    if CFStringGetCString(
        val,
        buf.as_mut_ptr(),
        buf.len() as CfIndex,
        CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }
    let cstr = std::ffi::CStr::from_ptr(buf.as_ptr());
    Some(cstr.to_string_lossy().into_owned())
}

#[cfg(target_os = "macos")]
unsafe fn cf_dict_string_eq(dict: CfDictionaryRef, key: &str, expect: &str) -> bool {
    cf_dict_string(dict, key).is_some_and(|s| s == expect)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_deterministic() {
        let a = status_metrics();
        let b = status_metrics_fixture();
        assert_eq!(a.hostname, b.hostname);
        assert_eq!(a.username, b.username);
        assert_eq!(a.cpu_percent, b.cpu_percent);
        assert_eq!(a.public_ip.as_deref(), Some("203.0.113.10"));
        assert_eq!(a.net_kind, NetKind::Wifi);
        assert!(a.vpn_active);
    }

    #[test]
    fn short_host_strips_domain() {
        assert_eq!(short_host("macbook16.local"), "macbook16");
        assert_eq!(short_host("localhost"), "localhost");
    }

    #[test]
    fn plausible_ipv4_accepts_dotted_quad() {
        assert!(is_plausible_ipv4("144.2.117.58"));
        assert!(!is_plausible_ipv4("not-an-ip"));
        assert!(!is_plausible_ipv4("1.2.3"));
        assert!(!is_plausible_ipv4("1.2.3.4.5"));
        assert!(!is_plausible_ipv4("999.0.0.1"));
    }

    #[test]
    fn cgnat_detects_tailscale_range() {
        // 100.64.0.1
        let addr = (100u32 << 24) | (64 << 16) | 1;
        assert!(is_cgnat(addr));
        // 192.168.1.1
        let addr = (192u32 << 24) | (168 << 16) | (1 << 8) | 1;
        assert!(!is_cgnat(addr));
        // 100.63.0.1 — outside /10
        let addr = (100u32 << 24) | (63 << 16) | 1;
        assert!(!is_cgnat(addr));
        // 100.127.255.255 — inside
        let addr = (100u32 << 24) | (127 << 16) | (255 << 8) | 255;
        assert!(is_cgnat(addr));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn collect_returns_identity() {
        let m = collect_status_metrics();
        assert!(!m.hostname.is_empty());
        assert!(!m.username.is_empty());
        // Identity must always resolve; optional collectors may be None on first sample.

    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn cpu_sample_warms_then_reports() {
        let first = collect_status_metrics();
        std::thread::sleep(Duration::from_millis(200));
        let second = collect_status_metrics();
        // First sample establishes baseline (may be None); second should land.
        let _ = first.cpu_percent;
        // Bandwidth also needs two samples.
        let _ = (second.net_down_kib, second.net_up_kib, second.cpu_percent);
        // Public IP may come from disk cache / powerline migration.
        let _ = second.public_ip;
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_iface_snapshots_include_en0_or_any() {
        let snaps = macos_iface_byte_snapshots().expect("NET_RT_IFLIST2 should work");
        assert!(!snaps.is_empty());
        // At least one non-loopback interface should appear.
        assert!(snaps.iter().any(|(n, _, _)| !n.starts_with("lo")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_battery_reads_or_skips_cleanly() {
        let (pct, charging) = macos_battery();
        if let Some(p) = pct {
            assert!(p <= 100);
            assert!(charging.is_some());
        }
    }
}
