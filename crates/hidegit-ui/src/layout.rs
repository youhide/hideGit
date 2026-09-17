//! How the window divides itself, and what dragging a divider does to it.
//!
//! Until this module existed every division in the main window was a number
//! written into a `view`: the sidebar was `Fixed(230.0)`, the graph and the
//! detail pane below it were `FillPortion(6)` and `FillPortion(4)`, and the
//! file list inside the working directory was `Fixed(280.0)`. Each of those is
//! a reasonable guess about a window nobody had seen yet, and a guess is all it
//! can be — a 13" laptop reading a wide diff and a 32" monitor reading a branch
//! list want opposite things, and neither could ask for them.
//!
//! # Two units, on purpose
//!
//! The sidebar and the file list are held in **pixels**. What they need is set
//! by their contents — a branch name, a path — and that does not get longer
//! because the window did. A sidebar that grew with the window would take space
//! from the diff to show the same names in the same font.
//!
//! The graph and the detail pane are held as a **fraction** of the space they
//! share, because they are two halves of one view rather than a panel beside a
//! document: at 60/40 the graph is the subject and the commit under it is the
//! footnote, and that relationship is what a taller window should preserve.
//!
//! # Why a drag arrives as a delta
//!
//! A press on a divider says nothing about where the pointer is — `mouse_area`
//! reports that a press happened, not its position — so the first movement
//! after the grab is the anchor, and every size is computed from the pointer's
//! distance from it rather than from its absolute position. That is also what
//! makes the arithmetic here testable without a window: nothing in this module
//! knows where anything is on screen.
//!
//! `extent` is the one measurement the view has to supply, because the size of
//! the space being divided cannot be derived from a pointer delta. It is taken
//! at the moment of the grab, from the `responsive` wrapper in
//! [`screen::repository`](crate::screen::repository), and it is what turns a
//! pixel delta into a fraction and what bounds a pixel width.

/// A divider that can be dragged, named for the pane that moves when it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    /// Between the sidebar and everything to its right.
    Sidebar,
    /// Between the graph and the detail pane under it.
    Detail,
    /// Between the file list and the diff, inside the working directory.
    Files,
}

/// A drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    split: Split,
    /// The size of the space being divided, measured when the grab happened.
    extent: f32,
    /// What the split was when it was grabbed. Every position during the drag
    /// is computed from this rather than from the previous position, so a drag
    /// that leaves and re-enters the window does not accumulate error.
    base: f32,
    /// Where the pointer was when it first moved after the grab.
    anchor: Option<(f32, f32)>,
}

/// How the window's panes are divided, and the drag that is changing it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    sidebar: f32,
    detail: f32,
    files: f32,
    drag: Option<Drag>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: Self::SIDEBAR,
            detail: Self::DETAIL,
            files: Self::FILES,
            drag: None,
        }
    }
}

impl Layout {
    /// The sidebar's width before anyone drags it.
    pub const SIDEBAR: f32 = 230.0;
    /// The detail pane's share of the height it splits with the graph.
    pub const DETAIL: f32 = 0.4;
    /// The file list's width inside the working directory.
    pub const FILES: f32 = 280.0;

    /// Narrow enough to be worth doing, wide enough to still read a branch name.
    const SIDEBAR_BOUNDS: (f32, f32) = (160.0, 520.0);
    const FILES_BOUNDS: (f32, f32) = (180.0, 620.0);
    /// Neither the graph nor the detail pane may be dragged out of existence: a
    /// pane with no height is indistinguishable from a bug, and there is no
    /// divider left to drag it back with.
    const DETAIL_BOUNDS: (f32, f32) = (0.15, 0.85);

    /// The layout a config file asks for, made safe to use.
    ///
    /// A hand-edited file — or one written by a version that bounded these
    /// differently — must not be able to produce a sidebar wider than the
    /// window or a detail pane of zero height, so every value goes through the
    /// same clamp a drag does.
    pub fn restore(sidebar: f32, detail: f32, files: f32) -> Self {
        Self {
            sidebar: clamp(sidebar, Self::SIDEBAR_BOUNDS, Self::SIDEBAR),
            detail: clamp(detail, Self::DETAIL_BOUNDS, Self::DETAIL),
            files: clamp(files, Self::FILES_BOUNDS, Self::FILES),
            drag: None,
        }
    }

    /// The sidebar's width, in pixels.
    pub fn sidebar(&self) -> f32 {
        self.sidebar
    }

    /// The detail pane's share of the column it splits with the graph, as a
    /// fraction between zero and one.
    pub fn detail(&self) -> f32 {
        self.detail
    }

    /// The file list's width, in pixels.
    pub fn files(&self) -> f32 {
        self.files
    }

