//! The commit graph canvas.
//!
//! Virtualised: only the rows intersecting the viewport, plus overscan, are
//! laid out and drawn. Row height is fixed, so a scroll position maps to a row
//! index by arithmetic and hit testing is a division rather than a search
//! through per-node geometry. That is what keeps a 100,000-commit history
//! cheap to scroll.
//!
//! Geometry comes from `hidegit-core`; everything here is theme, colour and
//! interaction. See `docs/COMMIT_GRAPH.md#rendering`.

use hidegit_core::graph::{EdgeRole, NodeKind};
use hidegit_core::model::{RefKind, RefName};
use iced::widget::canvas::{self, Frame, Path, Stroke, Text};
use iced::{Color, Font, Pixels, Point, Rectangle, Renderer, Size, Vector, mouse};

use crate::format;
use crate::message::RepoMessage;
use crate::state::{GraphView, ROW_HEIGHT, Selection};
use crate::theme::Palette;

/// Horizontal distance between lanes.
const LANE_WIDTH: f32 = 14.0;
/// Space before the first lane.
const LANE_LEFT: f32 = 14.0;
/// Radius of a commit node.
const NODE_RADIUS: f32 = 4.0;
/// Widest graph column before lanes collapse into an overflow indicator.
///
/// A repository with dozens of concurrent branches exceeds any sensible column
/// budget, and shrinking every column to fit makes all of them illegible.
const MAX_LANES: usize = 12;
/// Gap between the graph column and the summary text.
const TEXT_GAP: f32 = 16.0;
/// Width of the scrollbar gutter on the right.
const SCROLLBAR_WIDTH: f32 = 8.0;

/// How wide a strip along the right edge answers to a press.
///
/// Wider than the bar it draws, because an 8px target is a target people miss —
/// and missing it selects a commit, which is the wrong thing to do by accident.
const SCROLLBAR_HIT: f32 = 16.0;

/// The shortest the thumb is allowed to get.
///
/// At a hundred thousand commits the proportional height is under a pixel, and
/// a thumb nobody can grab is the same as no thumb.
const MIN_THUMB: f32 = 24.0;

/// How far the pointer must travel before a press becomes a drag.
///
/// Small enough that a deliberate drag feels immediate, large enough that the
/// hand tremor in an ordinary click never arms a merge.
const DRAG_THRESHOLD: f32 = 6.0;

/// The height of a ref badge, shared by drawing and hit-testing.
const BADGE_HEIGHT: f32 = 16.0;

/// The width of a ref badge holding `label`.
///
/// Shared by drawing and hit-testing on purpose: two copies of this arithmetic
/// would drift, and the symptom would be a badge that responds a few pixels
/// away from where it is painted.
fn badge_width(label: &str) -> f32 {
    label.chars().count() as f32 * META_SIZE * CHAR_WIDTH + 12.0
}

const SUMMARY_SIZE: f32 = 13.0;
const META_SIZE: f32 = 12.0;
/// Rough advance width per character, for laying out badges without a text
/// measurement pass. Deliberately generous, so a badge never clips its label.
const CHAR_WIDTH: f32 = 0.58;

/// A drawable view of loaded history.
#[derive(Debug)]
pub struct GraphCanvas<'a> {
    pub view: &'a GraphView,
    pub palette: &'a Palette,
    pub selection: Option<&'a Selection>,
    pub focused: bool,
    /// How many whole rows fit, measured from the widget's real height.
    pub viewport_rows: usize,
    pub cache: &'a canvas::Cache,
}

/// Where the scrollbar's thumb is, and how far it can travel.
///
/// `None` when everything fits, which is also when no scrollbar is drawn — the
/// two have to agree or the bar would be draggable while invisible.
#[derive(Debug, Clone, Copy)]
struct Thumb {
    top: f32,
    height: f32,
    /// The distance the top may move, so a position maps back to a fraction.
    travel: f32,
}

