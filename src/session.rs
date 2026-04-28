use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const SESSION_ENV_VAR: &str = "HERDR_SESSION";
pub const DEFAULT_SESSION_NAME: &str = "default";

const MAX_SESSION_NAME_LEN: usize = 64;
const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const STOP_WAIT_POLL: Duration = Duration::from_millis(25);

static EXPLICIT_SESSION_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionInfo {
    pub name: String,
    pub default: bool,
    pub running: bool,
    pub socket_path: String,
    pub session_dir: String,
}

pub fn configure_from_args(args: &[String]) -> Result<Vec<String>, String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut requested_session = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--session" {
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --session".to_string());
            };
            requested_session = Some(value.clone());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--session=") {
            requested_session = Some(value.to_string());
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    if let Some(session) = requested_session {
        let session = normalize_name(&session)?;
        if let Some(session) = session {
            std::env::set_var(SESSION_ENV_VAR, session);
        } else {
            std::env::remove_var(SESSION_ENV_VAR);
        }
        EXPLICIT_SESSION_REQUESTED.store(true, Ordering::Relaxed);
    } else if std::env::var_os(crate::api::SOCKET_PATH_ENV_VAR).is_some() {
        EXPLICIT_SESSION_REQUESTED.store(false, Ordering::Relaxed);
    } else if let Ok(session) = std::env::var(SESSION_ENV_VAR) {
        if normalize_name(&session)?.is_none() {
            std::env::remove_var(SESSION_ENV_VAR);
        }
        EXPLICIT_SESSION_REQUESTED.store(false, Ordering::Relaxed);
    } else {
        EXPLICIT_SESSION_REQUESTED.store(false, Ordering::Relaxed);
    }

    Ok(cleaned)
}

pub fn active_name() -> Option<String> {
    std::env::var(SESSION_ENV_VAR)
        .ok()
        .filter(|name| name != DEFAULT_SESSION_NAME)
        .filter(|name| validate_name(name).is_ok())
}

pub fn explicit_session_requested() -> bool {
    EXPLICIT_SESSION_REQUESTED.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn clear_explicit_session_for_test() {
    EXPLICIT_SESSION_REQUESTED.store(false, Ordering::Relaxed);
}

pub fn data_dir() -> PathBuf {
    data_dir_for(active_name().as_deref())
}

pub fn data_dir_for(name: Option<&str>) -> PathBuf {
    let config_dir = crate::config::config_dir();
    match name {
        Some(name) => config_dir.join("sessions").join(name),
        None => config_dir,
    }
}

pub fn api_socket_path_for(name: Option<&str>) -> PathBuf {
    data_dir_for(name).join("herdr.sock")
}

pub fn active_api_socket_path() -> PathBuf {
    if explicit_session_requested() {
        return api_socket_path_for(active_name().as_deref());
    }
    if let Ok(path) = std::env::var(crate::api::SOCKET_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }
    api_socket_path_for(active_name().as_deref())
}

pub fn client_socket_path_for(name: Option<&str>) -> PathBuf {
    data_dir_for(name).join("herdr-client.sock")
}

pub fn list_sessions() -> std::io::Result<Vec<SessionInfo>> {
    let mut sessions = vec![session_info(None)];
    let sessions_dir = crate::config::config_dir().join("sessions");
    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(sessions),
        Err(err) => return Err(err),
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name != DEFAULT_SESSION_NAME && validate_name(&name).is_ok() {
            names.push(name);
        }
    }
    names.sort();
    sessions.extend(names.iter().map(|name| session_info(Some(name))));
    Ok(sessions)
}

pub fn session_info(name: Option<&str>) -> SessionInfo {
    let default = name.is_none();
    let display_name = name.unwrap_or(DEFAULT_SESSION_NAME).to_string();
    let socket_path = api_socket_path_for(name);
    let session_dir = data_dir_for(name);
    SessionInfo {
        name: display_name,
        default,
        running: is_running_at(&socket_path),
        socket_path: socket_path.display().to_string(),
        session_dir: session_dir.display().to_string(),
    }
}

pub fn parse_target_name(name: &str) -> Result<Option<String>, String> {
    normalize_name(name)
}

pub fn stop_session(name: Option<&str>) -> Result<SessionInfo, String> {
    stop_session_with_timeout(name, STOP_WAIT_TIMEOUT)
}

