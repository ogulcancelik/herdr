//! Sound notifications for agent state changes.
//!
//! Embeds mp3 files in the binary and plays them via system audio tools.
//! Uses afplay (macOS), Windows MediaPlayer, or decoder-capable Linux audio
//! players — no Rust audio dependencies.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::warn;

const DISABLE_SOUND_ENV: &str = "HERDR_DISABLE_SOUND";

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
    let _ = crate::terminal_notify::emit_bell();
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
    Command::new("powershell.exe")
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
        Command::new(self.program)
            .args(self.args)
            .arg(path)
            .output()
    }
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

    #[test]
    fn windows_media_player_script_accepts_literal_path_argument() {
        let script = windows_media_player_script();

        assert!(script.contains("param([string]$Path)"));
        assert!(script.contains("Resolve-Path -LiteralPath $Path"));
        assert!(script.contains("System.Windows.Media.MediaPlayer"));
    }
}
