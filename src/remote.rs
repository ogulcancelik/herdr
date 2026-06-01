//! Remote thin-client launcher over SSH command stdio.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const UPDATE_MANIFEST_URL: &str = "https://herdr.dev/latest.json";
const REMOTE_BINARY_ENV_VAR: &str = "HERDR_REMOTE_BINARY";
pub(crate) const REATTACH_COMMAND_ENV_VAR: &str = "HERDR_REATTACH_COMMAND";

pub(crate) const REMOTE_KEYBINDINGS_ENV_VAR: &str = "HERDR_REMOTE_KEYBINDINGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKeybindings {
    Local,
    Server,
}

impl RemoteKeybindings {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "server" => Ok(Self::Server),
            _ => Err("--remote-keybindings must be 'local' or 'server'".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub(crate) target: String,
    pub(crate) keybindings: RemoteKeybindings,
    pub(crate) live_handoff: bool,
}

pub(crate) fn extract_remote_args(
    args: &[String],
) -> Result<(Vec<String>, Option<RemoteLaunch>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut remote_target = None;
    let mut keybindings = RemoteKeybindings::Local;
    let mut keybindings_seen = false;
    let mut live_handoff = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--handoff" {
            live_handoff = true;
            index += 1;
            continue;
        }
        if arg == "--remote" {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote".to_string());
            };
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote=") {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 1;
            continue;
        }
        if arg == "--remote-keybindings" {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote-keybindings".to_string());
            };
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote-keybindings=") {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    let remote = remote_target.map(|target| RemoteLaunch {
        target,
        keybindings,
        live_handoff,
    });
    if remote.is_none() && keybindings_seen {
        return Err("--remote-keybindings requires --remote".to_string());
    }
    if remote.is_none() && live_handoff {
        cleaned.push("--handoff".to_string());
    }

    Ok((cleaned, remote))
}

fn validate_remote_target(target: &str) -> Result<&str, String> {
    if target.is_empty() {
        return Err("missing value for --remote".to_string());
    }
    if target.starts_with('-') {
        return Err("--remote target must not start with '-'".to_string());
    }
    Ok(target)
}

pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let prepared_remote = prepare_remote_herdr(&remote.target, remote.live_handoff)?;
    ensure_remote_server_ready(
        &remote.target,
        &prepared_remote.remote_herdr,
        prepared_remote.installed_or_replaced,
        remote.live_handoff,
    )?;

    let _bridge = SshStdioBridge::start(
        remote.target,
        prepared_remote.remote_herdr,
        local_socket.clone(),
        session_name,
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    ensure_remote_server_running()?;

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = UnixStream::connect(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    let mut stdout = io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;

    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let _ = copy_flush(&mut stdin, &mut stdin_to_socket);
        let _ = stdin_to_socket.shutdown(std::net::Shutdown::Write);
    });

    copy_flush(&mut socket_to_stdout, &mut stdout).map(|_| ())
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status.protocol == Some(CURRENT_PROTOCOL) {
            return Ok(());
        }
        return Err(io::Error::other(format!(
            "remote herdr server is running with protocol {}, but this bridge needs protocol {CURRENT_PROTOCOL}; rerun `herdr --remote` from an interactive terminal to approve stopping it",
            protocol_label(status.protocol)
        )));
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

impl RemotePlatform {
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            _ => return None,
        };
        let arch = match arch.trim() {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn local() -> Self {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "unknown"
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        };

        Self { os, arch }
    }

    fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Debug, Clone)]
struct RemoteHerdr {
    install_suffix: String,
    shell_path: String,
    platform: RemotePlatform,
}

impl RemoteHerdr {
    fn for_platform(platform: RemotePlatform) -> Self {
        let install_suffix = ".local/bin/herdr".to_string();
        let shell_path = format!("\"$HOME/{install_suffix}\"");
        Self {
            install_suffix,
            shell_path,
            platform,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.shell_path = shell_path;
        self
    }
}

#[derive(Deserialize)]
struct RemoteUpdateManifest {
    version: String,
    protocol: Option<u32>,
    assets: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_remote_manifest_releases")]
    releases: BTreeMap<String, RemoteReleaseMetadata>,
}

#[derive(Deserialize)]
struct RemoteReleaseMetadata {
    protocol: Option<u32>,
    #[serde(default)]
    assets: BTreeMap<String, String>,
}

fn deserialize_remote_manifest_releases<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, RemoteReleaseMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Object(object)) => object
            .into_iter()
            .filter_map(|(version, release)| {
                serde_json::from_value::<RemoteReleaseMetadata>(release)
                    .ok()
                    .map(|metadata| (version, metadata))
            })
            .collect(),
        _ => BTreeMap::new(),
    })
}