fn stop_session_with_timeout(name: Option<&str>, timeout: Duration) -> Result<SessionInfo, String> {
    let socket_path = api_socket_path_for(name);
    let request = serde_json::json!({
        "id": "cli:session:stop",
        "method": "server.stop",
        "params": {}
    });
    let mut stream = UnixStream::connect(&socket_path).map_err(|err| {
        format!(
            "session {} is not running or cannot be reached at {}: {err}",
            name.unwrap_or(DEFAULT_SESSION_NAME),
            socket_path.display()
        )
    })?;
    stream
        .write_all(request.to_string().as_bytes())
        .map_err(|err| err.to_string())?;
    stream.write_all(b"\n").map_err(|err| err.to_string())?;
    stream.flush().map_err(|err| err.to_string())?;

    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| err.to_string())?;
    let response: serde_json::Value = serde_json::from_str(&line).map_err(|err| err.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error.to_string());
    }
    if !wait_until_stopped(&socket_path, timeout) {
        return Err(format!(
            "session {} did not stop within {}ms; socket is still reachable at {}",
            name.unwrap_or(DEFAULT_SESSION_NAME),
            timeout.as_millis(),
            socket_path.display()
        ));
    }
    Ok(session_info(name))
}

pub fn delete_session(name: &str) -> Result<SessionInfo, String> {
    if name == DEFAULT_SESSION_NAME {
        return Err("deleting the default session is not supported".to_string());
    }
    validate_name(name)?;
    let socket_path = api_socket_path_for(Some(name));
    if is_running_at(&socket_path) {
        return Err(format!(
            "session {name} is running; stop it before deleting"
        ));
    }
    let info = session_info(Some(name));
    let dir = data_dir_for(Some(name));
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(info),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(info),
        Err(err) => Err(err.to_string()),
    }
}

fn is_running_at(socket_path: &Path) -> bool {
    socket_path.exists() && UnixStream::connect(socket_path).is_ok()
}

fn wait_until_stopped(socket_path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_running_at(socket_path) {
            return true;
        }
        std::thread::sleep(STOP_WAIT_POLL);
    }
    !is_running_at(socket_path)
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("session name cannot be empty".to_string());
    }
    if name.len() > MAX_SESSION_NAME_LEN {
        return Err(format!(
            "session name cannot be longer than {MAX_SESSION_NAME_LEN} bytes"
        ));
    }
    if name == "." || name == ".." {
        return Err("session name cannot be . or ..".to_string());
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "session name may only contain ASCII letters, numbers, '.', '_' and '-'".to_string(),
        );
    }
    Ok(())
}

