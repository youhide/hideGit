//! The left sidebar: one tree, one mental model for "places I can jump to".
//!
//! Working directory, local branches, remotes, tags — and, from M4, pull
//! requests. Sections that belong to a later milestone are absent rather than
//! present-and-empty, because an empty "PULL REQUESTS" heading reads as "you have
//! none" and that would be a lie.
//!
//! Rows became actionable in M3. Each carries its own controls, revealed as a
//! glyph rather than a menu, the same way a staging row carries `+`, `−` and `✕` —
//! and a `⋯` that opens the action sheet for everything that does not fit.

use hidegit_core::model::{Branch, Divergence, Head, StashEntry, Tag};
use hidegit_core::ops::{CheckoutTarget, StartPoint, StashOp};
use iced::widget::{Space, button, column, container, row, scrollable, text, tooltip};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::{ActionSheet, OpenRepo, Prompt, PromptField, PromptKind, Selection};
use crate::theme::Palette;

const HEADING_SIZE: f32 = 11.0;
const ITEM_SIZE: f32 = 13.0;

/// The sidebar emits both kinds of message.
///
/// Most rows address one repository (`RepoMessage`), but raising a sheet or a
/// prompt is application state, so those are top-level. `index` is what lets a
/// row build a `Message::Repo(index, …)` to put *inside* a sheet item.
pub fn view<'a>(repo: &'a OpenRepo, index: usize, palette: &'a Palette) -> Element<'a, Message> {
    let mut sections = column![].spacing(2).padding(Padding::from([8, 0]));

    sections =
        sections.push(working_directory(repo, palette).map(move |m| Message::Repo(index, m)));
    sections = sections.push(Space::new().height(8));

    sections = sections.push(section_heading(
        "LOCAL",
        repo.refs.locals.len(),
        Some((
            "New branch…",
            Message::PromptRequested(Box::new(new_branch_prompt(StartPoint::Head))),
        )),
        palette,
    ));
    for branch in &repo.refs.locals {
        sections = sections.push(local_branch_row(repo, branch, index, palette));
    }

    if !repo.refs.remotes.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(section_heading(
            "REMOTES",
            repo.refs.remotes.len(),
            None,
            palette,
        ));
        for branch in &repo.refs.remotes {
            sections = sections.push(remote_branch_row(branch, index, palette));
        }
    }

    if !repo.refs.tags.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(section_heading("TAGS", repo.refs.tags.len(), None, palette));
        for tag in &repo.refs.tags {
            sections = sections.push(tag_row(tag, palette).map(move |m| Message::Repo(index, m)));
        }
    }

    // Only when there is one. An empty "STASHES" heading reads as "you have no
    // stashes" — which until M3 would have been a lie about a missing feature, and
    // now would be a lie about the repository.
    if !repo.stashes.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(section_heading(
            "STASHES",
            repo.stashes.len(),
            None,
            palette,
        ));
        for entry in &repo.stashes {
            sections = sections.push(stash_row(repo, entry, index, palette));
        }
    }

    let palette = *palette;
    container(scrollable(sections).height(Fill))
        .width(Length::Fixed(230.0))
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(palette.surface.into()),
            ..container::Style::default()
        })
        .into()
}

/// The prompt that collects a new branch's name.
fn new_branch_prompt(from: StartPoint) -> Prompt {
    let where_from = match &from {
        StartPoint::Head => "the current commit".to_owned(),
        StartPoint::Commit(id) => id.short(7),
        StartPoint::Ref(name) => name.clone(),
    };

    Prompt {
        // Checking out what you just created is what people mean by "new
        // branch"; creating one to leave behind is the rarer thing and it has
        // its own message.
        kind: PromptKind::NewBranch {
            from,
            checkout: true,
        },
        title: "New branch".to_owned(),
        confirm_label: "Create".to_owned(),
        fields: vec![PromptField::new(
            format!("Name, starting from {where_from}"),
            "feat/something",
        )],
    }
}