impl RemoteUpdateManifest {
    fn release_for_version(&self, version: &str) -> Option<RemoteManifestReleaseRef<'_>> {
        if self.version.trim_start_matches('v') == version {
            return Some(RemoteManifestReleaseRef {
                protocol: self.protocol,
                assets: &self.assets,
            });
        }

        self.releases.get(version).and_then(|release| {
            (!release.assets.is_empty()).then_some(RemoteManifestReleaseRef {
                protocol: release.protocol,
                assets: &release.assets,
            })
        })
    }
}

#[derive(Clone, Copy)]
struct RemoteManifestReleaseRef<'a> {
    protocol: Option<u32>,
    assets: &'a BTreeMap<String, String>,
}

struct InstallSource {
    path: PathBuf,
    temporary_dir: Option<PathBuf>,
}

struct PreparedRemoteHerdr {
    remote_herdr: RemoteHerdr,
    installed_or_replaced: bool,
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: None,
        }
    }

    fn temporary(path: PathBuf, temporary_dir: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
        }
    }

    fn cleanup(&self) {
        if let Some(dir) = &self.temporary_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn prepare_remote_herdr(
    target: &str,
    live_handoff_enabled: bool,
) -> io::Result<PreparedRemoteHerdr> {
    let platform = detect_remote_platform(target)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);
    let override_binary = remote_binary_override_path()?;
    let path_remote_herdr = remote_binary_on_path_any(target, &remote_herdr)?;

    if override_binary.is_none() {
        if let Some(path_remote_herdr) = path_remote_herdr
            .as_ref()
            .filter(|candidate| remote_binary_matches(target, candidate).unwrap_or(false))
        {
            return Ok(PreparedRemoteHerdr {
                remote_herdr: path_remote_herdr.clone(),
                installed_or_replaced: false,
            });
        }
        if remote_binary_matches(target, &remote_herdr)? {
            return Ok(PreparedRemoteHerdr {
                remote_herdr,
                installed_or_replaced: false,
            });
        }
    }

    if let Some(status_probe_herdr) = path_remote_herdr.as_ref().or_else(|| {
        remote_binary_exists(target, &remote_herdr)
            .ok()
            .and_then(|exists| exists.then_some(&remote_herdr))
    }) {
        confirm_remote_install_with_running_server(
            target,
            status_probe_herdr,
            live_handoff_enabled,
        )?;
    }
    confirm_remote_install(
        target,
        &remote_herdr,
        &install_source_description(&remote_herdr.platform, override_binary.as_deref()),
    )?;
    let source = resolve_install_source(&remote_herdr.platform, override_binary)?;
    let install_result = install_remote_herdr(target, &remote_herdr, &source.path);
    source.cleanup();
    install_result?;

    if !remote_binary_matches(target, &remote_herdr)? {
        return Err(io::Error::other(format!(
            "installed remote herdr at {}, but it did not report version {CURRENT_VERSION}",
            remote_herdr.shell_path
        )));
    }
    warn_if_remote_bin_not_on_path(target)?;

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        installed_or_replaced: true,
    })
}

fn detect_remote_platform(target: &str) -> io::Result<RemotePlatform> {
    let output = ssh_output(target, "uname -s; uname -m")?;
    if !output.status.success() {
        return Err(command_failed("remote platform detection failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    RemotePlatform::from_uname(os, arch).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported remote platform: {} {}",
            os.trim(),
            arch.trim()
        ))
    })
}

fn remote_binary_on_path_any(
    target: &str,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteHerdr>> {
    let output = ssh_output(target, remote_path_probe_any_command())?;
    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_herdr_from_path_probe_any(remote_herdr, &stdout))
}

fn remote_path_probe_any_command() -> &'static str {
    r#"path=$(command -v herdr) || exit 1
test -n "$path" || exit 1
printf '%s\n' "$path"
"#
}