/// What the canvas remembers between events.
///
/// A drag has to survive the cursor leaving the widget — releasing the button
/// over the sidebar must not leave the thumb stuck to the pointer — so it is
/// canvas state rather than something derived from the cursor each frame.
#[derive(Debug, Default)]
pub struct CanvasState {
    /// Where inside the thumb the drag started, in pixels from its top.
    grab: Option<f32>,
    /// A branch badge the pointer went down on, and where.
    ///
    /// A press is not yet a drag: it becomes one only once the pointer has
    /// moved past [`DRAG_THRESHOLD`], so clicking a badge still selects its
    /// commit instead of arming an operation nobody asked for.
    press: Option<Press>,
    /// True once the press has travelled far enough to be a drag.
    dragging: bool,
}

#[derive(Debug, Clone)]
struct Press {
    branch: RefName,
    at: Point,
}

impl GraphCanvas<'_> {
    /// The scrollbar's geometry, or `None` when there is nothing to scroll.
    fn thumb(&self, height: f32) -> Option<Thumb> {
        // Sized against the total reachable commits, not just the loaded ones,
        // so the thumb does not jump as pages arrive.
        let total = self.view.total.max(self.view.commits.len()).max(1) as f32;
        let visible = self.viewport_rows.max(1) as f32;
        if visible >= total {
            return None;
        }

        let thumb_height = (visible / total * height).max(MIN_THUMB);
        let travel = (height - thumb_height).max(0.0);
        let progress = (self.view.scroll / (total - visible)).clamp(0.0, 1.0);

        Some(Thumb {
            top: progress * travel,
            height: thumb_height,
            travel,
        })
    }

    /// The scroll fraction a thumb top of `y` corresponds to.
    fn fraction_at(&self, y: f32, thumb: Thumb) -> f32 {
        if thumb.travel <= 0.0 {
            return 0.0;
        }
        (y / thumb.travel).clamp(0.0, 1.0)
    }

    /// The row index under a point, or `None` past the end of history.
    ///
    /// Hit testing by arithmetic rather than by searching node geometry — the
    /// whole reason row height is fixed.
    fn row_at(&self, y: f32) -> Option<usize> {
        let row = (self.view.scroll + y / ROW_HEIGHT).floor();
        if row < 0.0 {
            return None;
        }
        let row = row as usize;
        (row < self.view.commits.len()).then_some(row)
    }

    fn lane_x(&self, lane: usize) -> f32 {
        LANE_LEFT + lane.min(MAX_LANES) as f32 * LANE_WIDTH
    }

    fn text_left(&self, lanes: usize) -> f32 {
        LANE_LEFT + lanes.min(MAX_LANES + 1) as f32 * LANE_WIDTH + TEXT_GAP
    }
}

impl canvas::Program<RepoMessage> for GraphCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<RepoMessage>> {
        // The widget is the only thing that knows its own height, so it is what
        // tells the state how many rows a viewport holds.
        let rows = (bounds.height / ROW_HEIGHT).floor().max(0.0) as usize;
        if rows != self.view.viewport_rows {
            return Some(canvas::Action::publish(RepoMessage::ViewportChanged(rows)));
        }

