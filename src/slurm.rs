//! Slurm/Pyxis attach: an `srun` stdio bridge to a Herdr server running inside
//! an existing Slurm allocation.
//!
//! `herdr slurm attach <job_id>` creates a local owner-only Unix forwarding
//! socket, launches a single `srun` job step that runs `remote-client-bridge`
//! inside the allocation (optionally inside a Pyxis container), byte-forwards
//! the Herdr client protocol over that step's stdin/stdout, and launches the
//! normal thin client against the local socket.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::remote::{
    BRIDGE_EXIT_PROTOCOL_MISMATCH, BRIDGE_EXIT_SERVER_SPAWN_FAILED, BRIDGE_EXIT_SOCKET_UNREACHABLE,
};

const SOCKET_PERMISSION_MODE: u32 = 0o600;
const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const SRUN_TEARDOWN_WAIT: Duration = Duration::from_secs(2);
const SRUN_TEARDOWN_POLL: Duration = Duration::from_millis(50);
const STDERR_RING_LIMIT: usize = 200;
/// Longest Unix socket path that fits `sun_path` portably (104 on macOS, 108
/// on Linux); reserve 1 byte for the trailing NUL and use the smaller cap.
const MAX_SOCKET_PATH: usize = 103;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlurmLaunch {
    pub(crate) job_id: String,
    pub(crate) node: Option<String>,
    /// `Some` only when the user explicitly passed `--session`; never
    /// synthesized, so the bridge falls back to the `default` session.
    pub(crate) session_name: Option<String>,
    pub(crate) herdr_path: Option<String>,
    pub(crate) container: SlurmContainerOptions,
    pub(crate) overlap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SlurmContainerOptions {
    pub(crate) name: Option<String>,
    pub(crate) image: Option<String>,
    pub(crate) mounts: Vec<String>,
    pub(crate) workdir: Option<String>,
    pub(crate) env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlurmJobInfo {
    user_id: u32,
    user_name: Option<String>,
    state: String,
    nodes: Vec<String>,
    batch_host: Option<String>,
}

/// A Slurm job record before its compact node list is expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedJob {
    user_id: u32,
    user_name: Option<String>,
    state: String,
    node_list: String,
    batch_host: Option<String>,
}

// --- CLI parsing ----------------------------------------------------------

/// Recognizes the positional `slurm attach <job_id> [options]` token sequence.
/// Returns the args with the Slurm tokens removed plus the parsed launch.
pub(crate) fn extract_slurm_args(
    args: &[String],
) -> Result<(Vec<String>, Option<SlurmLaunch>), String> {
    if args.get(1).map(String::as_str) != Some("slurm") {
        return Ok((args.to_vec(), None));
    }
    if args.get(2).map(String::as_str) != Some("attach") {
        return Err("usage: herdr slurm attach <job_id> [options]".to_string());
    }
    let Some(job_id) = args.get(3).cloned() else {
        return Err("usage: herdr slurm attach <job_id> [options]".to_string());
    };
    validate_job_id(&job_id)?;

    let mut node = None;
    let mut herdr_path = None;
    let mut container = SlurmContainerOptions::default();
    let mut overlap = true;

    let mut index = 4;
    while index < args.len() {
        let arg = args[index].clone();
        if arg == "--no-overlap" {
            overlap = false;
            index += 1;
            continue;
        }
        if is_flag(&arg, "--node") {
            let (value, next) = take_value(args, index, "--node")?;
            if value.is_empty() {
                return Err("missing value for --node".to_string());
            }
            node = Some(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--herdr-path") {
            let (value, next) = take_value(args, index, "--herdr-path")?;
            if value.is_empty() {
                return Err("--herdr-path must not be empty".to_string());
            }
            herdr_path = Some(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--container-name") {
            let (value, next) = take_value(args, index, "--container-name")?;
            container.name = Some(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--container-image") {
            let (value, next) = take_value(args, index, "--container-image")?;
            container.image = Some(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--container-mounts") {
            let (value, next) = take_value(args, index, "--container-mounts")?;
            container.mounts.push(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--container-workdir") {
            let (value, next) = take_value(args, index, "--container-workdir")?;
            container.workdir = Some(value);
            index = next;
            continue;
        }
        if is_flag(&arg, "--container-env") {
            let (value, next) = take_value(args, index, "--container-env")?;
            container.env = Some(value);
            index = next;
            continue;
        }
        return Err(format!("unknown option for 'herdr slurm attach': {arg}"));
    }

    // `--session` is consumed earlier by `session::configure_from_args`; only
    // forward it to the bridge when the user explicitly requested one.
    let session_name = if crate::session::explicit_session_requested() {
        crate::session::active_name()
    } else {
        None
    };

    let launch = SlurmLaunch {
        job_id,
        node,
        session_name,
        herdr_path,
        container,
        overlap,
    };
    let cleaned = args.first().cloned().into_iter().collect();
    Ok((cleaned, Some(launch)))
}

fn is_flag(arg: &str, flag: &str) -> bool {
    arg == flag || (arg.starts_with(flag) && arg.as_bytes().get(flag.len()) == Some(&b'='))
}

fn take_value(args: &[String], index: usize, flag: &str) -> Result<(String, usize), String> {
    let arg = &args[index];
    let eq_prefix = format!("{flag}=");
    if let Some(value) = arg.strip_prefix(eq_prefix.as_str()) {
        return Ok((value.to_string(), index + 1));
    }
    match args.get(index + 1) {
        Some(value) => Ok((value.clone(), index + 2)),
        None => Err(format!("missing value for {flag}")),
    }
}

fn validate_job_id(job_id: &str) -> Result<(), String> {
    if job_id.is_empty() || !job_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "invalid Slurm job id '{job_id}': expected a numeric job id"
        ));
    }
    Ok(())
}

// --- entry point ----------------------------------------------------------

pub(crate) fn run_slurm(launch: SlurmLaunch) -> io::Result<()> {
    if let Err(err) = run_slurm_inner(launch) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
    Ok(())
}

fn run_slurm_inner(launch: SlurmLaunch) -> Result<(), String> {
    validate_job_id(&launch.job_id)?;
    let job = resolve_job_info(&launch.job_id)?;
    validate_job(&job, &launch.job_id)?;
    let node = select_node(&launch, &job.nodes, &launch.job_id)?;

    let local_socket =
        local_forward_socket_path(&launch.job_id, &node, launch.session_name.as_deref());
    crate::ipc::prepare_socket_path(&local_socket, |path| {
        format!(
            "herdr Slurm forwarding socket {} is already in use",
            path.display()
        )
    })
    .map_err(|err| err.to_string())?;
    let listener = UnixListener::bind(&local_socket).map_err(|err| {
        format!(
            "failed to create forwarding socket {}: {err}",
            local_socket.display()
        )
    })?;
    crate::ipc::restrict_socket_permissions(&local_socket, SOCKET_PERMISSION_MODE)
        .map_err(|err| err.to_string())?;
    let _guard = SocketGuard {
        path: local_socket.clone(),
    };
    install_signal_cleanup(&local_socket);

    let pyxis = launch.container != SlurmContainerOptions::default();
    let mut summary = format!(
        "herdr: attaching to Slurm job {} on node {} (session: {})",
        launch.job_id,
        node,
        launch.session_name.as_deref().unwrap_or("default"),
    );
    if pyxis {
        summary.push_str(" [pyxis]");
    }
    if let Some(host) = &job.batch_host {
        summary.push_str(&format!(" [batch host: {host}]"));
    }
    eprintln!("{summary}");

    let mut srun = Command::new("srun")
        .args(srun_args(&launch, &node))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                "'srun' was not found; run 'herdr slurm attach' from a Slurm submit host"
                    .to_string()
            } else {
                format!("failed to start the srun bridge step: {err}")
            }
        })?;

    let srun_stdin = srun.stdin.take().expect("srun stdin piped");
    let srun_stdout = srun.stdout.take().expect("srun stdout piped");
    let srun_stderr = srun.stderr.take().expect("srun stderr piped");

    let attached = Arc::new(AtomicBool::new(false));
    let should_stop = Arc::new(AtomicBool::new(false));
    let stderr_log: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

    let stderr_thread =
        spawn_stderr_relay(srun_stderr, Arc::clone(&attached), Arc::clone(&stderr_log));
    let bridge_thread = spawn_bridge(
        listener,
        srun_stdin,
        srun_stdout,
        Arc::clone(&attached),
        Arc::clone(&should_stop),
    );

    let reattach = reattach_command(&launch, &node);
    let client_result = crate::remote::run_client_process(&local_socket, &reattach);

    // Teardown: unblock a still-pending accept, then close/kill the srun step.
    should_stop.store(true, Ordering::Release);
    let outcome = wait_with_timeout(&mut srun);
    let _ = bridge_thread.join();
    let _ = stderr_thread.join();

    match outcome {
        SrunOutcome::Exited(Some(0)) | SrunOutcome::KilledOnTeardown => {
            client_result.map_err(|err| err.to_string())
        }
        SrunOutcome::Exited(code) => {
            let lines = stderr_log
                .lock()
                .map(|log| log.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            Err(classify_srun_failure(code, &lines))
        }
    }
}

// --- Slurm metadata -------------------------------------------------------

fn resolve_job_info(job_id: &str) -> Result<SlurmJobInfo, String> {
    let parsed = run_scontrol_job(job_id)?;
    let nodes = expand_nodes(&parsed.node_list)?;
    Ok(SlurmJobInfo {
        user_id: parsed.user_id,
        user_name: parsed.user_name,
        state: parsed.state,
        nodes,
        batch_host: parsed.batch_host,
    })
}

/// Queries `scontrol`, preferring `--json` and falling back to `-o`.
fn run_scontrol_job(job_id: &str) -> Result<ParsedJob, String> {
    match Command::new("scontrol")
        .args(["show", "job", job_id, "--json"])
        .output()
    {
        Ok(output) if output.status.success() => {
            if let Ok(parsed) = parse_job_json(&String::from_utf8_lossy(&output.stdout)) {
                return Ok(parsed);
            }
            // JSON serializer absent or schema unexpected: fall back to `-o`.
        }
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(
                "'scontrol' was not found; run 'herdr slurm attach' from a Slurm submit host"
                    .to_string(),
            );
        }
        Err(err) => return Err(format!("failed to run scontrol: {err}")),
    }

    let output = Command::new("scontrol")
        .args(["show", "job", job_id, "-o"])
        .output()
        .map_err(|err| format!("failed to run scontrol: {err}"))?;
    if !output.status.success() {
        return Err(format!("Slurm job {job_id} was not found"));
    }
    parse_job_oneline(&String::from_utf8_lossy(&output.stdout))
}

