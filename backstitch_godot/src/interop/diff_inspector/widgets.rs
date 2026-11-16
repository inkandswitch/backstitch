//! Dumb Godot controls used by the diff inspector.
//!
//! Widgets take already-resolved data and report what the user does through signals. They never
//! touch the editor (`EditorInterface`) and know nothing about `ProjectDiff`.

pub mod change_marker;
pub mod foldable_section;
pub mod property_row;
pub mod section_metrics;