    /// Is this divider the one being dragged right now?
    ///
    /// Only so the divider can say so: a hairline that does not change under
    /// the pointer gives no sign that the drag was picked up at all.
    pub fn dragging(&self, split: Split) -> bool {
        self.drag.is_some_and(|drag| drag.split == split)
    }

    /// Is any divider being dragged?
    ///
    /// The pointer subscription is only alive while this is true: listening to
    /// every mouse movement in the session to serve a drag that is not
    /// happening is a message per frame for nothing.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// A divider was pressed. `extent` is the size of the space it divides.
    pub fn grab(&mut self, split: Split, extent: f32) {
        let base = match split {
            Split::Sidebar => self.sidebar,
            Split::Detail => self.detail,
            Split::Files => self.files,
        };

        self.drag = Some(Drag {
            split,
            extent: if extent.is_finite() && extent > 0.0 {
                extent
            } else {
                f32::INFINITY
            },
            base,
            anchor: None,
        });
    }

    /// The pointer moved, in window coordinates.
    pub fn drag_to(&mut self, x: f32, y: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }

        let Some(drag) = &mut self.drag else {
            return;
        };

        let Some((from_x, from_y)) = drag.anchor else {
            drag.anchor = Some((x, y));
            return;
        };

        let drag = *drag;
        match drag.split {
            Split::Sidebar => {
                self.sidebar = clamp(
                    drag.base + (x - from_x),
                    bounded_by(Self::SIDEBAR_BOUNDS, drag.extent),
                    Self::SIDEBAR,
                );
            }
            Split::Files => {
                self.files = clamp(
                    drag.base + (x - from_x),
                    bounded_by(Self::FILES_BOUNDS, drag.extent),
                    Self::FILES,
                );
            }
            // The detail pane is *below* the divider, so dragging downwards
            // makes it smaller. Getting this sign wrong produces a pane that
            // runs away from the pointer, which is why it has its own test.
            Split::Detail => {
                self.detail = clamp(
                    drag.base - (y - from_y) / drag.extent,
                    Self::DETAIL_BOUNDS,
                    Self::DETAIL,
                );
            }
        }
    }

    /// The pointer was released, wherever it was released.
    pub fn release(&mut self) {
        self.drag = None;
    }

    /// Double-clicking a divider puts it back where it shipped.
    ///
    /// The way out of a layout somebody has dragged into uselessness, and the
    /// reason this needs no settings entry: the control that broke it is the
    /// control that fixes it.
    pub fn reset(&mut self, split: Split) {
        match split {
            Split::Sidebar => self.sidebar = Self::SIDEBAR,
            Split::Detail => self.detail = Self::DETAIL,
            Split::Files => self.files = Self::FILES,
        }
        // A double click is a press as well, so a grab is in flight by the time
        // this arrives. Leaving it would let the next pointer movement drag the
        // divider away from the default it was just put back to.
        self.drag = None;
    }
}

/// The bounds a pixel split gets inside a space of `extent` pixels.
///
/// Half the space, at most: a sidebar that can eat the window is a sidebar
/// somebody has to drag back before they can read anything, and on a narrow
/// window the fixed maximum is more than the whole width.
fn bounded_by((low, high): (f32, f32), extent: f32) -> (f32, f32) {
    (low, high.min(extent / 2.0).max(low))
}