fn parse_job_json(stdout: &str) -> Result<ParsedJob, String> {
    let value: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|err| format!("could not parse scontrol JSON output: {err}"))?;
    let job = value
        .get("jobs")
        .and_then(|jobs| jobs.get(0))
        .ok_or_else(|| "scontrol JSON output contained no job records".to_string())?;

    let user_id =
        job.get("user_id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "scontrol JSON output is missing user_id".to_string())? as u32;
    let user_name = job
        .get("user_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let state = json_job_state(job)?;
    let node_list = job
        .get("nodes")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let batch_host = job
        .get("batch_host")
        .and_then(serde_json::Value::as_str)
        .filter(|host| !host.is_empty())
        .map(str::to_string);

    Ok(ParsedJob {
        user_id,
        user_name,
        state,
        node_list,
        batch_host,
    })
}

/// `job_state` is a string in older Slurm and an array of strings in newer.
fn json_job_state(job: &serde_json::Value) -> Result<String, String> {
    match job.get("job_state") {
        Some(serde_json::Value::String(state)) => Ok(state.clone()),
        Some(serde_json::Value::Array(states)) => states
            .first()
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "scontrol JSON job_state array was empty".to_string()),
        _ => Err("scontrol JSON output is missing job_state".to_string()),
    }
}

