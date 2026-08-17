//! Every screen state, laid out headlessly.
//!
//! The rest of `app.rs`'s tests drive `update` and assert on the state that
//! comes out, which is half of what a UI does. The other half — turning that
//! state into a widget tree and laying it out — ran zero times under test, so
//! roughly four thousand lines of `view` code across the widgets and both
//! screens were only ever executed by a human opening the application. In a
//! language where an out-of-bounds index in a layout function ends the process,
//! that is the wrong half to leave uncovered.
//!
//! [`iced_test`] builds the interface against a real headless renderer, so
//! constructing a [`Simulator`] runs the full layout pass — the same one the
//! window runs. A test here therefore fails if a `view` panics, and can also
//! ask what actually reached the screen.
//!
//! These live in their own file rather than in `app.rs`'s test module because
//! that file is already long enough to be hard to move around in.

use super::*;
use crate::message::QuietBound;

/// Lays the whole interface out the way the window would.
///
/// `view` rather than `screen`: the modal and toast layers — settings, action
/// sheets, prompts, the device-code dialog — are wrapped on at that level, and
/// testing the inner one would leave every overlay uncovered.
///
/// Returns the simulator rather than a bool so a test can go on to ask what is
/// on screen; simply calling this is already an assertion that nothing in the
/// tree panicked while being laid out.
fn render(app: &Hidegit) -> iced_test::Simulator<'_, Message> {
    iced_test::simulator(app.view())
}

/// Asserts a screen lays out *and* that a piece of text a user would look for
/// reached it.
///
/// The text matters: a `view` that silently rendered nothing would lay out
/// perfectly happily, and a test that only checked for the absence of a panic
/// would pass against it.
fn shows(app: &Hidegit, text: &str) {
    let mut ui = render(app);
    assert!(
        ui.find(text).is_ok(),
        "expected {text:?} on screen, and it was not there"
    );
}

#[test]
fn the_welcome_screen_offers_the_two_ways_in() {
    let app = Hidegit::default();

    shows(&app, "Open a repository…");
}

#[test]
fn a_repository_lays_out_with_its_history() {
    let app = app_with(3);

    // The graph is a canvas, so what is assertable around it is the chrome.
    let mut ui = render(&app);
    assert!(ui.find("Commit 1").is_ok() || ui.find("main").is_ok());
}

/// The working directory, loaded the way selecting it actually loads it.
///
/// Setting `status` alone is not enough: the detail pane is a separate state
/// machine, and without the selection it stays on the history it was showing.
fn app_staging() -> Hidegit {
    let mut app = app_with(3);
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::Selected(Selection::WorkingDirectory),
    ));
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
            status: dirty(),
            staged: Diff::default(),
            unstaged: Diff::default(),
        }))),
    ));
    app
}

#[test]
fn a_dirty_working_directory_lays_out_all_three_lists() {
    let app = app_staging();

    shows(&app, "staged.txt");
    shows(&app, "changed.txt");
    shows(&app, "new.txt");
}

#[test]
fn the_conflict_resolver_lays_out_with_both_sides() {
    // The resolver renders inside the staging pane, so a conflicted status is
    // not enough on its own — the detail pane has to be showing the working
    // directory, which is what selecting it does.
    let mut app = app_resolving();
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::Selected(Selection::WorkingDirectory),
    ));
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::StatusLoaded(Box::new(Ok(StatusLoad {
            status: conflicted(),
            staged: Diff::default(),
            unstaged: Diff::default(),
        }))),
    ));

    // The resolver only occupies the pane once the conflicted row is the one
    // selected — until then the staging lists are what is on screen.
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::StagingRowSelected(crate::state::StagingRow {
            section: Section::Conflicted,
            index: 0,
        }),
    ));

    shows(&app, "OURS");
    shows(&app, "THEIRS");
    shows(&app, "RESULT");
}

#[test]
fn the_rebase_plan_editor_lays_out_every_verb() {
    let app = app_planning();

    // All six verbs are on every row deliberately — they are the whole
    // vocabulary of an interactive rebase — so a layout that dropped them
    // would be a real regression rather than a cosmetic one.
    for verb in ["pick", "reword", "edit", "squash", "fixup", "drop"] {
        shows(&app, verb);
    }
}

