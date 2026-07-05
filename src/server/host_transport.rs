//! Transport for host links. The server code only sees `LinkTransport`;
//! tests use `UnixSocketTransport` against a second in-process listener,
//! production uses `SshTransport` built on the `remote/unix.rs` bridge
//! helpers.
//!
//! Both remote bridge commands (`remote-client-bridge`, `remote-api-bridge`)
//! call `ensure_remote_server_running()` on the remote host as part of
//! connecting. `open_api`/`open_terminal` return as soon as the local ssh
//! process spawns, while that startup check runs remotely afterwards, so
//! merely opening the two channels in order still races two independent ssh
//! sessions against it and can double-spawn the remote server daemon (the
//! loser exits cleanly on `AddrInUse` -- harmless but wasteful). Callers
//! should complete one request/response round trip on the API channel
//! before opening the terminal channel.
//!
//! The remote bridge only signals a failed/closed backend via EOF, not an
//! explicit error frame, so a transport consumer must treat SSH child exit
//! or stdout EOF as terminal failure of all in-flight API requests on that
//! channel.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

use tracing::{info, warn};

/// One open channel to the remote host: the byte stream plus a closer that
/// tears the channel down from any thread. The stream itself is consumed by
/// `split` or moved into a reader thread, so detach/teardown needs this
/// separate handle to unblock a reader stuck in `read()`.
pub(crate) struct LinkChannel {
    pub(crate) stream: Box<dyn ReadWriteStream>,
    /// Closes the underlying connection out-of-band; a reader blocked in
    /// `read()` on the stream (or its read half) returns promptly with EOF
    /// or an error. Idempotent, and a no-op once the channel is torn down.
    pub(crate) close: Box<dyn Fn() + Send + Sync>,
}

pub(crate) trait LinkTransport: Send {
    /// Open a bidirectional byte stream to the remote JSON API socket.
    fn open_api(&self) -> std::io::Result<LinkChannel>;
    /// Open a bidirectional byte stream to the remote binary client socket.
    fn open_terminal(&self) -> std::io::Result<LinkChannel>;
}

/// A bidirectional byte stream. Usable single-threaded through the
/// `Read + Write` supertraits (write a request, read the response), or
/// split into independently owned halves so a reader thread and a writer
/// thread can run full-duplex concurrently -- the terminal channel is
/// driven that way (see the reader/writer threads in
/// `src/server/client_transport.rs` and `bridge_connection` in
/// `src/remote/unix.rs`).
pub(crate) trait ReadWriteStream: Read + Write + Send {
    /// Splits into (read half, write half). Dropping the write half signals
    /// EOF to the remote side while the read half stays usable for draining.
    /// Infallible because both transports prepare the two underlying handles
    /// when the channel is opened, so any failure surfaces there as a
    /// connect error instead of mid-session.
    fn split(self: Box<Self>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>);
}

/// Test/CI transport: both channels are plain unix sockets on this machine.
// Constructed only by tests today; consumed by the host event loop /
// remote pane adoption (Task 6).
#[allow(dead_code)]
pub(crate) struct UnixSocketTransport {
    pub(crate) api_socket: PathBuf,
    pub(crate) client_socket: PathBuf,
}

impl UnixSocketTransport {
    // consumed by the host event loop / remote pane adoption (Task 6)
    #[allow(dead_code)]
    fn connect(path: &std::path::Path) -> std::io::Result<LinkChannel> {
        let stream = std::os::unix::net::UnixStream::connect(path)?;
        // Clone the fds up front so `split` stays infallible: a failed
        // try_clone surfaces here as a connect error instead of somewhere
        // deep in the read/write plumbing mid-session.
        let read_half = stream.try_clone()?;
        let close_handle = stream.try_clone()?;
        let close: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            // shutdown() acts on the socket, not the fd, so this unblocks a
            // reader on the read half even though it runs on a clone.
            let _ = close_handle.shutdown(std::net::Shutdown::Both);
        });
        Ok(LinkChannel {
            stream: Box::new(UnixDuplexStream {
                read_half,
                write_half: UnixWriteHalf(stream),
            }),
            close,
        })
    }
}

