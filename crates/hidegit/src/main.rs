//! hideGit: argument parsing, configuration, logging and window bootstrap.
//!
//! Everything domain-shaped lives in `hidegit-core`; everything visual lives in
//! `hidegit-ui`. This crate is the wiring between them and the operating
//! system.

mod config;

use std::path::PathBuf;

use hidegit_core::{GitError, MINIMUM_GIT_VERSION, git_preflight};
use hidegit_ui::{Hidegit, Message};
use iced::{Size, Subscription, Task, window};

use crate::config::{Config, Geometry, Paths, RecentRepository, State};

const HELP: &str = "\
hideGit — a desktop Git client with pull request alerts

USAGE:
    hidegit [OPTIONS] [PATH]...

ARGUMENTS:
    PATH    A repository to open on startup. Give several for a tab each

OPTIONS:
    -h, --help       Print this message
    -V, --version    Print the version
";

/// The window icon, embedded so the binary stays self-contained.
///
/// Regenerated from `assets/icon.png` by `cargo run -p xtask -- icons`.
const WINDOW_ICON: &[u8] = include_bytes!("../../../assets/generated/window-icon-256.png");

/// The Wayland `app_id` and X11 `WM_CLASS`.
///
/// Wayland compositors find an application's icon by matching this against an
/// installed `.desktop` file, so it has to stay identical to the filename of
/// `packaging/linux/com.youhide.hidegit.desktop`. It also matches the
/// `ProjectDirs` qualifier in `config.rs` and the macOS bundle identifier.
///
/// Linux-only because `PlatformSpecific` is a different struct on every
/// backend, and only the Linux one carries an application id.
#[cfg(target_os = "linux")]
const APPLICATION_ID: &str = "com.youhide.hidegit";

/// The application, plus the state only the binary is responsible for.
struct Shell {
    ui: Hidegit,
    paths: Option<Paths>,
    config: Config,
    geometry: Geometry,
}

#[derive(Debug, Clone)]
enum ShellMessage {
    Ui(Message),
    Resized(Size),
    CloseRequested,
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("HIDEGIT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("hidegit=info,warn")),
        )
        .init();

    let initial = match parse_arguments() {
        Arguments::Run(path) => path,
        Arguments::Exit(message) => {
            println!("{message}");
            return Ok(());
        }
    };

    // Checked once, here, so a missing `git` is an actionable message rather
    // than a mystery failure the first time someone pushes.
    if let Err(error) = git_preflight() {
        report_missing_git(&error);
        return Ok(());
    }

    let paths = Paths::discover();
    if paths.is_none() {
        tracing::warn!("no platform config directory; settings will not persist this session");
    }

    let config: Config = paths
        .as_ref()
        .map(|p| config::load(&p.config))
        .unwrap_or_default();
    let state: State = paths
        .as_ref()
        .map(|p| config::load(&p.state))
        .unwrap_or_default();

    let geometry = if config.window.remember_geometry {
        state.window.sanitised()
    } else {
        Geometry::default()
    };
    let recents: Vec<PathBuf> = state.recents.into_iter().map(|r| r.path).collect();

    // `mut` is only used on Linux; see the cfg block below.
    #[allow(unused_mut)]
    let mut window_settings = window::Settings {
        size: Size::new(geometry.width, geometry.height),
        position: match (geometry.x, geometry.y) {
            (Some(x), Some(y)) => window::Position::Specific(iced::Point::new(x, y)),
            _ => window::Position::Centered,
        },
        min_size: Some(Size::new(900.0, 560.0)),
        icon: window_icon(),
        ..window::Settings::default()
    };

    #[cfg(target_os = "linux")]
    {
        window_settings.platform_specific.application_id = APPLICATION_ID.to_owned();
    }

    iced::application(
        move || {
            boot(
                initial.clone(),
                recents.clone(),
                paths.clone(),
                config.clone(),
                geometry.clone(),
            )
        },
        update,
        view,
    )
    .title(|shell: &Shell| shell.ui.title())
    .theme(|shell: &Shell| shell.ui.theme())
    .subscription(subscription)
    .window(window_settings)
    // Closing is intercepted so the window geometry is written first.
    .exit_on_close_request(false)
    .run()
}

/// Decodes the embedded icon into something the window system will take.
///
/// Only Windows and X11 act on this. macOS has no per-window icon at all — it
/// reads the Dock icon from the `.app` bundle — and Wayland matches the
/// window's `app_id` against an installed `.desktop` file instead. Both of
/// those live in `packaging/`.
///
/// Never fatal: an icon is not worth refusing to start over.
fn window_icon() -> Option<window::Icon> {
    fn decode() -> Result<window::Icon, Box<dyn std::error::Error>> {
        // png 0.18 wants `BufRead + Seek`, which a bare `&[u8]` is not.
        let mut reader = png::Decoder::new(std::io::Cursor::new(WINDOW_ICON)).read_info()?;
        let capacity = reader
            .output_buffer_size()
            .ok_or("icon is too large to decode")?;

        let mut rgba = vec![0; capacity];
        let info = reader.next_frame(&mut rgba)?;

        if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
            return Err(format!(
                "expected 8-bit RGBA, got {:?} at {:?}",
                info.color_type, info.bit_depth
            )
            .into());
        }

        rgba.truncate(info.buffer_size());
        Ok(window::icon::from_rgba(rgba, info.width, info.height)?)
    }

    match decode() {
        Ok(icon) => Some(icon),
        Err(error) => {
            tracing::warn!("could not decode the window icon: {error}");
            None
        }
    }
}