fn parse_job_oneline(stdout: &str) -> Result<ParsedJob, String> {
    let mut fields: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for token in stdout.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            fields.entry(key).or_insert(value);
        }
    }

    let user_field = fields
        .get("UserId")
        .ok_or_else(|| "scontrol output is missing UserId".to_string())?;
    let (user_id, user_name) = parse_user_field(user_field)?;
    let state = fields
        .get("JobState")
        .ok_or_else(|| "scontrol output is missing JobState".to_string())?
        .to_string();
    let node_list = fields
        .get("NodeList")
        .map(|nodes| nodes.to_string())
        .unwrap_or_default();
    let batch_host = fields
        .get("BatchHost")
        .filter(|host| !host.is_empty() && **host != "(null)")
        .map(|host| host.to_string());

    Ok(ParsedJob {
        user_id,
        user_name,
        state,
        node_list,
        batch_host,
    })
}

/// Parses `UserId=alice(1000)` or a bare numeric uid into `(uid, name)`.
fn parse_user_field(field: &str) -> Result<(u32, Option<String>), String> {
    if let (Some(open), Some(close)) = (field.find('('), field.find(')')) {
        if open < close {
            let uid = field[open + 1..close]
                .parse()
                .map_err(|_| format!("could not parse uid from '{field}'"))?;
            let name = field[..open].to_string();
            let name = (!name.is_empty()).then_some(name);
            return Ok((uid, name));
        }
    }
    let uid = field
        .parse()
        .map_err(|_| format!("could not parse uid from '{field}'"))?;
    Ok((uid, None))
}