/// The label and count every section heading shares, whatever it emits.
///
/// Generic over the message type because the staging view's headings carry no
/// actions and address one repository, while the sidebar's carry a `+` that
/// raises application-level state. One layout, two message types.
fn heading_row<'a, M: 'a>(
    label: &'a str,
    count: usize,
    palette: &Palette,
) -> iced::widget::Row<'a, M> {
    let muted = palette.muted;
    let count = if count > 0 {
        text(count.to_string()).size(HEADING_SIZE).color(muted)
    } else {
        text("").size(HEADING_SIZE)
    };

    row![
        text(label).size(HEADING_SIZE).color(muted).font(Font {
            weight: iced::font::Weight::Semibold,
            ..Font::DEFAULT
        }),
        Space::new().width(Fill),
        count,
    ]
    .spacing(6)
    .align_y(Center)
}

/// A section heading, with the `+` that adds to it when there is one.
fn section_heading<'a>(
    label: &'a str,
    count: usize,
    add: Option<(&'a str, Message)>,
    palette: &Palette,
) -> Element<'a, Message> {
    let palette = *palette;
    let mut heading = heading_row(label, count, &palette);

    if let Some((tip, message)) = add {
        heading = heading.push(hinted(
            button(
                container(
                    text("+")
                        .size(ITEM_SIZE)
                        .font(Font::MONOSPACE)
                        .color(palette.muted),
                )
                .padding(Padding::from([0, 4])),
            )
            .padding(0)
            .style(move |_, status| item_style(palette, false, status))
            .on_press(message)
            .into(),
            tip.to_owned(),
            palette,
        ));
    }

    container(heading).padding(Padding::from([4, 12])).into()
}

pub(crate) fn heading<'a>(
    label: &'a str,
    count: usize,
    palette: &Palette,
) -> Element<'a, RepoMessage> {
    container(heading_row(label, count, palette))
        .padding(Padding::from([4, 12]))
        .into()
}

fn working_directory<'a>(repo: &OpenRepo, palette: &Palette) -> Element<'a, RepoMessage> {
    let selected = matches!(repo.selection, Some(Selection::WorkingDirectory));
    let palette = *palette;

    // The badge counts every entry across all four lists, so a file both
    // staged and edited again counts twice — the same way `git status` lists
    // it under two headings.
    let count = repo.status.change_count();
    let badge = if count > 0 {
        text(count.to_string())
            .size(HEADING_SIZE)
            .color(palette.accent)
    } else {
        text("").size(HEADING_SIZE)
    };

    let label = row![
        text("WORKING DIRECTORY")
            .size(HEADING_SIZE)
            .color(palette.muted),
        Space::new().width(Fill),
        badge,
    ]
    .align_y(Center);

    button(container(label).padding(Padding::from([4, 12])))
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, selected, status))
        .on_press(RepoMessage::Selected(Selection::WorkingDirectory))
        .into()
}

/// A local branch: where it is, how far it has drifted, and what can be done.
fn local_branch_row<'a>(
    repo: &OpenRepo,
    branch: &Branch,
    index: usize,
    palette: &Palette,
) -> Element<'a, Message> {
    let is_head = matches!(&repo.head, Head::Branch { name, .. } if name.full == branch.name.full);
    let palette = *palette;
    let short = branch.name.short.clone();

    let name = text(short.clone())
        .size(ITEM_SIZE)
        .color(if is_head { palette.text } else { palette.muted })
        .font(if is_head {
            Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }
        } else {
            Font::DEFAULT
        });

    // The current branch is marked with a glyph as well as with weight and
    // colour, so it is identifiable without relying on either.
    let marker = text(if is_head { "▸" } else { " " })
        .size(ITEM_SIZE)
        .color(palette.accent);

    let mut label = row![marker, name].spacing(6).align_y(Center);
    if let Some(drift) = divergence_label(repo.divergence_of(&branch.name.full)) {
        label = label.push(Space::new().width(Fill));
        label = label.push(text(drift).size(HEADING_SIZE).color(palette.muted));
    }

    let sheet = branch_sheet(branch, index, is_head, repo.refs.locals.len());

    row![
        button(container(label).padding(Padding::from([3, 12])))
            .width(Fill)
            .padding(0)
            .style(move |_, status| item_style(palette, false, status))
            .on_press(Message::Repo(
                index,
                RepoMessage::Selected(Selection::Commit(branch.target)),
            )),
        action_button("⋯", format!("Actions for {short}"), sheet, palette),
    ]
    .align_y(Center)
    .into()
}

