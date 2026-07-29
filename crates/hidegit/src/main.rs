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
    hidegit [OPTIONS] [PATH]

ARGUMENTS:
    PATH    A repository to open on startup

OPTIONS:
    -h, --help       Print this message
    -V, --version    Print the version
";

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

    let window_settings = window::Settings {
        size: Size::new(geometry.width, geometry.height),
        position: match (geometry.x, geometry.y) {
            (Some(x), Some(y)) => window::Position::Specific(iced::Point::new(x, y)),
            _ => window::Position::Centered,
        },
        min_size: Some(Size::new(900.0, 560.0)),
        ..window::Settings::default()
    };

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

fn boot(
    initial: Option<PathBuf>,
    recents: Vec<PathBuf>,
    paths: Option<Paths>,
    config: Config,
    geometry: Geometry,
) -> (Shell, Task<ShellMessage>) {
    let (ui, task) = Hidegit::new(initial, recents);

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
        ShellMessage::Ui(message) => shell.ui.update(message).map(ShellMessage::Ui),

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
    Run(Option<PathBuf>),
    Exit(String),
}

fn parse_arguments() -> Arguments {
    let mut path = None;

    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-h" | "--help" => return Arguments::Exit(HELP.to_owned()),
            "-V" | "--version" => {
                return Arguments::Exit(format!("hidegit {}", env!("CARGO_PKG_VERSION")));
            }
            other if other.starts_with('-') => {
                return Arguments::Exit(format!("unknown option: {other}\n\n{HELP}"));
            }
            other => path = Some(PathBuf::from(other)),
        }
    }

    Arguments::Run(path)
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