        // A drag in progress is tracked against the window rather than against
        // the widget: dragging past the bottom edge is how anyone scrolls to
        // the end, and `position_in` gives up the moment the cursor leaves.
        if let Some(grab) = state.grab {
            match event {
                iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    let y = cursor.position()?.y - bounds.y;
                    let thumb = self.thumb(bounds.height)?;
                    let fraction = self.fraction_at(y - grab, thumb);
                    return Some(
                        canvas::Action::publish(RepoMessage::GraphScrolledTo(fraction))
                            .and_capture(),
                    );
                }
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    state.grab = None;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                _ => return None,
            }
        }

        // A press on a branch badge that has travelled far enough becomes a
        // drag, and the drop is what arms an operation. Tracked against the
        // window like the scrollbar drag above, so leaving the widget and
        // coming back does not silently cancel it.
        if let Some(press) = state.press.clone() {
            match event {
                iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    let now = cursor.position()?;
                    if !state.dragging
                        && (now.x - press.at.x).hypot(now.y - press.at.y) > DRAG_THRESHOLD
                    {
                        state.dragging = true;
                    }
                    return state.dragging.then(canvas::Action::request_redraw);
                }
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    let was_dragging = state.dragging;
                    state.press = None;
                    state.dragging = false;
                    if !was_dragging {
                        // A press that never moved is a click, and the click
                        // already selected the commit on the way down.
                        return Some(canvas::Action::request_redraw());
                    }

                    let target = cursor
                        .position_in(bounds)
                        .and_then(|p| self.branch_at(p))
                        // Dropping a branch on itself asks for nothing.
                        .filter(|t| t.full != press.branch.full);

                    return Some(match target {
                        Some(target) => canvas::Action::publish(RepoMessage::BranchDropped {
                            source: press.branch.short.clone(),
                            target: target.short,
                        })
                        .and_capture(),
                        // Dropped on empty space, or on itself: nothing happens
                        // and nothing is said. An unfinished gesture is not an
                        // error to report.
                        None => canvas::Action::request_redraw().and_capture(),
                    });
                }
                _ => return None,
            }
        }

        let position = cursor.position_in(bounds)?;

        match event {
            iced::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let pixels = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -y * ROW_HEIGHT * 3.0,
                    mouse::ScrollDelta::Pixels { y, .. } => -y,
                };
                Some(canvas::Action::publish(RepoMessage::GraphScrolled(pixels)).and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // The scrollbar takes the click before the rows do. Without
                // this the only way to move through a hundred thousand commits
                // is the wheel, and clicking the bar selects whatever commit
                // happens to be behind it.
                if let Some(thumb) = self.thumb(bounds.height)
                    && position.x >= bounds.width - SCROLLBAR_HIT
                {
                    let on_thumb = (thumb.top..thumb.top + thumb.height).contains(&position.y);

                    // Clicking the track jumps the thumb under the cursor and
                    // then drags from its middle, so the page follows the
                    // pointer instead of leaping once and stopping.
                    let grab = if on_thumb {
                        position.y - thumb.top
                    } else {
                        thumb.height / 2.0
                    };
                    state.grab = Some(grab);

                    let fraction = self.fraction_at(position.y - grab, thumb);
                    return Some(
                        canvas::Action::publish(RepoMessage::GraphScrolledTo(fraction))
                            .and_capture(),
                    );
                }

                // A press on a branch badge might become a drag. It still
                // selects on the way down, so a badge behaves like the rest of
                // the row until the pointer actually moves.
                if let Some(branch) = self.branch_at(position) {
                    state.press = Some(Press {
                        branch,
                        at: cursor.position()?,
                    });
                }

                let row = self.row_at(position.y)?;
                let commit = self.view.commits.get(row)?;
                Some(
                    canvas::Action::publish(RepoMessage::Selected(Selection::Commit(commit.id)))
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        // Redraws that do not move the viewport or change the commit list reuse
        // the geometry rather than rebuilding it.
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            self.draw_rows(frame, bounds.size());
            self.draw_scrollbar(frame, bounds.size());
        });

        vec![geometry]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.grab.is_some() || state.dragging {
            return mouse::Interaction::Grabbing;
        }

        match cursor.position_in(bounds) {
            // The scrollbar is not a row, so it does not offer a row's cursor.
            Some(position)
                if self.thumb(bounds.height).is_some()
                    && position.x >= bounds.width - SCROLLBAR_HIT =>
            {
                mouse::Interaction::Grab
            }
            // A branch badge offers the grab hand, which is the only cue that
            // the gesture exists at all — the badges look identical otherwise.
            Some(position) if self.branch_at(position).is_some() => mouse::Interaction::Grab,
            Some(_) if !self.view.is_empty() => mouse::Interaction::Pointer,
            _ => mouse::Interaction::default(),
        }
    }
}