fn boot(
    initial: Vec<PathBuf>,
    recents: Vec<PathBuf>,
    paths: Option<Paths>,
    config: Config,
    geometry: Geometry,
) -> (Shell, Task<ShellMessage>) {
    let (ui, task) = Hidegit::new(initial, recents, config.alerts.clone(), &config.theme.name);

    (
        Shell {
            ui,
            paths,
            config,
            geometry,
        },
        task.map(ShellMessage::Ui),
    )
}

fn update(shell: &mut Shell, message: ShellMessage) -> Task<ShellMessage> {
    match message {
        ShellMessage::Ui(message) => {
            // Settings apply as they are made, so they are written as they are
            // made too. Checked before the message moves into `update`, and the
            // values are read back out afterwards — the interface owns them
            // while it runs, the file owns them between runs.
            let touched_settings =
                matches!(message, Message::ThemeChosen(_) | Message::AlertToggled(_));
            let task = shell.ui.update(message).map(ShellMessage::Ui);
            if touched_settings {
                shell.config.theme.name = shell.ui.app.theme.name.clone();
                shell.config.alerts = shell.ui.app.alerts.clone();
                if let Some(paths) = &shell.paths {
                    config::save_settings(
                        &paths.config,
                        &config::Settings {
                            theme: shell.config.theme.name.clone(),
                            alerts: shell.config.alerts.clone(),
                        },
                    );
                }
            }
            task
        }

        ShellMessage::Resized(size) => {
            shell.geometry.width = size.width;
            shell.geometry.height = size.height;
            Task::none()
        }

        ShellMessage::CloseRequested => {
            persist(shell);
            iced::exit()
        }
    }
}

/// Writes what should survive a restart. Never fatal: losing a window position
/// is not worth refusing to quit over.
fn persist(shell: &Shell) {
    let Some(paths) = &shell.paths else {
        return;
    };

    let state = State {
        window: if shell.config.window.remember_geometry {
            shell.geometry.sanitised()
        } else {
            Geometry::default()
        },
        recents: shell
            .ui
            .app
            .recents
            .iter()
            .map(|path| RecentRepository { path: path.clone() })
            .collect(),
    };

    config::save(&paths.state, &state);
}

fn view(shell: &Shell) -> iced::Element<'_, ShellMessage> {
    shell.ui.view().map(ShellMessage::Ui)
}

fn subscription(shell: &Shell) -> Subscription<ShellMessage> {
    Subscription::batch([
        shell.ui.subscription().map(ShellMessage::Ui),
        window::resize_events().map(|(_, size)| ShellMessage::Resized(size)),
        window::close_requests().map(|_| ShellMessage::CloseRequested),
    ])
}

enum Arguments {
    /// The repositories to open, in the order given — one tab each.
    Run(Vec<PathBuf>),
    Exit(String),
}

fn parse_arguments() -> Arguments {
    let mut paths = Vec::new();

    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-h" | "--help" => return Arguments::Exit(HELP.to_owned()),
            "-V" | "--version" => {
                return Arguments::Exit(format!("hidegit {}", env!("CARGO_PKG_VERSION")));
            }
            other if other.starts_with('-') => {
                return Arguments::Exit(format!("unknown option: {other}\n\n{HELP}"));
            }
            // Every path is kept, not just the last: with tabs, `hidegit a b`
            // opening one of them and silently dropping the other would be a
            // worse answer than either opening both or refusing.
            other => paths.push(PathBuf::from(other)),
        }
    }

    Arguments::Run(paths)
}

/// Says what is wrong and what to do about it, on the terminal and in a dialog.
///
/// A GUI launched from a desktop icon has no terminal to print to, so the
/// message has to reach the user somewhere they will see it.
fn report_missing_git(error: &GitError) {
    let (title, body) = match error {
        GitError::GitTooOld { found, .. } => (
            "hideGit needs a newer git",
            format!(
                "Found git {found}, but hideGit needs {MINIMUM_GIT_VERSION} or newer.\n\n\
                 Reads run on gitoxide, but pushing, merging and rebasing shell out to the \
                 system git, and the machine-readable formats hideGit parses are only stable \
                 from {MINIMUM_GIT_VERSION}."
            ),
        ),
        _ => (
            "hideGit needs git on your PATH",
            format!(
                "No git binary was found.\n\n\
                 hideGit reads repositories with gitoxide, but pushing, merging and rebasing \
                 shell out to the system git — which also carries your credential helpers, \
                 hooks and configuration.\n\n\
                 Install git {MINIMUM_GIT_VERSION} or newer and start hideGit again."
            ),
        ),
    };

    tracing::error!("{title}: {body}");
    eprintln!("{title}\n\n{body}");

    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(&body)
        .show();
}
