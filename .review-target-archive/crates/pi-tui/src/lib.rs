//! pi-tui — terminal UI primitives with differential rendering.
//!
//! Mirrors upstream pi-tui: maintains a virtual buffer, redraws only the
//! lines that changed since the last frame, and wraps writes in synchronized
//! output (DEC 2026) when the terminal supports it to avoid flicker.

pub mod editor;
pub mod renderer;
pub mod theme;

pub use editor::{Editor, EditorEvent};
pub use renderer::{DiffRenderer, Frame, Line, Span};
pub use theme::{ColorSpec, NamedColor, Theme, ThemeRegistry};