/// Expands a compact node list (`gpu[041-044]`) via `scontrol show hostnames`.
fn expand_nodes(node_list: &str) -> Result<Vec<String>, String> {
    if node_list.is_empty() || node_list == "(null)" {
        return Ok(Vec::new());
    }
    let output = Command::new("scontrol")
        .args(["show", "hostnames", node_list])
        .output()
        .map_err(|err| format!("failed to run scontrol show hostnames: {err}"))?;
    if !output.status.success() {
        return Err(format!("could not expand Slurm node list '{node_list}'"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn validate_job(job: &SlurmJobInfo, job_id: &str) -> Result<(), String> {
    if job.state != "RUNNING" {
        return Err(format!(
            "Slurm job {job_id} is {}; Herdr can only attach to RUNNING jobs",
            job.state
        ));
    }
    let euid = unsafe { libc::geteuid() };
    if euid != job.user_id {
        let owner = job
            .user_name
            .clone()
            .unwrap_or_else(|| format!("uid {}", job.user_id));
        return Err(format!(
            "Slurm job {job_id} belongs to {owner}, but you are running as uid {euid}"
        ));
    }
    if job.nodes.is_empty() {
        return Err(format!("Slurm job {job_id} has no allocated nodes"));
    }
    Ok(())
}

fn select_node(launch: &SlurmLaunch, nodes: &[String], job_id: &str) -> Result<String, String> {
    match &launch.node {
        Some(requested) => {
            if nodes.iter().any(|node| node == requested) {
                Ok(requested.clone())
            } else {
                Err(format!(
                    "node {requested} is not allocated to Slurm job {job_id}\nallocated nodes: {}",
                    nodes.join(",")
                ))
            }
        }
        None if nodes.len() == 1 => Ok(nodes[0].clone()),
        None => Err(format!(
            "Slurm job {job_id} spans multiple nodes: {}\npass --node <node> to select where the Herdr server is running",
            nodes.join(",")
        )),
    }
}

// --- srun command ---------------------------------------------------------

/// Builds the discrete `srun` arguments (no shell command string is used).
fn srun_args(launch: &SlurmLaunch, node: &str) -> Vec<String> {
    let mut args = vec![
        format!("--jobid={}", launch.job_id),
        "--nodes=1".to_string(),
        "--ntasks=1".to_string(),
    ];
    if launch.overlap {
        args.push("--overlap".to_string());
    }
    args.push(format!("--nodelist={node}"));

    let container = &launch.container;
    if let Some(name) = &container.name {
        args.push("--container-name".to_string());
        args.push(name.clone());
    }
    if let Some(image) = &container.image {
        args.push("--container-image".to_string());
        args.push(image.clone());
    }
    for mount in &container.mounts {
        args.push("--container-mounts".to_string());
        args.push(mount.clone());
    }
    if let Some(workdir) = &container.workdir {
        args.push("--container-workdir".to_string());
        args.push(workdir.clone());
    }
    if let Some(env) = &container.env {
        args.push("--container-env".to_string());
        args.push(env.clone());
    }

    args.push(
        launch
            .herdr_path
            .clone()
            .unwrap_or_else(|| "herdr".to_string()),
    );
    if let Some(session) = &launch.session_name {
        args.push("--session".to_string());
        args.push(session.clone());
    }
    args.push("remote-client-bridge".to_string());
    args
}

/// Builds a resolved reattach command with `--node` always pinned.
fn reattach_command(launch: &SlurmLaunch, node: &str) -> String {
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let mut parts = vec![
        shell_quote(&program),
        "slurm".to_string(),
        "attach".to_string(),
        launch.job_id.clone(),
        "--node".to_string(),
        shell_quote(node),
    ];
    if let Some(session) = &launch.session_name {
        parts.push("--session".to_string());
        parts.push(shell_quote(session));
    }
    if let Some(path) = &launch.herdr_path {
        parts.push("--herdr-path".to_string());
        parts.push(shell_quote(path));
    }
    if !launch.overlap {
        parts.push("--no-overlap".to_string());
    }
    let container = &launch.container;
    if let Some(name) = &container.name {
        parts.push("--container-name".to_string());
        parts.push(shell_quote(name));
    }
    if let Some(image) = &container.image {
        parts.push("--container-image".to_string());
        parts.push(shell_quote(image));
    }
    for mount in &container.mounts {
        parts.push("--container-mounts".to_string());
        parts.push(shell_quote(mount));
    }
    if let Some(workdir) = &container.workdir {
        parts.push("--container-workdir".to_string());
        parts.push(shell_quote(workdir));
    }
    if let Some(env) = &container.env {
        parts.push("--container-env".to_string());
        parts.push(shell_quote(env));
    }
    parts.join(" ")
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

// --- bridge ---------------------------------------------------------------

enum SrunOutcome {
    Exited(Option<i32>),
    KilledOnTeardown,
}

struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn install_signal_cleanup(socket: &Path) {
    let socket = socket.to_path_buf();
    // Rust `Drop` does not run on signals; clean the socket file on SIGINT.
    // A SIGTERM/SIGKILL leak is reclaimed by `prepare_socket_path` next run.
    let _ = ctrlc::set_handler(move || {
        let _ = std::fs::remove_file(&socket);
        std::process::exit(130);
    });
}

fn spawn_stderr_relay(
    stderr: std::process::ChildStderr,
    attached: Arc<AtomicBool>,
    log: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            // Show srun/Pyxis startup progress until the client takes the
            // terminal; afterwards only retain a bounded buffer.
            if !attached.load(Ordering::Acquire) {
                eprintln!("{line}");
            }
            if let Ok(mut log) = log.lock() {
                if log.len() >= STDERR_RING_LIMIT {
                    log.pop_front();
                }
                log.push_back(line);
            }
        }
    })
}

fn spawn_bridge(
    listener: UnixListener,
    srun_stdin: ChildStdin,
    srun_stdout: ChildStdout,
    attached: Arc<AtomicBool>,
    should_stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        if let Err(err) = listener.set_nonblocking(true) {
            eprintln!("herdr slurm: bridge listener setup failed: {err}");
            return;
        }
        loop {
            if should_stop.load(Ordering::Acquire) {
                return;
            }
            match listener.accept() {
                Ok((stream, _addr)) => {
                    if let Err(err) = stream.set_nonblocking(false) {
                        eprintln!("herdr slurm: bridge connection setup failed: {err}");
                        return;
                    }
                    attached.store(true, Ordering::Release);
                    bridge_once(stream, srun_stdin, srun_stdout);
                    return;
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(BRIDGE_ACCEPT_POLL);
                }
                Err(err) => {
                    eprintln!("herdr slurm: bridge listener failed: {err}");
                    return;
                }
            }
        }
    })
}

