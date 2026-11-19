//! Dumb Godot controls used by the diff inspector.
//!
//! Widgets take already-resolved data and report what the user does through signals. They never
//! touch the editor (`EditorInterface`) and know nothing about `ProjectDiff`.

pub mod change_marker;
pub mod diff_property_view;
pub mod foldable_section;
pub mod section_metrics;
