use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;

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

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

fn set_host_color_scheme_reports(enabled: bool) -> io::Result<()> {
    use std::io::Write;

    let sequence = if enabled {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE
    } else {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE
    };
    io::stdout().write_all(sequence.as_bytes())?;
    io::stdout().flush()
}

mod agent_resume;
mod api;
mod app;
mod build_info;
#[cfg(not(windows))]
mod checksum;
mod cli;
mod client;
mod config;
mod detect;
mod events;
mod ghostty;
mod handoff_runtime;
mod input;
mod integration;
mod ipc;
mod kitty_graphics;
mod layout;
mod logging;
mod pane;
mod persist;
mod platform;
mod plugin_command;
mod plugin_paths;
mod product_announcements;
mod protocol;
mod pty;
mod raw_input;
mod release_notes;
mod remote;
mod render_prof;
mod selection;
mod server;
mod session;
mod sound;
mod terminal;
mod terminal_modes;
mod terminal_notify;
mod terminal_theme;
mod ui;
mod update;
mod workspace;
mod worktree;

fn init_logging() {
    crate::logging::init_file_logging("herdr.log");
}

const DEFAULT_CONFIG: &str = r##"# herdr configuration
# Place this file at ~/.config/herdr/config.toml

# Show first-run notification setup on startup.
# Missing also shows onboarding; set false after you've chosen.
# onboarding = true

[theme]
# Built-in themes: catppuccin, terminal, tokyo-night, dracula, nord,
#                  gruvbox, one-dark, solarized, kanagawa, rose-pine,
#                  vesper
# name = "catppuccin"

# Follow host terminal light/dark appearance and switch Herdr UI themes.
# Existing manual behavior is unchanged unless this is true.
# auto_switch = false
# dark_name = "catppuccin"
# light_name = "catppuccin-latte"

# Override individual color tokens on top of the base theme.
# Accepts: hex (#rrggbb), named colors, rgb(r,g,b), or panel_bg = "reset"
# [theme.custom]
# panel_bg = "reset"
# accent = "#f5c2e7"
# red = "#ff6188"
# green = "#a6e3a1"

[terminal]
# Executable used for new interactive panes.
# Empty means $SHELL, then /bin/sh.
# default_shell = ""

# Startup mode for new interactive pane shells: "auto", "login", or "non_login".
# "auto" uses login shells on macOS and keeps the current behavior elsewhere.
# shell_mode = "auto"

# CWD policy for new panes, tabs, and workspaces when no explicit --cwd is provided.
# Use "follow" to inherit the source pane/workspace, "home" for $HOME,
# "current" for Herdr's process directory, or a fixed path such as "~/Projects".
# new_cwd = "follow"

[update]
# Update channel used by background version checks and `herdr update`.
# Use "stable" for normal releases or "preview" for opt-in preview builds.
# channel = "stable"

# Check herdr.dev for new Herdr versions in the background.
# version_check = true

# Check herdr.dev for remote agent-detection manifest updates in the background.
# manifest_check = true

[keys]
# Prefix key to enter prefix mode (default: "ctrl+b")
# Examples: "ctrl+b", "f12", "esc", "-"
# Action bindings use explicit syntax: "prefix+n" requires the prefix;
# "ctrl+alt+n" is a direct terminal-mode shortcut.
# Accepted key syntax: plain keys, ctrl/shift/alt/cmd/super modifiers, and special keys like enter/tab/esc/left/right/up/down.
# Named punctuation such as minus, comma, ampersand, plus, and backtick is also accepted.
# Most reliable direct bindings are ctrl+letter, function keys, and explicit modified chords.
# alt+..., cmd/super, and punctuation-with-modifiers may depend on your terminal/tmux setup.
# prefix = "ctrl+b"

