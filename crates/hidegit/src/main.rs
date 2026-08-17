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

/// How often collected window geometry is written out.
///
/// Long enough that dragging a window edge costs one write rather than one per
/// frame; short enough that the most anyone loses to a kill is a few seconds of
/// resizing. The recents list does not wait for this — it is written as it
/// changes, because losing which repositories you had open is the part that
/// would actually be noticed.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// The application, plus the state only the binary is responsible for.
struct Shell {
    ui: Hidegit,
    paths: Option<Paths>,
    config: Config,
    geometry: Geometry,
    /// Window geometry has moved since it was last written.
    ///
    /// Resize events arrive per frame while a window is being dragged, so they
    /// are collected here and flushed on a timer rather than writing a file for
    /// each one. The recents list is not batched: it changes at most once per
    /// repository opened, and it is the half worth not losing.
    unsaved_geometry: bool,
}

#[derive(Debug, Clone)]
enum ShellMessage {
    Ui(Message),
    Resized(Size),
    Moved(iced::Point),
    /// The periodic write of anything that has been collected since the last.
    Flush,
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
    // Off disk once, here, rather than every time the panel is opened: a theme
    // is a file somebody wrote, not something that changes while hideGit runs.
    let custom = match &paths {
        Some(paths) => hidegit_ui::theme::Theme::load_dir(&paths.themes),
        None => hidegit_ui::theme::Custom::default(),
    };

    let (mut ui, task) = Hidegit::new(
        initial,
        recents,
        config.alerts.clone(),
        &config.theme.name,
        custom,
    );

    // Known now rather than at the first toggle: with no config directory,
    // nothing persists for the whole session, and the panel should say that the
    // moment it is opened instead of claiming to have saved and then losing it.
    if paths.is_none() {
        ui.app.settings_error = Some(config::SaveError::NoConfigDirectory.to_string());
    }

    // Set here rather than passed through `Hidegit::new`: the interface only
    // shows this one, and the shell is what acts on it.
    ui.app.remember_geometry = config.window.remember_geometry;