#[cfg(test)]
fn remote_herdr_from_path_probe(remote_herdr: &RemoteHerdr, stdout: &str) -> Option<RemoteHerdr> {
    let mut lines = stdout.lines();
    let path = lines.next()?;
    let version = lines.next()?.trim();
    let status = lines.next()?;
    let protocol = parse_client_status_json(status)?.protocol;
    if !path.starts_with('/')
        || version != format!("herdr {CURRENT_VERSION}")
        || protocol != CURRENT_PROTOCOL
    {
        return None;
    }

    Some(remote_herdr.clone().with_shell_path(shell_quote(path)))
}

fn remote_herdr_from_path_probe_any(
    remote_herdr: &RemoteHerdr,
    stdout: &str,
) -> Option<RemoteHerdr> {
    let mut lines = stdout.lines();
    let path = lines.next()?;
    if !path.starts_with('/') {
        return None;
    }
    Some(remote_herdr.clone().with_shell_path(shell_quote(path)))
}

fn remote_binary_matches(target: &str, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    let command = format!(
        "test -x {0} && {0} --version && {0} status client --json",
        remote_herdr.shell_path
    );
    let output = ssh_output(target, &command)?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let version = lines.next().unwrap_or_default().trim();
    let status = lines.next().unwrap_or_default();
    Ok(version == format!("herdr {CURRENT_VERSION}")
        && parse_client_status_json(status)
            .map(|status| status.protocol == CURRENT_PROTOCOL)
            .unwrap_or(false))
}

fn remote_binary_exists(target: &str, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    let command = format!("test -x {}", remote_herdr.shell_path);
    Ok(ssh_output(target, &command)?.status.success())
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local herdr binary".to_string()
    } else {
        format!(
            "the {CURRENT_VERSION} release asset for {}",
            platform.asset_key()
        )
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        return Ok(InstallSource::persistent(path));
    }

    if *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    download_release_asset(platform)
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if *platform != RemotePlatform::local() {
        return false;
    }

    std::env::current_exe()
        .map(|path| !crate::update::is_package_manager_managed_exe_path(&path))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        live_handoff: bool,
    },
    NotRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteServerRestartReason {
    ProtocolMismatch,
    BinaryUpdated,
    VersionMismatch,
}

fn ensure_remote_server_ready(
    target: &str,
    remote_herdr: &RemoteHerdr,
    remote_binary_changed: bool,
    live_handoff_enabled: bool,
) -> io::Result<()> {
    let status = remote_server_status(target, remote_herdr)?;
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
    } = status
    else {
        return Ok(());
    };

    let Some(reason) =
        remote_server_restart_reason(version.as_deref(), protocol, remote_binary_changed)
    else {
        return Ok(());
    };

    if live_handoff_enabled
        && live_handoff
        && confirm_remote_server_handoff(target, version.as_deref(), protocol, reason)?
    {
        match live_handoff_remote_server(target, remote_herdr) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if confirm_remote_server_stop(target, version.as_deref(), protocol, reason)? {
        stop_remote_server(target, remote_herdr)?;
    }
    Ok(())
}

fn remote_server_restart_reason(
    version: Option<&str>,
    protocol: Option<u32>,
    remote_binary_changed: bool,
) -> Option<RemoteServerRestartReason> {
    if protocol != Some(CURRENT_PROTOCOL) {
        return Some(RemoteServerRestartReason::ProtocolMismatch);
    }
    if remote_binary_changed {
        return Some(RemoteServerRestartReason::BinaryUpdated);
    }
    if version != Some(CURRENT_VERSION) {
        return Some(RemoteServerRestartReason::VersionMismatch);
    }
    None
}

fn confirm_remote_install_with_running_server(
    target: &str,
    remote_herdr: &RemoteHerdr,
    live_handoff_enabled: bool,
) -> io::Result<()> {
    let status = match remote_server_status(target, remote_herdr) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote herdr server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote herdr server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote herdr binary? [Y/n] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer == "n" || answer == "no" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote herdr install cancelled",
                ));
            }
            return Ok(());
        }
    };
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
    } = status
    else {
        return Ok(());
    };
    if live_handoff_enabled && live_handoff {
        return Ok(());
    }

    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "remote herdr server on {target} is running v{} protocol {}; run from an interactive terminal to approve updating the remote binary",
            version_label(version.as_deref()),
            protocol_label(protocol)
        )));
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!(
        "  server: v{} protocol {}",
        version_label(version.as_deref()),
        protocol_label(protocol)
    );
    eprintln!(
        "this attach will not preserve running panes unless you pass --handoff and the remote server supports live handoff."
    );
    eprintln!();
    eprint!("continue installing the remote herdr binary? [Y/n] ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr install cancelled",
        ));
    }

    Ok(())
}

