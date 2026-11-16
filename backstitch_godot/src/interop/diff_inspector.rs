//! The diff inspector: shows a `ProjectDiff` as a tree of foldable sections with property editors.
//!
//! The code is split into domains:
//! - [`model`]: a pure (no `godot` imports) view-model built from a `ProjectDiff`.
//! - [`widgets`]: dumb Godot controls that render resolved data and emit intents.
//! - [`inspector`]: the presenter; builds widgets from the model and wires everything together.
//! - [`navigator`]: the only place that drives the editor (`EditorInterface`) in response to clicks.

pub mod builder;
pub mod change_type_ext;
pub mod godot_diff;
pub mod inspector;
pub mod model;
pub mod navigator;
pub mod property_host;
pub mod script_editor_hack;
pub mod value_resolver;
pub mod widgets;