/// Clamps, and refuses `NaN`.
///
/// `f32::clamp` panics on a `NaN` bound and propagates a `NaN` value, and a
/// size of `NaN` reaches the layout engine as a pane that never draws.
fn clamp(value: f32, (low, high): (f32, f32), fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grabs a divider and moves the pointer, which takes two movements: the
    /// first is the anchor, the second is the drag.
    fn dragged(split: Split, extent: f32, from: (f32, f32), to: (f32, f32)) -> Layout {
        let mut layout = Layout::default();
        layout.grab(split, extent);
        layout.drag_to(from.0, from.1);
        layout.drag_to(to.0, to.1);
        layout
    }

    #[test]
    fn dragging_the_sidebar_right_widens_it_by_what_the_pointer_travelled() {
        let layout = dragged(Split::Sidebar, 1440.0, (230.0, 400.0), (310.0, 400.0));

        assert_eq!(layout.sidebar(), Layout::SIDEBAR + 80.0);
    }

    #[test]
    fn the_first_movement_is_the_anchor_and_moves_nothing() {
        // Otherwise the divider would jump to wherever the pointer happened to
        // be when the press was reported, which is not where it was pressed.
        let mut layout = Layout::default();
        layout.grab(Split::Sidebar, 1440.0);
        layout.drag_to(900.0, 400.0);

        assert_eq!(layout.sidebar(), Layout::SIDEBAR);
    }

    #[test]
    fn the_sidebar_cannot_be_dragged_away_or_over_the_window() {
        let shrunk = dragged(Split::Sidebar, 1440.0, (230.0, 400.0), (-900.0, 400.0));
        assert_eq!(shrunk.sidebar(), Layout::SIDEBAR_BOUNDS.0);

        let grown = dragged(Split::Sidebar, 1440.0, (230.0, 400.0), (9000.0, 400.0));
        assert_eq!(grown.sidebar(), Layout::SIDEBAR_BOUNDS.1);
    }

    #[test]
    fn a_narrow_window_bounds_the_sidebar_before_the_fixed_maximum_does() {
        // 520 of a 700-pixel window leaves 180 for the graph, the diff and the
        // commit message together.
        let layout = dragged(Split::Sidebar, 700.0, (230.0, 400.0), (9000.0, 400.0));

        assert_eq!(layout.sidebar(), 350.0, "half the window, not the maximum");
    }

    #[test]
    fn dragging_the_detail_divider_down_makes_the_detail_pane_smaller() {
        // 100 pixels down a 1000-pixel column is a tenth of it.
        let layout = dragged(Split::Detail, 1000.0, (700.0, 500.0), (700.0, 600.0));

        assert!(
            (layout.detail() - (Layout::DETAIL - 0.1)).abs() < f32::EPSILON,
            "the pane below the divider shrinks as the divider goes down, got {}",
            layout.detail()
        );
    }

    #[test]
    fn neither_pane_can_be_dragged_out_of_existence() {
        let flattened = dragged(Split::Detail, 1000.0, (700.0, 500.0), (700.0, 5000.0));
        assert_eq!(flattened.detail(), Layout::DETAIL_BOUNDS.0);

        let raised = dragged(Split::Detail, 1000.0, (700.0, 500.0), (700.0, -5000.0));
        assert_eq!(raised.detail(), Layout::DETAIL_BOUNDS.1);
    }

    #[test]
    fn a_release_ends_the_drag_and_later_movement_does_nothing() {
        let mut layout = Layout::default();
        layout.grab(Split::Sidebar, 1440.0);
        layout.drag_to(230.0, 400.0);
        layout.drag_to(300.0, 400.0);
        layout.release();
        layout.drag_to(600.0, 400.0);

        assert_eq!(layout.sidebar(), Layout::SIDEBAR + 70.0);
        assert!(!layout.is_dragging());
    }

    #[test]
    fn every_drag_is_measured_from_where_it_started() {
        // Not from the previous position: a delta applied to the current size
        // every time drifts, because the size is clamped and the pointer is not.
        let mut layout = Layout::default();
        layout.grab(Split::Sidebar, 1440.0);
        layout.drag_to(230.0, 400.0);
        layout.drag_to(-900.0, 400.0);
        layout.drag_to(280.0, 400.0);

        assert_eq!(
            layout.sidebar(),
            Layout::SIDEBAR + 50.0,
            "the excursion into the clamp did not move the origin"
        );
    }

    #[test]
    fn resetting_puts_a_divider_back_and_drops_the_drag_that_did_it() {
        let mut layout = dragged(Split::Files, 1200.0, (280.0, 400.0), (500.0, 400.0));
        assert_ne!(layout.files(), Layout::FILES);

        layout.grab(Split::Files, 1200.0);
        layout.reset(Split::Files);

        assert_eq!(layout.files(), Layout::FILES);
        assert!(
            !layout.is_dragging(),
            "a double click is a press too, and the drag it armed would undo the reset"
        );
    }

    #[test]
    fn only_the_divider_being_dragged_says_it_is() {
        let mut layout = Layout::default();
        layout.grab(Split::Detail, 1000.0);

        assert!(layout.dragging(Split::Detail));
        assert!(!layout.dragging(Split::Sidebar));
    }

    #[test]
    fn a_pointer_that_reports_nonsense_moves_nothing() {
        let layout = dragged(Split::Sidebar, 1440.0, (230.0, 400.0), (f32::NAN, 400.0));

        assert_eq!(layout.sidebar(), Layout::SIDEBAR);
    }

    #[test]
    fn a_config_file_cannot_ask_for_a_pane_that_does_not_fit() {
        let restored = Layout::restore(9000.0, 2.0, -5.0);

        assert_eq!(restored.sidebar(), Layout::SIDEBAR_BOUNDS.1);
        assert_eq!(restored.detail(), Layout::DETAIL_BOUNDS.1);
        assert_eq!(restored.files(), Layout::FILES_BOUNDS.0);
    }

    #[test]
    fn a_config_file_full_of_nonsense_falls_back_to_the_defaults() {
        let restored = Layout::restore(f32::NAN, f32::INFINITY, f32::NEG_INFINITY);

        assert_eq!(restored, Layout::default());
    }
}