fn remote_server_status(
    target: &str,
    remote_herdr: &RemoteHerdr,
) -> io::Result<RemoteServerStatus> {
    let command = format!("{} status server --json", remote_herdr.shell_path);
    let output = ssh_output(target, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_remote_server_status_json(stdout.trim())
}

#[derive(Debug, Deserialize)]
struct RemoteClientStatusJson {
    protocol: u32,
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    serde_json::from_str(status).ok()
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    if !parsed.running {
        return Ok(RemoteServerStatus::NotRunning);
    }

    Ok(RemoteServerStatus::Running {
        version: parsed.version,
        protocol: parsed.protocol,
        live_handoff: parsed
            .capabilities
            .is_some_and(|capabilities| capabilities.live_handoff),
    })
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    protocol: Option<u32>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        if reason == RemoteServerRestartReason::ProtocolMismatch {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} is running with protocol {}, but this client needs protocol {CURRENT_PROTOCOL}; run from an interactive terminal to approve stopping it",
                protocol_label(protocol)
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use v{CURRENT_VERSION} after it restarts.",
            version_label(version)
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!(
        "  server: v{} protocol {}",
        version_label(version),
        protocol_label(protocol)
    );
    eprintln!("  prepared binary: v{CURRENT_VERSION} protocol {CURRENT_PROTOCOL}");
    eprintln!();

    match reason {
        RemoteServerRestartReason::ProtocolMismatch => {
            eprintln!(
                "the remote server protocol does not match this client. the remote server must be stopped before attaching."
            );
        }
        RemoteServerRestartReason::BinaryUpdated => {
            eprintln!(
                "the remote herdr binary was installed or replaced. restart the remote server so it uses the prepared binary."
            );
        }
        RemoteServerRestartReason::VersionMismatch => {
            eprintln!(
                "the remote server is still running a different herdr version. restart it so it uses the prepared binary."
            );
        }
    }

    let prompt = if reason == RemoteServerRestartReason::ProtocolMismatch {
        "stop the remote server and continue attaching? [Y/n] "
    } else {
        "restart the remote server now? [Y/n] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        if reason == RemoteServerRestartReason::ProtocolMismatch {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "remote herdr server stop cancelled",
            ));
        }
        return Ok(false);
    }

    Ok(true)
}

