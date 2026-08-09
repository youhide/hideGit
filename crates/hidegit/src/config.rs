//! Configuration and persisted state.
//!
//! Human-editable TOML on purpose, in the platform's conventional directories.
//! There is no hosted sync service: no server to run, and no question about
//! what hideGit does with your data, because it does not have it.
//!
//! **Every value has a working default.** A missing or partially corrupt file
//! produces defaults plus a warning, never a failure to start — a client that
//! refuses to open because of a stray character in a settings file is worse
//! than one that ignores the character.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Where each file lives on this platform.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Settings the user edits.
    pub config: PathBuf,
    /// Things hideGit records for itself.
    pub state: PathBuf,
}

impl Paths {
    pub fn discover() -> Option<Self> {
        let dirs = ProjectDirs::from("com", "youhide", "hidegit")?;

        Some(Self {
            config: dirs.config_dir().join("config.toml"),
            state: dirs.data_dir().join("state.toml"),
        })
    }
}

/// Settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: ThemeConfig,
    pub window: WindowConfig,
    /// Which pull request alerts to send, and when not to.
    ///
    /// Defined in `hidegit-forge` rather than here, so there is one definition
    /// rather than a config copy and a UI copy that drift apart.
    pub alerts: hidegit_forge::AlertPrefs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Name of the theme to use. Custom themes are TOML files in the config
    /// directory (M6); an unknown name falls back to the default.
    pub name: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "hidegit-dark".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    /// Reopen at the size and position the window was last closed at.
    pub remember_geometry: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            remember_geometry: true,
        }
    }
}

/// What hideGit records between runs.
///
/// Repositories are a list of tables rather than a list of strings so a
/// workspace concept — the multi-repository question deferred to M6 — can add
/// fields here without invalidating everyone's file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct State {
    pub window: Geometry,
    #[serde(rename = "recent")]
    pub recents: Vec<RecentRepository>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Geometry {
    pub width: f32,
    pub height: f32,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            width: 1440.0,
            height: 900.0,
            x: None,
            y: None,
        }
    }
}

impl Geometry {
    /// Clamps to something a window manager will actually honour.
    ///
    /// A geometry file written on a monitor that is no longer attached must
    /// not produce a window three pixels wide or one placed off-screen.
    pub fn sanitised(&self) -> Self {
        Self {
            width: self.width.clamp(640.0, 16_384.0),
            height: self.height.clamp(400.0, 16_384.0),
            x: self.x.filter(|v| v.is_finite() && *v > -10_000.0),
            y: self.y.filter(|v| v.is_finite() && *v > -10_000.0),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecentRepository {
    pub path: PathBuf,
}

impl Default for RecentRepository {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
        }
    }
}

/// Reads a TOML file, falling back to defaults with a warning.
pub fn load<T: Default + serde::de::DeserializeOwned>(path: &Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "no file yet; using defaults");
            return T::default();
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "could not read; using defaults");
            return T::default();
        }
    };

    match toml::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "file is not valid; using defaults. It will be rewritten on exit."
            );
            T::default()
        }
    }
}

/// Writes a TOML file, creating its directory.
///
/// Failure is logged, never fatal: losing a window position is not worth
/// refusing to quit over.
pub fn save<T: Serialize>(path: &Path, value: &T) {
    let Ok(text) = toml::to_string_pretty(value) else {
        tracing::warn!(path = %path.display(), "could not serialise; not writing");
        return;
    };

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(path = %parent.display(), error = %e, "could not create directory");
        return;
    }

    if let Err(e) = std::fs::write(path, text) {
        tracing::warn!(path = %path.display(), error = %e, "could not write");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_produces_defaults_rather_than_a_failure() {
        let config: Config = load(Path::new("/definitely/not/here/config.toml"));
        assert_eq!(config.theme.name, "hidegit-dark");
        assert!(config.window.remember_geometry);
    }

    #[test]
    fn a_corrupt_file_produces_defaults_rather_than_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not [ valid toml").unwrap();

        let config: Config = load(&path);
        assert_eq!(config.theme.name, "hidegit-dark");
    }

    #[test]
    fn alert_preferences_default_to_the_table_in_the_spec() {
        let config: Config = load(Path::new("/definitely/not/here/config.toml"));

        assert!(config.alerts.enabled);
        assert!(config.alerts.events.checks_failed);
        assert!(
            !config.alerts.events.checks_passed,
            "the one event that fires when nothing needs doing"
        );
    }

    #[test]
    fn one_alert_setting_can_be_changed_without_restating_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[alerts.quiet_hours]\nenabled = true\nfrom = 21\n").unwrap();

        let config: Config = load(&path);
        assert!(config.alerts.quiet_hours.enabled);
        assert_eq!(config.alerts.quiet_hours.from, 21);
        assert_eq!(config.alerts.quiet_hours.to, 8, "the default end");
        assert!(
            config.alerts.events.checks_failed,
            "and the rest are untouched"
        );
    }

    #[test]
    fn a_partially_valid_file_keeps_what_it_can() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[window]\nremember_geometry = false\n").unwrap();

        let config: Config = load(&path);
        assert!(!config.window.remember_geometry);
        assert_eq!(
            config.theme.name, "hidegit-dark",
            "an absent section falls back to its default"
        );
    }

    #[test]
    fn state_round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let state = State {
            window: Geometry {
                width: 1200.0,
                height: 800.0,
                x: Some(40.0),
                y: Some(60.0),
            },
            recents: vec![RecentRepository {
                path: PathBuf::from("/src/hideGit"),
            }],
        };
        save(&path, &state);

        let read: State = load(&path);
        assert_eq!(read.window.width, 1200.0);
        assert_eq!(read.recents.len(), 1);
        assert_eq!(read.recents[0].path, PathBuf::from("/src/hideGit"));
    }

    #[test]
    fn an_absurd_geometry_is_clamped_to_something_usable() {
        let geometry = Geometry {
            width: 3.0,
            height: -100.0,
            x: Some(-999_999.0),
            y: Some(f32::NAN),
        }
        .sanitised();

        assert_eq!(geometry.width, 640.0);
        assert_eq!(geometry.height, 400.0);
        assert_eq!(geometry.x, None, "a position off any monitor is discarded");
        assert_eq!(geometry.y, None);
    }
}
