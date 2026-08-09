//! Pull requests: the sidebar section, and the detail pane behind a row.
//!
//! The panel's job is to make four states that look alike in a list read as
//! four different things — not signed in, signed in but hideGit cannot see this
//! repository, nothing open, and "this is what it looked like before the
//! network went". Each carries its own next action rather than an absence.

use hidegit_forge::{
    CheckState, MergeState, PrRole, PullRequest, PullRequestDetail, ReviewState, ReviewVerdict,
};
use iced::widget::{Space, button, column, container, row, scrollable, text, tooltip};
use iced::{Center, Color, Fill, Font, Padding};

use crate::Element;
use crate::format;
use crate::message::{Message, RepoMessage};
use crate::state::{App, OpenRepo, PrState, Prompt, PromptField, PromptKind, Selection};
use crate::theme::Palette;

const HEADING_SIZE: f32 = 11.0;
const ITEM_SIZE: f32 = 13.0;

/// The `PULL REQUESTS` section.
///
/// Absent entirely when the repository has no forge remote: a repository whose
/// only remote is a path on disk has no pull requests to have, and a heading
/// saying "you have none" would be answering a question nobody asked.
pub fn section<'a>(
    app: &'a App,
    repo: &'a OpenRepo,
    index: usize,
    palette: &Palette,
) -> Option<Element<'a, Message>> {
    repo.prs.repo.as_ref()?;

    let palette = *palette;
    let mut section = column![].spacing(2);

    section = section.push(crate::widget::sidebar::section_heading(
        "PULL REQUESTS",
        repo.prs.items.len(),
        create_action(app, repo),
        &palette,
    ));

    if app.forge.no_keychain {
        return Some(
            section
                .push(note(
                    "hideGit cannot store a token on this machine, so pull requests are off. \
                     A keychain — Secret Service on Linux — is what it needs.",
                    palette,
                ))
                .into(),
        );
    }

    if !app.forge.is_connected() {
        return Some(
            section
                .push(action_row(
                    "Connect to GitHub",
                    Message::ConnectRequested,
                    palette,
                ))
                .into(),
        );
    }

    match &repo.prs.state {
        PrState::Idle | PrState::Loading => {
            return Some(section.push(note("Loading…", palette)).into());
        }
        // Its own row rather than an empty list. "You have no open pull
        // requests" and "hideGit cannot see this repository" look identical as
        // an absence and mean opposite things.
        PrState::NotInstalled { install_url } => {
            return Some(
                section
                    .push(note(
                        "hideGit is not installed on this repository, so it cannot read its \
                         pull requests.",
                        palette,
                    ))
                    .push(action_row(
                        "Install hideGit here",
                        Message::OpenUrl(install_url.clone()),
                        palette,
                    ))
                    .into(),
            );
        }
        // An indicator, never a dialog. What was last known stays on screen
        // below it, because stale information beats none.
        PrState::Stale(why) => {
            section = section.push(stale(why, palette));
        }
        PrState::Loaded => {}
    }

    if repo.prs.items.is_empty() {
        return Some(section.push(note("No open pull requests", palette)).into());
    }

    for (role, items) in repo.prs.grouped() {
        section = section.push(role_heading(role, palette));
        for pr in items {
            section = section.push(row_for(repo, pr, index, palette));
        }
    }

    Some(section.push(footer(app, index, palette)).into())
}

/// Who you are signed in as, and the two things you can do about it.
///
/// In the panel rather than in a settings screen, because this is where the
/// question "why am I not seeing my pull requests?" gets asked — and the answer
/// is often "as the wrong account".
fn footer<'a>(app: &App, index: usize, palette: Palette) -> Element<'a, Message> {
    let who = app
        .forge
        .identity
        .as_ref()
        .map_or_else(String::new, |i| i.login.clone());

    container(
        row![
            text(who).size(HEADING_SIZE).color(palette.muted),
            Space::new().width(Fill),
            link(
                "Refresh",
                Message::Repo(index, RepoMessage::PrsRefreshRequested),
                palette
            ),
            link("Sign out", Message::DisconnectRequested, palette),
        ]
        .spacing(8)
        .align_y(Center),
    )
    .padding(Padding::from([6, 12]))
    .into()
}

fn link<'a>(label: &'a str, message: Message, palette: Palette) -> Element<'a, Message> {
    button(text(label).size(HEADING_SIZE).color(palette.accent))
        .padding(0)
        .style(move |_, status| crate::widget::sidebar::item_style(palette, false, status))
        .on_press(message)
        .into()
}