fn confirm_remote_server_handoff(
    target: &str,
    version: Option<&str>,
    protocol: Option<u32>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        if reason == RemoteServerRestartReason::ProtocolMismatch {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} is running with protocol {}, but this client needs protocol {CURRENT_PROTOCOL}; run from an interactive terminal to approve live handoff or stopping it",
                protocol_label(protocol)
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use v{CURRENT_VERSION} after it restarts.",
            version_label(version)
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!(
        "  server: v{} protocol {}",
        version_label(version),
        protocol_label(protocol)
    );
    eprintln!("  prepared binary: v{CURRENT_VERSION} protocol {CURRENT_PROTOCOL}");
    eprintln!();

    match reason {
        RemoteServerRestartReason::ProtocolMismatch => {
            eprintln!(
                "the remote server protocol does not match this client. herdr will try to hand off live pane processes to the prepared remote server before the old server exits."
            );
        }
        RemoteServerRestartReason::BinaryUpdated => {
            eprintln!(
                "the remote herdr binary was installed or replaced. herdr will try to hand off live pane processes to the prepared remote server."
            );
        }
        RemoteServerRestartReason::VersionMismatch => {
            eprintln!(
                "the remote server is still running a different herdr version. herdr will try to hand off live pane processes to the prepared remote server."
            );
        }
    }

    eprint!("live-handoff remote panes to the prepared server? [Y/n] ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer != "n" && answer != "no")
}

fn live_handoff_remote_server(target: &str, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let command = format!(
        "{} server live-handoff --import-exe {} --expected-protocol {CURRENT_PROTOCOL} --expected-version {CURRENT_VERSION}",
        remote_herdr.shell_path,
        remote_herdr.shell_path
    );
    let output = ssh_output(target, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    eprintln!(
        "handed off the remote herdr server on {target}; reconnecting to the prepared server."
    );
    Ok(())
}

fn stop_remote_server(target: &str, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let command = format!("{} server stop", remote_herdr.shell_path);
    let output = ssh_output(target, &command)?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }

    wait_for_remote_server_shutdown(target, remote_herdr)?;
    eprintln!("stopped the remote herdr server on {target}; it will restart when the remote client bridge attaches.");
    Ok(())
}

fn wait_for_remote_server_shutdown(target: &str, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(target, remote_herdr)? == RemoteServerStatus::NotRunning {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote herdr server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn warn_if_remote_bin_not_on_path(target: &str) -> io::Result<()> {
    let output = ssh_output(
        target,
        "case \":$PATH:\" in *\":$HOME/.local/bin:\"*) exit 0 ;; *) exit 1 ;; esac",
    )?;
    if !output.status.success() {
        eprintln!(
            "herdr: installed remote binary to ~/.local/bin/herdr, but ~/.local/bin is not in the remote PATH"
        );
    }
    Ok(())
}

fn download_release_asset(platform: &RemotePlatform) -> io::Result<InstallSource> {
    let manifest_output = Command::new("curl")
        .args([
            "-sfL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            UPDATE_MANIFEST_URL,
        ])
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("curl failed: {err}")))?;
    if !manifest_output.status.success() {
        return Err(command_failed(
            "failed to fetch update manifest",
            &manifest_output,
        ));
    }

    let manifest: RemoteUpdateManifest = serde_json::from_slice(&manifest_output.stdout)
        .map_err(|err| io::Error::other(format!("failed to parse update manifest JSON: {err}")))?;

    let asset_key = platform.asset_key();
    let release = manifest.release_for_version(CURRENT_VERSION).ok_or_else(|| {
        io::Error::other(format!(
            "release manifest does not include herdr {CURRENT_VERSION}; build herdr for {} or install it there manually",
            platform.asset_key()
        ))
    })?;
    if let Some(protocol) = release.protocol {
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "release manifest has herdr {CURRENT_VERSION} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching herdr on the remote host manually"
            )));
        }
    }
    let url = release.assets.get(&asset_key).ok_or_else(|| {
        io::Error::other(format!(
            "no {asset_key} binary in the release manifest for herdr {CURRENT_VERSION}"
        ))
    })?;

    let dir = private_download_dir(&asset_key)?;
    let path = dir.join("herdr.tmp");
    let status = Command::new("curl")
        .args(["-sfL", "--max-time", "120", "-o"])
        .arg(&path)
        .arg(url)
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("download failed: {err}")))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::other("download failed"));
    }

    Ok(InstallSource::temporary(path, dir))
}

fn private_download_dir(asset_key: &str) -> io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..100 {
        let dir = base.join(format!(
            "herdr-remote-{}-{}-{attempt}",
            std::process::id(),
            asset_key
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create private herdr remote download directory",
    ))
}

fn confirm_remote_install(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_description: &str,
) -> io::Result<()> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote herdr {CURRENT_VERSION} is not installed at {}; run from an interactive terminal to approve installation",
            remote_herdr.shell_path
        )));
    }

    eprintln!(
        "matching herdr {CURRENT_VERSION} is not installed on {target} for {}.",
        remote_herdr.platform.asset_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description, remote_herdr.shell_path
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr installation cancelled",
        ));
    }

    Ok(())
}

fn install_remote_herdr(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_path: &Path,
) -> io::Result<()> {
    let script = format!(
        r#"dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
cat > "$tmp"
chmod 755 "$tmp"
mv "$tmp" "$dest"
"#,
        install_suffix = remote_herdr.install_suffix
    );

    let mut child = Command::new("ssh")
        .arg("-T")
        .arg(target)
        .arg(format!("sh -eu -c {}", shell_quote(&script)))
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh install: {err}")))?;

    let mut source = File::open(source_path)?;
    let copy_result = if let Some(mut stdin) = child.stdin.take() {
        io::copy(&mut source, &mut stdin).map(|_| ())
    } else {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "ssh install stdin missing",
        ))
    };
    let status = child.wait()?;
    copy_result?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "remote install exited with {status}"
        )))
    }
}

fn ssh_output(target: &str, command: &str) -> io::Result<Output> {
    Command::new("ssh")
        .arg("-T")
        .arg(target)
        .arg(command)
        .output()
}

fn remote_bridge_command(remote_herdr: &RemoteHerdr, session_name: &str) -> String {
    let mut command = format!("exec {}", remote_herdr.shell_path);
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command.push_str(" remote-client-bridge");
    command
}

fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = if program.is_empty() { "herdr" } else { program };
    let mut command = format!("{} --remote {}", shell_quote(program), shell_quote(target));
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

struct SshStdioBridge {
    local_socket: PathBuf,
    should_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SshStdioBridge {
    fn start(
        target: String,
        remote_herdr: RemoteHerdr,
        local_socket: PathBuf,
        session_name: String,
    ) -> io::Result<Self> {
        let _ = std::fs::remove_file(&local_socket);
        let listener = UnixListener::bind(&local_socket)?;
        crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)?;
        listener.set_nonblocking(true)?;

        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if let Err(err) = stream.set_nonblocking(false) {
                            eprintln!(
                                "herdr: remote bridge failed to prepare client socket: {err}"
                            );
                            continue;
                        }
                        if let Err(err) =
                            bridge_connection(stream, &target, &remote_herdr, &session_name)
                        {
                            eprintln!("herdr: remote bridge failed: {err}");
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        eprintln!("herdr: remote bridge listener failed: {err}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            should_stop,
            thread: Some(thread),
        })
    }
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.local_socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn bridge_connection(
    stream: UnixStream,
    target: &str,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
) -> io::Result<()> {
    let mut command = Command::new("ssh");
    command
        .arg("-T")
        .arg(target)
        .arg(remote_bridge_command(remote_herdr, session_name));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin missing"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdout missing"))?;
    let mut stream_to_child = stream.try_clone()?;
    let mut child_to_stream = stream;

    let upload = thread::spawn(move || {
        let _ = copy_flush(&mut stream_to_child, &mut child_stdin);
    });
    let download = thread::spawn(move || {
        let _ = copy_flush(&mut child_stdout, &mut child_to_stream);
        let _ = child_to_stream.shutdown(std::net::Shutdown::Write);
    });

    let status = child.wait()?;
    let _ = upload.join();
    let _ = download.join();

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("ssh bridge exited with {status}"),
        ))
    }
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };

        writer.write_all(&buffer[..bytes_read])?;
        writer.flush()?;
        total += bytes_read as u64;
    }
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env("HERDR_RENDER_ENCODING", "terminal-ansi")
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);

    let tmpdir = std::env::temp_dir();
    let readable = tmpdir.join(format!(
        "herdr-remote-{pid}-{target_clean}-{session_clean}.sock"
    ));
    if fits_unix_socket_path(&readable) {
        return readable;
    }

    // macOS' per-user TMPDIR (~49 chars under /var/folders/...) can push the
    // readable name past sun_path's 104-byte ceiling. Fall back to a hashed
    // short name in TMPDIR, then to /tmp as a last resort when TMPDIR itself
    // is longer than the budget. The hash covers the full unsanitized
    // target/session so uniqueness does not depend on the prefix truncation;
    // the prefix is kept only for debuggability.
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("herdr-r-{pid}-{target_prefix}-{hash}.sock");
    let short_in_tmp = tmpdir.join(&short_name);
    if fits_unix_socket_path(&short_in_tmp) {
        return short_in_tmp;
    }
    PathBuf::from("/tmp").join(short_name)
}

fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // sun_path is byte-limited: 104 bytes on macOS, 108 on Linux. Reserve
    // 1 byte for the trailing NUL and use the smaller cap for portability.
    const MAX: usize = 103;
    path.as_os_str().as_bytes().len() <= MAX
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["herdr".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["herdr".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["herdr".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["herdr".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["herdr".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote=prod".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["herdr".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_maps_uname_values() {
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .asset_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Darwin", "arm64")
                .unwrap()
                .asset_key(),
            "macos-aarch64"
        );
        assert!(RemotePlatform::from_uname("FreeBSD", "x86_64").is_none());
    }

    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/herdr",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/herdr --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "herdr --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "herdr --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "herdr --remote host --handoff"
        );
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec \"$HOME/.local/bin/herdr\" remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_probe_uses_path_binary_when_version_matches() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let stdout = matching_path_probe_stdout("/usr/bin/herdr");
        let remote_herdr =
            remote_herdr_from_path_probe(&remote_herdr, &stdout).expect("matching path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /usr/bin/herdr remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_probe_quotes_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let stdout = matching_path_probe_stdout("/opt/herdr bin/herdr");
        let remote_herdr =
            remote_herdr_from_path_probe(&remote_herdr, &stdout).expect("matching path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_probe_uses_macos_path_binary_when_version_matches() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });
        let stdout = matching_path_probe_stdout("/opt/homebrew/bin/herdr");
        let remote_herdr =
            remote_herdr_from_path_probe(&remote_herdr, &stdout).expect("matching path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /opt/homebrew/bin/herdr remote-client-bridge"
        );
        assert_eq!(remote_herdr.platform.asset_key(), "macos-aarch64");
    }

    #[test]
    fn remote_path_probe_quotes_single_quotes_in_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let stdout = matching_path_probe_stdout("/opt/herdr's/bin/herdr");
        let remote_herdr =
            remote_herdr_from_path_probe(&remote_herdr, &stdout).expect("matching path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr'\\''s/bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_probe_ignores_version_mismatch() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_probe(
            &remote_herdr,
            &format!("/usr/bin/herdr\nherdr 0.0.0\n{{\"protocol\":{CURRENT_PROTOCOL}}}\n"),
        );

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_probe_ignores_relative_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let stdout = matching_path_probe_stdout("bin/herdr");
        let remote_herdr = remote_herdr_from_path_probe(&remote_herdr, &stdout);

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_probe_ignores_protocol_mismatch() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let stdout = format!("/usr/bin/herdr\nherdr {CURRENT_VERSION}\n{{\"protocol\":0}}\n");
        let remote_herdr = remote_herdr_from_path_probe(&remote_herdr, &stdout);

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn parse_client_status_json_reads_protocol() {
        assert_eq!(
            parse_client_status_json(r#"{"version":"x","protocol":8,"binary":"/bin/herdr"}"#)
                .map(|status| status.protocol),
            Some(8)
        );
        assert!(parse_client_status_json(r#"{"protocol":"unknown"}"#).is_none());
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: true
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_no_handoff() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_update_manifest_uses_root_assets_for_latest_version() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.3",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(String::as_str),
            Some("https://example.com/latest")
        );
    }

    #[test]
    fn remote_update_manifest_reads_archived_release_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(String::as_str),
            Some("https://example.com/archive")
        );
    }

    #[test]
    fn remote_update_manifest_uses_archived_release_protocol() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "protocol": 41,
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            Some(41)
        );
    }

    #[test]
    fn remote_update_manifest_does_not_inherit_latest_protocol_for_archived_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            None
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_stop_for_protocol_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some(CURRENT_VERSION), Some(0), false),
            Some(RemoteServerRestartReason::ProtocolMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_offers_restart_after_binary_update() {
        assert_eq!(
            remote_server_restart_reason(Some(CURRENT_VERSION), Some(CURRENT_PROTOCOL), true),
            Some(RemoteServerRestartReason::BinaryUpdated)
        );
    }

    #[test]
    fn remote_server_restart_reason_offers_restart_for_version_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some("0.0.0"), Some(CURRENT_PROTOCOL), false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
        assert_eq!(
            remote_server_restart_reason(None, Some(CURRENT_PROTOCOL), false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_allows_current_server() {
        assert_eq!(
            remote_server_restart_reason(Some(CURRENT_VERSION), Some(CURRENT_PROTOCOL), false),
            None
        );
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/herdr-aarch64")), false),
            "HERDR_REMOTE_BINARY (/tmp/herdr-aarch64)"
        );
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local herdr binary"
        );
    }

    #[test]
    fn install_source_description_uses_release_asset_when_local_binary_cannot_seed_remote() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            format!(
                "the {CURRENT_VERSION} release asset for {}",
                platform.asset_key()
            )
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary_without_temporary_cleanup() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/herdr-aarch64")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/herdr-aarch64"));
        assert!(source.temporary_dir.is_none());
    }

    fn matching_path_probe_stdout(path: &str) -> String {
        format!("{path}\nherdr {CURRENT_VERSION}\n{{\"protocol\":{CURRENT_PROTOCOL}}}\n")
    }

    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("herdr-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("herdr-r-"),
            "expected hashed fallback, got {filename}"
        );
    }

    #[test]
    fn install_source_cleanup_removes_temporary_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-install-source-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("create temp dir");
        let path = dir.join("herdr.tmp");
        fs::write(&path, b"test").expect("write temp file");

        InstallSource::temporary(path, dir.clone()).cleanup();

        assert!(!dir.exists());
    }
}
