//! User-facing text, in one place per language.
//!
//! Keys in the code, strings in a TOML catalogue. English is
//! `locales/en.toml`, compiled in — a translation is the same file with the
//! values replaced, dropped in a `locales` directory beside `config.toml` as
//! `pt-BR.toml`. Same shape as `themes/`, same place, found the same way. See
//! [ADR-0008](../../../docs/adr/0008-translations-as-toml-catalogues.md) for
//! why there is no i18n runtime behind this.
//!
//! **What is not translated**, and must not be: Git's own stderr, which reaches
//! the user verbatim on purpose ([ADR-0002](../../../docs/adr/0002-git-backend-hybrid.md)),
//! and anything that came out of a repository — branch names, paths, remote
//! URLs, commit messages. A paraphrase of a Git error is worse than the error,
//! and worse again in a second language.
//!
//! A key the translation is missing falls back to English and is reported. A
//! half-finished translation shows English where it is unfinished, never a key
//! and never an empty label.

use std::collections::BTreeMap;
use std::path::Path;

/// The English catalogue, and the only place a user-facing string is written.
///
/// Compiled in rather than read from disk: it is the fallback for every other
/// language, so an installation with no files at all still has every string.
const ENGLISH: &str = include_str!("../locales/en.toml");

/// The translations hideGit ships, by the tag their file is named for.
///
/// Compiled in for the same reason English is: a translation that only existed
/// as a file the user had to find and copy would not be a translation hideGit
/// *has*. A file of the same name in the user's own `locales` directory still
/// wins — theirs is a correction or a work in progress, and being overruled by
/// the binary is the one thing that would make writing one pointless.
const BUNDLED: &[(&str, &str)] = &[("pt-BR", include_str!("../locales/pt-BR.toml"))];

/// The language a catalogue is written in, as its file is named.
pub const DEFAULT_LOCALE: &str = "en";

/// A translation that could not be used as written, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub locale: String,
    pub reason: String,
}

/// Every string the interface can show, in one language with English behind it.
#[derive(Debug, Clone)]
pub struct Catalogue {
    /// Which language this is, as the file was named.
    pub locale: String,
    strings: BTreeMap<String, String>,
    english: BTreeMap<String, String>,
    pub problems: Vec<Problem>,
}

impl Default for Catalogue {
    fn default() -> Self {
        let english = parse(ENGLISH).expect("the built-in English catalogue parses");
        Self {
            locale: DEFAULT_LOCALE.to_owned(),
            strings: english.clone(),
            english,
            problems: Vec::new(),
        }
    }
}

impl Catalogue {
    /// The catalogue for `locale` using only what is compiled in.
    ///
    /// For an installation with nowhere to keep a config directory. Before a
    /// translation shipped this could only ever have been English, so the
    /// caller used [`Catalogue::default`] and ignored the language entirely;
    /// with one compiled in, that would show a Brazilian user English for the
    /// want of a directory they never asked for.
    pub fn for_locale(locale: &str) -> Self {
        // No guard for English: it is the source rather than a translation, so
        // it is never in `BUNDLED` and asking for it finds nothing to replace
        // the default with. `load` needs one because a `locales/en.toml` on
        // disk would otherwise overrule the built-in copy.
        let mut catalogue = Self::default();
        if let Some(strings) = bundled(locale).and_then(|text| parse(text).ok()) {
            catalogue.locale = locale.to_owned();
            catalogue.strings = strings;
        }
        catalogue
    }

    /// Loads `locale` from `dir`, falling back to English for anything it does
    /// not have — or for everything, if it has nothing usable.
    ///
    /// Never fatal. A translation that will not parse is a language that is not
    /// available, not a window that does not open.
    pub fn load(dir: &Path, locale: &str) -> Self {
        let mut catalogue = Self::default();
        if locale == DEFAULT_LOCALE {
            return catalogue;
        }

        let path = dir.join(format!("{locale}.toml"));
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => match bundled(locale) {
                Some(text) => text.to_owned(),
                // Not a problem worth reporting: asking for a language nobody
                // has written is answered by English, which is what happens.
                None => return catalogue,
            },
        };