# Prefix-mode actions
# help = "prefix+?"
# settings = "prefix+s"
# detach = "prefix+q"
# reload_config = "prefix+shift+r"
# open_notification_target = "prefix+o"
# workspace_picker = "prefix+w"
# goto = "prefix+g"
# new_workspace = "prefix+shift+n"
# new_worktree = "prefix+shift+g"
# open_worktree = ""    # optional, unset by default
# remove_worktree = ""  # optional, unset by default; opens confirmation
# rename_workspace = "prefix+shift+w"
# close_workspace = "prefix+shift+d"
# previous_workspace = "" # optional, unset by default
# next_workspace = ""     # optional, unset by default
# previous_agent = ""     # optional, unset by default
# next_agent = ""         # optional, unset by default
# focus_agent = ""        # optional indexed binding, e.g. "prefix+alt+1..9"
# remote_image_paste = "ctrl+v" # only active in herdr --remote; empty disables raw-key image paste
# new_tab = "prefix+c"
# rename_tab = "prefix+shift+t"
# previous_tab = "prefix+p"
# next_tab = "prefix+n"
# Repeatable form: after the action fires, tapping the
# same key again within ~500ms repeats it without pressing the prefix again.
# Opt-in per binding, default off. Also supported on: previous_workspace,
# next_workspace, previous_agent, next_agent, cycle_pane_next,
# cycle_pane_previous, swap_pane_left/down/up/right.
# previous_tab = { key = "prefix+p", repeat = true }
# switch_tab = "prefix+1..9"
# switch_workspace = ""   # optional indexed binding, e.g. "prefix+shift+1..9"
# close_tab = "prefix+shift+x"
# rename_pane = "prefix+shift+p"
# edit_scrollback = "prefix+e"
# focus_pane_left = "prefix+h"
# focus_pane_down = "prefix+j"
# focus_pane_up = "prefix+k"
# focus_pane_right = "prefix+l"
# cycle_pane_next = "prefix+tab"
# cycle_pane_previous = "prefix+shift+tab"
# last_pane = ""          # optional, unset by default; bind e.g. "prefix+tab" for global back-and-forth
# split_vertical = "prefix+v"
# split_horizontal = "prefix+minus"
# close_pane = "prefix+x"
# zoom = "prefix+z"       # legacy alias: fullscreen
# resize_mode = "prefix+r"
# toggle_sidebar = "prefix+b"

# Navigate-mode movement. These local shortcuts win while navigate mode is open.
# They are independent from focus_pane_*. Do not include prefix+, esc, enter, tab, or 1..9 here.
# navigate_workspace_up = "up"
# navigate_workspace_down = "down"
# navigate_pane_left = "h"      # left arrow always focuses the pane to the left
# navigate_pane_down = "j"
# navigate_pane_up = "k"
# navigate_pane_right = "l"     # right arrow always focuses the pane to the right

# Custom commands use the same binding syntax.
# type = "shell" runs detached in the background.
# type = "pane" opens a temporary pane and closes it when the command exits.
# On Windows, command strings run through cmd.exe /d /c.
# [[keys.command]]
# key = "prefix+alt+g"
# type = "pane"
# command = "lazygit"

# Legacy indexed shortcut config is still parsed for compatibility.
# Prefer switch_tab, switch_workspace, and focus_agent for new configs.
# [keys.indexed]
# tabs = ""       # e.g. "ctrl" makes ctrl+1..9 switch tabs directly
# workspaces = "" # e.g. "ctrl+shift" makes ctrl+shift+1..9 switch workspaces directly
# agents = ""     # e.g. "alt" makes alt+1..9 focus agent rows directly

# [worktrees]
# directory = "~/.herdr/worktrees"

[ui]
# Sidebar width (auto-scaled based on workspace names, this sets the default)
# sidebar_width = 26

# Minimum sidebar width when expanded (columns)
# sidebar_min_width = 18

# Maximum sidebar width when expanded (columns)
# sidebar_max_width = 36

# Collapsed sidebar presentation: "compact" keeps the narrow status rail, "hidden" uses zero width.
# sidebar_collapsed_mode = "compact"

# Terminal width at or below which Herdr uses the mobile single-column layout.
# Increase this for foldables, tablets, or wide phone terminals.
# mobile_width_threshold = 64

# Capture mouse input for Herdr's mouse UI.
# Set false to let the terminal handle normal clicks, such as Cmd-clicking URLs.
# Pane apps like lazygit and btop can still receive mouse when they request it.
# mouse_capture = true