impl GraphCanvas<'_> {
    fn draw_rows(&self, frame: &mut Frame, size: Size) {
        let (from, layout) = self.view.layout_visible();
        if layout.rows.is_empty() {
            return;
        }

        let visible = self.view.visible_range();
        let text_left = self.text_left(layout.width);

        for row_index in visible.clone() {
            let Some(row) = layout.rows.get(row_index - from) else {
                continue;
            };
            let Some(commit) = self.view.commits.get(row_index) else {
                continue;
            };

            let top = (row_index as f32 - self.view.scroll) * ROW_HEIGHT;
            if top > size.height || top + ROW_HEIGHT < 0.0 {
                continue;
            }
            let middle = top + ROW_HEIGHT / 2.0;
            let bottom = top + ROW_HEIGHT;

            let selected = matches!(
                self.selection,
                Some(Selection::Commit(id)) if *id == commit.id
            );
            if selected {
                let tint = if self.focused {
                    Color {
                        a: 0.22,
                        ..self.palette.accent
                    }
                } else {
                    Color {
                        a: 0.10,
                        ..self.palette.accent
                    }
                };
                frame.fill_rectangle(
                    Point::new(0.0, top),
                    Size::new(size.width, ROW_HEIGHT),
                    tint,
                );
            }

            // Edges come out of the layout already sorted so pass-throughs are
            // drawn first and the lines that carry meaning land on top.
            for edge in &row.edges {
                let (x_from, x_to) = (self.lane_x(edge.from), self.lane_x(edge.to));
                let colour = self.palette.lane(edge.to.max(edge.from));

                let path = match edge.role {
                    EdgeRole::Continue => {
                        Path::line(Point::new(x_from, top), Point::new(x_from, bottom))
                    }
                    EdgeRole::Parent => {
                        Path::line(Point::new(x_from, middle), Point::new(x_from, bottom))
                    }
                    // Right angles read as noise at the density a graph
                    // reaches, so corners are quadratic curves.
                    EdgeRole::Merge => Path::new(|b| {
                        b.move_to(Point::new(x_from, middle));
                        b.quadratic_curve_to(Point::new(x_to, middle), Point::new(x_to, bottom));
                    }),
                    EdgeRole::Close => Path::new(|b| {
                        b.move_to(Point::new(x_from, top));
                        b.quadratic_curve_to(Point::new(x_from, middle), Point::new(x_to, middle));
                    }),
                };

                frame.stroke(&path, Stroke::default().with_color(colour).with_width(1.6));
            }

            // The half of this node's own lane that descends into it. Edges
            // describe what leaves a node, so without this every non-tip commit
            // would have a gap above it.
            if row.incoming {
                let x = self.lane_x(row.lane);
                frame.stroke(
                    &Path::line(Point::new(x, top), Point::new(x, middle)),
                    Stroke::default()
                        .with_color(self.palette.lane(row.lane))
                        .with_width(1.6),
                );
            }

            self.draw_node(frame, row.kind, self.lane_x(row.lane), middle, row.lane);

            let mut x = text_left;
            for name in &commit.refs {
                x = self.draw_badge(frame, name, x, middle);
            }

            let meta_width = 190.0;
            let summary_width = (size.width - x - meta_width - SCROLLBAR_WIDTH).max(40.0);
            frame.fill_text(Text {
                content: format::truncate(&commit.summary, summary_width),
                position: Point::new(x, middle),
                color: self.palette.text,
                size: Pixels(SUMMARY_SIZE),
                font: Font::DEFAULT,
                align_y: iced::alignment::Vertical::Center,
                ..Text::default()
            });

            let meta = format!(
                "{}   {}",
                format::truncate(&commit.author.name, 110.0),
                format::relative_time(commit.time)
            );
            frame.fill_text(Text {
                content: meta,
                position: Point::new(size.width - SCROLLBAR_WIDTH - 8.0, middle),
                color: self.palette.muted,
                size: Pixels(META_SIZE),
                font: Font::DEFAULT,
                align_x: iced::alignment::Horizontal::Right.into(),
                align_y: iced::alignment::Vertical::Center,
                ..Text::default()
            });
        }
    }

    /// Draws a commit node.
    ///
    /// Shape carries meaning independently of colour: a merge is hollow, a root
    /// is square, a boundary fades. A graph that only encodes structure in hue
    /// is unreadable for anyone with a colour vision deficiency.
    fn draw_node(&self, frame: &mut Frame, kind: NodeKind, x: f32, y: f32, lane: usize) {
        let colour = self.palette.lane(lane);
        let centre = Point::new(x, y);

        match kind {
            NodeKind::Normal => {
                frame.fill(&Path::circle(centre, NODE_RADIUS), colour);
            }
            NodeKind::Merge => {
                frame.fill(
                    &Path::circle(centre, NODE_RADIUS + 1.0),
                    self.palette.background,
                );
                frame.stroke(
                    &Path::circle(centre, NODE_RADIUS + 0.5),
                    Stroke::default().with_color(colour).with_width(2.0),
                );
            }
            NodeKind::Root => {
                let side = NODE_RADIUS * 1.8;
                frame.fill_rectangle(
                    Point::new(x - side / 2.0, y - side / 2.0),
                    Size::new(side, side),
                    colour,
                );
            }
            NodeKind::Boundary => {
                // Faded, and with no line running out of the bottom, so a
                // partially loaded history does not read as a truncated one.
                frame.fill(
                    &Path::circle(centre, NODE_RADIUS),
                    Color { a: 0.35, ..colour },
                );
            }
        }
    }

    /// Draws a branch or tag badge and returns the x to continue from.
    fn draw_badge(&self, frame: &mut Frame, name: &RefName, x: f32, middle: f32) -> f32 {
        let colour = match name.kind {
            RefKind::LocalBranch => self.palette.accent,
            RefKind::RemoteBranch => self.palette.muted,
            RefKind::Tag => self.palette.warning,
            RefKind::Special => self.palette.muted,
        };

        let label = format::truncate(&name.short, 140.0);
        let width = badge_width(&label);
        let height = BADGE_HEIGHT;

        frame.fill(
            &Path::rounded_rectangle(
                Point::new(x, middle - height / 2.0),
                Size::new(width, height),
                4.0.into(),
            ),
            Color { a: 0.18, ..colour },
        );
        frame.fill_text(Text {
            content: label,
            position: Point::new(x + 6.0, middle),
            color: colour,
            size: Pixels(META_SIZE),
            font: Font::DEFAULT,
            align_y: iced::alignment::Vertical::Center,
            ..Text::default()
        });

        x + width + 6.0
    }

    /// Which branch badge sits under `position`, if any.
    ///
    /// Walks the same geometry `draw_badge` lays out — through the same
    /// `badge_width`, so the box that responds and the box that is drawn cannot
    /// drift apart.
    fn branch_at(&self, position: Point) -> Option<RefName> {
        let row = self.row_at(position.y)?;
        let commit = self.view.commits.get(row)?;

        // The same arithmetic `draw_rows` uses, so the badge that responds is
        // the badge that was drawn.
        let middle = (row as f32 - self.view.scroll) * ROW_HEIGHT + ROW_HEIGHT / 2.0;
        if (position.y - middle).abs() > BADGE_HEIGHT / 2.0 {
            return None;
        }

        let (_, layout) = self.view.layout_visible();
        let mut x = self.text_left(layout.width);
        for name in &commit.refs {
            let width = badge_width(&format::truncate(&name.short, 140.0));
            if position.x >= x && position.x < x + width {
                // Tags are not draggable: there is no operation that merges or
                // rebases onto one, and offering the gesture would promise it.
                return matches!(name.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
                    .then(|| name.clone());
            }
            x += width + 6.0;
        }
        None
    }

    fn draw_scrollbar(&self, frame: &mut Frame, size: Size) {
        let Some(thumb) = self.thumb(size.height) else {
            return;
        };

        let x = size.width - SCROLLBAR_WIDTH;
        frame.fill_rectangle(
            Point::new(x, 0.0),
            Size::new(SCROLLBAR_WIDTH, size.height),
            Color {
                a: 0.35,
                ..self.palette.surface
            },
        );

        frame.fill(
            &Path::rounded_rectangle(
                Point::new(x + 1.5, thumb.top),
                Size::new(SCROLLBAR_WIDTH - 3.0, thumb.height),
                3.0.into(),
            ),
            Color {
                a: 0.55,
                ..self.palette.muted
            },
        );
    }
}

