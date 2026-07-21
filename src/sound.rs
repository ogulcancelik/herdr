//! Sound notifications for agent state changes.
//!
//! Embeds mp3 files in the binary and plays them via system audio tools.
//! Uses afplay (macOS), Windows MediaPlayer, or decoder-capable Linux audio
//! players — no Rust audio dependencies.

use std::io::Write;
#[cfg(not(any(windows, target_os = "macos")))]
use std::io::{Read, Result as IoResult};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(any(windows, target_os = "macos")))]
use std::time::{Duration, Instant};

use tracing::warn;

const DISABLE_SOUND_ENV: &str = "HERDR_DISABLE_SOUND";
#[cfg(not(any(windows, target_os = "macos")))]
const AUDIO_PLAYER_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(not(any(windows, target_os = "macos")))]
const AUDIO_PLAYER_POLL_INTERVAL: Duration = Duration::from_millis(25);

static SOUND_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static SOUND_DONE: &[u8] = include_bytes!("../assets/sounds/done.mp3");
static SOUND_REQUEST: &[u8] = include_bytes!("../assets/sounds/request.mp3");

/// Which notification sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Agent finished work (transitioned to Idle).
    Done,
    /// Agent needs input (transitioned to Blocked).
    Request,
}

/// Play a notification sound in a background thread.
/// Silently does nothing if no audio player is available.
pub fn play(sound: Sound, config: &crate::config::SoundConfig) {
    if sound_playback_disabled_by_env() {
        return;
    }

    let custom_path = config.path_for(sound);
    std::thread::spawn(move || {
        if let Some(path) = custom_path {
            match play_file(&path) {
                Ok(()) => return,
                Err(err) => {
                    warn!(path = %path.display(), sound = ?sound, err = %err, "custom sound playback failed, falling back to built-in sound")
                }
            }
        }

        let data = match sound {
            Sound::Done => SOUND_DONE,
            Sound::Request => SOUND_REQUEST,
        };

        if let Err(err) = play_bytes(data) {
            warn!(sound = ?sound, err = %err, "sound playback failed");
        }
    });
}

fn sound_playback_disabled_by_env() -> bool {
    std::env::var_os(DISABLE_SOUND_ENV).is_some() || std::env::var_os("NEXTEST").is_some()
}

fn play_file(path: &Path) -> Result<(), String> {
    match run_player(path) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!("player exited with {}", output.status)),
        Err(err) => Err(err),
    }
}

fn play_bytes(data: &[u8]) -> Result<(), String> {
    // Write to a temp file because the supported audio players need a file path.
    let tmp = temp_sound_path();
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    drop(file);

    let result = run_player(&tmp);

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!("player exited with {}", output.status)),
        Err(e) => Err(e),
    }
}

fn temp_sound_path() -> PathBuf {
    let id = SOUND_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("herdr-sound-{}-{id}.mp3", std::process::id()))
}

#[cfg(windows)]
fn run_player(path: &Path) -> Result<Output, String> {
    run_windows_player(path)
}

