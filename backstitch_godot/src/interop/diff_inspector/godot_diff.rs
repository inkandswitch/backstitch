use backstitch::diff::differ::ProjectDiff;
use backstitch::project::project_api::DiffViewModel;
use backstitch::project::project_base::DiffStatus;
use godot::builtin::GString;
use godot::classes::RefCounted;
use godot::prelude::*;

use crate::interop::godot_helpers::ToGodotExt;

/// A handle to a diff, handed out to GDScript.
///
/// The diff inspector reads the typed [`ProjectDiff`] out of it directly. GDScript code that
/// still wants the old Dictionary representation (e.g. the scene change highlighter) can get it
/// with [`get_dict()`](Self::get_dict).
#[derive(GodotClass)]
#[class(tool, no_init, base=RefCounted)]
pub struct GodotDiff {
    base: Base<RefCounted>,
    title: String,
    status: DiffStatus,
    /// The Dictionary representation of the diff, created on first use.
    dict: Option<Variant>,
}

#[godot_api]
impl GodotDiff {
    #[func]
    pub fn get_title(&self) -> GString {
        GString::from(&self.title)
    }

    /// Whether the diff is still being computed.
    #[func]
    pub fn is_loading(&self) -> bool {
        matches!(self.status, DiffStatus::Loading)
    }

    /// The diff as a Dictionary of `{ file path => file diff }`, or the string `"loading"` while it's loading.
    #[func]
    pub fn get_dict(&mut self) -> Variant {
        if self.dict.is_none() {
            self.dict = Some(self.status._to_godot());
        }
        self.dict.clone().unwrap_or_default()
    }
}

impl GodotDiff {
    pub fn from_view_model(diff: &dyn DiffViewModel) -> Gd<Self> {
        let title = diff.get_title().clone();
        let status = diff.get_diff().clone();
        Gd::from_init_fn(|base| Self {
            base,
            title,
            status,
            dict: None,
        })
    }

    /// The diff, if it has finished loading.
    pub fn project_diff(&self) -> Option<&ProjectDiff> {
        match &self.status {
            DiffStatus::Ready(diff) => Some(diff),
            DiffStatus::Loading => None,
        }
    }
}
