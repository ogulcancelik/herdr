use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use tracing::info;

pub(crate) const HERDR_ENV_VAR: &str = "HERDR_ENV";
pub(crate) const HERDR_ENV_VALUE: &str = "1";
const NESTED_HERDR_MESSAGES: [&str; 6] = [
    "inception detected. we need to go deeper... said no one ever.",
    "recursion is a pathway to many abilities some consider to be... unnatural.",
    "you were so preoccupied with whether you could, you didn't stop to think if you should. — dr. malcolm",
    "recursive herdring is disabled. somewhere, a call stack breathes a sigh of relief.",
    "recursive descent denied. there is, in fact, such a thing as too much herdr.",
    "recursion detected. base case not found. aborting.",
];

mod api;
mod app;
mod cli;
mod config;
mod detect;
mod events;
mod ghostty;
mod input;
mod integration;
mod layout;
mod pane;
mod persist;
mod platform;
mod raw_input;
mod release_notes;
mod selection;
mod sound;
mod terminal_theme;
mod ui;
mod update;
mod workspace;

fn init_logging() {
    use std::fs::{self, OpenOptions};
    use tracing_subscriber::EnvFilter;

    let log_dir = crate::config::config_dir();
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("herdr.log");

    // Rotate: truncate if over 5MB
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > 5 * 1024 * 1024 {
            let _ = fs::remove_file(&log_path);
        }
    }

    let file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(f) => f,
        Err(_) => return, // can't open log file, proceed without logging
    };

    let filter =
        EnvFilter::try_from_env("HERDR_LOG").unwrap_or_else(|_| EnvFilter::new("herdr=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .with_target(false)
        .init();
}

const DEFAULT_CONFIG: &str = r##"# herdr configuration
# Place this file at ~/.config/herdr/config.toml

# Show first-run notification setup on startup.
# Missing also shows onboarding; set false after you've chosen.
# onboarding = true

[theme]
# Built-in themes: catppuccin, tokyo-night, dracula, nord, gruvbox,
#                  one-dark, solarized, kanagawa, rose-pine
# name = "catppuccin"

# Override individual color tokens on top of the base theme.
# Accepts: hex (#rrggbb), named colors, or rgb(r,g,b)
# [theme.custom]
# accent = "#f5c2e7"
# red = "#ff6188"
# green = "#a6e3a1"

[keys]
# Prefix key to enter navigate mode (default: "ctrl+b")
# Examples: "ctrl+b", "f12", "esc", "-"
# Accepted syntax: plain keys, ctrl/shift/alt modifiers, and special keys like enter/tab/esc/left/right/up/down
# Most reliable bindings are plain keys, ctrl+letter, esc/tab/enter, and function keys.
# alt+... and punctuation-with-modifiers may depend on your terminal/tmux setup.
# prefix = "ctrl+b"

# Navigate-mode actions
# new_workspace = "n"
# rename_workspace = "shift+n"
# close_workspace = "d"
# previous_workspace = "" # optional, unset by default
# next_workspace = ""     # optional, unset by default
# new_tab = "c"
# rename_tab = ""         # optional, unset by default
# previous_tab = ""       # optional, unset by default
# next_tab = ""           # optional, unset by default
# close_tab = ""          # optional, unset by default
# focus_pane_left = ""    # optional, unset by default
# focus_pane_down = ""    # optional, unset by default
# focus_pane_up = ""      # optional, unset by default
# focus_pane_right = ""   # optional, unset by default
# split_vertical = "v"
# split_horizontal = "-"
# close_pane = "x"
# fullscreen = "f"
# resize_mode = "r"
# toggle_sidebar = "b"

[ui]
# Sidebar width (auto-scaled based on workspace names, this sets the default)
# sidebar_width = 26

# Ask for confirmation before closing a workspace
# confirm_close = true

# Accent color for highlights, borders, and navigation UI.
# Accepts: hex (#89b4fa), named colors (cyan, blue, magenta), or rgb(r,g,b)
# accent = "cyan"

# Optional visual toast notifications for background workspace events
[ui.toast]
# enabled = false

# Play sounds when agents change state in background workspaces
[ui.sound]
# enabled = true
# Optional custom mp3 sound files. Relative paths are resolved from this config file's directory.
# path = "sounds/notification.mp3"   # one mp3 file for all sound notifications
# done_path = "sounds/done.mp3"      # overrides only finished notifications
# request_path = "sounds/request.mp3" # overrides only needs-attention notifications

# Per-agent overrides: default | on | off
# By default, droid is muted.
# [ui.sound.agents]
# droid = "off"

[advanced]
# Allow launching herdr from inside a herdr-managed pane.
# allow_nested = false
# Maximum scrollback buffer size in bytes retained per pane terminal.
# Matches Ghostty's default scrollback-limit behavior.
# scrollback_limit_bytes = 10000000
"##;

fn should_block_nested(config: &config::Config) -> bool {
    should_block_nested_for_env(config, std::env::var(HERDR_ENV_VAR).ok().as_deref())
}

fn should_block_nested_for_env(config: &config::Config, herdr_env: Option<&str>) -> bool {
    !config.advanced.allow_nested && herdr_env == Some(HERDR_ENV_VALUE)
}

fn random_nested_message() -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as usize)
        .unwrap_or(0);
    let index = (nanos ^ (std::process::id() as usize)) % NESTED_HERDR_MESSAGES.len();
    NESTED_HERDR_MESSAGES[index]
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if let cli::CommandOutcome::Handled(code) = cli::maybe_run(&args)? {
        std::process::exit(code);
    }

    // Subcommands and flags (no TUI, no logging needed)
    if args.get(1).map(|s| s.as_str()) == Some("update") {
        match update::self_update() {
            Ok(_) => return Ok(()),
            Err(e) => {
                eprintln!("update failed: {e}");
                std::process::exit(1);
            }
        }
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("herdr — terminal workspace manager for AI coding agents");
        println!();
        println!("Usage: herdr [options]");
        println!("       herdr update");
        println!("       herdr workspace <subcommand> ...");
        println!("       herdr tab <subcommand> ...");
        println!("       herdr pane <subcommand> ...");
        println!("       herdr wait <subcommand> ...");
        println!("       herdr integration <subcommand> ...");
        println!();
        println!("Commands:");
        println!("  update              Download and install the latest version");
        println!("  workspace           workspace helpers over the socket api");
        println!("  tab                 tab helpers over the socket api");
        println!("  pane                pane control helpers over the socket api");
        println!("  wait                blocking wait helpers over the socket api");
        println!("  integration         manage built-in agent integrations");
        println!();
        println!("Options:");
        println!("  --no-session        Don't restore or save sessions");
        println!("  --default-config    Print default configuration and exit");
        println!("  --version, -V       Print version and exit");
        println!("  --show-changelog    Preview the current version's release notes");
        println!("  --help, -h          Show this help");
        println!();
        println!("Config: {}", config::config_path().display());
        println!(
            "Logs:   {}",
            config::config_dir().join("herdr.log").display()
        );
        println!("Env:    HERDR_CONFIG_PATH overrides config file path");
        println!("Home:   https://herdr.dev");
        return Ok(());
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("herdr {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--default-config") {
        print!("{DEFAULT_CONFIG}");
        return Ok(());
    }

    // Reject unknown flags
    let known_flags = [
        "--no-session",
        "--version",
        "-V",
        "--default-config",
        "--show-changelog",
        "--help",
        "-h",
    ];
    for arg in &args[1..] {
        if arg.starts_with('-') && !known_flags.contains(&arg.as_str()) {
            eprintln!("unknown option: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
        if !arg.starts_with('-')
            && !["update", "workspace", "pane", "wait", "integration"].contains(&arg.as_str())
        {
            eprintln!("unknown command: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
    }

    let loaded_config = config::Config::load();
    if should_block_nested(&loaded_config.config) {
        eprintln!("\x1b[1merror:\x1b[0m nested herdr is disabled by default.");
        eprintln!("see configuration if you want to enable it.");
        eprintln!();
        eprintln!("\x1b[2m\"{}\"\x1b[0m", random_nested_message());
        std::process::exit(1);
    }

    init_logging();

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let _api_server = match api::start_server(api_tx, event_hub.clone()) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr is already running");
            eprintln!("socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let no_session = std::env::args().any(|a| a == "--no-session");
    let show_changelog = std::env::args().any(|a| a == "--show-changelog");
    let in_tmux = std::env::var("TMUX").is_ok();

    let original_hook = std::panic::take_hook();
    let panic_in_tmux = in_tmux;
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        if panic_in_tmux {
            let _ = std::io::Write::write_all(&mut io::stdout(), b"\x1b[>4;0m");
        }
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        ratatui::restore();
        original_hook(info);
    }));

    let config = &loaded_config.config;
    let config_diagnostic = if loaded_config.diagnostics.is_empty() {
        None
    } else if loaded_config.diagnostics.len() == 1 {
        Some(loaded_config.diagnostics[0].clone())
    } else {
        Some(format!(
            "{} (and {} more)",
            loaded_config.diagnostics[0],
            loaded_config.diagnostics.len() - 1
        ))
    };
    info!("herdr starting, pid={}", std::process::id());

    // Background auto-update (non-blocking, best-effort)
    // Downloads and installs new version silently, notifies TUI when done.
    // Skipped in --no-session mode (testing).

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let result = rt.block_on(async {
        let mut terminal = ratatui::init();
        execute!(
            io::stdout(),
            EnableMouseCapture,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;

        // tmux doesn't understand kitty keyboard protocol push (\e[>1u).
        // It uses modifyOtherKeys mode to send CSI u sequences for modified keys.
        // Enable modifyOtherKeys mode 2 so tmux sends Shift+Enter as \e[13;2u etc.
        if in_tmux {
            use std::io::Write;
            std::io::stdout().write_all(b"\x1b[>4;2m")?;
            std::io::stdout().flush()?;
        }

        let startup_release_notes = if show_changelog {
            crate::release_notes::load_preview_from_local_changelog(env!("CARGO_PKG_VERSION"))
        } else {
            crate::release_notes::load_pending_for_current_version()
        };

        let mut app = app::App::new(
            config,
            no_session,
            config_diagnostic,
            startup_release_notes,
            api_rx,
            event_hub,
        );
        let result = app.run(&mut terminal).await;

        // Reset modifyOtherKeys if we enabled it
        if in_tmux {
            use std::io::Write;
            std::io::stdout().write_all(b"\x1b[>4;0m")?;
            std::io::stdout().flush()?;
        }

        execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture
        )?;
        ratatui::restore();

        // Drop app (and all workspaces/panes) before runtime shuts down
        drop(app);

        result
    });

    // Shut down runtime immediately — kills lingering PTY reader/writer tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    info!("herdr exiting");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_herdr_blocks_when_env_is_set() {
        let config = config::Config::default();
        assert!(should_block_nested_for_env(&config, Some(HERDR_ENV_VALUE)));
    }

    #[test]
    fn nested_herdr_does_not_block_when_allowed() {
        let config: config::Config = toml::from_str("[advanced]\nallow_nested = true\n").unwrap();
        assert!(!should_block_nested_for_env(&config, Some(HERDR_ENV_VALUE)));
    }

    #[test]
    fn nested_herdr_does_not_block_without_env() {
        let config = config::Config::default();
        assert!(!should_block_nested_for_env(&config, None));
    }

    #[test]
    fn random_nested_message_comes_from_known_set() {
        let message = random_nested_message();
        assert!(NESTED_HERDR_MESSAGES.contains(&message));
    }

    #[test]
    fn nested_message_strings_no_longer_repeat_herdr_prefix() {
        assert!(NESTED_HERDR_MESSAGES
            .iter()
            .all(|message| !message.starts_with("herdr:")));
    }
}