/// The `+` on the heading: open a pull request from the branch you are on.
///
/// Offered only when there is somewhere for it to go — a detached HEAD has no
/// branch to open one from, and an action that cannot work is absent rather
/// than present and refusing.
fn create_action(app: &App, repo: &OpenRepo) -> Option<(&'static str, Message)> {
    if !app.forge.is_connected() || matches!(repo.prs.state, PrState::NotInstalled { .. }) {
        return None;
    }

    let hidegit_core::model::Head::Branch { name, .. } = &repo.head else {
        return None;
    };
    let head = name.short.clone();

    // The base is the repository's own default as GitHub knows it, which
    // hideGit does not — so it takes the branch the upstream tracks, and the
    // prompt shows it rather than deciding silently.
    let base = repo
        .refs
        .locals
        .iter()
        .find(|b| b.name.short == "main")
        .or_else(|| repo.refs.locals.iter().find(|b| b.name.short == "master"))
        .map_or_else(|| "main".to_owned(), |b| b.name.short.clone());

    if head == base {
        return None;
    }

    Some((
        "New pull request…",
        Message::PromptRequested(Box::new(Prompt {
            title: format!("Open a pull request: {head} → {base}"),
            kind: PromptKind::NewPullRequest { head, base },
            confirm_label: "Open".to_owned(),
            fields: vec![
                PromptField::new("Title", "what this changes"),
                PromptField::new("Description (optional)", "why"),
            ],
        })),
    ))
}

/// A group heading: YOURS, AWAITING YOUR REVIEW, ASSIGNED TO YOU.
fn role_heading<'a>(role: PrRole, palette: Palette) -> Element<'a, Message> {
    container(
        text(role.heading())
            .size(HEADING_SIZE - 1.0)
            .color(palette.muted),
    )
    .padding(Padding::from([4, 12]))
    .into()
}

/// One pull request.
///
/// Two glyphs, always in the same two positions: check state, then review
/// state. A fixed layout is what lets the column be read down rather than
/// each row parsed.
fn row_for<'a>(
    repo: &OpenRepo,
    pr: &PullRequest,
    index: usize,
    palette: Palette,
) -> Element<'a, Message> {
    let selected = matches!(repo.selection, Some(Selection::PullRequest(n)) if n == pr.number);
    let number = pr.number;

    let (check_glyph, check_colour) = check_marks(pr.checks, palette);
    let (review_glyph, review_colour) = review_marks(pr.review, palette);

    let title = if pr.draft {
        format!("#{number} {} (draft)", pr.title)
    } else {
        format!("#{number} {}", pr.title)
    };

    let mut label = row![
        text(check_glyph).size(ITEM_SIZE).color(check_colour),
        text(review_glyph).size(ITEM_SIZE).color(review_colour),
        text(title).size(ITEM_SIZE).color(if pr.draft {
            palette.muted
        } else {
            palette.text
        }),
    ]
    .spacing(6)
    .align_y(Center);

    // Only when it is *known* to conflict. `MergeState::Unknown` is GitHub
    // still computing, and a conflict marker on it would be a guess the user
    // acts on.
    if pr.merge == MergeState::Conflicting {
        label = label.push(Space::new().width(Fill));
        label = label.push(text("⚠").size(ITEM_SIZE).color(palette.warning));
    }

    row![
        button(container(label).padding(Padding::from([3, 12])))
            .width(Fill)
            .padding(0)
            .style(move |_, status| crate::widget::sidebar::item_style(palette, selected, status))
            .on_press(Message::Repo(
                index,
                RepoMessage::Selected(Selection::PullRequest(number)),
            )),
        open_button(index, number, palette),
    ]
    .align_y(Center)
    .into()
}

fn open_button<'a>(index: usize, number: u64, palette: Palette) -> Element<'a, Message> {
    let control = button(
        container(text("↗").size(ITEM_SIZE).font(Font::MONOSPACE)).padding(Padding::from([3, 8])),
    )
    .padding(0)
    .style(move |_, status| crate::widget::sidebar::item_style(palette, false, status))
    .on_press(Message::Repo(index, RepoMessage::PrOpenRequested(number)));

    tooltip(
        control,
        container(
            text(format!("Open #{number} on GitHub"))
                .size(11.0)
                .color(palette.text),
        )
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

/// `CheckState` as a glyph and a colour.
///
/// Colour alone would not do: `Passing` and `Failing` differ only in hue, and
/// the glyph is what carries the meaning for anyone who cannot tell them apart.
fn check_marks(state: CheckState, palette: Palette) -> (&'static str, Color) {
    match state {
        // Not "no checks have run" — nothing is configured, so nothing is said.
        CheckState::None => (" ", palette.muted),
        CheckState::Pending => ("◌", palette.muted),
        CheckState::Passing => ("✓", palette.success),
        CheckState::Failing => ("✗", palette.danger),
    }
}

fn review_marks(state: ReviewState, palette: Palette) -> (&'static str, Color) {
    match state {
        ReviewState::NotRequired => (" ", palette.muted),
        ReviewState::Required => ("◍", palette.muted),
        ReviewState::Approved => ("✓", palette.success),
        ReviewState::ChangesRequested => ("↺", palette.warning),
    }
}

fn note<'a>(message: &'a str, palette: Palette) -> Element<'a, Message> {
    container(text(message).size(ITEM_SIZE - 1.0).color(palette.muted))
        .padding(Padding::from([4, 12]))
        .into()
}