impl LinkTransport for UnixSocketTransport {
    fn open_api(&self) -> std::io::Result<LinkChannel> {
        Self::connect(&self.api_socket)
    }
    fn open_terminal(&self) -> std::io::Result<LinkChannel> {
        Self::connect(&self.client_socket)
    }
}

/// A connected unix socket held as two pre-cloned handles so `split` cannot
/// fail. Unsplit, it reads from one and writes to the other -- both refer
/// to the same underlying socket.
struct UnixDuplexStream {
    read_half: std::os::unix::net::UnixStream,
    write_half: UnixWriteHalf,
}

impl Read for UnixDuplexStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_half.read(buf)
    }
}

impl Write for UnixDuplexStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_half.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.write_half.flush()
    }
}

impl ReadWriteStream for UnixDuplexStream {
    fn split(self: Box<Self>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        (Box::new(self.read_half), Box::new(self.write_half))
    }
}

/// Write half of a unix-socket channel. Dropping it half-closes the socket
/// (`shutdown(Write)`) so the peer sees EOF, mirroring how dropping the ssh
/// write half closes the remote bridge's stdin.
struct UnixWriteHalf(std::os::unix::net::UnixStream);

impl Write for UnixWriteHalf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Drop for UnixWriteHalf {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Write);
    }
}

/// Production transport: spawns one `ssh -T <target> <bridge command>` per
/// channel, matching the per-connection invocation `bridge_connection` uses
/// in `src/remote/unix.rs`, but hands back the child's stdio as the stream
/// instead of copying it into a local unix socket. The spawned ssh's stderr
/// is inherited, so its connection diagnostics land on the server daemon's
/// stderr.
///
/// `ssh_options` is an owned clone of the *paths* written by
/// [`crate::remote::write_managed_ssh_config`]. It does not own the
/// underlying `ManagedSshConfig` guard, so the caller that builds an
/// `SshTransport` with `Some(ssh_options)` must keep that guard alive for
/// at least as long as this `SshTransport` is used -- the config file and
/// control socket it points at are removed as soon as the guard drops, the
/// same ownership rule `RemoteSsh` follows for its own managed config.
/// Production links should pass the managed options: the managed config
/// sets `ServerAliveInterval 15` / `ServerAliveCountMax 4`, so a dead peer
/// is detected in about a minute; with `None`, dead-peer detection falls
/// back to kernel TCP defaults, which can take hours.
// Constructed by `HeadlessServer::build_host_transport` (Task 9) for
// production `host.attach`; no unit test here (needs a real ssh
// binary/target), covered by the homelab checklist instead.
pub(crate) struct SshTransport {
    pub(crate) target: String,
    pub(crate) remote_herdr: crate::remote::RemoteHerdr,
    pub(crate) session_name: String,
    pub(crate) ssh_options: Option<crate::remote::ManagedSshOptions>,
}

impl SshTransport {
    fn open_channel(&self, remote_command: String) -> std::io::Result<LinkChannel> {
        let mut command = std::process::Command::new("ssh");
        crate::remote::apply_managed_ssh_options(&mut command, self.ssh_options.as_ref());
        command.arg("-T").arg(&self.target).arg(remote_command);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());

        let mut child = command.spawn().map_err(|err| {
            std::io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}"))
        })?;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "ssh bridge stdin missing",
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "ssh bridge stdout missing",
            ));
        };

        let child: SharedChild = Arc::new(Mutex::new(Some(child)));
        let close_child = Arc::clone(&child);
        let close: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            // Child::kill (SIGKILL on unix) ends the ssh session so a reader
            // blocked on its stdout sees EOF. Reaping stays with the read
            // half's Drop, which takes the Child out of the shared slot; once
            // that has happened this closure finds the slot empty and is a
            // no-op, so a recycled pid can never be signalled.
            if let Some(child) = lock_child(&close_child).as_mut() {
                let _ = child.kill();
            }
        });
        Ok(LinkChannel {
            stream: Box::new(SshChildStream {
                stdin,
                read_half: SshReadHalf { child, stdout },
            }),
            close,
        })
    }
}

