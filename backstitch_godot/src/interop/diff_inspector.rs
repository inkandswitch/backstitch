//! The diff inspector: shows a `ProjectDiff` as a tree of foldable sections with property editors.

mod builder;
pub mod change_type_ext;
pub mod godot_diff;
pub mod inspector;
pub mod model;
pub mod navigator;
mod property_host;
mod script_editor_hack;
mod value_resolver;
pub mod widgets;