/// Ahead/behind as a compact pair, or nothing at all.
///
/// `None` means the branch tracks nothing, which is not the same as being level
/// with a remote and must not read as it: showing `↑0 ↓0` there would claim an
/// upstream that does not exist. A branch that is level shows nothing either —
/// the absence *is* the "no news" state, and a column of zeroes is noise.
fn divergence_label(divergence: Option<Divergence>) -> Option<String> {
    let drift = divergence?;
    if drift.is_in_sync() {
        return None;
    }

    let mut label = String::new();
    if drift.ahead > 0 {
        label.push_str(&format!("↑{}", drift.ahead));
    }
    if drift.behind > 0 {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(&format!("↓{}", drift.behind));
    }
    Some(label)
}

/// What can be done to a local branch.
///
/// Takes what it needs rather than the whole repository, so the rules about which
/// actions are offered can be exercised without building one.
fn branch_sheet(branch: &Branch, index: usize, is_head: bool, local_count: usize) -> ActionSheet {
    let name = branch.name.short.clone();
    let mut sheet = ActionSheet::new(name.clone());

    // Checking out the branch you are already on does nothing, so it is not
    // offered — an action that is a no-op is worse than an absent one.
    if !is_head {
        sheet = sheet.item(
            "Checkout",
            Message::Repo(
                index,
                RepoMessage::CheckoutRequested(CheckoutTarget::Branch(name.clone())),
            ),
        );
    }

    sheet = sheet.item(
        "New branch from here…",
        Message::PromptRequested(Box::new(new_branch_prompt(StartPoint::Ref(
            branch.name.full.clone(),
        )))),
    );
    sheet = sheet.item(
        "Rename…",
        Message::PromptRequested(Box::new(Prompt {
            kind: PromptKind::RenameBranch { from: name.clone() },
            title: format!("Rename {name}"),
            confirm_label: "Rename".to_owned(),
            // Opens holding the current name, the way `git commit --amend` opens
            // holding the message it is replacing.
            fields: vec![PromptField::prefilled("New name", name.clone())],
        })),
    );

    // Deleting the branch you are standing on is something Git refuses, so it is
    // not offered either. Nor is deleting the only branch there is.
    if !is_head && local_count > 1 {
        sheet = sheet.destructive(
            "Delete",
            Message::Repo(index, RepoMessage::BranchDeleteRequested { name }),
        );
    }

    sheet
}

/// A remote-tracking branch. Checking one out means creating a local branch that
/// tracks it, which is a different operation from checking out a local one.
fn remote_branch_row<'a>(branch: &Branch, index: usize, palette: &Palette) -> Element<'a, Message> {
    let palette = *palette;
    let short = branch.name.short.clone();

    // Marked distinctly from a local branch: they used to render through the same
    // function and were indistinguishable, which mattered as soon as clicking one
    // did something different.
    let label = row![
        text("⇢").size(ITEM_SIZE).color(palette.muted),
        text(short.clone()).size(ITEM_SIZE).color(palette.muted),
    ]
    .spacing(6)
    .align_y(Center);

    // `origin/feat` → a local `feat`. Everything after the first `/` is the
    // branch name, because a remote branch may legitimately contain slashes.
    let local = short
        .split_once('/')
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_else(|| short.clone());

    let sheet = ActionSheet::new(short.clone()).item(
        format!("Checkout as {local}"),
        Message::Repo(
            index,
            RepoMessage::CheckoutRequested(CheckoutTarget::TrackRemote {
                remote_ref: short.clone(),
                local,
            }),
        ),
    );

    row![
        button(container(label).padding(Padding::from([3, 12])))
            .width(Fill)
            .padding(0)
            .style(move |_, status| item_style(palette, false, status))
            .on_press(Message::Repo(
                index,
                RepoMessage::Selected(Selection::Commit(branch.target)),
            )),
        action_button("⋯", format!("Actions for {short}"), sheet, palette),
    ]
    .align_y(Center)
    .into()
}