impl LinkTransport for SshTransport {
    fn open_api(&self) -> std::io::Result<LinkChannel> {
        self.open_channel(crate::remote::remote_api_bridge_command(
            &self.remote_herdr,
            &self.session_name,
        ))
    }

    fn open_terminal(&self) -> std::io::Result<LinkChannel> {
        self.open_channel(crate::remote::remote_bridge_command(
            &self.remote_herdr,
            &self.session_name,
        ))
    }
}

/// The ssh child kept behind a mutex so the channel closer and the read
/// half's reaping Drop can coordinate: whoever needs the child locks the
/// slot, and reaping empties it.
type SharedChild = Arc<Mutex<Option<Child>>>;

fn lock_child(child: &SharedChild) -> std::sync::MutexGuard<'_, Option<Child>> {
    // The critical sections here never panic, but if one ever does, carry
    // on with the data rather than skipping teardown/reaping.
    child
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Wraps one ssh child process's stdio as a single bidirectional stream:
/// reads come from the child's stdout, writes go to its stdin. Field order
/// matters for the unsplit drop path: `stdin` drops first (EOF to the
/// remote bridge), then the read half reaps the child.
struct SshChildStream {
    stdin: std::process::ChildStdin,
    read_half: SshReadHalf,
}

impl Read for SshChildStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_half.read(buf)
    }
}

impl Write for SshChildStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stdin.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stdin.flush()
    }
}

impl ReadWriteStream for SshChildStream {
    fn split(self: Box<Self>) -> (Box<dyn Read + Send>, Box<dyn Write + Send>) {
        // The Child rides with the read half: that is the side that observes
        // EOF, so it can reap as soon as the stream ends. Dropping the write
        // half only closes the child's stdin, which the remote bridge
        // (`bridge_stdio_to_socket`) propagates as a clean write shutdown.
        (Box::new(self.read_half), Box::new(self.stdin))
    }
}

/// Read half of an ssh channel; owns the child for reaping.
struct SshReadHalf {
    child: SharedChild,
    stdout: std::process::ChildStdout,
}

impl Read for SshReadHalf {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for SshReadHalf {
    fn drop(&mut self) {
        // Mirrors src/server/handoff.rs's cleanup_failed_import_child: check
        // for a natural exit first, then kill, and always wait() to reap --
        // otherwise a killed child lingers as a zombie until the server
        // process itself exits. The exit status is logged because it is the
        // only diagnostic an ssh failure leaves behind (ssh exits 255 when
        // the connection itself failed).
        let Some(mut child) = lock_child(&self.child).take() else {
            return;
        };
        let pid = child.id();
        match child.try_wait() {
            Ok(Some(status)) => {
                info!(pid, status = %status, "ssh link channel exited");
                return;
            }
            Ok(None) => {}
            Err(err) => {
                warn!(pid, err = %err, "failed to poll ssh link channel before teardown");
            }
        }
        if let Err(err) = child.kill() {
            warn!(pid, err = %err, "failed to kill ssh link channel");
        }
        match child.wait() {
            Ok(status) => {
                info!(pid, status = %status, "ssh link channel reaped");
            }
            Err(err) => {
                warn!(pid, err = %err, "failed to reap ssh link channel");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::Duration;

    // herdr has no tempfile dev-dependency; match the existing pattern in
    // e.g. src/server/headless.rs and src/remote/unix.rs tests of hand
    // rolling a unique directory under std::env::temp_dir().
    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn transport_for(sock: &std::path::Path) -> UnixSocketTransport {
        UnixSocketTransport {
            api_socket: sock.to_path_buf(),
            client_socket: sock.to_path_buf(),
        }
    }

    #[test]
    fn unix_transport_round_trips_a_line() {
        let dir = unique_temp_dir("host-transport-test");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let echo = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            s.write_all(line.as_bytes()).unwrap();
        });
        let mut stream = transport_for(&sock).open_api().unwrap().stream;
        stream.write_all(b"{\"ping\":1}\n").unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).unwrap();
        assert_eq!(line, "{\"ping\":1}\n");
        echo.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_halves_run_from_separate_threads_and_write_drop_signals_eof() {
        let dir = unique_temp_dir("host-transport-split");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let echo = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // Echo everything back once the client's write half drops; this
            // read_to_end only returns if that drop propagates as EOF.
            let mut received = Vec::new();
            s.try_clone().unwrap().read_to_end(&mut received).unwrap();
            s.write_all(&received).unwrap();
        });
        let channel = transport_for(&sock).open_api().unwrap();
        let (mut read_half, mut write_half) = channel.stream.split();
        let writer = std::thread::spawn(move || {
            write_half.write_all(b"across threads").unwrap();
            // write_half drops here -> shutdown(Write) -> peer sees EOF
        });
        let mut echoed = Vec::new();
        read_half.read_to_end(&mut echoed).unwrap();
        assert_eq!(echoed, b"across threads");
        writer.join().unwrap();
        echo.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn closer_unblocks_a_reader_stuck_in_read() {
        let dir = unique_temp_dir("host-transport-close");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // Park the accepted server end on the main thread so it stays open
        // without sending anything: the client reader genuinely blocks.
        let (server_tx, server_rx) = mpsc::channel();
        let accept = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            server_tx.send(s).unwrap();
        });
        let channel = transport_for(&sock).open_api().unwrap();
        let _server_end = server_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server accept");
        accept.join().unwrap();

