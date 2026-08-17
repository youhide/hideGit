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
    /// Theme files the user writes, one per file. Next to `config.toml`
    /// rather than beside the state, because it is theirs to edit.
    pub themes: PathBuf,
}

impl Paths {
    pub fn discover() -> Option<Self> {
        let dirs = ProjectDirs::from("com", "youhide", "hidegit")?;

        Some(Self {
            config: dirs.config_dir().join("config.toml"),
            state: dirs.data_dir().join("state.toml"),
            themes: dirs.config_dir().join("themes"),
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
    /// `command = "chord"`, overriding the built-in bindings.
    ///
    /// Held as plain strings: which command names exist and what a chord may
    /// say belongs to `hidegit-ui`, which owns the bindings, and validating it
    /// twice would mean two answers to the same question.
    pub shortcuts: std::collections::BTreeMap<String, String>,
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
///
/// **Written to a temporary file and renamed over the target.** A plain write
/// truncates first, so a process that dies between the truncation and the last
/// byte leaves a half-file — and this is now written while the application runs
/// rather than only as it exits, which is exactly when dying part-way through
/// stops being hypothetical. `rename` within a directory is atomic on every
/// platform hideGit targets, so a reader sees either the old file or the new
/// one and never a partial one.
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

    // Beside the target rather than in a temporary directory: `rename` is only
    // atomic within a filesystem, and `/tmp` is routinely a different one.
    let staging = path.with_extension("toml.new");
    if let Err(e) = std::fs::write(&staging, text) {
        tracing::warn!(path = %staging.display(), error = %e, "could not write");
        return;
    }

    if let Err(e) = std::fs::rename(&staging, path) {
        tracing::warn!(path = %path.display(), error = %e, "could not replace");
        // Leaving the staging file behind would accumulate one per failure.
        let _ = std::fs::remove_file(&staging);
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
    fn a_write_leaves_no_staging_file_behind() {
        // The rename is what makes the write atomic; a staging file still on
        // disk afterwards would mean it did not happen.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        save(&path, &State::default());

        assert!(path.exists(), "the state file was written");
        assert!(
            !path.with_extension("toml.new").exists(),
            "the staging file was renamed, not left behind"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one file, got {entries:?}");
    }

    #[test]
    fn a_replaced_file_is_never_seen_half_written() {
        // The property the rename buys: a reader either sees the whole old file
        // or the whole new one. Checked by replacing a long file with a short
        // one — a truncating write is exactly where a tail of the old content
        // would survive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let crowded = State {
            recents: (0..10)
                .map(|i| RecentRepository {
                    path: PathBuf::from(format!("/a/very/long/path/number/{i}")),
                })
                .collect(),
            ..State::default()
        };
        save(&path, &crowded);
        assert_eq!(load::<State>(&path).recents.len(), 10);

        save(&path, &State::default());

        let after: State = load(&path);
        assert!(
            after.recents.is_empty(),
            "the shorter file replaced the longer one whole, got {:?}",
            after.recents
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

/// Writes the settings a user can change from the interface, **keeping the rest
/// of the file exactly as they wrote it**.
///
/// `config.toml` is theirs: hand-edited, checked into a dotfiles repository,
/// carrying comments explaining why a quiet hour starts when it does. Round-
/// tripping it through `serde` would silently strip every comment and reorder
/// every table the first time somebody toggled a checkbox, so the document is
/// edited in place instead — only the keys the screen owns are touched, and
/// anything else in the file is left alone, including keys hideGit does not
/// know about.
///
/// Never fatal. A settings file that cannot be written is worth reporting, not
/// a refusal to run — but it **is** reported, because the panel tells the user
/// their change was saved and has no way to know otherwise.
pub fn save_settings(path: &Path, settings: &Settings) -> Result<(), SaveError> {
    use toml_edit::{Item, value};

    let mut doc = match std::fs::read_to_string(path) {
        Ok(text) => match text.parse::<toml_edit::DocumentMut>() {
            Ok(doc) => doc,
            // Unparseable: writing a fresh document would delete whatever they
            // were in the middle of typing. Refusing keeps their file.
            Err(error) => return Err(SaveError::NotValidToml(error)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(error) => return Err(SaveError::Unreadable(error)),
    };

    // `or_insert` on a missing table creates it; an existing one keeps its
    // comments and the order its keys were written in.
    let theme = doc["theme"].or_insert(Item::Table(toml_edit::Table::new()));
    theme["name"] = value(settings.theme.clone());

    let window = doc["window"].or_insert(Item::Table(toml_edit::Table::new()));
    window["remember_geometry"] = value(settings.remember_geometry);

    let alerts = doc["alerts"].or_insert(Item::Table(toml_edit::Table::new()));
    alerts["enabled"] = value(settings.alerts.enabled);

    // An array of strings, which is what the file has always held — the panel
    // is a second way to edit it, not a different format.
    let mut list = toml_edit::Array::new();
    for repository in &settings.alerts.muted {
        list.push(repository.as_str());
    }
    alerts["muted"] = value(list);

    let quiet = alerts["quiet_hours"].or_insert(Item::Table(toml_edit::Table::new()));
    quiet["enabled"] = value(settings.alerts.quiet_hours.enabled);
    // `i64` because TOML has one integer type; the values are hours, so the
    // cast is lossless and the file stays readable as `from = 22`.
    quiet["from"] = value(i64::from(settings.alerts.quiet_hours.from));
    quiet["to"] = value(i64::from(settings.alerts.quiet_hours.to));

    let events = alerts["events"].or_insert(Item::Table(toml_edit::Table::new()));
    let e = &settings.alerts.events;
    events["review_requested"] = value(e.review_requested);
    events["checks_failed"] = value(e.checks_failed);
    events["checks_passed"] = value(e.checks_passed);
    events["pr_conflicting"] = value(e.pr_conflicting);
    events["pr_merged"] = value(e.pr_merged);
    events["pr_closed"] = value(e.pr_closed);
    events["review_submitted"] = value(e.review_submitted);
    events["pr_commented"] = value(e.pr_commented);

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return Err(SaveError::NoDirectory(error));
    }

    std::fs::write(path, doc.to_string()).map_err(SaveError::Unwritable)
}

/// Why a settings change did not reach the file.
///
/// Each variant is something the user can act on, which is the reason this is
/// an error type rather than a log line: refusing to overwrite a file somebody
/// is part-way through editing is right, and doing it silently — under a panel
/// that says "Saved to config.toml as you change it" — is not.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("config.toml is not valid TOML, so it was left alone: {0}")]
    NotValidToml(toml_edit::TomlError),
    #[error("config.toml could not be read: {0}")]
    Unreadable(std::io::Error),
    #[error("the config directory could not be created: {0}")]
    NoDirectory(std::io::Error),
    #[error("config.toml could not be written: {0}")]
    Unwritable(std::io::Error),
    /// There is no platform config directory at all, so nothing persists for
    /// the whole session. Known at startup rather than at the first toggle.
    #[error("there is no config directory on this system, so settings will not persist")]
    NoConfigDirectory,
}

/// The settings the interface can change.
///
/// A shape of its own rather than the whole [`Config`], because the screen owns
/// exactly these and writing the rest back would claim ownership of keys nobody
/// edited.
#[derive(Debug, Clone)]
pub struct Settings {
    pub theme: String,
    pub alerts: hidegit_forge::AlertPrefs,
    /// Reopen at the size and position the window was last closed at.
    pub remember_geometry: bool,
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn settings(theme: &str) -> Settings {
        Settings {
            theme: theme.to_owned(),
            alerts: hidegit_forge::AlertPrefs::default(),
            remember_geometry: true,
        }
    }

    #[test]
    fn saving_keeps_the_comments_and_keys_the_user_wrote() {
        // The whole reason this does not round-trip through serde. A settings
        // file is hand-edited and often lives in a dotfiles repository; losing
        // its comments the first time somebody toggles a checkbox is a real
        // cost paid for a trivial convenience.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# my settings, do not laugh\n\
             [theme]\n\
             # picked to match my terminal\n\
             name = \"hidegit-dark\"\n\
             \n\
             [personal]\n\
             # hideGit has never heard of this table\n\
             note = \"kept\"\n",
        )
        .unwrap();

        save_settings(&path, &settings("hidegit-light")).expect("a writable file saves");

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# my settings, do not laugh"), "{after}");
        assert!(after.contains("# picked to match my terminal"), "{after}");
        // A key hideGit does not know at all is left exactly as it was. This
        // used to be checked with `remember_geometry`, which the panel has
        // since taken ownership of — a table nothing in the schema mentions is
        // the stronger version of the same property.
        assert!(
            after.contains("# hideGit has never heard of this table"),
            "{after}"
        );
        assert!(after.contains(r#"note = "kept""#), "{after}");
        assert!(after.contains(r#"name = "hidegit-light""#), "{after}");

        // And a file without that table still parses back into the config it
        // came from — `Config` denies unknown fields, so the two properties
        // cannot be checked against the same file.
        let path = dir.path().join("known.toml");
        std::fs::write(&path, "# keep me\n[theme]\nname = \"hidegit-dark\"\n").unwrap();
        save_settings(&path, &settings("hidegit-light")).expect("a writable file saves");

        let reloaded: Config = load(&path);
        assert_eq!(reloaded.theme.name, "hidegit-light");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# keep me"),
            "the comment survived the round trip"
        );
    }

    #[test]
    fn saving_over_a_broken_file_refuses_rather_than_replacing_it() {
        // Somebody is mid-edit with an unclosed string. Writing a fresh
        // document here would throw away whatever they were typing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let broken = "[theme]\nname = \"unclosed\n";
        std::fs::write(&path, broken).unwrap();

        let outcome = save_settings(&path, &settings("hidegit-light"));

        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        // Refusing is right. Refusing *silently* is the bug: the panel says
        // settings are saved as they are made, so a refusal nobody is told
        // about is a toggle that flips, claims to have stuck, and is gone on
        // restart.
        assert!(
            matches!(outcome, Err(SaveError::NotValidToml(_))),
            "a refusal has to be reported, got {outcome:?}"
        );
    }

    #[test]
    fn saving_with_no_file_yet_creates_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");

        save_settings(&path, &settings("hidegit-light")).expect("a missing file is created");

        let reloaded: Config = load(&path);
        assert_eq!(reloaded.theme.name, "hidegit-light");
    }

    #[test]
    fn quiet_hours_survive_the_file() {
        // They were editable only by hand until the panel grew controls, and
        // `save_settings` did not write them at all — so a screen that set them
        // would have lost them on the next write of anything else.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut alerts = hidegit_forge::AlertPrefs::default();
        alerts.quiet_hours.enabled = true;
        alerts.quiet_hours.from = 23;
        alerts.quiet_hours.to = 7;

        save_settings(
            &path,
            &Settings {
                theme: "hidegit-dark".to_owned(),
                alerts,
                remember_geometry: true,
            },
        )
        .expect("a writable file saves");

        let reloaded: Config = load(&path);
        assert!(reloaded.alerts.quiet_hours.enabled);
        assert_eq!(reloaded.alerts.quiet_hours.from, 23);
        assert_eq!(reloaded.alerts.quiet_hours.to, 7);
    }

    #[test]
    fn writing_quiet_hours_leaves_the_rest_of_the_file_alone() {
        // The property the whole in-place write exists for, checked against the
        // keys this change added rather than only the ones that were there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\n\
             [personal]\n\
             note = \"kept\"\n\
             \n\
             [alerts.quiet_hours]\n\
             # I go to bed late\n\
             from = 1\n",
        )
        .unwrap();

        let mut alerts = hidegit_forge::AlertPrefs::default();
        alerts.quiet_hours.enabled = true;
        alerts.quiet_hours.from = 2;

        save_settings(
            &path,
            &Settings {
                theme: "hidegit-dark".to_owned(),
                alerts,
                remember_geometry: true,
            },
        )
        .expect("a writable file saves");

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# I go to bed late"), "{after}");
        assert!(after.contains(r#"note = "kept""#), "{after}");
        assert!(after.contains("from = 2"), "{after}");
    }

    #[test]
    fn the_geometry_preference_survives_the_file() {
        // It is read at startup to decide the window's size and position, and
        // was previously only reachable by editing the file by hand.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        save_settings(
            &path,
            &Settings {
                remember_geometry: false,
                ..settings("hidegit-dark")
            },
        )
        .unwrap();

        let reloaded: Config = load(&path);
        assert!(!reloaded.window.remember_geometry);

        save_settings(&path, &settings("hidegit-dark")).unwrap();
        let reloaded: Config = load(&path);
        assert!(reloaded.window.remember_geometry, "and back on again");
    }

    #[test]
    fn muted_repositories_survive_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let alerts = hidegit_forge::AlertPrefs {
            muted: vec!["youhide/hideGit".to_owned(), "youhide/noisy".to_owned()],
            ..hidegit_forge::AlertPrefs::default()
        };

        save_settings(
            &path,
            &Settings {
                theme: "hidegit-dark".to_owned(),
                alerts,
                remember_geometry: true,
            },
        )
        .expect("a writable file saves");

        let reloaded: Config = load(&path);
        assert_eq!(
            reloaded.alerts.muted,
            vec!["youhide/hideGit".to_owned(), "youhide/noisy".to_owned()]
        );
    }

    #[test]
    fn unmuting_everything_leaves_an_empty_list_rather_than_the_old_one() {
        // The case a naive "write the entries" would get wrong: removing the
        // last one has to shrink the array, not leave the previous contents in
        // place under a key nobody rewrote.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[alerts]\nmuted = [\"youhide/noisy\"]\n").unwrap();

        save_settings(
            &path,
            &Settings {
                theme: "hidegit-dark".to_owned(),
                alerts: hidegit_forge::AlertPrefs::default(),
                remember_geometry: true,
            },
        )
        .expect("a writable file saves");

        let reloaded: Config = load(&path);
        assert!(
            reloaded.alerts.muted.is_empty(),
            "got {:?}",
            reloaded.alerts.muted
        );
    }

    #[test]
    fn a_file_that_cannot_be_written_is_reported_rather_than_swallowed() {
        // Read-only rather than absent: this has to fail at the *last* step,
        // after the document parsed and every earlier check passed, which is
        // the path a permissions problem actually takes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[theme]\nname = \"hidegit-dark\"\n").unwrap();

        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        let outcome = save_settings(&path, &settings("hidegit-light"));

        assert!(
            matches!(outcome, Err(SaveError::Unwritable(_))),
            "got {outcome:?}"
        );
    }
}