/// Byte-forwards one local connection to the `srun` step's stdin/stdout.
fn bridge_once(stream: UnixStream, srun_stdin: ChildStdin, srun_stdout: ChildStdout) {
    let mut stream_to_srun = match stream.try_clone() {
        Ok(clone) => clone,
        Err(err) => {
            eprintln!("herdr slurm: bridge failed to clone connection: {err}");
            return;
        }
    };
    let mut srun_to_stream = stream;
    let mut srun_stdin = srun_stdin;
    let mut srun_stdout = srun_stdout;

    let upload = thread::spawn(move || {
        let _ = crate::remote::copy_flush(&mut stream_to_srun, &mut srun_stdin);
        // Dropping `srun_stdin` closes the step's stdin, ending the cascade.
    });
    let _ = crate::remote::copy_flush(&mut srun_stdout, &mut srun_to_stream);
    let _ = srun_to_stream.shutdown(Shutdown::Write);
    let _ = upload.join();
}

/// Waits for the `srun` step to exit on its own, killing it after a grace
/// period so a wedged step cannot leak an accounted job step.
fn wait_with_timeout(child: &mut Child) -> SrunOutcome {
    let deadline = Instant::now() + SRUN_TEARDOWN_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return SrunOutcome::Exited(status.code()),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SrunOutcome::KilledOnTeardown;
                }
                thread::sleep(SRUN_TEARDOWN_POLL);
            }
            Err(_) => return SrunOutcome::KilledOnTeardown,
        }
    }
}