/// The stale marker: a status indicator, never a dialog.
fn stale<'a>(why: &str, palette: Palette) -> Element<'a, Message> {
    let label = format!("Showing the last known state — {why}");
    container(text(label).size(HEADING_SIZE).color(palette.warning))
        .padding(Padding::from([4, 12]))
        .into()
}

/// A row that reads as the next action rather than as an absence.
fn action_row<'a>(label: &'a str, message: Message, palette: Palette) -> Element<'a, Message> {
    button(
        container(text(label).size(ITEM_SIZE).color(palette.accent))
            .padding(Padding::from([4, 12])),
    )
    .width(Fill)
    .padding(0)
    .style(move |_, status| crate::widget::sidebar::item_style(palette, false, status))
    .on_press(message)
    .into()
}

// ---- the detail pane -----------------------------------------------------

/// One pull request, opened from a row.
pub fn detail<'a>(detail: &'a PullRequestDetail, palette: &'a Palette) -> Element<'a, RepoMessage> {
    let pr = &detail.pr;
    let palette = *palette;

    let heading = column![
        text(format!("#{} {}", pr.number, pr.title))
            .size(16.0)
            .color(palette.text)
            .font(Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }),
        text(format!(
            "{} · {} → {} · {}",
            pr.author,
            pr.head,
            pr.base,
            format::relative_time(pr.updated),
        ))
        .size(12.0)
        .color(palette.muted),
    ]
    .spacing(4);

    let mut facts = row![].spacing(16).align_y(Center);
    facts = facts.push(fact(
        check_label(pr.checks),
        check_marks(pr.checks, palette).1,
    ));
    facts = facts.push(fact(
        review_label(pr.review),
        review_marks(pr.review, palette).1,
    ));
    facts = facts.push(fact(merge_label(pr.merge), merge_colour(pr.merge, palette)));

    let stats = text(format!(
        "{} commit(s) · {} file(s) · +{} −{}",
        detail.commits, detail.changed_files, detail.additions, detail.deletions
    ))
    .size(12.0)
    .color(palette.muted);

    let body: Element<'_, RepoMessage> = if detail.body.trim().is_empty() {
        text("No description")
            .size(13.0)
            .color(palette.muted)
            .into()
    } else {
        text(detail.body.as_str())
            .size(13.0)
            .color(palette.text)
            .into()
    };

    let mut reviews = column![].spacing(4);
    if !detail.reviews.is_empty() {
        reviews = reviews.push(text("REVIEWS").size(HEADING_SIZE).color(palette.muted));
        for review in &detail.reviews {
            reviews = reviews.push(
                text(format!("{} — {}", review.author, verdict(review.verdict)))
                    .size(12.0)
                    .color(palette.text),
            );
        }
    }

    let open = button(text("Open on GitHub").size(12.0))
        .padding(Padding::from([4, 10]))
        .style(move |_, status| crate::widget::sidebar::item_style(palette, false, status))
        .on_press(RepoMessage::PrOpenRequested(pr.number));

    scrollable(
        column![
            heading,
            facts,
            stats,
            Space::new().height(8),
            body,
            Space::new().height(8),
            reviews,
            Space::new().height(8),
            open,
        ]
        .spacing(8)
        .padding(16),
    )
    .height(Fill)
    .into()
}

fn fact<'a>(label: String, colour: Color) -> Element<'a, RepoMessage> {
    text(label).size(12.0).color(colour).into()
}

fn check_label(state: CheckState) -> String {
    match state {
        CheckState::None => "No checks".to_owned(),
        CheckState::Pending => "Checks running".to_owned(),
        CheckState::Passing => "Checks passing".to_owned(),
        CheckState::Failing => "Checks failing".to_owned(),
    }
}