#[test]
fn the_blame_pane_lays_out_over_the_diff() {
    // Blame replaces the diff rather than sitting beside it, so this is also
    // the assertion that it actually took the pane: the diff's placeholder must
    // not still be there.
    let mut app = app_with(2);
    let history = commits(2);
    let id = history[0].id;
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::Selected(Selection::Commit(id)),
    ));
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::DetailLoaded(Box::new(Ok(CommitLoad {
            id,
            detail: hidegit_core::model::CommitDetail {
                commit: history[0].clone(),
                changes: Vec::new(),
                stats: Default::default(),
            },
            diff: Diff::default(),
        }))),
    ));
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::BlameLoaded(Box::new(Ok(blame_load("src/main.rs")))),
    ));

    // The header names the revision, because blame answers a different question
    // at each one.
    shows(&app, "src/main.rs");
    shows(&app, &format!("blamed at {}", id.short(7)));
    shows(&app, "Close");
}

#[test]
fn the_settings_panel_lays_out() {
    let mut app = app_with(1);
    let _ = app.update(Message::SettingsRequested);

    shows(&app, "Settings");
    shows(&app, "Saved to config.toml as you change it.");
}

#[test]
fn the_settings_panel_offers_quiet_hours() {
    // Reachable only by editing `config.toml` before this, which for most
    // people means not reachable.
    let mut app = app_with(1);
    let _ = app.update(Message::SettingsRequested);

    shows(&app, "Quiet hours");
    // The window's ends read as a clock, not as bare numbers.
    shows(&app, "22:00");
    shows(&app, "08:00");
}

#[test]
fn a_window_that_covers_nothing_says_so() {
    // Equal ends silence nothing — that is what `QuietHours::covers` decides,
    // and the panel is where somebody would otherwise have to guess it.
    let mut app = app_with(1);
    let _ = app.update(Message::SettingsRequested);
    let _ = app.update(Message::QuietHoursToggled);
    let _ = app.update(Message::QuietHourChosen(QuietBound::To, 22));

    shows(
        &app,
        "A window that starts and ends at the same hour silences nothing.",
    );
}

#[test]
fn the_command_palette_lays_out_with_its_commands_and_their_chords() {
    let mut app = app_with(1);
    let _ = app.update(Message::PaletteRequested);

    shows(&app, "Remotes");
    shows(&app, "Push");
    shows(
        &app,
        crate::widget::shortcuts::chord_label("Cmd+Shift+U").as_str(),
    );
}

#[test]
fn the_command_palette_says_when_nothing_matches() {
    // Rather than an empty box, which reads as the palette being broken.
    let mut app = app_with(1);
    let _ = app.update(Message::PaletteRequested);
    let _ = app.update(Message::PaletteQueryChanged("zzz".to_owned()));

    shows(&app, "Nothing matches that.");
}

/// A commit whose diff is one file, with one hunk of the given lines.
fn app_showing_a_diff(name: &str) -> Hidegit {
    use hidegit_core::model::{
        ChangeStatus, CommitDetail, Diff, DiffLine, DiffStats, FileChange, FileDiff,
        FileDiffContent, Hunk, LineKind,
    };

    let mut app = app_with(2);
    let repo = app.app.repos.get_mut(0).unwrap();
    let line = |kind, text: &str| DiffLine {
        kind,
        old_lineno: Some(1),
        new_lineno: Some(1),
        text: text.to_owned(),
        no_newline: false,
    };

    repo.detail = crate::state::DetailPane::Commit {
        detail: Box::new(CommitDetail {
            commit: repo.graph.commits[0].clone(),
            changes: vec![FileChange {
                path: std::path::PathBuf::from(name),
                status: ChangeStatus::Modified,
            }],
            stats: DiffStats {
                files_changed: 1,
                insertions: 1,
                deletions: 1,
            },
        }),
        diff: Box::new(Diff {
            files: vec![FileDiff {
                path: std::path::PathBuf::from(name),
                status: ChangeStatus::Modified,
                content: FileDiffContent::Text {
                    hunks: vec![Hunk {
                        old_start: 1,
                        old_lines: 1,
                        new_start: 1,
                        new_lines: 1,
                        header: "@@ -1 +1 @@".to_owned(),
                        lines: vec![
                            line(LineKind::Removed, "let answer = 41;"),
                            line(LineKind::Added, "let answer = 42;"),
                            line(LineKind::Context, "// unchanged"),
                        ],
                    }],
                },
            }],
            stats: DiffStats {
                files_changed: 1,
                insertions: 1,
                deletions: 1,
            },
        }),
        file: 0,
    };
    app
}

