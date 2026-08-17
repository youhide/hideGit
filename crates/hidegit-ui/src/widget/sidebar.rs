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

use hidegit_core::model::{
    Branch, Divergence, Head, Remote, StashEntry, Submodule, SubmoduleState, Tag, Worktree,
};
use hidegit_core::ops::{CheckoutTarget, StartPoint, StashOp};
use iced::widget::{Space, button, column, container, row, scrollable, text, tooltip};
use iced::{Center, Fill, Font, Length, Padding};

use crate::Element;
use crate::message::{Message, RepoMessage};
use crate::state::{ActionSheet, App, OpenRepo, Prompt, PromptField, PromptKind, Selection};
use crate::theme::Palette;

const HEADING_SIZE: f32 = 11.0;
const ITEM_SIZE: f32 = 13.0;

/// The sidebar emits both kinds of message.
///
/// Most rows address one repository (`RepoMessage`), but raising a sheet or a
/// prompt is application state, so those are top-level. `index` is what lets a
/// row build a `Message::Repo(index, …)` to put *inside* a sheet item.
pub fn view<'a>(
    app: &'a App,
    repo: &'a OpenRepo,
    index: usize,
    palette: &'a Palette,
) -> Element<'a, Message> {
    let mut sections = column![].spacing(2).padding(Padding::from([8, 0]));

    sections = sections.push(working_directory(repo, index, palette));
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

    // Two levels: the remote, then the branches on it. Every *configured* remote
    // appears, fetched or not — a remote with no tracking refs is still a remote,
    // and grouping only by ref name would hide it.
    let grouped = repo.remotes_with_branches();
    sections = sections.push(Space::new().height(8));
    sections = sections.push(section_heading(
        "REMOTES",
        grouped.len(),
        Some((
            "Add a remote…",
            Message::PromptRequested(Box::new(Prompt {
                kind: PromptKind::AddRemote,
                title: "Add a remote".to_owned(),
                confirm_label: "Add".to_owned(),
                fields: vec![
                    PromptField::new("Name", "origin"),
                    PromptField::new("URL", "https://example.com/repo.git"),
                ],
            })),
        )),
        palette,
    ));
    for (remote, branches) in &grouped {
        sections = sections.push(remote_row(remote, branches.len(), index, palette));
        for branch in branches {
            sections = sections.push(remote_branch_row(branch, index, palette));
        }
    }

    sections = sections.push(Space::new().height(8));
    sections = sections.push(section_heading(
        "TAGS",
        repo.refs.tags.len(),
        Some((
            "New tag at HEAD…",
            Message::PromptRequested(Box::new(new_tag_prompt(StartPoint::Head, false))),
        )),
        palette,
    ));
    for tag in &repo.refs.tags {
        sections = sections.push(tag_row(repo, tag, index, palette));
    }

    // Only when there is one, unlike the sections above. `LOCAL`, `REMOTES` and
    // `TAGS` show empty because their heading carries the `+` that creates the
    // first one, and "you have no remotes" is a true and useful thing to read. A
    // stash is not created from a heading — it comes from the working directory —
    // so an empty `STASHES` would be chrome with nothing behind it.
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

    // Only when there is more than one, and that is not the STASHES rule with a
    // different number. Every repository has a worktree — the one being looked
    // at — so a section listing exactly it would be a heading over a line the
    // whole window already says. A *second* checkout is the fact worth showing.
    if repo.worktrees.len() > 1 {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(section_heading(
            "WORKTREES",
            repo.worktrees.len(),
            None,
            palette,
        ));
        for worktree in &repo.worktrees {
            sections = sections.push(worktree_row(worktree, index, palette));
        }
    }

    // Same rule as STASHES, for the same reason: a submodule is not created
    // from a heading — it comes from a `.gitmodules` somebody committed — so an
    // empty SUBMODULES would be chrome with nothing behind it, on the
    // overwhelming majority of repositories.
    if !repo.submodules.is_empty() {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(section_heading(
            "SUBMODULES",
            repo.submodules.len(),
            None,
            palette,
        ));
        for submodule in &repo.submodules {
            sections = sections.push(submodule_row(submodule, index, palette));
        }
    }

    // Last, and absent entirely when no remote names a forge repository: a
    // repository whose only remote is a path on disk has no pull requests to
    // have, which is not the same as having none.
    if let Some(prs) = crate::widget::pr::section(app, repo, index, palette) {
        sections = sections.push(Space::new().height(8));
        sections = sections.push(prs);
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
pub(crate) fn section_heading<'a>(
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

fn working_directory<'a>(repo: &OpenRepo, index: usize, palette: &Palette) -> Element<'a, Message> {
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

    let row = button(container(label).padding(Padding::from([4, 12])))
        .width(Fill)
        .padding(0)
        .style(move |_, status| item_style(palette, selected, status))
        .on_press(Message::Repo(
            index,
            RepoMessage::Selected(Selection::WorkingDirectory),
        ));

    // Stashing comes from here rather than from a STASHES heading, because a stash
    // is made *out of* the working directory — and there is nothing to stash when
    // it is clean, so the control is absent rather than present and refusing.
    if repo.status.is_clean() {
        return row.into();
    }

    let untracked = !repo.status.untracked.is_empty();
    let sheet = ActionSheet::new(format!(
        "{} change(s) in the working directory",
        repo.status.change_count()
    ))
    .item(
        "Stash changes…",
        Message::PromptRequested(Box::new(stash_prompt(false))),
    )
    .item(
        if untracked {
            "Stash changes and untracked files…"
        } else {
            // Offered even with nothing untracked, because "include untracked" is
            // about what the stash *would* take and the user may be about to add
            // something. Saying so keeps the two options from looking identical.
            "Stash changes, including any untracked files…"
        },
        Message::PromptRequested(Box::new(stash_prompt(true))),
    );

    row![
        row,
        action_button(
            "⋯",
            "Actions for the working directory".to_owned(),
            sheet,
            palette
        ),
    ]
    .align_y(Center)
    .into()
}

/// The prompt that collects a stash's message.
///
/// The one prompt that can be accepted empty: Git writes its own `WIP on …` when no
/// message is given, and refusing to stash without one would be hideGit inventing a
/// requirement Git does not have.
fn stash_prompt(include_untracked: bool) -> Prompt {
    Prompt {
        kind: PromptKind::StashPush { include_untracked },
        title: if include_untracked {
            "Stash changes and untracked files".to_owned()
        } else {
            "Stash changes".to_owned()
        },
        confirm_label: "Stash".to_owned(),
        fields: vec![PromptField::new(
            "Message (optional)",
            "what you were in the middle of",
        )],
    }
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

    // A detached HEAD has no branch to merge *into*, so those two actions are
    // absent rather than pointing at nothing.
    let head_name = match &repo.head {
        hidegit_core::model::Head::Branch { name, .. } => Some(name.short.clone()),
        _ => None,
    };
    let sheet = branch_sheet(
        branch,
        index,
        is_head,
        repo.refs.locals.len(),
        head_name.as_deref(),
        held_elsewhere(&repo.worktrees, &branch.name.full),
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
/// `head_name` is the branch `HEAD` is on, if it is on one. A detached `HEAD`
/// has no branch to merge *into*, so those actions are absent rather than
/// pointing at nothing.
/// Whether another worktree has this branch checked out.
///
/// The rule worktrees impose on everything else: a branch checked out in one
/// cannot be checked out in another, so this is what keeps the sheet from
/// offering a checkout Git would refuse and a second worktree it would refuse
/// as well.
fn held_elsewhere(worktrees: &[Worktree], full: &str) -> bool {
    worktrees.iter().any(|worktree| {
        !worktree.is_current
            && matches!(&worktree.head, Some(Head::Branch { name, .. }) if name.full == full)
    })
}

fn branch_sheet(
    branch: &Branch,
    index: usize,
    is_head: bool,
    local_count: usize,
    head_name: Option<&str>,
    held_elsewhere: bool,
) -> ActionSheet {
    let name = branch.name.short.clone();
    let mut sheet = ActionSheet::new(name.clone());

    // Checking out the branch you are already on does nothing, so it is not
    // offered — an action that is a no-op is worse than an absent one. Nor is
    // one another worktree is holding: Git refuses that outright, and a control
    // that always fails is worse than an absent one too.
    if !is_head && !held_elsewhere {
        sheet = sheet.item(
            "Checkout",
            Message::Repo(
                index,
                RepoMessage::CheckoutRequested(CheckoutTarget::Branch(name.clone())),
            ),
        );

        // Where a worktree is made from, because a worktree is made *out of* a
        // branch. The `WORKTREES` heading would be the other candidate, and it
        // is absent on exactly the repositories where somebody wants to make a
        // second checkout — the same reason stashing is offered from the
        // working-directory row rather than from a `STASHES` heading.
        sheet = sheet.item(
            "Check out in a new worktree…",
            Message::Repo(
                index,
                RepoMessage::WorktreeDestinationRequested(name.clone()),
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

    // Merging or rebasing a branch onto itself is a no-op Git would refuse, so
    // neither is offered on the branch you are standing on. Both name the
    // current branch, because "Merge" alone leaves the direction to be guessed
    // and getting the direction wrong is the classic way to ruin an afternoon.
    if !is_head && let Some(head) = head_name {
        sheet = sheet.item(
            format!("Merge {name} into {head}"),
            Message::Repo(index, RepoMessage::MergeRequested(name.clone())),
        );
        sheet = sheet.item(
            format!("Rebase {head} onto {name}…"),
            Message::Repo(index, RepoMessage::RebaseRequested(name.clone())),
        );
        // The interactive form is a separate entry rather than a checkbox on
        // the first: one runs immediately after a confirmation, the other opens
        // a screen where nothing happens until you say so, and collapsing them
        // would make which you get depend on a toggle you might not notice.
        sheet = sheet.item(
            format!("Rebase {head} onto {name}, interactively…"),
            Message::Repo(index, RepoMessage::RebasePlanRequested(name.clone())),
        );
    }

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

/// The prompt that collects a tag's name, and its message when annotated.
fn new_tag_prompt(at: StartPoint, annotated: bool) -> Prompt {
    let where_at = match &at {
        StartPoint::Head => "the current commit".to_owned(),
        StartPoint::Commit(id) => id.short(7),
        StartPoint::Ref(name) => name.clone(),
    };

    let mut fields = vec![PromptField::new(format!("Name, at {where_at}"), "v1.0.0")];
    if annotated {
        // An annotated tag is an object with a message; a lightweight one is only a
        // ref. Which it is, is decided before the prompt opens rather than by
        // whether the message happens to be filled in.
        fields.push(PromptField::new("Message", "what this release is"));
    }

    Prompt {
        kind: PromptKind::NewTag { at, annotated },
        title: if annotated {
            "New annotated tag".to_owned()
        } else {
            "New tag".to_owned()
        },
        confirm_label: "Create".to_owned(),
        fields,
    }
}

/// A named remote and its URL.
fn remote_row<'a>(
    remote: &Remote,
    branch_count: usize,
    index: usize,
    palette: &Palette,
) -> Element<'a, Message> {
    let palette = *palette;
    let name = remote.name.clone();

    // The URL is what distinguishes two remotes with similar names, and it is the
    // thing people check when a push goes somewhere unexpected.
    let label = row![
        text("☁").size(ITEM_SIZE).color(palette.muted),
        text(name.clone()).size(ITEM_SIZE).color(palette.text),
        Space::new().width(Fill),
        // A remote that has never been fetched has no branches, and saying so is
        // more useful than an empty space.
        text(if branch_count == 0 {
            "not fetched".to_owned()
        } else {
            branch_count.to_string()
        })
        .size(HEADING_SIZE)
        .color(palette.muted),
    ]
    .spacing(6)
    .align_y(Center);

    let sheet = ActionSheet::new(remote.url_summary())
        .item("Fetch", Message::Repo(index, RepoMessage::FetchRequested))
        .item(
            "Edit URL…",
            Message::PromptRequested(Box::new(Prompt {
                kind: PromptKind::EditRemote { name: name.clone() },
                title: format!("URL for {name}"),
                confirm_label: "Save".to_owned(),
                fields: vec![PromptField::prefilled("URL", remote.fetch_url.clone())],
            })),
        )
        .destructive(
            "Remove",
            Message::Repo(index, RepoMessage::RemoteRemoveRequested(name.clone())),
        );

    row![
        container(label).width(Fill).padding(Padding::from([4, 12])),
        action_button("⋯", format!("Actions for {name}"), sheet, palette),
    ]
    .align_y(Center)
    .into()
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

/// A submodule: where it sits, and whether its checkout agrees with the
/// superproject.
///
/// It carries a `⋯` only when there is something to do — a submodule already at
/// the recorded commit has no action that would change anything, and offering
/// one would be a control that does nothing. There is still no *selection*: a
/// submodule is not a place in this repository's history to jump to, it is a
/// pointer at another repository's.
///
/// The tooltip carries the URL and what the state means; the glyph is Git's own
/// `-`, ` ` and `+`.
fn submodule_row<'a>(
    submodule: &Submodule,
    index: usize,
    palette: &Palette,
) -> Element<'a, Message> {
    let palette = *palette;
    let state = submodule.state();

    // Git's own column, so somebody who has read `git submodule status` in a
    // terminal recognises it here without learning a second notation.
    let (glyph, colour) = match state {
        SubmoduleState::Current => (" ", palette.muted),
        SubmoduleState::Moved => ("+", palette.accent),
        SubmoduleState::Uninitialised => ("-", palette.muted),
    };

    // Both commits when they disagree, in that order. A single hash would leave
    // the user asking which of the two it was — and *which pointer is wrong* is
    // the only question a submodule ever raises.
    let at = match (state, submodule.recorded, submodule.checked_out) {
        (SubmoduleState::Uninitialised, _, _) => "not initialised".to_owned(),
        (SubmoduleState::Moved, Some(recorded), Some(checked_out)) => {
            format!("{} → {}", recorded.short(7), checked_out.short(7))
        }
        (_, Some(id), _) => id.short(7),
        (_, None, _) => "not staged".to_owned(),
    };

    let label = row![
        text(glyph)
            .size(ITEM_SIZE)
            .font(Font::MONOSPACE)
            .color(colour),
        text(submodule.path.display().to_string())
            .size(ITEM_SIZE)
            .color(palette.text),
        Space::new().width(Fill),
        text(at)
            .size(HEADING_SIZE)
            .font(Font::MONOSPACE)
            .color(colour),
    ]
    .spacing(6)
    .align_y(Center);

    let explanation = match state {
        SubmoduleState::Current => "at the commit the superproject records",
        SubmoduleState::Moved => "moved off the commit the superproject records",
        SubmoduleState::Uninitialised => "declared, but not checked out here",
    };
    let tip = if submodule.url.is_empty() {
        format!("{}: {explanation}", submodule.path.display())
    } else {
        format!("{} — {explanation}", submodule.url)
    };

    let row = hinted(
        container(label)
            .width(Fill)
            .padding(Padding::from([3, 12]))
            .into(),
        tip,
        palette,
    );

    let path = submodule.path.clone();
    let Some(sheet) = submodule_sheet(submodule, index) else {
        return row;
    };

    row![
        row,
        action_button(
            "⋯",
            format!("Actions for {}", path.display()),
            sheet,
            palette
        ),
    ]
    .align_y(Center)
    .into()
}

/// A worktree: where the checkout is, and what it has checked out there.
///
/// The branch is the load-bearing half. A branch checked out in one worktree
/// cannot be checked out in another, so this row is the answer to a refused
/// checkout — including for a worktree whose directory is gone, which still
/// holds its branch until somebody prunes it.
///
/// Not a button, and not selectable: another checkout is not a place in this
/// repository's history to jump to. The actions arrive with the operations that
/// would run them.
fn worktree_row<'a>(worktree: &Worktree, index: usize, palette: &Palette) -> Element<'a, Message> {
    let palette = *palette;

    // `▸` for the checkout being looked at, matching the local-branch row's
    // marker for the branch `HEAD` is on: both mean "this is the one you are
    // standing in".
    let marker = text(if worktree.is_current { "▸" } else { " " })
        .size(ITEM_SIZE)
        .color(palette.accent);

    // The directory name rather than the whole path, which does not fit in
    // 230px and whose useful end is the last component anyway. The full path is
    // in the tooltip.
    let name = worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| worktree.path.display().to_string());

    let (state, colour) = if worktree.prunable {
        // Said plainly rather than by dimming it: the directory is gone and the
        // registration is not, which is a thing to act on, not a shade.
        ("gone".to_owned(), palette.muted)
    } else if worktree.locked.is_some() {
        ("locked".to_owned(), palette.muted)
    } else {
        (worktree_head(worktree), palette.muted)
    };

    let label = row![
        marker,
        text(name).size(ITEM_SIZE).color(if worktree.is_current {
            palette.text
        } else {
            palette.muted
        }),
        Space::new().width(Fill),
        text(state).size(HEADING_SIZE).color(colour),
    ]
    .spacing(6)
    .align_y(Center);

    let mut tip = worktree.path.display().to_string();
    if let Some(reason) = &worktree.locked {
        // An empty reason is what `git worktree lock` records when it was given
        // none, and saying "locked" twice is better than a dangling colon.
        tip = if reason.is_empty() {
            format!("{tip} — locked")
        } else {
            format!("{tip} — locked: {reason}")
        };
    }
    if worktree.prunable {
        tip = format!("{tip} — the directory is gone; `git worktree prune` clears it");
    }

    let row = hinted(
        container(label)
            .width(Fill)
            .padding(Padding::from([3, 12]))
            .into(),
        tip,
        palette,
    );

    let Some(sheet) = worktree_sheet(worktree, index) else {
        return row;
    };

    row![
        row,
        action_button(
            "⋯",
            format!("Actions for {}", worktree.path.display()),
            sheet,
            palette
        ),
    ]
    .align_y(Center)
    .into()
}

/// What a worktree row offers, or `None` when it has nothing to offer.
///
/// Three rows have nothing. The **main** worktree cannot be removed and cannot
/// be locked, so every action here is refused for it. The **current** one
/// cannot be removed either — `git worktree remove` will not take the directory
/// you are standing in — and offering it would be a control that always fails.
/// A **locked** one is locked precisely so that nothing removes or prunes it;
/// unlocking is how that is undone, and hideGit does not offer a way around a
/// decision the user already made.
fn worktree_sheet(worktree: &Worktree, index: usize) -> Option<ActionSheet> {
    if worktree.is_main || worktree.is_current || worktree.locked.is_some() {
        return None;
    }

    let at = worktree.path.display().to_string();
    if worktree.prunable {
        // Not "remove": there is no directory left to remove. Pruning is what
        // clears the registration, and it is the only thing that would work.
        return Some(
            ActionSheet::new(format!("{at} is registered, and its directory is gone")).item(
                "Clear the stale registration",
                Message::Repo(index, RepoMessage::WorktreePruneRequested),
            ),
        );
    }

    Some(ActionSheet::new(at).destructive(
        "Remove",
        Message::Repo(
            index,
            RepoMessage::WorktreeRemoveRequested {
                path: worktree.path.clone(),
            },
        ),
    ))
}

/// What a worktree has checked out, in the vocabulary Git uses for it.
fn worktree_head(worktree: &Worktree) -> String {
    match &worktree.head {
        Some(Head::Branch { name, .. }) => name.short.clone(),
        // Git's own word. `git worktree list` prints `(detached HEAD)`, and
        // inventing a synonym would teach something unusable elsewhere.
        Some(Head::Detached { target }) => format!("detached at {}", target.short(7)),
        Some(Head::Unborn { name }) => format!("{} (unborn)", name.short),
        None => "unreadable".to_owned(),
    }
}

/// What a submodule row offers, or `None` when it has nothing to offer.
///
/// Separate from the row so the decision is a value a test can read rather than
/// a widget tree it would have to click through.
///
/// A submodule already at the recorded commit gets nothing: `git submodule
/// update` would run and change nothing, and a control that does nothing is
/// worse than no control.
fn submodule_sheet(submodule: &Submodule, index: usize) -> Option<ActionSheet> {
    let path = submodule.path.clone();
    let request = |init| {
        Message::Repo(
            index,
            RepoMessage::SubmoduleUpdateRequested {
                path: path.clone(),
                init,
            },
        )
    };

    match submodule.state() {
        SubmoduleState::Current => None,
        SubmoduleState::Uninitialised => Some(
            ActionSheet::new(format!(
                "{} is declared but not checked out",
                path.display()
            ))
            // `--init`, because there is nothing set up to update.
            .item("Set up and check out", request(true)),
        ),
        // Not destructive: `git submodule update` refuses rather than
        // discarding when the nested checkout has uncommitted work, and the
        // commits it moves off stay in the nested repository's own reflog. No
        // `--init` either — a submodule that moved is already set up, and
        // asking for it anyway would be asking for something the user did not.
        SubmoduleState::Moved => Some(
            ActionSheet::new(format!(
                "{} is not at the commit the superproject records",
                path.display()
            ))
            .item("Return it to the recorded commit", request(false)),
        ),
    }
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

fn tag_row<'a>(
    repo: &OpenRepo,
    tag: &Tag,
    index: usize,
    palette: &Palette,
) -> Element<'a, Message> {
    let palette = *palette;
    // Annotated and lightweight tags look different, because they are.
    let glyph = if tag.annotated { "◆" } else { "◇" };
    let short = tag.name.short.clone();

    let label = row![
        text(glyph).size(ITEM_SIZE).color(palette.warning),
        text(short.clone()).size(ITEM_SIZE).color(palette.muted),
    ]
    .spacing(6)
    .align_y(Center);

    let mut sheet = ActionSheet::new(format!(
        "{short} · {}",
        if tag.annotated {
            "annotated"
        } else {
            "lightweight"
        }
    ))
    .item(
        "Checkout (detaches HEAD)",
        Message::Repo(
            index,
            RepoMessage::CheckoutRequested(CheckoutTarget::Commit(tag.target)),
        ),
    )
    .item(
        "New branch from here…",
        Message::PromptRequested(Box::new(new_branch_prompt(StartPoint::Ref(
            tag.name.full.clone(),
        )))),
    );

    // Pushing a tag needs somewhere to push it to.
    if let Some(remote) = repo.default_remote() {
        sheet = sheet.item(
            format!("Push to {remote}"),
            Message::Repo(
                index,
                RepoMessage::TagPushRequested {
                    remote,
                    name: short.clone(),
                },
            ),
        );
    }

    sheet = sheet.destructive(
        "Delete",
        Message::Repo(index, RepoMessage::TagDeleteRequested(short.clone())),
    );

    row![
        button(container(label).padding(Padding::from([3, 12])))
            .width(Fill)
            .padding(0)
            .style(move |_, status| item_style(palette, false, status))
            .on_press(Message::Repo(
                index,
                RepoMessage::Selected(Selection::Commit(tag.target)),
            )),
        action_button("⋯", format!("Actions for {short}"), sheet, palette),
    ]
    .align_y(Center)
    .into()
}

pub(crate) fn item_style(
    palette: Palette,
    selected: bool,
    status: button::Status,
) -> button::Style {
    let background = match (selected, status) {
        // From the theme, not the accent at an alpha: see `Palette::selection`.
        (true, _) => Some(palette.selection.into()),
        // Hover stays an alpha of the *text* colour, which inverts correctly on
        // its own — it is a neutral darkening on light and a neutral lightening
        // on dark, with no hue to go muddy.
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
    use std::path::PathBuf;

    fn submodule(recorded: Option<&str>, checked_out: Option<&str>) -> Submodule {
        let id = |hex: &str| hidegit_core::ObjectId::from_hex(&hex.repeat(40)).unwrap();

        Submodule {
            name: "vendor/lib".to_owned(),
            path: PathBuf::from("vendor/lib"),
            url: "https://example.invalid/lib.git".to_owned(),
            branch: None,
            recorded: recorded.map(id),
            checked_out: checked_out.map(id),
        }
    }

    fn linked(name: &str) -> Worktree {
        Worktree {
            path: PathBuf::from(format!("/elsewhere/{name}")),
            head: Some(Head::Branch {
                name: RefName {
                    kind: RefKind::LocalBranch,
                    full: format!("refs/heads/{name}"),
                    short: name.to_owned(),
                },
                target: hidegit_core::ObjectId::from_hex(&"0".repeat(40)).unwrap(),
            }),
            is_current: false,
            is_main: false,
            locked: None,
            prunable: false,
        }
    }

    #[test]
    fn the_worktree_being_looked_at_does_not_count_as_holding_a_branch_elsewhere() {
        // It holds it, but it is not *elsewhere* — and treating it as such
        // would strip the checkout action off every branch in the repository
        // the moment a second worktree existed.
        let mut here = linked("main");
        here.is_current = true;
        assert!(!held_elsewhere(&[here], "refs/heads/main"));
    }

    #[test]
    fn another_worktree_on_the_branch_counts_and_a_different_one_does_not() {
        let side = linked("side");
        assert!(held_elsewhere(
            std::slice::from_ref(&side),
            "refs/heads/side"
        ));
        assert!(!held_elsewhere(&[side], "refs/heads/other"));
    }

    #[test]
    fn a_branch_offers_a_new_worktree_where_it_offers_a_checkout() {
        // A worktree is made *out of* a branch, so this is the row it belongs
        // on — and the `WORKTREES` heading is absent on exactly the repository
        // where somebody wants to make a second checkout.
        let sheet = branch_sheet(
            &branch("feat/graph", None),
            0,
            false,
            2,
            Some("main"),
            false,
        );

        assert!(
            sheet.items.iter().any(|item| matches!(
                &item.message,
                Message::Repo(0, RepoMessage::WorktreeDestinationRequested(name))
                    if name == "feat/graph"
            )),
            "no way to make a worktree from the branch it would hold: {:?}",
            sheet.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_branch_another_worktree_holds_offers_neither_checkout_nor_a_second_one() {
        // Git refuses both outright — a branch checked out in one worktree
        // cannot be checked out in another — and a control that always fails is
        // worse than an absent one.
        let sheet = branch_sheet(&branch("feat/graph", None), 0, false, 2, Some("main"), true);

        assert!(
            !sheet.items.iter().any(|item| matches!(
                &item.message,
                Message::Repo(_, RepoMessage::CheckoutRequested(_))
                    | Message::Repo(_, RepoMessage::WorktreeDestinationRequested(_))
            )),
            "offered something git would refuse: {:?}",
            sheet.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        assert!(
            sheet
                .items
                .iter()
                .any(|item| item.label.starts_with("Merge")),
            "the actions that still work are still there"
        );
    }

    #[test]
    fn the_main_worktree_offers_nothing_because_every_action_would_be_refused() {
        let mut main = linked("main");
        main.is_main = true;
        assert!(worktree_sheet(&main, 0).is_none());
    }

    #[test]
    fn the_worktree_being_looked_at_cannot_be_removed_from_inside_it() {
        // `git worktree remove` will not take the directory you are standing
        // in, so offering it would be a control that always fails.
        let mut current = linked("side");
        current.is_current = true;
        assert!(worktree_sheet(&current, 0).is_none());
    }

    #[test]
    fn a_locked_worktree_offers_nothing_rather_than_a_way_around_the_lock() {
        // Locking one is how the user says "leave this alone". Unlocking is how
        // that is undone; hideGit does not offer a route past a decision the
        // user already made.
        let mut locked = linked("side");
        locked.locked = Some("on the external drive".to_owned());
        assert!(worktree_sheet(&locked, 0).is_none());

        // And a locked one whose directory is gone is still locked — which is
        // exactly the case locking exists for.
        locked.prunable = true;
        assert!(worktree_sheet(&locked, 0).is_none());
    }

    #[test]
    fn a_live_worktree_offers_removal_marked_destructive() {
        let sheet = worktree_sheet(&linked("side"), 0).expect("there is something to do");

        assert!(matches!(
            sheet.items.as_slice(),
            [item] if item.destructive
                && matches!(
                    &item.message,
                    Message::Repo(0, RepoMessage::WorktreeRemoveRequested { path })
                        if path == &PathBuf::from("/elsewhere/side")
                )
        ));
    }

    #[test]
    fn a_worktree_whose_directory_is_gone_offers_pruning_rather_than_removal() {
        // There is no directory left to remove, so "Remove" would be the wrong
        // verb for the only operation that would work.
        let mut stale = linked("side");
        stale.prunable = true;
        let sheet = worktree_sheet(&stale, 0).expect("there is something to do");

        assert!(matches!(
            sheet.items.as_slice(),
            [item] if matches!(
                &item.message,
                Message::Repo(0, RepoMessage::WorktreePruneRequested)
            )
        ));
    }

    #[test]
    fn a_submodule_already_at_the_recorded_commit_offers_nothing() {
        // An update would run and change nothing. A control that does nothing
        // is worse than no control.
        assert!(submodule_sheet(&submodule(Some("a"), Some("a")), 0).is_none());
    }

    #[test]
    fn a_submodule_with_no_checkout_offers_to_set_one_up() {
        let sheet =
            submodule_sheet(&submodule(Some("a"), None), 0).expect("there is something to do here");

        assert!(
            sheet.title.contains("vendor/lib"),
            "the sheet names what it acts on: {}",
            sheet.title
        );
        assert!(matches!(
            sheet.items.as_slice(),
            [item] if matches!(
                &item.message,
                Message::Repo(0, RepoMessage::SubmoduleUpdateRequested { path, init: true })
                    if path == &PathBuf::from("vendor/lib")
            )
        ));
    }

    #[test]
    fn a_submodule_that_moved_offers_to_return_it_without_asking_for_init() {
        // It is already set up, so `--init` would be asking for something the
        // user did not.
        let sheet = submodule_sheet(&submodule(Some("a"), Some("b")), 0)
            .expect("there is something to do here");

        assert!(matches!(
            sheet.items.as_slice(),
            [item] if matches!(
                &item.message,
                Message::Repo(0, RepoMessage::SubmoduleUpdateRequested { init: false, .. })
            )
        ));
        assert!(
            !sheet.items[0].destructive,
            "git refuses rather than discarding uncommitted work, and the commits stay in the \
             nested reflog"
        );
    }

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
    fn merge_and_rebase_name_both_branches() {
        // "Merge" alone leaves the direction to be guessed, and guessing the
        // direction wrong is the classic way to ruin an afternoon.
        let other = branch("feature", None);
        let sheet = branch_sheet(&other, 0, false, 2, Some("main"), false);
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();

        assert!(
            labels.contains(&"Merge feature into main"),
            "got {labels:?}"
        );
        assert!(
            labels.contains(&"Rebase main onto feature…"),
            "got {labels:?}"
        );
    }

    #[test]
    fn the_current_branch_offers_neither_merge_nor_rebase() {
        // Both would be a no-op Git refuses.
        let head = branch("main", None);
        let sheet = branch_sheet(&head, 0, true, 2, Some("main"), false);
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();

        assert!(
            !labels.iter().any(|l| l.starts_with("Merge")),
            "got {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with("Rebase")),
            "got {labels:?}"
        );
    }

    #[test]
    fn a_detached_head_has_no_branch_to_merge_into() {
        let other = branch("feature", None);
        let sheet = branch_sheet(&other, 0, false, 2, None, false);
        let labels: Vec<&str> = sheet.items.iter().map(|i| i.label.as_str()).collect();

        assert!(
            !labels.iter().any(|l| l.starts_with("Merge")),
            "got {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.starts_with("Rebase")),
            "got {labels:?}"
        );
    }

    #[test]
    fn a_sheet_does_not_offer_actions_that_would_be_refused() {
        // Checking out the branch you are already on is a no-op, and deleting it
        // is something Git refuses outright. An action that cannot work is worse
        // than an absent one.
        let head = branch("main", Some("refs/remotes/origin/main"));
        let sheet = branch_sheet(&head, 0, true, 2, Some("main"), false);
        let offered = labels(&sheet);

        assert!(!offered.contains(&"Checkout"), "already on it: {offered:?}");
        assert!(!offered.contains(&"Delete"), "standing on it: {offered:?}");
        assert!(offered.contains(&"Rename…"), "renaming HEAD is fine");
    }

    #[test]
    fn a_branch_that_is_not_checked_out_offers_everything() {
        let other = branch("feat/graph", None);
        let sheet = branch_sheet(&other, 0, false, 2, Some("main"), false);
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
        let sheet = branch_sheet(&only, 0, false, 1, Some("main"), false);
        let offered = labels(&sheet);

        assert!(!offered.contains(&"Delete"), "got {offered:?}");
    }

    #[test]
    fn a_deletion_is_marked_destructive_so_it_does_not_look_like_the_rest() {
        let other = branch("feat/graph", None);
        let sheet = branch_sheet(&other, 0, false, 2, Some("main"), false);

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