/// The offset a row sits at, for tests and for the detail pane's scroll sync.
pub fn row_offset(row: usize, scroll: f32) -> Vector {
    Vector::new(0.0, (row as f32 - scroll) * ROW_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidegit_core::model::{Commit, ObjectId, Signature};
    use time::OffsetDateTime;

    fn view_with(count: usize, scroll: f32) -> GraphView {
        let who = Signature {
            name: "test".into(),
            email: "t@example.invalid".into(),
            time: OffsetDateTime::UNIX_EPOCH,
        };
        let commits: Vec<Commit> = (0..count)
            .map(|i| Commit {
                id: ObjectId::from_hex(&format!("{i:040x}")).unwrap(),
                parents: Vec::new(),
                summary: format!("commit {i}"),
                body: None,
                author: who.clone(),
                committer: who.clone(),
                time: OffsetDateTime::UNIX_EPOCH,
                refs: Vec::new(),
            })
            .collect();

        let mut view = GraphView {
            total: count,
            scroll,
            viewport_rows: 10,
            ..GraphView::default()
        };
        view.append(commits);
        view
    }

    fn canvas_for<'a>(
        view: &'a GraphView,
        palette: &'a Palette,
        cache: &'a canvas::Cache,
    ) -> GraphCanvas<'a> {
        GraphCanvas {
            view,
            palette,
            selection: None,
            focused: true,
            viewport_rows: 10,
            cache,
        }
    }

    #[test]
    fn a_click_maps_to_a_row_by_arithmetic() {
        let view = view_with(100, 0.0);
        let palette = Palette::DARK;
        let cache = canvas::Cache::new();
        let canvas = canvas_for(&view, &palette, &cache);

        assert_eq!(canvas.row_at(0.0), Some(0));
        assert_eq!(canvas.row_at(ROW_HEIGHT * 3.5), Some(3));
    }

    #[test]
    fn hit_testing_accounts_for_the_scroll_offset() {
        let view = view_with(100, 20.0);
        let palette = Palette::DARK;
        let cache = canvas::Cache::new();
        let canvas = canvas_for(&view, &palette, &cache);

        assert_eq!(
            canvas.row_at(0.0),
            Some(20),
            "the top of the viewport is the row scrolled to, not row zero"
        );
    }

    #[test]
    fn a_click_past_the_end_of_history_selects_nothing() {
        let view = view_with(3, 0.0);
        let palette = Palette::DARK;
        let cache = canvas::Cache::new();
        let canvas = canvas_for(&view, &palette, &cache);

        assert_eq!(canvas.row_at(ROW_HEIGHT * 10.0), None);
    }

    #[test]
    fn only_the_visible_rows_plus_overscan_are_laid_out() {
        let mut view = view_with(50_000, 30_000.0);
        view.viewport_rows = 40;

        let range = view.visible_range();
        assert!(
            range.len() < 100,
            "laying out {} rows to draw 40 is not virtualisation",
            range.len()
        );
        assert!(range.contains(&30_000));
    }

    #[test]
    fn lanes_past_the_column_budget_collapse_instead_of_shrinking_everything() {
        let view = view_with(1, 0.0);
        let palette = Palette::DARK;
        let cache = canvas::Cache::new();
        let canvas = canvas_for(&view, &palette, &cache);

        assert_eq!(canvas.lane_x(MAX_LANES), canvas.lane_x(MAX_LANES + 40));
    }
}