/// One stash entry: its message, the branch it came from, and what can be done.
fn stash_row<'a>(
    repo: &OpenRepo,
    entry: &StashEntry,
    index: usize,
    palette: &Palette,
) -> Element<'a, Message> {
    let palette = *palette;
    let at = entry.index;
    let selected = repo.selection == Some(Selection::Stash(at));

    // Git's own vocabulary: `stash@{0}` is what the user would type, so it is what
    // they are shown, rather than an invented ordinal.
    let position = text(format!("{{{at}}}"))
        .size(HEADING_SIZE)
        .font(Font::MONOSPACE)
        .color(palette.muted);

    let label = row![
        position,
        text(entry.message.clone())
            .size(ITEM_SIZE)
            .color(palette.text),
    ]
    .spacing(6)
    .align_y(Center);

    let title = match &entry.branch {
        Some(branch) => format!("stash@{{{at}}} on {branch}"),
        None => format!("stash@{{{at}}}"),
    };
    let sheet = ActionSheet::new(title.clone())
        // Apply first: it is the reversible one. Pop is the same thing plus a
        // deletion, and Drop is the deletion alone.
        .item(
            "Apply, keeping the stash",
            Message::Repo(index, RepoMessage::StashRequested(StashOp::Apply(at))),
        )
        .item(
            "Pop, removing the stash",
            Message::Repo(index, RepoMessage::StashRequested(StashOp::Pop(at))),
        )
        .destructive(
            "Drop",
            Message::Repo(index, RepoMessage::StashDropRequested(at)),
        );

    row![
        button(container(label).padding(Padding::from([3, 12])))
            .width(Fill)
            .padding(0)
            .style(move |_, status| item_style(palette, selected, status))
            .on_press(Message::Repo(
                index,
                RepoMessage::Selected(Selection::Stash(at)),
            )),
        action_button("⋯", format!("Actions for {title}"), sheet, palette),
    ]
    .align_y(Center)
    .into()
}

/// The `⋯` on a row, which opens its action sheet.
///
/// A glyph because the sidebar is 230px wide, with the item's name behind a
/// tooltip so what it acts on is discoverable rather than guessed — the same
/// trade-off the staging rows make.
fn action_button<'a>(
    glyph: &'a str,
    label: String,
    sheet: ActionSheet,
    palette: Palette,
) -> Element<'a, Message> {
    let control = button(
        container(text(glyph).size(ITEM_SIZE).font(Font::MONOSPACE)).padding(Padding::from([3, 8])),
    )
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(Message::SheetRequested(Box::new(sheet)));

    hinted(control.into(), label, palette)
}

/// Puts a word behind a glyph, so a one-character control is discoverable.
fn hinted<'a>(
    control: Element<'a, Message>,
    label: String,
    palette: Palette,
) -> Element<'a, Message> {
    tooltip(
        control,
        container(text(label).size(11.0).color(palette.text))
            .padding(Padding::from([4, 6]))
            .style(move |_| container::Style {
                background: Some(palette.background.into()),
                border: iced::Border {
                    color: palette.border,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..container::Style::default()
            }),
        tooltip::Position::Left,
    )
    .into()
}