    (
        Shell {
            ui,
            unsaved_geometry: false,
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
            let touched_settings = matches!(
                message,
                Message::ThemeChosen(_)
                    | Message::AlertToggled(_)
                    | Message::QuietHoursToggled
                    | Message::QuietHourChosen(..)
                    | Message::RepositoryMuteToggled(_)
                    | Message::RememberGeometryToggled
            );
            // Opening a repository is what changes the recents list, and it is
            // the only thing that does. Written at once rather than at exit,
            // because there are ordinary ways to quit that never reach an exit
            // handler — Cmd+Q on macOS closes the window without ever emitting
            // `CloseRequested`, and a kill or a panic reaches nothing at all.
            let touched_recents = matches!(message, Message::RepositoryOpened(_));
            let task = shell.ui.update(message).map(ShellMessage::Ui);
            if touched_settings {
                shell.config.theme.name = shell.ui.app.theme.name.clone();
                shell.config.alerts = shell.ui.app.alerts.clone();
                shell.config.window.remember_geometry = shell.ui.app.remember_geometry;

                // The outcome goes back to the panel, which otherwise says the
                // change was saved whatever happened to the file.
                let outcome = match &shell.paths {
                    Some(paths) => config::save_settings(
                        &paths.config,
                        &config::Settings {
                            theme: shell.config.theme.name.clone(),
                            alerts: shell.config.alerts.clone(),
                            remember_geometry: shell.config.window.remember_geometry,
                        },
                    ),
                    None => Err(config::SaveError::NoConfigDirectory),
                };

                shell.ui.app.settings_error = match outcome {
                    Ok(()) => None,
                    Err(error) => {
                        tracing::warn!(%error, "the settings change was not written");
                        Some(error.to_string())
                    }
                };
            }

            if touched_recents {
                persist(shell);
            }
            task
        }

        ShellMessage::Resized(size) => {
            shell.geometry.width = size.width;
            shell.geometry.height = size.height;
            shell.unsaved_geometry = true;
            Task::none()
        }

        ShellMessage::Moved(position) => {
            shell.geometry.x = Some(position.x);
            shell.geometry.y = Some(position.y);
            shell.unsaved_geometry = true;
            Task::none()
        }

        ShellMessage::Flush => {
            if shell.unsaved_geometry {
                persist(shell);
            }
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
///
/// Called as things change rather than only on the way out. Quitting does not
/// reliably run anything: on macOS, Cmd+Q closes the window without emitting
/// `CloseRequested` at all, so an exit-only write loses the session for the
/// most ordinary quit there is.
fn persist(shell: &mut Shell) {
    // Cleared first, and unconditionally: with nowhere to write, there is
    // nothing pending either, and leaving the flag set would have the timer
    // call this every five seconds for the life of the session.
    shell.unsaved_geometry = false;

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
        // There is no `move_events()` to match `resize_events()`, so the moves
        // are filtered out of the full window stream. Without this the window
        // remembered its size and never its position: `x` and `y` were read at
        // startup and written back unchanged, so a window dragged to a second
        // monitor reopened where it was two sessions ago.
        window::events().filter_map(|(_, event)| match event {
            window::Event::Moved(position) => Some(ShellMessage::Moved(position)),
            _ => None,
        }),
        // The only timer in the application, and it exists because resize
        // events arrive per frame: collecting them and writing once is the
        // difference between one file write and one per frame of a drag.
        iced::time::every(FLUSH_INTERVAL).map(|_| ShellMessage::Flush),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell with nowhere to write, which is all these need: what is under
    /// test is which events reach the geometry, not the file behind it.
    fn shell() -> Shell {
        Shell {
            ui: Hidegit::default(),
            paths: None,
            config: Config::default(),
            geometry: Geometry::default(),
            unsaved_geometry: false,
        }
    }

    #[test]
    fn moving_the_window_records_where_it_went() {
        // Nothing wrote `x`/`y` before this: the values saved on exit were
        // whatever had been loaded at startup, echoed back. So "remember
        // geometry" remembered the size and reopened at the position from
        // whenever it was last set by hand — or centred, forever.
        let mut shell = shell();
        assert_eq!(shell.geometry.x, None, "a fresh geometry has no position");

        let _ = update(
            &mut shell,
            ShellMessage::Moved(iced::Point::new(120.0, 80.0)),
        );

        assert_eq!(shell.geometry.x, Some(120.0));
        assert_eq!(shell.geometry.y, Some(80.0));
        assert!(shell.unsaved_geometry, "and it is owed a write");
    }

    #[test]
    fn resizing_records_the_size_without_touching_the_position() {
        let mut shell = shell();
        let _ = update(
            &mut shell,
            ShellMessage::Moved(iced::Point::new(10.0, 20.0)),
        );

        let _ = update(&mut shell, ShellMessage::Resized(Size::new(1000.0, 700.0)));

        assert_eq!(shell.geometry.width, 1000.0);
        assert_eq!(shell.geometry.height, 700.0);
        assert_eq!(shell.geometry.x, Some(10.0), "a resize is not a move");
        assert_eq!(shell.geometry.y, Some(20.0));
    }

    #[test]
    fn a_flush_with_nowhere_to_write_still_stops_asking() {
        // Otherwise the timer would call `persist` every five seconds for the
        // life of a session that has no config directory.
        let mut shell = shell();
        let _ = update(&mut shell, ShellMessage::Moved(iced::Point::new(1.0, 2.0)));
        assert!(shell.unsaved_geometry);

        let _ = update(&mut shell, ShellMessage::Flush);

        assert!(!shell.unsaved_geometry);
    }

    #[test]
    fn turning_the_geometry_switch_off_stops_the_window_being_recorded() {
        // The point of the setting. Everything else about it is plumbing: the
        // switch is only worth having if `persist` writes the default rather
        // than wherever the window happens to be.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell();
        shell.paths = Some(config::Paths {
            config: dir.path().join("config.toml"),
            state: dir.path().join("state.toml"),
            themes: dir.path().join("themes"),
        });

        let _ = update(
            &mut shell,
            ShellMessage::Moved(iced::Point::new(300.0, 200.0)),
        );
        let _ = update(&mut shell, ShellMessage::Flush);
        let recorded: State = config::load(&dir.path().join("state.toml"));
        assert_eq!(recorded.window.x, Some(300.0), "on, it is remembered");

        let _ = update(
            &mut shell,
            ShellMessage::Ui(Message::RememberGeometryToggled),
        );
        let _ = update(
            &mut shell,
            ShellMessage::Moved(iced::Point::new(400.0, 250.0)),
        );
        let _ = update(&mut shell, ShellMessage::Flush);

        let recorded: State = config::load(&dir.path().join("state.toml"));
        assert_eq!(
            recorded.window.x,
            Geometry::default().x,
            "off, the position is not written"
        );
    }

    #[test]
    fn the_geometry_switch_reaches_the_settings_file() {
        // The interface holds it, the shell writes it: a toggle that flipped on
        // screen and never reached `config.toml` would be back on at restart.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell();
        shell.paths = Some(config::Paths {
            config: dir.path().join("config.toml"),
            state: dir.path().join("state.toml"),
            themes: dir.path().join("themes"),
        });

        let _ = update(
            &mut shell,
            ShellMessage::Ui(Message::RememberGeometryToggled),
        );

        assert_eq!(shell.ui.app.settings_error, None, "it was written");
        let reloaded: Config = config::load(&dir.path().join("config.toml"));
        assert!(!reloaded.window.remember_geometry);
    }

    #[test]
    fn a_position_survives_the_file() {
        // The other half: recording it is useless if it does not round-trip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let state = State {
            window: Geometry {
                width: 1200.0,
                height: 800.0,
                x: Some(64.0),
                y: Some(32.0),
            },
            ..State::default()
        };
        config::save(&path, &state);

        let loaded: State = config::load(&path);
        assert_eq!(loaded.window.x, Some(64.0));
        assert_eq!(loaded.window.y, Some(32.0));
        assert_eq!(loaded.window.sanitised().x, Some(64.0));
    }
}