fn review_label(state: ReviewState) -> String {
    match state {
        ReviewState::NotRequired => "No review required".to_owned(),
        ReviewState::Required => "Review required".to_owned(),
        ReviewState::Approved => "Approved".to_owned(),
        ReviewState::ChangesRequested => "Changes requested".to_owned(),
    }
}

/// `Unknown` says so rather than guessing.
///
/// GitHub computes mergeability lazily, so this is the ordinary answer for the
/// first poll after a push — "checking" is what it is doing, and claiming
/// either outcome would be a coin toss the user acts on.
fn merge_label(state: MergeState) -> String {
    match state {
        MergeState::Mergeable => "No conflicts".to_owned(),
        MergeState::Conflicting => "Conflicts with the base branch".to_owned(),
        MergeState::Unknown => "Checking for conflicts".to_owned(),
    }
}

fn merge_colour(state: MergeState, palette: Palette) -> Color {
    match state {
        MergeState::Mergeable => palette.success,
        MergeState::Conflicting => palette.warning,
        MergeState::Unknown => palette.muted,
    }
}

fn verdict(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Approved => "approved",
        ReviewVerdict::ChangesRequested => "requested changes",
        ReviewVerdict::Commented => "commented",
        ReviewVerdict::Dismissed => "dismissed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidegit_forge::{PrRole, PullRequest};
    use std::collections::BTreeSet;
    use time::OffsetDateTime;

    fn pr(number: u64, roles: &[PrRole]) -> PullRequest {
        PullRequest {
            number,
            title: "feat: something".to_owned(),
            url: format!("https://github.com/youhide/hideGit/pull/{number}"),
            author: "youhide".to_owned(),
            head: "feat/x".to_owned(),
            base: "main".to_owned(),
            draft: false,
            updated: OffsetDateTime::UNIX_EPOCH,
            roles: roles.iter().copied().collect::<BTreeSet<_>>(),
            review: ReviewState::Required,
            checks: CheckState::Passing,
            merge: MergeState::Mergeable,
            comments: 0,
        }
    }

    #[test]
    fn a_pull_request_is_listed_once_under_its_strongest_role() {
        // Listing one you wrote *and* were assigned to under two headings would
        // make the section's count disagree with what is under it.
        let panel = crate::state::PrPanel {
            repo: None,
            items: vec![pr(47, &[PrRole::Author, PrRole::Assignee])],
            state: PrState::Loaded,
            ..crate::state::PrPanel::default()
        };

        let grouped = panel.grouped();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0, PrRole::Author);
        assert_eq!(grouped[0].1.len(), 1);
    }

    #[test]
    fn headings_are_ordered_by_how_strong_the_claim_on_you_is() {
        let panel = crate::state::PrPanel {
            repo: None,
            items: vec![
                pr(1, &[PrRole::Assignee]),
                pr(2, &[PrRole::Reviewer]),
                pr(3, &[PrRole::Author]),
            ],
            state: PrState::Loaded,
            ..crate::state::PrPanel::default()
        };

        let roles: Vec<PrRole> = panel.grouped().into_iter().map(|(role, _)| role).collect();
        assert_eq!(
            roles,
            vec![PrRole::Author, PrRole::Reviewer, PrRole::Assignee]
        );
    }

    #[test]
    fn a_heading_with_nothing_under_it_is_not_rendered() {
        let panel = crate::state::PrPanel {
            repo: None,
            items: vec![pr(1, &[PrRole::Author])],
            state: PrState::Loaded,
            ..crate::state::PrPanel::default()
        };

        assert_eq!(panel.grouped().len(), 1, "only the one that has items");
    }

    #[test]
    fn no_checks_configured_shows_nothing_rather_than_a_pending_marker() {
        // "Nothing is configured" and "nothing has reported yet" look identical
        // if both get a spinner, and they mean different things.
        let palette = Palette::DARK;
        assert_eq!(check_marks(CheckState::None, palette).0, " ");
        assert_ne!(check_marks(CheckState::Pending, palette).0, " ");
    }

    #[test]
    fn check_and_review_state_are_distinguishable_without_colour() {
        // Colour alone would leave passing and failing differing only in hue.
        let palette = Palette::DARK;
        let glyphs = [
            check_marks(CheckState::Passing, palette).0,
            check_marks(CheckState::Failing, palette).0,
            check_marks(CheckState::Pending, palette).0,
        ];
        let unique: BTreeSet<&str> = glyphs.iter().copied().collect();
        assert_eq!(unique.len(), glyphs.len(), "each state has its own glyph");
    }

    #[test]
    fn an_unknown_merge_state_says_it_is_checking_rather_than_claiming_either_answer() {
        assert_eq!(merge_label(MergeState::Unknown), "Checking for conflicts");
    }
}
