//! iced screens, widgets, theme and the commit-graph canvas.
//!
//! The application state, message shapes and screen inventory follow
//! `docs/UI_SPEC.md`. iced types are confined to this crate on purpose: iced
//! 0.14 is pre-1.0, so a breaking upgrade is expected, and keeping the blast
//! radius to one crate is the mitigation.

pub mod alerts;
pub mod app;
pub mod forge;
pub mod format;
pub mod message;
pub mod state;
pub mod theme;
pub mod watcher;

pub mod screen {
    pub mod repository;
    pub mod welcome;
}

pub mod widget {
    pub mod blame;
    pub mod detail;
    pub mod diff;
    pub mod graph;
    pub mod overlay;
    pub mod plan;
    pub mod pr;
    pub mod resolver;
    pub mod search;
    pub mod settings;
    pub mod shortcuts;
    pub mod sidebar;
    pub mod staging;
    pub mod tabs;
}

pub use app::Hidegit;
pub use message::{Message, RepoMessage};
pub use theme::{Palette, Theme};

/// The element type every view in this crate returns.
pub type Element<'a, Message> = iced::Element<'a, Message, iced::Theme, iced::Renderer>;