#[test]
fn a_highlighted_diff_lays_out_in_both_modes() {
    // Both views execute, and the line arrives split rather than whole — the
    // render harness sees `text` widgets and not the insides of rich ones, so
    // a line that is no longer findable is a line that was highlighted. What
    // the pieces are coloured is checked in `crate::highlight`.
    let mut app = app_showing_a_diff("src/main.rs");

    {
        let mut ui = render(&app);
        assert!(ui.find("@@ -1 +1 @@").is_ok(), "the hunk header is plain");
        assert!(
            ui.find("let answer = 42;").is_err(),
            "the line was not highlighted at all"
        );
    }

    let _ = app.update(Message::Repo(0, RepoMessage::DiffModeToggled));
    let mut ui = render(&app);
    assert!(ui.find("@@ -1 +1 @@").is_ok(), "side by side");
    assert!(ui.find("let answer = 42;").is_err());
}

#[test]
fn a_diff_of_a_file_nothing_can_highlight_still_shows_its_lines() {
    // The fallback the whole feature has to have: an extension syntect does not
    // know renders the line whole rather than failing.
    let app = app_showing_a_diff("notes.wibble");

    shows(&app, "let answer = 42;");
}

#[test]
fn a_filter_narrows_the_file_list_and_says_how_much_it_hid() {
    // "2 file(s)" over a commit that touched four is a quiet lie, so the count
    // says both numbers while a filter is on.
    let mut app = app_showing_a_commit();

    let stat = crate::format::diff_stat(10, 2);
    shows(&app, "Cargo.toml");
    shows(&app, &format!("4 file(s)   {stat}"));

    let _ = app.update(Message::Repo(
        0,
        RepoMessage::FileFilterChanged("src/".to_owned()),
    ));

    shows(&app, "src/app.rs");
    shows(&app, &format!("2 of 4 file(s)   {stat}"));
    let mut ui = render(&app);
    assert!(
        ui.find("Cargo.toml").is_err(),
        "a file that does not match is gone"
    );
}

#[test]
fn a_filtered_row_opens_the_file_it_names_rather_than_the_one_in_that_position() {
    // The failure this feature could have shipped with, silently: rows address
    // the commit's list, and a filtered index would open whichever file
    // happened to sit in that slot. Here `src/theme.rs` is the fourth file and
    // the second row — clicking it must ask for 3, not 1.
    let mut app = app_showing_a_commit();
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::FileFilterChanged("theme".to_owned()),
    ));

    let mut ui = render(&app);
    ui.click("src/theme.rs").expect("the row is on screen");

    let sent: Vec<Message> = ui.into_messages().collect();
    assert!(
        sent.iter()
            .any(|message| matches!(message, Message::Repo(0, RepoMessage::FileSelected(3)))),
        "got {sent:?}"
    );
}

#[test]
fn a_filter_matching_nothing_says_so_rather_than_emptying_the_pane() {
    let mut app = app_showing_a_commit();

    let _ = app.update(Message::Repo(
        0,
        RepoMessage::FileFilterChanged("zzz".to_owned()),
    ));

    shows(&app, "No file matches that.");
}