fn classify_srun_failure(code: Option<i32>, stderr_lines: &[String]) -> String {
    let stderr = stderr_lines.join("\n");
    let stderr = stderr.trim_end();
    let stderr_block = if stderr.is_empty() {
        String::new()
    } else {
        format!("\n\n{stderr}")
    };

    match code {
        // 127: srun could not exec the herdr binary inside the job.
        Some(127) => format!(
            "failed to start the Herdr bridge inside the Slurm job (srun exited with status 127)\n\
             \n\
             Herdr must be available inside the job environment.\n\
             Try --herdr-path /path/to/herdr or load the required module before attaching.{stderr_block}"
        ),
        Some(BRIDGE_EXIT_SOCKET_UNREACHABLE) => format!(
            "the Slurm bridge started, but it could not connect to herdr-client.sock\n\
             \n\
             If Herdr is running inside a Pyxis container, attach with the same --container-name as the Herdr server.\n\
             If you passed --container-image, you likely created a fresh container rather than re-entering the server's container.{stderr_block}"
        ),
        Some(BRIDGE_EXIT_PROTOCOL_MISMATCH) => format!(
            "the Herdr server inside the Slurm job uses an incompatible protocol version\n\
             \n\
             Update the herdr binary inside the job (module or --herdr-path) to match this client.{stderr_block}"
        ),
        Some(BRIDGE_EXIT_SERVER_SPAWN_FAILED) => format!(
            "the Herdr server inside the Slurm job failed to start{stderr_block}"
        ),
        Some(other) => format!(
            "the Slurm bridge step failed (srun exited with status {other}){stderr_block}"
        ),
        None => format!("the Slurm bridge step was terminated by a signal{stderr_block}"),
    }
}

// --- socket path ----------------------------------------------------------

fn local_forward_socket_path(job_id: &str, node: &str, session: Option<&str>) -> PathBuf {
    let pid = std::process::id();
    let session = session.unwrap_or("default");
    let tmpdir = std::env::temp_dir();

    let readable = tmpdir.join(format!(
        "herdr-slurm-{pid}-{job}-{node}-{session}.sock",
        job = sanitize_component(job_id),
        node = sanitize_component(node),
        session = sanitize_component(session),
    ));
    if fits_socket_path(&readable) {
        return readable;
    }

    let hash = short_hash(&format!("{job_id}\0{node}\0{session}"));
    let short_name = format!("herdr-s-{pid}-{hash}.sock");
    let short_in_tmp = tmpdir.join(&short_name);
    if fits_socket_path(&short_in_tmp) {
        return short_in_tmp;
    }
    PathBuf::from("/tmp").join(short_name)
}

fn fits_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH
}

fn sanitize_component(input: &str) -> String {
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
    sanitized.trim_matches('-').chars().take(24).collect()
}

fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn launch(job_id: &str, node: Option<&str>) -> SlurmLaunch {
        SlurmLaunch {
            job_id: job_id.to_string(),
            node: node.map(str::to_string),
            session_name: None,
            herdr_path: None,
            container: SlurmContainerOptions::default(),
            overlap: true,
        }
    }

    fn running_job(user_id: u32) -> SlurmJobInfo {
        SlurmJobInfo {
            user_id,
            user_name: Some("alice".to_string()),
            state: "RUNNING".to_string(),
            nodes: vec!["gpu1".to_string()],
            batch_host: None,
        }
    }

    #[test]
    fn extract_parses_attach_with_options() {
        crate::session::clear_explicit_session_for_test();
        let args = strs(&[
            "herdr",
            "slurm",
            "attach",
            "123456",
            "--node",
            "gpu042",
            "--no-overlap",
            "--container-name",
            "ctr",
            "--container-mounts",
            "/a:/a",
            "--container-mounts",
            "/b:/b",
        ]);
        let (cleaned, parsed) = extract_slurm_args(&args).unwrap();
        let parsed = parsed.expect("slurm launch");
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(parsed.job_id, "123456");
        assert_eq!(parsed.node.as_deref(), Some("gpu042"));
        assert!(!parsed.overlap);
        assert_eq!(parsed.container.name.as_deref(), Some("ctr"));
        assert_eq!(parsed.container.mounts, vec!["/a:/a", "/b:/b"]);
        assert!(parsed.session_name.is_none());
    }

    #[test]
    fn extract_rejects_missing_job_id() {
        let err = extract_slurm_args(&strs(&["herdr", "slurm", "attach"])).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn extract_rejects_non_numeric_job_id() {
        let err = extract_slurm_args(&strs(&["herdr", "slurm", "attach", "abc"])).unwrap_err();
        assert!(err.contains("invalid Slurm job id"));
    }

    #[test]
    fn extract_rejects_unknown_option() {
        let err = extract_slurm_args(&strs(&["herdr", "slurm", "attach", "1", "--takeover"]))
            .unwrap_err();
        assert!(err.contains("unknown option"));
    }

    #[test]
    fn extract_passes_through_non_slurm_args() {
        let args = strs(&["herdr", "status"]);
        let (cleaned, parsed) = extract_slurm_args(&args).unwrap();
        assert!(parsed.is_none());
        assert_eq!(cleaned, args);
    }

    #[test]
    fn job_id_validation_rejects_non_numeric() {
        assert!(validate_job_id("123").is_ok());
        assert!(validate_job_id("12_3").is_err());
        assert!(validate_job_id("").is_err());
    }

    #[test]
    fn srun_args_omit_overlap_and_session_by_default() {
        let mut value = launch("123", None);
        value.overlap = false;
        let args = srun_args(&value, "gpu1");
        assert!(args.contains(&"--jobid=123".to_string()));
        assert!(args.contains(&"--nodelist=gpu1".to_string()));
        assert!(!args.contains(&"--overlap".to_string()));
        assert!(!args.contains(&"--session".to_string()));
        assert_eq!(args.last().unwrap(), "remote-client-bridge");
    }

    #[test]
    fn srun_args_include_overlap_session_and_pyxis_as_discrete_args() {
        let mut value = launch("9", None);
        value.session_name = Some("train".to_string());
        value.herdr_path = Some("/opt/herdr".to_string());
        value.container.name = Some("ctr".to_string());
        value.container.mounts = vec!["/a:/a".to_string(), "/b:/b".to_string()];
        let args = srun_args(&value, "n1");

        assert!(args.contains(&"--overlap".to_string()));
        assert!(args.contains(&"/opt/herdr".to_string()));
        let session = args.iter().position(|a| a == "--session").unwrap();
        assert_eq!(args[session + 1], "train");
        let name = args.iter().position(|a| a == "--container-name").unwrap();
        assert_eq!(args[name + 1], "ctr");
        assert_eq!(
            args.iter().filter(|a| *a == "--container-mounts").count(),
            2
        );
    }

    #[test]
    fn select_node_uses_sole_node() {
        let value = launch("5", None);
        assert_eq!(
            select_node(&value, &["gpu1".to_string()], "5").unwrap(),
            "gpu1"
        );
    }

    #[test]
    fn select_node_requires_node_for_multi_node_jobs() {
        let value = launch("5", None);
        let err = select_node(&value, &["gpu1".to_string(), "gpu2".to_string()], "5").unwrap_err();
        assert!(err.contains("spans multiple nodes"));
    }

    #[test]
    fn select_node_rejects_node_outside_allocation() {
        let value = launch("5", Some("gpu9"));
        let err = select_node(&value, &["gpu1".to_string(), "gpu2".to_string()], "5").unwrap_err();
        assert!(err.contains("not allocated"));
    }

    #[test]
    fn validate_job_accepts_running_job_owned_by_caller() {
        let euid = unsafe { libc::geteuid() };
        assert!(validate_job(&running_job(euid), "1").is_ok());
    }

    #[test]
    fn validate_job_rejects_other_users_job() {
        let euid = unsafe { libc::geteuid() };
        let job = running_job(euid.wrapping_add(1));
        let err = validate_job(&job, "1").unwrap_err();
        assert!(err.contains("belongs to"));
    }

    #[test]
    fn validate_job_rejects_non_running_job() {
        let euid = unsafe { libc::geteuid() };
        let mut job = running_job(euid);
        job.state = "PENDING".to_string();
        let err = validate_job(&job, "1").unwrap_err();
        assert!(err.contains("PENDING"));
    }

    #[test]
    fn classify_failure_maps_known_codes() {
        assert!(classify_srun_failure(Some(127), &[]).contains("--herdr-path"));
        assert!(
            classify_srun_failure(Some(BRIDGE_EXIT_SOCKET_UNREACHABLE), &[])
                .contains("herdr-client.sock")
        );
        assert!(
            classify_srun_failure(Some(BRIDGE_EXIT_PROTOCOL_MISMATCH), &[]).contains("protocol")
        );
        assert!(
            classify_srun_failure(Some(BRIDGE_EXIT_SERVER_SPAWN_FAILED), &[])
                .contains("failed to start")
        );
        let lines = vec!["srun: error: boom".to_string()];
        assert!(classify_srun_failure(Some(1), &lines).contains("srun: error: boom"));
    }

    #[test]
    fn reattach_command_pins_node_and_carries_flags() {
        let mut value = launch("77", None);
        value.session_name = Some("s".to_string());
        value.overlap = false;
        let command = reattach_command(&value, "gpu5");
        assert!(command.contains("slurm attach 77"));
        assert!(command.contains("--node gpu5"));
        assert!(command.contains("--session s"));
        assert!(command.contains("--no-overlap"));
    }

    #[test]
    fn socket_path_uses_readable_name_when_it_fits() {
        let path = local_forward_socket_path("123456", "gpu042", None);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(name.starts_with("herdr-slurm-"), "got {name}");
        assert!(name.contains("-123456-gpu042-default."), "got {name}");
        assert!(fits_socket_path(&path));
    }

    #[test]
    fn socket_path_falls_back_to_hash_for_long_names() {
        let long_node = "n".repeat(120);
        let path = local_forward_socket_path("123456", &long_node, Some("session"));
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        assert!(name.starts_with("herdr-s-"), "got {name}");
        assert!(fits_socket_path(&path));
    }

    #[test]
    fn forwarding_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("herdr-slurm-perm-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _listener = UnixListener::bind(&path).unwrap();
        crate::ipc::restrict_socket_permissions(&path, SOCKET_PERMISSION_MODE).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, SOCKET_PERMISSION_MODE);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_oneline_extracts_scoped_fields() {
        let line = "JobId=123 JobName=train UserId=alice(1000) JobState=RUNNING \
                    NodeList=gpu[041-042] BatchHost=gpu041";
        let job = parse_job_oneline(line).unwrap();
        assert_eq!(job.user_id, 1000);
        assert_eq!(job.user_name.as_deref(), Some("alice"));
        assert_eq!(job.state, "RUNNING");
        assert_eq!(job.node_list, "gpu[041-042]");
        assert_eq!(job.batch_host.as_deref(), Some("gpu041"));
    }

    #[test]
    fn parse_json_handles_state_array_and_string() {
        let array = r#"{"jobs":[{"job_id":123,"user_id":1000,"user_name":"alice",
            "job_state":["RUNNING"],"nodes":"gpu041","batch_host":"gpu041"}]}"#;
        let job = parse_job_json(array).unwrap();
        assert_eq!(job.user_id, 1000);
        assert_eq!(job.state, "RUNNING");
        assert_eq!(job.node_list, "gpu041");

        let string = r#"{"jobs":[{"job_id":1,"user_id":5,"job_state":"PENDING","nodes":""}]}"#;
        let job = parse_job_json(string).unwrap();
        assert_eq!(job.state, "PENDING");
        assert!(job.node_list.is_empty());
    }
}