        let close = channel.close;
        let (mut read_half, _write_half) = channel.stream.split();
        let (result_tx, result_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8];
            let _ = result_tx.send(read_half.read(&mut buf));
        });
        // Bias the test toward closing a reader that is already blocked in
        // read(); close-before-read also passes (immediate EOF), so this is
        // not a correctness wait.
        std::thread::sleep(Duration::from_millis(50));
        close();
        let result = result_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("reader did not unblock after close()");
        match result {
            Ok(0) | Err(_) => {}
            Ok(n) => panic!("reader returned unexpected data ({n} bytes) after close()"),
        }
        reader.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Symmetric with `closer_unblocks_a_reader_stuck_in_read`: proves
    /// `close()` also interrupts a blocked WRITE, not just a blocked read.
    /// This is what `RemotePaneAttach`'s writer thread (`src/server/
    /// remote_pane.rs`) relies on for bounded teardown against a wedged
    /// remote -- a writer stuck inside `write_message` because the peer's
    /// receive buffer is full and nothing is reading it.
    ///
    /// The peer end (`_server_end`) is kept open but never read from, so the
    /// kernel socket buffer genuinely fills and the writer thread's
    /// `write_all` call blocks for real (unix domain socket buffers are a
    /// few hundred KiB by default; a hundred 1 MiB chunks reliably exceeds
    /// that). Dropping the peer instead would fail the write fast with
    /// `EPIPE`/`ECONNRESET`, which would not distinguish "close() unblocked
    /// a genuinely blocked write" from "the write never blocked at all".
    #[test]
    fn closer_unblocks_a_writer_stuck_in_a_blocked_write() {
        let dir = unique_temp_dir("host-transport-close-write");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (server_tx, server_rx) = mpsc::channel();
        let accept = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            server_tx.send(s).unwrap();
        });
        let channel = transport_for(&sock).open_api().unwrap();
        let _server_end = server_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("server accept");
        accept.join().unwrap();

        let close = channel.close;
        let (_read_half, mut write_half) = channel.stream.split();
        let (result_tx, result_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let chunk = vec![0u8; 1024 * 1024];
            let mut result = Ok(());
            for _ in 0..100 {
                if let Err(err) = write_half.write_all(&chunk) {
                    result = Err(err);
                    break;
                }
            }
            let _ = result_tx.send(result);
        });
        // Bias the test toward closing a writer that is already blocked in
        // write(); close-before-any-write also passes (the very first write
        // then fails fast), so this is not a correctness wait.
        std::thread::sleep(Duration::from_millis(200));
        close();
        let result = result_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("writer did not unblock after close()");
        // Either outcome proves the write returned instead of hanging
        // forever: a clean full drain (unlikely given the volume written
        // against an unread peer, but not itself a failure) or an error from
        // the now-closed socket.
        let _ = result;
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