#[test]
fn the_reference_prints_the_chord_a_command_answers_to_now() {
    // A reference that still prints Cmd+Shift+U after Push was rebound is the
    // drift the reference exists to prevent, one config file later.
    let mut app = app_with(1);
    app.app.keymap = crate::keymap::Keymap::parse([("push", "Cmd+U")]).shared();
    let _ = app.update(Message::ShortcutsRequested);

    shows(
        &app,
        crate::widget::shortcuts::chord_label("Cmd+U").as_str(),
    );
    let mut ui = render(&app);
    assert!(
        ui.find(crate::widget::shortcuts::chord_label("Cmd+Shift+U").as_str())
            .is_err(),
        "the chord it no longer answers to is gone"
    );
}

#[test]
fn the_shortcut_reference_lays_out_with_its_bindings() {
    let mut app = app_with(1);
    let _ = app.update(Message::ShortcutsRequested);

    shows(&app, "Keyboard shortcuts");
    shows(&app, "Remotes");
    shows(&app, "Push");
    shows(
        &app,
        crate::widget::shortcuts::chord_label("Cmd+Shift+U").as_str(),
    );
}

#[test]
fn the_settings_panel_lists_a_custom_theme_by_its_file_name() {
    // The two that ship are shown as "Dark" and "Light" — labels, not config
    // keys. A custom theme's name is one somebody chose, so it is shown as
    // written.
    let mut app = app_with(1);
    app.app
        .themes
        .push(crate::theme::Theme::from_toml("zinc", "accent = \"#0550ae\"").unwrap());
    let _ = app.update(Message::SettingsRequested);

    shows(&app, "Dark");
    shows(&app, "Light");
    shows(&app, "zinc");
}

#[test]
fn the_settings_panel_offers_the_geometry_switch() {
    let mut app = app_with(1);
    let _ = app.update(Message::SettingsRequested);

    shows(&app, "Window");
    shows(&app, "Reopen at the last size and position");
}

#[test]
fn the_settings_panel_lists_a_muted_repository_that_is_not_open() {
    // The case that decides the shape of the list: a muted entry which vanished
    // when you closed its tab would be a setting you could not undo without
    // editing the file.
    let mut app = app_with(1);
    app.app.alerts.muted = vec!["youhide/not-open".to_owned()];
    let _ = app.update(Message::SettingsRequested);

    shows(&app, "Muted repositories");
    shows(&app, "youhide/not-open");
}

#[test]
fn the_settings_panel_stops_claiming_it_saved_when_it_did_not() {
    // The toggle flips either way — the value lives in memory and applies at
    // once — so the footer is the only thing on screen that can tell a change
    // that survived from one that will be gone on restart.
    let mut app = app_with(1);
    let _ = app.update(Message::SettingsRequested);
    app.app.settings_error = Some("config.toml is not valid TOML".to_owned());

    let mut ui = render(&app);
    assert!(
        ui.find("Saved to config.toml as you change it.").is_err(),
        "the panel went on claiming the change was saved"
    );
    shows(&app, "Not saved — config.toml is not valid TOML");
}

#[test]
fn an_action_sheet_lays_out_over_the_screen() {
    let mut app = app_with_branches();
    let _ = app.update(Message::Repo(
        0,
        RepoMessage::BranchDropped {
            source: "feature".to_owned(),
            target: "main".to_owned(),
        },
    ));

    shows(&app, "Merge feature into main");
}

#[test]
fn the_tab_bar_appears_only_once_a_second_repository_is_open() {
    let one = app_with(1);
    let mut ui = render(&one);
    // A bar showing a single tab costs a row of screen to say something you can
    // already see, so with one repository there is deliberately no bar at all.
    assert!(
        ui.find("2").is_err(),
        "a single repository should not draw a tab bar"
    );

    let mut two = app_with(1);
    let _ = two.update(Message::RepositoryOpened(Box::new(Ok(opened(2)))));
    assert_eq!(two.app.repos.len(), 2, "the fixture opened a second one");

    // Laying out with the bar present is the part that was never executed.
    let _ = render(&two);
}

#[test]
fn a_screen_with_no_repository_open_still_lays_out() {
    // Reachable by closing the last tab, and the state most likely to index
    // into something that is no longer there.
    let mut app = app_with(1);
    let _ = app.update(Message::CloseRepository(0));

    assert!(app.app.repos.is_empty(), "the fixture closed the only tab");
    let _ = render(&app);
}
