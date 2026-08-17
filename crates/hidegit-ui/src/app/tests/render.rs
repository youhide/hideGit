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