        match parse(&text) {
            Ok(strings) => {
                catalogue.locale = locale.to_owned();

                // Counted rather than listed. A translation started yesterday
                // is missing hundreds of keys, and naming them all says less
                // than the number does.
                let missing = catalogue
                    .english
                    .keys()
                    .filter(|key| !strings.contains_key(*key))
                    .count();
                if missing > 0 {
                    catalogue.problems.push(Problem {
                        locale: locale.to_owned(),
                        reason: format!(
                            "{missing} of {} strings are not translated yet, and show in English.",
                            catalogue.english.len()
                        ),
                    });
                }

                catalogue.strings = strings;
            }
            Err(error) => catalogue.problems.push(Problem {
                locale: locale.to_owned(),
                reason: format!("{} is not valid TOML: {error}", path.display()),
            }),
        }

        catalogue
    }

    /// The string for `key`.
    ///
    /// English when the current language does not have it. A key that is in no
    /// catalogue at all is returned as itself — a wrong-looking label is easier
    /// to find and fix than a blank one, and it cannot happen without a typo in
    /// the code, since English ships complete.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings
            .get(key)
            .or_else(|| self.english.get(key))
            .map_or(key, String::as_str)
    }

    /// The singular or the plural form, by count.
    ///
    /// Two forms, which is what English and Portuguese need. A language with
    /// more needs [ADR-0008](../../../docs/adr/0008-translations-as-toml-catalogues.md)
    /// superseded, and this is where it would change.
    pub fn plural<'a>(&'a self, count: usize, one: &'a str, other: &'a str) -> &'a str {
        self.get(if count == 1 { one } else { other })
    }

    /// Every key, for the test that keeps the catalogue and the code in step.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.english.keys().map(String::as_str)
    }
}

/// Reads a catalogue into flat dotted keys.
///
/// Both shapes work, and that is deliberate. `settings.title = "Settings"` is a
/// nested table to TOML; `"settings.title" = "Settings"` is one literal key.
/// A translator copying the file will not reliably keep the quotes, and a
/// dropped quote silently changing the shape of the file is a trap rather than
/// a format. Nested tables are flattened back to dotted keys, so both spellings
/// reach `get` the same way.
fn parse(text: &str) -> Result<BTreeMap<String, String>, toml::de::Error> {
    let value: toml::Value = toml::from_str(text)?;
    let mut strings = BTreeMap::new();
    flatten(String::new(), &value, &mut strings);
    Ok(strings)
}

/// Walks a parsed document, joining table names with dots.
///
/// Anything that is not a string or a table is skipped rather than refused: a
/// stray number in a translation is one key that falls back to English, not a
/// language that will not load.
fn flatten(prefix: String, value: &toml::Value, out: &mut BTreeMap<String, String>) {
    match value {
        toml::Value::String(text) => {
            out.insert(prefix, text.clone());
        }
        toml::Value::Table(table) => {
            for (key, value) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(path, value, out);
            }
        }
        _ => {}
    }
}

/// The language to use, from the environment.
///
/// `LC_ALL`, then `LC_MESSAGES`, then `LANG` — the order the C library reads
/// them in, so hideGit agrees with the rest of the system rather than inventing
/// its own precedence. `pt_BR.UTF-8` becomes `pt-BR`, which is how the file is
/// named.
pub fn from_environment() -> String {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(name)
            && let Some(tag) = language_tag(&value)
        {
            return tag;
        }
    }

    DEFAULT_LOCALE.to_owned()
}

/// `pt_BR.UTF-8` → `pt-BR`. `None` for the values that mean "no locale".
/// The translation hideGit ships for `locale`, if it ships one.
fn bundled(locale: &str) -> Option<&'static str> {
    BUNDLED
        .iter()
        .find(|(tag, _)| *tag == locale)
        .map(|(_, text)| *text)
}

