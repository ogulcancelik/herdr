pub mod schema;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use tracing::{debug, error, info, warn};

use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, Request, ResponseResult, SuccessResponse,
};

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var(SOCKET_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }

    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("herdr.sock");
    }

    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("herdr/herdr.sock");
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/herdr/herdr.sock");
    }

    PathBuf::from("/tmp/herdr.sock")
}

pub struct ServerHandle {
    _thread: std::thread::JoinHandle<()>,
    path: PathBuf,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), err = %err, "failed to remove api socket on shutdown");
            }
        }
    }
}

pub fn start_server(
    api_tx: std::sync::mpsc::Sender<ApiRequestMessage>,
) -> std::io::Result<ServerHandle> {
    let path = socket_path();
    prepare_socket_path(&path)?;

    let listener = UnixListener::bind(&path)?;
    info!(path = %path.display(), "api server listening");

    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = handle_connection(stream, &api_tx) {
                        warn!(err = %err, "api connection failed");
                    }
                }
                Err(err) => {
                    error!(err = %err, "api listener accept failed");
                    break;
                }
            }
        }
        debug!("api server thread exiting");
    });

    Ok(ServerHandle {
        _thread: thread,
        path,
    })
}

fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if let Err(err) = fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    api_tx: &std::sync::mpsc::Sender<ApiRequestMessage>,
) -> std::io::Result<()> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&stream);
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
    }

    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let response = match serde_json::from_str::<Request>(line) {
        Ok(request) => handle_request(request, api_tx),
        Err(err) => serde_json::to_string(&ErrorResponse {
            id: String::new(),
            error: ErrorBody {
                code: "invalid_request".into(),
                message: format!("invalid request: {err}"),
            },
        })?,
    };

    stream.write_all(response.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn handle_request(request: Request, api_tx: &std::sync::mpsc::Sender<ApiRequestMessage>) -> String {
    let request_id = request.id.clone();
    match request.method {
        Method::Ping(_) => serde_json::to_string(&SuccessResponse {
            id: request.id,
            result: ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").into(),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
                .to_string()
        }),
        Method::PaneWaitForOutput(params) => wait_for_output(request_id, params, api_tx),
        _ => dispatch_to_app(request, api_tx),
    }
}

fn wait_for_output(
    request_id: String,
    params: crate::api::schema::PaneWaitForOutputParams,
    api_tx: &std::sync::mpsc::Sender<ApiRequestMessage>,
) -> String {
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let regex = match &params.r#match {
        crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
            Ok(regex) => Some(regex),
            Err(err) => {
                return serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "invalid_regex".into(),
                        message: err.to_string(),
                    },
                })
                .unwrap();
            }
        },
        crate::api::schema::OutputMatch::Substring { .. } => None,
    };

    loop {
        let read_request = Request {
            id: format!("{request_id}:read"),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: params.pane_id.clone(),
                source: params.source.clone(),
                lines: params.lines,
                strip_ansi: params.strip_ansi,
            }),
        };
        let response = dispatch_to_app(read_request, api_tx);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return response;
        };
        if value.get("error").is_some() {
            let mut value = value;
            value["id"] = serde_json::Value::String(request_id);
            return serde_json::to_string(&value).unwrap();
        }

        let read_value = value["result"]["read"].clone();
        let Ok(read) = serde_json::from_value::<crate::api::schema::PaneReadResult>(read_value)
        else {
            return serde_json::to_string(&ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "internal_error".into(),
                    message: "failed to decode pane read result".into(),
                },
            })
            .unwrap();
        };

        let matched_line = match &params.r#match {
            crate::api::schema::OutputMatch::Substring { value } => read
                .text
                .lines()
                .find(|line| line.contains(value))
                .map(|line| line.to_string()),
            crate::api::schema::OutputMatch::Regex { .. } => regex.as_ref().and_then(|re| {
                read.text
                    .lines()
                    .find(|line| re.is_match(line))
                    .map(|line| line.to_string())
            }),
        };
        if matched_line.is_some() {
            let revision = read.revision;
            return serde_json::to_string(&SuccessResponse {
                id: request_id,
                result: ResponseResult::OutputMatched {
                    pane_id: params.pane_id,
                    revision,
                    matched_line,
                    read,
                },
            })
            .unwrap();
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return serde_json::to_string(&ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "timeout".into(),
                    message: "timed out waiting for output match".into(),
                },
            })
            .unwrap();
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn dispatch_to_app(
    request: Request,
    api_tx: &std::sync::mpsc::Sender<ApiRequestMessage>,
) -> String {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    if let Err(err) = api_tx.send(ApiRequestMessage {
        request,
        respond_to,
    }) {
        return serde_json::to_string(&ErrorResponse {
            id: String::new(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: format!("failed to dispatch request: {err}"),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#.to_string()
        });
    }

    response_rx.recv().unwrap_or_else(|err| {
        serde_json::to_string(&ErrorResponse {
            id: String::new(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: format!("request handling failed: {err}"),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#.to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_prefers_explicit_env_override() {
        let unique = format!("/tmp/herdr-test-{}.sock", std::process::id());
        std::env::set_var(SOCKET_PATH_ENV_VAR, &unique);
        assert_eq!(socket_path(), PathBuf::from(&unique));
        std::env::remove_var(SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn ping_request_returns_pong() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
        );

        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_1");
        assert!(matches!(parsed.result, ResponseResult::Pong { .. }));
    }

    #[test]
    fn request_dispatches_to_app_channel() {
        let (tx, rx) = std::sync::mpsc::channel();
        let request = Request {
            id: "req_2".into(),
            method: Method::WorkspaceList(crate::api::schema::EmptyParams::default()),
        };

        let request_for_thread = request.clone();
        let thread = std::thread::spawn(move || handle_request(request_for_thread, &tx));

        let msg = rx.recv().unwrap();
        assert_eq!(msg.request.id, "req_2");
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: "req_2".into(),
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let response = thread.join().unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_2");
    }
}