fn tag_row<'a>(tag: &Tag, palette: &Palette) -> Element<'a, RepoMessage> {
    let palette = *palette;
    // Annotated and lightweight tags look different, because they are.
    let glyph = if tag.annotated { "◆" } else { "◇" };

    button(
        container(
            row![
                text(glyph).size(ITEM_SIZE).color(palette.warning),
                text(tag.name.short.clone())
                    .size(ITEM_SIZE)
                    .color(palette.muted),
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding(Padding::from([3, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| item_style(palette, false, status))
    .on_press(RepoMessage::Selected(Selection::Commit(tag.target)))
    .into()
}

pub(crate) fn item_style(
    palette: Palette,
    selected: bool,
    status: button::Status,
) -> button::Style {
    let background = match (selected, status) {
        (true, _) => Some(
            iced::Color {
                a: 0.22,
                ..palette.accent
            }
            .into(),
        ),
        (false, button::Status::Hovered) => Some(
            iced::Color {
                a: 0.08,
                ..palette.text
            }
            .into(),
        ),
        _ => None,
    };

    button::Style {
        background,
        text_color: palette.text,
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidegit_core::model::{RefKind, RefName};

    fn branch(short: &str, upstream: Option<&str>) -> Branch {
        Branch {
            name: RefName {
                kind: RefKind::LocalBranch,
                full: format!("refs/heads/{short}"),
                short: short.to_owned(),
            },
            target: hidegit_core::ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            upstream: upstream.map(str::to_owned),
        }
    }

    #[test]
    fn a_branch_that_tracks_nothing_shows_nothing_rather_than_two_zeroes() {
        // `↑0 ↓0` would claim an upstream that does not exist. The sidebar has to
        // tell "no remote to compare with" apart from "level with the remote".
        assert_eq!(divergence_label(None), None);
    }

    #[test]
    fn a_branch_level_with_its_upstream_also_shows_nothing() {
        // The absence *is* the "no news" state; a column of zeroes is noise.
        assert_eq!(divergence_label(Some(Divergence::default())), None);
    }

    #[test]
    fn drift_reads_as_arrows_with_only_the_non_zero_side_shown() {
        assert_eq!(
            divergence_label(Some(Divergence {
                ahead: 2,
                behind: 0
            })),
            Some("↑2".to_owned())
        );
        assert_eq!(
            divergence_label(Some(Divergence {
                ahead: 0,
                behind: 3
            })),
            Some("↓3".to_owned())
        );
        assert_eq!(
            divergence_label(Some(Divergence {
                ahead: 2,
                behind: 3
            })),
            Some("↑2 ↓3".to_owned())
        );
    }

    #[test]
    fn a_remote_branchs_local_name_drops_only_the_remote() {
        // `origin/feat/graph` becomes `feat/graph`, not `feat`: a branch name may
        // legitimately contain slashes, and only the first segment is the remote.
        let local = |short: &str| {
            short
                .split_once('/')
                .map(|(_, rest)| rest.to_owned())
                .unwrap_or_else(|| short.to_owned())
        };

        assert_eq!(local("origin/main"), "main");
        assert_eq!(local("origin/feat/graph"), "feat/graph");
        assert_eq!(local("upstream/release/1.x"), "release/1.x");
    }

    /// The labels a sheet offers, for asserting on what is and is not there.
    fn labels(sheet: &ActionSheet) -> Vec<&str> {
        sheet.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn a_sheet_does_not_offer_actions_that_would_be_refused() {
        // Checking out the branch you are already on is a no-op, and deleting it
        // is something Git refuses outright. An action that cannot work is worse
        // than an absent one.
        let head = branch("main", Some("refs/remotes/origin/main"));
        let sheet = branch_sheet(&head, 0, true, 2);
        let offered = labels(&sheet);

        assert!(!offered.contains(&"Checkout"), "already on it: {offered:?}");
        assert!(!offered.contains(&"Delete"), "standing on it: {offered:?}");
        assert!(offered.contains(&"Rename…"), "renaming HEAD is fine");
    }

    #[test]
    fn a_branch_that_is_not_checked_out_offers_everything() {
        let other = branch("feat/graph", None);
        let sheet = branch_sheet(&other, 0, false, 2);
        let offered = labels(&sheet);

        assert!(offered.contains(&"Checkout"));
        assert!(offered.contains(&"Rename…"));
        assert!(offered.contains(&"Delete"));
    }

    #[test]
    fn the_only_branch_in_a_repository_cannot_be_deleted() {
        // Git refuses to leave a repository with no branches, so the action is
        // not offered rather than offered and then refused.
        let only = branch("main", None);
        let sheet = branch_sheet(&only, 0, false, 1);
        let offered = labels(&sheet);

        assert!(!offered.contains(&"Delete"), "got {offered:?}");
    }

    #[test]
    fn a_deletion_is_marked_destructive_so_it_does_not_look_like_the_rest() {
        let other = branch("feat/graph", None);
        let sheet = branch_sheet(&other, 0, false, 2);

        let delete = sheet
            .items
            .iter()
            .find(|i| i.label == "Delete")
            .expect("delete is offered");
        assert!(delete.destructive);
        assert!(
            sheet.items.iter().filter(|i| i.destructive).count() == 1,
            "only the deletion is destructive"
        );
    }
}