fn language_tag(value: &str) -> Option<String> {
    let bare = value.split(['.', '@']).next().unwrap_or_default();
    if bare.is_empty() || bare == "C" || bare == "POSIX" {
        return None;
    }

    Some(bare.replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn every_bundled_translation_parses_and_covers_every_english_key() {
        // A shipped translation with a hole in it would show the user a mixture
        // of two languages, and they have no file to fix. A contributor's own
        // file may be half-finished; this one may not.
        let english = parse(ENGLISH).expect("English parses");

        for (tag, text) in BUNDLED {
            let strings = parse(text).unwrap_or_else(|e| panic!("{tag} is not valid TOML: {e}"));

            let missing: Vec<_> = english
                .keys()
                .filter(|key| !strings.contains_key(*key))
                .collect();
            assert!(missing.is_empty(), "{tag} is missing {missing:?}");

            let extra: Vec<_> = strings
                .keys()
                .filter(|key| !english.contains_key(*key))
                .collect();
            assert!(
                extra.is_empty(),
                "{tag} has keys English does not, which are dead: {extra:?}"
            );
        }
    }

    #[test]
    fn a_bundled_translation_is_used_with_no_files_on_disk_at_all() {
        // The point of compiling it in. A translation that only existed as a
        // file the user had to find and copy would not be one hideGit has.
        let empty = tempfile::tempdir().unwrap();
        let catalogue = Catalogue::load(empty.path(), "pt-BR");

        assert_eq!(catalogue.locale, "pt-BR");
        assert_eq!(catalogue.get("settings.title"), "Configurações");
        assert!(
            catalogue.problems.is_empty(),
            "a complete translation reports nothing: {:?}",
            catalogue.problems
        );
    }

    #[test]
    fn a_file_on_disk_overrules_the_bundled_translation() {
        // Theirs is a correction or a work in progress. Being overruled by the
        // binary is the one thing that would make writing one pointless.
        let dir = catalogue_dir(&[("pt-BR.toml", r#""settings.title" = "Ajustes""#)]);
        let catalogue = Catalogue::load(dir.path(), "pt-BR");

        assert_eq!(catalogue.get("settings.title"), "Ajustes");
    }

    #[test]
    fn a_file_called_en_toml_does_not_overrule_the_built_in_english() {
        // English is the fallback for every other language, so it has to be the
        // complete one. A half-written `en.toml` in the config directory
        // replacing it would leave keys with nothing behind them at all.
        let dir = catalogue_dir(&[("en.toml", r#""settings.title" = "Preferences""#)]);
        let catalogue = Catalogue::load(dir.path(), "en");

        assert_eq!(catalogue.get("settings.title"), "Settings");
        assert_eq!(
            catalogue.get("settings.done"),
            "Done",
            "and nothing is lost"
        );
    }

    #[test]
    fn an_installation_with_no_config_directory_still_gets_a_shipped_translation() {
        // It has nowhere to put a `locales` directory, which is no reason to
        // show it a language it did not ask for.
        assert_eq!(
            Catalogue::for_locale("pt-BR").get("settings.title"),
            "Configurações"
        );
        assert_eq!(
            Catalogue::for_locale("fr").get("settings.title"),
            "Settings"
        );
        assert_eq!(Catalogue::for_locale("en").locale, DEFAULT_LOCALE);
    }

    #[test]
    fn a_language_hidegit_does_not_ship_still_falls_back_to_english() {
        let empty = tempfile::tempdir().unwrap();
        let catalogue = Catalogue::load(empty.path(), "fr");

        assert_eq!(catalogue.get("settings.title"), "Settings");
        assert!(
            catalogue.problems.is_empty(),
            "asking for a language nobody has written is not a problem to report"
        );
    }

    #[test]
    fn the_built_in_catalogue_parses_and_is_not_empty() {
        // It is the fallback for every other language: if it fails, every
        // string in the application does.
        let catalogue = Catalogue::default();

        assert_eq!(catalogue.locale, DEFAULT_LOCALE);
        assert!(catalogue.keys().count() > 5, "it has strings in it");
    }

    #[test]
    fn a_translation_replaces_what_it_covers_and_english_fills_the_rest() {
        let english = Catalogue::default();
        let one = english.keys().next().unwrap().to_owned();
        let two = english.keys().nth(1).unwrap().to_owned();

        let dir = catalogue_dir(&[("pt-BR.toml", &format!("\"{one}\" = \"traduzido\"\n"))]);
        let catalogue = Catalogue::load(dir.path(), "pt-BR");

        assert_eq!(catalogue.get(&one), "traduzido");
        assert_eq!(
            catalogue.get(&two),
            english.get(&two),
            "what the translation does not cover stays readable"
        );
    }

    #[test]
    fn a_half_finished_translation_says_how_much_is_left() {
        // Counted, not listed: a translation started yesterday is missing
        // hundreds of keys and naming them all says less than the number.
        let english = Catalogue::default();
        let one = english.keys().next().unwrap().to_owned();

        let dir = catalogue_dir(&[("pt-BR.toml", &format!("\"{one}\" = \"traduzido\"\n"))]);
        let catalogue = Catalogue::load(dir.path(), "pt-BR");

        let problem = catalogue.problems.first().expect("it is unfinished");
        assert_eq!(problem.locale, "pt-BR");
        assert!(
            problem.reason.contains("not translated yet"),
            "{}",
            problem.reason
        );
    }

    #[test]
    fn a_complete_translation_reports_nothing() {
        let english = Catalogue::default();
        let body: String = english
            .keys()
            .map(|key| format!("\"{key}\" = \"x\"\n"))
            .collect();

        let dir = catalogue_dir(&[("pt-BR.toml", &body)]);
        let catalogue = Catalogue::load(dir.path(), "pt-BR");

        assert!(catalogue.problems.is_empty(), "{:?}", catalogue.problems);
        assert_eq!(catalogue.locale, "pt-BR");
    }

    #[test]
    fn a_translation_that_will_not_parse_leaves_the_interface_in_english() {
        // A language that is not available, not a window that does not open.
        let dir = catalogue_dir(&[("pt-BR.toml", "this is not = = toml")]);
        let catalogue = Catalogue::load(dir.path(), "pt-BR");

        let english = Catalogue::default();
        let key = english.keys().next().unwrap();
        assert_eq!(catalogue.get(key), english.get(key));
        assert_eq!(catalogue.problems.len(), 1);
        assert!(catalogue.problems[0].reason.contains("not valid TOML"));
    }

    #[test]
    fn asking_for_a_language_nobody_has_written_is_not_a_problem() {
        // It is answered by English, which is exactly what happens. Reporting
        // it would put a warning on every machine outside the languages that
        // exist.
        let dir = catalogue_dir(&[]);
        let catalogue = Catalogue::load(dir.path(), "fr");

        assert!(catalogue.problems.is_empty());
        assert_eq!(catalogue.locale, DEFAULT_LOCALE);
    }

    #[test]
    fn a_key_nothing_defines_comes_back_as_itself() {
        // Only reachable through a typo in the code, since English ships
        // complete — and a wrong-looking label is easier to find than a blank.
        let catalogue = Catalogue::default();

        assert_eq!(
            catalogue.get("nothing.defines.this"),
            "nothing.defines.this"
        );
    }

    #[test]
    fn a_key_can_be_written_quoted_or_as_a_nested_table() {
        // A translator copying the file will not reliably keep the quotes, and
        // a dropped quote silently changing the shape of the file is a trap
        // rather than a format.
        let quoted = parse(r#""settings.title" = "Ajustes""#).unwrap();
        let nested = parse("[settings]\ntitle = \"Ajustes\"").unwrap();

        assert_eq!(quoted, nested);
        assert_eq!(quoted.get("settings.title").unwrap(), "Ajustes");
    }

    #[test]
    fn a_value_that_is_not_a_string_is_skipped_rather_than_refused() {
        // One key falling back to English, not a language that will not load.
        let strings = parse("[settings]\ntitle = \"Ajustes\"\nwidth = 40").unwrap();

        assert_eq!(strings.get("settings.title").unwrap(), "Ajustes");
        assert!(!strings.contains_key("settings.width"));
    }

    #[test]
    fn the_environment_is_read_the_way_the_c_library_reads_it() {
        assert_eq!(language_tag("pt_BR.UTF-8").as_deref(), Some("pt-BR"));
        assert_eq!(language_tag("en_GB").as_deref(), Some("en-GB"));
        assert_eq!(language_tag("pt_BR@euro").as_deref(), Some("pt-BR"));

        // The values that mean "no locale", which must not become a filename.
        for none in ["C", "POSIX", "", "C.UTF-8"] {
            assert_eq!(language_tag(none), None, "“{none}” became a language");
        }
    }

    #[test]
    fn one_is_singular_and_everything_else_is_not() {
        let catalogue = Catalogue::default();

        assert_eq!(catalogue.plural(1, "a.one", "a.other"), "a.one");
        assert_eq!(catalogue.plural(0, "a.one", "a.other"), "a.other");
        assert_eq!(catalogue.plural(2, "a.one", "a.other"), "a.other");
    }
}