fn normalize_name(name: &str) -> Result<Option<String>, String> {
    if name == DEFAULT_SESSION_NAME {
        return Ok(None);
    }
    validate_name(name)?;
    Ok(Some(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn configure_from_args_removes_global_session_option() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        let args = vec![
            "herdr".to_string(),
            "--session".to_string(),
            "work".to_string(),
            "workspace".to_string(),
            "list".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(std::env::var(SESSION_ENV_VAR).as_deref(), Ok("work"));
        assert!(explicit_session_requested());
        assert_eq!(cleaned, vec!["herdr", "workspace", "list"]);
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
    }

    #[test]
    fn configure_from_args_accepts_equals_form() {
        let _guard = env_lock().lock().unwrap();
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        let args = vec![
            "herdr".to_string(),
            "server".to_string(),
            "stop".to_string(),
            "--session=api".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(std::env::var(SESSION_ENV_VAR).as_deref(), Ok("api"));
        assert!(explicit_session_requested());
        assert_eq!(cleaned, vec!["herdr", "server", "stop"]);
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
    }

    #[test]
    fn configure_from_args_maps_default_session_name_to_default_path() {
        let _guard = env_lock().lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("herdr-session-default-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var(SESSION_ENV_VAR, "work");
        clear_explicit_session_for_test();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/inherited.sock");
        let args = vec![
            "herdr".to_string(),
            "--session".to_string(),
            DEFAULT_SESSION_NAME.to_string(),
            "workspace".to_string(),
            "list".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr", "workspace", "list"]);
        assert!(std::env::var(SESSION_ENV_VAR).is_err());
        assert!(explicit_session_requested());
        assert_eq!(
            active_api_socket_path(),
            config_home
                .join(crate::config::app_dir_name())
                .join("herdr.sock")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn env_session_does_not_mark_session_explicit() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var(SESSION_ENV_VAR, "env-session");
        EXPLICIT_SESSION_REQUESTED.store(true, Ordering::Relaxed);
        let args = vec![
            "herdr".to_string(),
            "workspace".to_string(),
            "list".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr", "workspace", "list"]);
        assert_eq!(std::env::var(SESSION_ENV_VAR).as_deref(), Ok("env-session"));
        assert!(!explicit_session_requested());
        std::env::remove_var(SESSION_ENV_VAR);
    }

    #[test]
    fn env_default_session_name_uses_default_path() {
        let _guard = env_lock().lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("herdr-env-session-default-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::set_var(SESSION_ENV_VAR, DEFAULT_SESSION_NAME);
        EXPLICIT_SESSION_REQUESTED.store(true, Ordering::Relaxed);
        let args = vec![
            "herdr".to_string(),
            "workspace".to_string(),
            "list".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr", "workspace", "list"]);
        assert!(std::env::var(SESSION_ENV_VAR).is_err());
        assert!(!explicit_session_requested());
        assert_eq!(
            active_api_socket_path(),
            config_home
                .join(crate::config::app_dir_name())
                .join("herdr.sock")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var(SESSION_ENV_VAR);
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        clear_explicit_session_for_test();
    }

    #[test]
    fn explicit_session_socket_ignores_inherited_socket_override() {
        let _guard = env_lock().lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("herdr-session-precedence-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::set_var(SESSION_ENV_VAR, "work");
        EXPLICIT_SESSION_REQUESTED.store(true, Ordering::Relaxed);
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/inherited.sock");

        let path = active_api_socket_path();

        assert_eq!(
            path,
            config_home
                .join(crate::config::app_dir_name())
                .join("sessions")
                .join("work")
                .join("herdr.sock")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn env_socket_override_wins_without_explicit_session() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var(SESSION_ENV_VAR, "work");
        clear_explicit_session_for_test();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/explicit.sock");

        assert_eq!(
            active_api_socket_path(),
            PathBuf::from("/tmp/explicit.sock")
        );

        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn env_socket_override_skips_invalid_env_session_validation_without_explicit_session() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var(SESSION_ENV_VAR, "bad/name");
        clear_explicit_session_for_test();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/herdr.sock");
        let args = vec![
            "herdr".to_string(),
            "workspace".to_string(),
            "list".to_string(),
        ];

        let cleaned = configure_from_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr", "workspace", "list"]);
        assert!(!explicit_session_requested());
        assert_eq!(active_api_socket_path(), PathBuf::from("/tmp/herdr.sock"));
        assert_eq!(std::env::var(SESSION_ENV_VAR).as_deref(), Ok("bad/name"));

        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn stop_session_fails_when_socket_remains_reachable_after_timeout() {
        let _guard = env_lock().lock().unwrap();
        let config_home = PathBuf::from(format!("/tmp/hs-stop-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        let session_name = "slow";
        let socket_path = api_socket_path_for(Some(session_name));
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&socket_path);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let keep_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let keep_running_for_thread = keep_running.clone();
        let handle = std::thread::spawn(move || {
            while keep_running_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Ok(reader_stream) = stream.try_clone() {
                            let mut request = String::new();
                            let _ = BufReader::new(reader_stream).read_line(&mut request);
                        }
                        let _ = stream.write_all(b"{\"id\":\"cli:session:stop\",\"result\":{}}\n");
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        let err = stop_session_with_timeout(Some(session_name), Duration::from_millis(75))
            .expect_err("still-running session should fail");

        assert!(err.contains("did not stop"), "{err}");
        assert!(
            err.contains(socket_path.to_string_lossy().as_ref()),
            "{err}"
        );
        keep_running.store(false, Ordering::Relaxed);
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&config_home);
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn invalid_names_are_rejected() {
        let _guard = env_lock().lock().unwrap();
        assert!(validate_name("../prod").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("work session").is_err());
    }

    #[test]
    fn parse_default_target_name_maps_to_default_session() {
        assert_eq!(parse_target_name(DEFAULT_SESSION_NAME).unwrap(), None);
        assert_eq!(parse_target_name("work").unwrap(), Some("work".to_string()));
    }

    #[test]
    fn delete_default_session_is_rejected() {
        assert!(delete_session(DEFAULT_SESSION_NAME).is_err());
    }

    #[test]
    fn list_sessions_skips_reserved_default_directory() {
        let _guard = env_lock().lock().unwrap();
        let config_home =
            std::env::temp_dir().join(format!("herdr-session-list-{}", std::process::id()));
        let sessions_dir = config_home
            .join(crate::config::app_dir_name())
            .join("sessions");
        std::fs::create_dir_all(sessions_dir.join(DEFAULT_SESSION_NAME)).unwrap();
        std::fs::create_dir_all(sessions_dir.join("work")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        std::env::remove_var(SESSION_ENV_VAR);
        clear_explicit_session_for_test();

        let sessions = list_sessions().unwrap();
        let names: Vec<_> = sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect();

        assert_eq!(names, vec![DEFAULT_SESSION_NAME, "work"]);
        std::fs::remove_dir_all(&config_home).unwrap();
        std::env::remove_var("XDG_CONFIG_HOME");
    }
}