#[cfg(target_os = "macos")]
fn run_player(path: &Path) -> Result<Output, String> {
    Command::new("afplay")
        .arg(path)
        .output()
        .map_err(|e| format!("no audio player available: {e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_player(path: &Path) -> Result<Output, String> {
    run_linux_player(path)
}

#[cfg(any(windows, test))]
fn windows_media_player_script() -> &'static str {
    r#"
param([string]$Path)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationCore
$resolved = (Resolve-Path -LiteralPath $Path).ProviderPath
$player = [System.Windows.Media.MediaPlayer]::new()
$script:done = $false
$script:failed = $null
$player.add_MediaEnded({ $script:done = $true })
$player.add_MediaFailed({
    param($sender, $eventArgs)
    $script:failed = $eventArgs.ErrorException
    $script:done = $true
})
$player.Open([Uri]::new($resolved))
$deadline = [DateTime]::UtcNow.AddSeconds(15)
while (-not $script:done -and -not $player.NaturalDuration.HasTimeSpan -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 25
}
if ($script:failed) { throw $script:failed }
$player.Play()
while (-not $script:done -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 50
}
$player.Close()
if ($script:failed) { throw $script:failed }
if (-not $script:done) { throw 'sound playback timed out' }
"#
}

#[cfg(windows)]
fn run_windows_player(path: &Path) -> Result<Output, String> {
    crate::noninteractive_process::command("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            windows_media_player_script(),
        ])
        .arg(path)
        .output()
        .map_err(|e| format!("Windows MediaPlayer playback failed: {e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
#[derive(Debug, Clone, Copy)]
struct AudioPlayer {
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(not(any(windows, target_os = "macos")))]
impl AudioPlayer {
    fn output(self, path: &Path) -> std::io::Result<Output> {
        self.output_with_timeout(path, AUDIO_PLAYER_TIMEOUT)
    }

    fn output_with_timeout(self, path: &Path, timeout: Duration) -> std::io::Result<Output> {
        let mut child = Command::new(self.program)
            .args(self.args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let Some(stdout) = child.stdout.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stdout was not piped"));
        };
        let Some(stderr) = child.stderr.take() else {
            terminate_and_reap(&mut child)?;
            return Err(std::io::Error::other("audio player stderr was not piped"));
        };
        let stdout_reader = read_output(stdout);
        let stderr_reader = read_output(stderr);
        let deadline = Instant::now() + timeout;

        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (stdout, stderr) = finish_output(stdout_reader, stderr_reader)?;
                    return Ok(Output {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {}
                Err(wait_err) => {
                    let cleanup_result = terminate_and_reap(&mut child);
                    let _ = finish_output(stdout_reader, stderr_reader);
                    cleanup_result?;
                    return Err(wait_err);
                }
            }

            let now = Instant::now();
            if now >= deadline {
                let cleanup_result = terminate_and_reap(&mut child);
                let _ = finish_output(stdout_reader, stderr_reader);
                cleanup_result?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{} playback timed out after {timeout:?}", self.program),
                ));
            }

            std::thread::sleep((deadline - now).min(AUDIO_PLAYER_POLL_INTERVAL));
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_output<R>(mut reader: R) -> std::thread::JoinHandle<IoResult<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
fn finish_output(
    stdout_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
    stderr_reader: std::thread::JoinHandle<IoResult<Vec<u8>>>,
) -> IoResult<(Vec<u8>, Vec<u8>)> {
    let stdout = stdout_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| std::io::Error::other("audio player stderr reader panicked"))??;
    Ok((stdout, stderr))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn terminate_and_reap(child: &mut std::process::Child) -> std::io::Result<()> {
    if let Err(kill_err) = child.kill() {
        if child.try_wait()?.is_none() {
            return Err(kill_err);
        }
    }
    child.wait().map(|_| ())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn linux_audio_players() -> &'static [AudioPlayer] {
    // Do not add bare aplay here. It does not decode MP3 and plays MP3 bytes as raw PCM.
    &[
        AudioPlayer {
            program: "paplay",
            args: &[],
        },
        AudioPlayer {
            program: "pw-play",
            args: &[],
        },
        AudioPlayer {
            program: "ffplay",
            args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
        },
        AudioPlayer {
            program: "mpg123",
            args: &["-q"],
        },
        AudioPlayer {
            program: "mpv",
            args: &["--no-video", "--really-quiet"],
        },
    ]
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_linux_player(path: &Path) -> Result<Output, String> {
    let mut errors = Vec::new();

    for player in linux_audio_players() {
        match player.output(path) {
            Ok(output) if output.status.success() => return Ok(output),
            Ok(output) => errors.push(player_error(*player, &output)),
            Err(err) => errors.push(format!("{} failed: {err}", player.program)),
        }
    }

    Err(format!(
        "no mp3-capable audio player available: {}",
        errors.join("; ")
    ))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn player_error(player: AudioPlayer, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    if stderr.is_empty() {
        format!("{} exited with {}", player.program, output.status)
    } else {
        format!("{} exited with {}: {stderr}", player.program, output.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_sound_paths_are_unique() {
        assert_ne!(temp_sound_path(), temp_sound_path());
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_players_are_mp3_capable() {
        let programs: Vec<&str> = linux_audio_players()
            .iter()
            .map(|player| player.program)
            .collect();

        assert_eq!(programs, ["paplay", "pw-play", "ffplay", "mpg123", "mpv"]);
        assert!(!programs.contains(&"aplay"));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_player_does_not_wait_forever() {
        let pid_path = temp_sound_path().with_extension("pid");
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 2",
                "herdr-sound-timeout-test",
            ],
        };
        let result = player.output_with_timeout(&pid_path, Duration::from_millis(100));
        let pid = std::fs::read_to_string(&pid_path)
            .expect("hanging test player should record its process ID");
        let _ = std::fs::remove_file(pid_path);

        let err = result.expect_err("hanging audio player should time out");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let status = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("test should inspect the timed-out player PID");
        assert!(
            !status.success(),
            "timed-out audio player should be terminated and reaped"
        );
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_audio_player_preserves_completed_output() {
        let player = AudioPlayer {
            program: "sh",
            args: &[
                "-c",
                "i=0; while [ \"$i\" -lt 8192 ]; do printf 0123456789abcdef; i=$((i + 1)); done; i=0; while [ \"$i\" -lt 8192 ]; do printf fedcba9876543210; i=$((i + 1)); done >&2; exit 7",
                "herdr-sound-output-test",
            ],
        };

        let output = player
            .output_with_timeout(Path::new("unused.mp3"), Duration::from_secs(5))
            .expect("completed audio player should return its output");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout.len(), 131_072);
        assert_eq!(output.stderr.len(), 131_072);
        assert!(output.stdout.starts_with(b"0123456789abcdef"));
        assert!(output.stderr.starts_with(b"fedcba9876543210"));
    }

    #[test]
    fn windows_media_player_script_accepts_literal_path_argument() {
        let script = windows_media_player_script();

        assert!(script.contains("param([string]$Path)"));
        assert!(script.contains("Resolve-Path -LiteralPath $Path"));
        assert!(script.contains("System.Windows.Media.MediaPlayer"));
    }
}
