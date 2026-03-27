//! Sound notifications for agent state changes.
//!
//! Embeds mp3 files in the binary and plays them via system audio tools.
//! Uses afplay (macOS) or paplay/aplay (Linux) — no Rust audio dependencies.

use std::io::Write;
use std::process::Command;

static SOUND_DONE: &[u8] = include_bytes!("../assets/sounds/done.mp3");
static SOUND_REQUEST: &[u8] = include_bytes!("../assets/sounds/request.mp3");

/// Which notification sound to play.
pub enum Sound {
    /// Agent finished work (transitioned to Idle).
    Done,
    /// Agent needs input (transitioned to Waiting).
    Request,
}

/// Play a notification sound in a background thread.
/// Silently does nothing if no audio player is available.
pub fn play(sound: Sound) {
    std::thread::spawn(move || {
        let data = match sound {
            Sound::Done => SOUND_DONE,
            Sound::Request => SOUND_REQUEST,
        };
        let _ = play_bytes(data);
    });
}

fn play_bytes(data: &[u8]) -> Result<(), String> {
    // Write to a temp file (audio players need a file path)
    let tmp = std::env::temp_dir().join(format!("herdr-sound-{}.mp3", std::process::id()));
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    drop(file);

    let result = if cfg!(target_os = "macos") {
        Command::new("afplay").arg(&tmp).output()
    } else {
        // Try paplay (PulseAudio) first, fall back to aplay (ALSA)
        Command::new("paplay").arg(&tmp).output().or_else(|_| {
            Command::new("aplay").arg(&tmp).output()
        })
    };

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!("player exited with {}", output.status)),
        Err(e) => Err(format!("no audio player available: {e}")),
    }
}
