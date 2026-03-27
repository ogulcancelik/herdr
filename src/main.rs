use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use tracing::info;

mod app;
mod config;
mod detect;
mod events;
mod input;
mod layout;
mod pane;
mod persist;
mod platform;
mod pty_callbacks;
mod selection;
mod sound;
mod ui;
mod update;
mod workspace;

fn init_logging() {
    use std::fs::{self, OpenOptions};
    use tracing_subscriber::EnvFilter;

    let log_dir = if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        std::path::PathBuf::from(dir).join("herdr")
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".config/herdr")
    } else {
        std::path::PathBuf::from("/tmp/herdr")
    };
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

    let filter = EnvFilter::try_from_env("HERDR_LOG")
        .unwrap_or_else(|_| EnvFilter::new("herdr=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .with_target(false)
        .init();
}

const DEFAULT_CONFIG: &str = r#"# herdr configuration
# Place this file at ~/.config/herdr/config.toml

[keys]
# Prefix key to enter navigate mode (e.g. "ctrl+s", "ctrl+a", "ctrl+b")
# prefix = "ctrl+s"

# Pane controls (in navigate mode)
# split_vertical = "v"
# split_horizontal = "-"
# close_pane = "x"
# fullscreen = "f"

[ui]
# Sidebar width (auto-scaled based on workspace names, this sets the default)
# sidebar_width = 26

# Ask for confirmation before closing a workspace
# confirm_close = true

# Accent color for highlights, borders, and navigation UI.
# Accepts: hex (#89b4fa), named colors (cyan, blue, magenta), or rgb(r,g,b)
# accent = "cyan"

# Play sounds when agents change state in background workspaces
# sound = true
"#;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

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
        println!();
        println!("Commands:");
        println!("  update              Download and install the latest version");
        println!();
        println!("Options:");
        println!("  --no-session        Don't restore or save sessions");
        println!("  --default-config    Print default configuration and exit");
        println!("  --version, -V       Print version and exit");
        println!("  --help, -h          Show this help");
        println!();
        println!("Config: ~/.config/herdr/config.toml");
        println!("Logs:   ~/.config/herdr/herdr.log");
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
    let known_flags = ["--no-session", "--version", "-V", "--default-config", "--help", "-h"];
    for arg in &args[1..] {
        if arg.starts_with('-') && !known_flags.contains(&arg.as_str()) {
            eprintln!("unknown option: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
        if !arg.starts_with('-') && arg != "update" {
            eprintln!("unknown command: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
    }

    init_logging();

    let no_session = std::env::args().any(|a| a == "--no-session");
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

    let config = config::Config::load();
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
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;

        // tmux doesn't understand kitty keyboard protocol push (\e[>1u).
        // It uses modifyOtherKeys mode to send CSI u sequences for modified keys.
        // Enable modifyOtherKeys mode 2 so tmux sends Shift+Enter as \e[13;2u etc.
        if in_tmux {
            use std::io::Write;
            std::io::stdout().write_all(b"\x1b[>4;2m")?;
            std::io::stdout().flush()?;
        }

        let mut app = app::App::new(&config, no_session);
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