# Host cursor policy: "auto", "native", or "drawn".
# "auto" draws Herdr's own cursor on Windows to avoid ConPTY cursor flicker, and uses the native terminal cursor elsewhere.
# "native" always uses the outer terminal cursor. "drawn" always draws Herdr's cursor as terminal cell content.
# host_cursor = "auto"

# Optional modifier that forwards right-click hold/drag gestures to pane apps instead of opening Herdr's pane menu.
# Empty/off disables this. Shift is intentionally unsupported because terminals commonly reserve Shift+mouse.
# right_click_passthrough_modifier = ""

# Force a full redraw when the outer terminal regains focus.
# Set false to reduce visible flashing when switching back to Herdr.
# Trade-off: rare host terminal surface corruption may persist until the next full redraw.
# redraw_on_focus_gained = true

# Pane scrollback lines to scroll per mouse wheel notch.
# mouse_scroll_lines = 3

# Ask for confirmation before closing a workspace
# confirm_close = true

# Ask for a tab name before creating a new tab.
# Set false to create tabs immediately with generated names.
# prompt_new_tab_name = true

# Draw borders around split panes.
# pane_borders = true

# Keep split panes visually separated instead of sharing divider borders.
# pane_gaps = true

# Show detected/reported agent labels in split pane borders when no manual pane name is set.
# show_agent_labels_on_pane_borders = false

# Hide the tab row when a workspace has exactly one tab.
# New tabs can still be created with the configured keybinding.
# hide_tab_bar_when_single_tab = false

# Agent panel ordering: "spaces" (grouped by space) or "priority" (attention queue).
# "workspaces" is accepted as an alias for "spaces".
# agent_panel_sort = "spaces"

# Accent color for highlights, borders, and navigation UI.
# Accepts: hex (#89b4fa), named colors (cyan, blue, magenta), or rgb(r,g,b)
# accent = "cyan"

# Background notification popup delivery
[ui.toast]
# off = disable pop-up notifications
# herdr = show in-app toasts
# terminal = ask the outer terminal to show a desktop notification
# system = ask the OS notification service directly
# delivery = "off"
# delay_seconds = 1

[ui.toast.herdr]
# position = "bottom-right"

[ui.toast.clipboard]
# enabled = true
# position = "bottom-center"

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

[session]
# Resume supported AI-agent panes into their native conversation sessions after
# a Herdr server restart. Requires official integrations that report session refs.
# resume_agents_on_restore = true

[remote]
# Whether herdr manages the ssh config used for `herdr --remote`.
# When true (default), herdr runs remote ssh through a generated config that
# includes your ~/.ssh/config first and adds ServerAliveInterval/
# ServerAliveCountMax as fallbacks (so any keepalive values you set yourself
# still win) to survive idle network/NAT timeouts. Herdr also uses a private
# per-attach OpenSSH control socket to reuse the first authenticated connection.
# Set false to run plain ssh against your ssh config unchanged — this does not
# force keepalive or multiplexing off, it only stops herdr from adding its own.
# manage_ssh_config = true

[experimental]
# Allow launching herdr from inside a herdr-managed pane.
# allow_nested = false
# Experimental local Kitty graphics rendering for attached clients.
# Requires a Kitty graphics-compatible outer terminal.
# kitty_graphics = false
# Save recent pane screen history across full server restarts.
pane_history = false
# While prefix mode is active, temporarily switch the macOS host input
# source to an ASCII-capable keyboard layout so prefix commands register
# even when a CJK IME is active, then restore the previous input source
# when prefix mode exits. macOS only; best-effort. Default: false.
# switch_ascii_input_source_in_prefix = false
# Expose the focused pane's cursor to the outer terminal so macOS input
# methods keep tracking the candidate window when TUIs paint their own
# cursor (Claude Code, pi, codex). Trade-off: extra cursor visible for
# apps that hide it without painting a replacement (vim normal mode, etc.).
# reveal_hidden_cursor_for_cjk_ime = false
# Optional allow-list: only reveal for focused panes whose detected agent
# matches one of these names. Empty means apply to any focused pane.
# If the list contains no valid names, the reveal does not apply.
# Accepted: pi, claude, codex, gemini, cursor, devin, cline, opencode,
# copilot, kimi, kiro, droid, amp, grok, hermes, kilo, qodercli, qoder.
# cjk_ime_agents = []
# Cursor shape rendered when reveal_hidden_cursor_for_cjk_ime is true.
# Values: block, steady_block (default), underline, steady_underline, bar, steady_bar.
# cjk_ime_cursor_shape = "steady_block"

[advanced]
# Maximum scrollback buffer size in bytes retained per pane terminal.
# Matches Ghostty's default scrollback-limit behavior.
# scrollback_limit_bytes = 10000000
"##;

fn should_block_nested(config: &config::Config) -> bool {
    should_block_nested_for_env(config, std::env::var(HERDR_ENV_VAR).ok().as_deref())
}

fn should_block_nested_for_env(config: &config::Config, herdr_env: Option<&str>) -> bool {
    !config.experimental.allow_nested && herdr_env == Some(HERDR_ENV_VALUE)
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

fn exit_if_nested_disabled(config: &config::Config) {
    if should_block_nested(config) {
        eprintln!("\x1b[1merror:\x1b[0m nested herdr is disabled by default.");
        eprintln!("see configuration if you want to enable it.");
        eprintln!();
        eprintln!("\x1b[2m\"{}\"\x1b[0m", random_nested_message());
        std::process::exit(1);
    }
}

fn main() -> io::Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let args = match session::configure_from_args(&raw_args) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    };
    let (args, remote_launch) = match remote::extract_remote_args(&args) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(2);
        }
    };

    if remote_launch.is_some()
        && args.get(1).is_some()
        && !args.iter().any(|a| {
            matches!(
                a.as_str(),
                "--help" | "-h" | "--version" | "-V" | "--default-config"
            )
        })
    {
        eprintln!("error: --remote can only be used with the default launch command");
        eprintln!("run 'herdr --help' for usage");
        std::process::exit(2);
    }

    if let cli::CommandOutcome::Handled(code) = cli::maybe_run(&args)? {
        std::process::exit(code);
    }

    // Subcommands and flags (no TUI, no logging needed)
    if args.get(1).map(|s| s.as_str()) == Some("remote-client-bridge") {
        return remote::run_remote_client_bridge();
    }

    if args.get(1).map(|s| s.as_str()) == Some("server") {
        return server::headless::run_server();
    }

    // Hidden client mode: connect to an existing server's client socket.
    if args.get(1).map(|s| s.as_str()) == Some("client") {
        let loaded_config = config::Config::load();
        exit_if_nested_disabled(&loaded_config.config);
        return client::run_client();
    }

    if args.get(1).map(|s| s.as_str()) == Some("update") {
        let options = match update::parse_self_update_args(&args[2..]) {
            Ok(options) => options,
            Err(err) if err.starts_with("usage:") => {
                eprintln!("{err}");
                std::process::exit(0);
            }
            Err(err) => {
                eprintln!("{err}");
                eprintln!("usage: herdr update [--handoff]");
                std::process::exit(2);
            }
        };
        match update::self_update(options) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if e.starts_with("self-update is disabled") {
                    eprintln!("{e}");
                } else {
                    eprintln!("update failed: {e}");
                }
                std::process::exit(1);
            }
        }
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("herdr — terminal workspace manager for AI coding agents");
        println!();
        println!("Usage: herdr [options]");
        println!("       herdr --session <name> [options]");
        println!("       herdr --remote <ssh-target> [--session <name>]");
        println!("       herdr session attach <name>");
        println!("       herdr completion zsh");
        println!("       herdr update [--handoff]");
        println!("       herdr channel set <stable|preview>");
        println!("       herdr server stop");
        println!("       herdr server reload-config");
        println!("       herdr api <subcommand> ...");
        println!("       herdr completion <shell>");
        println!("       herdr config <subcommand> ...");
        println!("       herdr channel <subcommand> ...");
        println!("       herdr workspace <subcommand> ...");
        println!("       herdr worktree <subcommand> ...");
        println!("       herdr tab <subcommand> ...");
        println!("       herdr notification <subcommand> ...");
        println!("       herdr agent <subcommand> ...");
        println!("       herdr pane <subcommand> ...");
        println!("       herdr wait <subcommand> ...");
        println!("       herdr session <subcommand> ...");
        println!("       herdr integration <subcommand> ...");
        println!();
        println!("Common commands:");
        for (command, description) in [
            ("herdr", "Launch or attach to the persistent session"),
            (
                "herdr status [server|client]",
                "Show local client and running server status",
            ),
            ("herdr update", "Download and install the latest version"),
            ("herdr completion zsh", "Generate shell completions for zsh"),
            (
                "herdr server stop",
                "Stop the running server via the API socket",
            ),
            (
                "herdr channel set <stable|preview>",
                "Choose the stable or preview update channel",
            ),
            (
                "herdr server reload-config",
                "Reload config.toml in the running server",
            ),
            (
                "herdr config reset-keys",
                "Back up config.toml and remove custom keybindings",
            ),
            (
                "herdr channel <subcommand>",
                "Manage the stable or preview update channel",
            ),
            (
                "herdr api <subcommand>",
                "Inspect socket API metadata and live runtime state",
            ),
            (
                "herdr workspace <subcommand>",
                "Workspace helpers over the socket API",
            ),
            (
                "herdr worktree <subcommand>",
                "Git worktree helpers over the socket API",
            ),
            ("herdr tab <subcommand>", "Tab helpers over the socket API"),
            (
                "herdr notification <subcommand>",
                "Notification helpers over the socket API",
            ),
            (
                "herdr agent <subcommand>",
                "Agent/terminal helpers over the socket API",
            ),
            (
                "herdr pane <subcommand>",
                "Pane control helpers over the socket API",
            ),
            (
                "herdr wait <subcommand>",
                "Blocking wait helpers over the socket API",
            ),
            (
                "herdr session <subcommand>",
                "Manage named persistent sessions",
            ),
            (
                "herdr integration <subcommand>",
                "Manage built-in agent integrations",
            ),
        ] {
            println!("  {command:<32} {description}");
        }
        println!();
        println!("Advanced commands:");
        println!("  {:<32} Run as headless server", "herdr server");
        println!();
        println!("Options:");
        println!("  --no-session        Run monolithically (no server/client, escape hatch)");
        println!("  --session <name>    Use or create a named persistent session");
        println!("  --remote <target>   Attach through SSH to a remote Herdr server");
        println!("  --remote-keybindings <local|server>");
        println!("                      Keybindings for --remote app attach (default: local)");
        println!("  --handoff           Opt into live handoff for update or remote attach");
        println!("  --default-config    Print default configuration and exit");
        println!("  --version, -V       Print version and exit");
        println!("  --help, -h          Show this help");
        println!();
        println!("Config: {}", config::config_path().display());
        println!("Logs:   {}", logging::help_log_paths_summary());
        println!("Env:    HERDR_CONFIG_PATH overrides config file path");
        println!("Home:   https://herdr.dev");
        return Ok(());
    }

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("herdr {}", crate::build_info::version());
        return Ok(());
    }

    if args.iter().any(|a| a == "--default-config") {
        print!("{DEFAULT_CONFIG}");
        return Ok(());
    }

    // Reject unknown flags
    let known_flags = [
        "--no-session",
        "--session",
        "--remote",
        "--remote-keybindings",
        "--version",
        "-V",
        "--default-config",
        "--help",
        "-h",
    ];
    for arg in &args[1..] {
        let arg_name = arg.split_once('=').map(|(name, _)| name).unwrap_or(arg);
        if arg.starts_with('-') && !known_flags.contains(&arg_name) {
            eprintln!("unknown option: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
        if !arg.starts_with('-')
            && ![
                "server",
                "client",
                "remote-client-bridge",
                "update",
                "status",
                "config",
                "channel",
                "workspace",
                "worktree",
                "pane",
                "wait",
                "session",
                "integration",
            ]
            .contains(&arg.as_str())
        {
            eprintln!("unknown command: {arg}");
            eprintln!("run 'herdr --help' for usage");
            std::process::exit(1);
        }
    }

    if let Some(remote_launch) = remote_launch {
        let remote_target = remote_launch.target.clone();
        if let Err(err) = remote::run_remote(remote_launch) {
            eprintln!("error: {err}");
            remote::print_remote_error_hint(&err, &remote_target);
            std::process::exit(1);
        }
        return Ok(());
    }

    let loaded_config = config::Config::load();
    exit_if_nested_disabled(&loaded_config.config);

    let no_session = args.iter().any(|a| a == "--no-session");

    // Auto-detect launch: when --no-session is NOT set, use server/client mode.
    // Check if a server is running, spawn one if needed, then attach as client.
    if !no_session {
        if let Err(err) = server::autodetect::auto_detect_launch() {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }

    // --- Monolithic mode (--no-session escape hatch) ---
    // This is the pre-mission single-process behavior.

    init_logging();

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();
    let _api_server = match api::start_server_with_capabilities(api_tx, event_hub.clone(), None) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr is already running");
            eprintln!("socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let modify_other_keys_mode = crate::input::host_modify_other_keys_mode();

    let original_hook = std::panic::take_hook();
    let panic_resets_modify_other_keys = modify_other_keys_mode.is_some();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("PANIC: {info}");
        if panic_resets_modify_other_keys {
            let _ = std::io::Write::write_all(&mut io::stdout(), b"\x1b[>4;0m");
        }
        if crate::kitty_graphics::is_enabled() {
            let _ = crate::kitty_graphics::clear_all_host_graphics();
        }
        let _ = execute!(
            io::stdout(),
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        let _ = crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout());
        let _ = set_host_color_scheme_reports(false);
        let _ = pop_keyboard_enhancement_flags();
        ratatui::restore();
        original_hook(info);
    }));

    let config = &loaded_config.config;
    let config_diagnostic = config::config_diagnostic_summary(&loaded_config.diagnostics);
    logging::startup("app");

    // Background update check (non-blocking, best-effort)
    // Only checks for newer versions and notifies the TUI.
    // Skipped in --no-session mode (testing).

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let result = rt.block_on(async {
        let mut terminal = ratatui::init();
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        if config.ui.mouse_capture {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        set_host_color_scheme_reports(true)?;
        push_keyboard_enhancement_flags()?;

        // Some hosts do not honor Kitty keyboard enhancement pushes for
        // Shift+Enter. Enable xterm modifyOtherKeys only on hosts where we
        // know it is needed and parseable, so modified Enter stays distinct.
        if let Some(mode) = modify_other_keys_mode {
            use std::io::Write;
            std::io::stdout().write_all(mode.set_sequence())?;
            std::io::stdout().flush()?;
        }

        let mut app = app::App::new(
            config,
            true, // no_session — monolithic mode never saves/restores sessions
            config_diagnostic,
            api_rx,
            event_hub,
        );
        let result = app.run(&mut terminal).await;

        // Reset modifyOtherKeys if we enabled it.
        if modify_other_keys_mode.is_some() {
            use std::io::Write;
            std::io::stdout().write_all(b"\x1b[>4;0m")?;
            std::io::stdout().flush()?;
        }

        if crate::kitty_graphics::is_enabled() {
            crate::kitty_graphics::clear_all_host_graphics()?;
        }
        pop_keyboard_enhancement_flags()?;
        execute!(
            io::stdout(),
            DisableFocusChange,
            DisableBracketedPaste,
            DisableMouseCapture
        )?;
        crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        set_host_color_scheme_reports(false)?;
        ratatui::restore();

        // Drop app (and all workspaces/panes) before runtime shuts down
        drop(app);

        result
    });

    // Shut down runtime immediately — kills lingering PTY reader/writer tasks
    rt.shutdown_timeout(std::time::Duration::from_millis(100));

    logging::shutdown("app");
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
        let config: config::Config =
            toml::from_str("[experimental]\nallow_nested = true\n").unwrap();
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
