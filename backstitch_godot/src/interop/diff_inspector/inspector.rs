use godot::builtin::GString;
use godot::classes::control::SizeFlags;
use godot::classes::{EditorInspector, IEditorInspector, Node, VBoxContainer};
use godot::prelude::*;

use crate::interop::godot_helpers::ThemeGetter;

use super::builder::{DiffWidgetBuilder, Icons, InspectorHandle};
use super::godot_diff::GodotDiff;
use super::model::DiffViewModel;

/// Shows a diff as a tree of foldable sections with property editors.
///
/// This is the presenter of the diff inspector: it turns a [`GodotDiff`] into a [`DiffModel`],
/// builds widgets for it, and re-emits what happens in the widgets as its own signals.
///
/// The actual work lives in [`model`](super::model), [`widgets`](super::widgets) and
/// [`navigator`](super::navigator).
#[derive(GodotClass)]
#[class(tool, base=EditorInspector, rename=DiffInspectorContainer)]
pub struct DiffInspector {
    base: Base<EditorInspector>,
    main_vbox: Option<Gd<VBoxContainer>>,
}

#[godot_api]
impl IEditorInspector for DiffInspector {
    fn init(base: Base<EditorInspector>) -> Self {
        Self {
            base,
            main_vbox: None,
        }
    }
}

#[godot_api]
impl DiffInspector {
    /// Emitted when the mouse enters a section that covers some nodes of a scene.
    #[signal]
    pub fn node_hovered(file_path: GString, node_path: VarArray);
    /// Emitted when the mouse leaves a section that covers some nodes of a scene.
    #[signal]
    pub fn node_unhovered(file_path: GString, node_path: VarArray);

    /// Shows the diff, replacing whatever was shown before.
    #[func]
    pub fn set_diff(&mut self, diff: Gd<GodotDiff>) {
        self.reset();

        let model = {
            let diff = diff.bind();
            let Some(project_diff) = diff.project_diff() else {
                // Still loading; nothing to show yet.
                return;
            };
            DiffViewModel::from_project_diff(project_diff)
        };

        let handle = InspectorHandle::new(&self.to_gd());
        let icons = Icons {
            added: self.get_theme_icon("file_added", "Editor"),
            removed: self.get_theme_icon("file_removed", "Editor"),
        };
        let sections = DiffWidgetBuilder::new(handle, icons).build(&model);

        let mut main_vbox = self.get_main_vbox();
        for section in sections {
            main_vbox.add_child(&section);
        }
    }

    /// Removes everything that is currently shown.
    #[func]
    pub fn reset(&mut self) {
        // Nothing is connected to this by us, but be sure to not leave connections to what we're about to free.
        if self.base().has_signal("theme_changed") {
            let connections = self.base().get_signal_connection_list("theme_changed");
            for connection in connections.iter_shared() {
                if let Some(callable) = connection
                    .get("callable")
                    .and_then(|callable| callable.try_to::<Callable>().ok())
                {
                    self.base_mut().disconnect("theme_changed", &callable);
                }
            }
        }

        let mut main_vbox = self.get_main_vbox();
        for mut child in main_vbox.get_children().iter_shared() {
            main_vbox.remove_child(&child);
            child.queue_free();
        }
    }

    #[func]
    pub fn get_main_vbox(&mut self) -> Gd<VBoxContainer> {
        if let Some(vbox) = &self.main_vbox {
            return vbox.clone();
        }
        let vbox = self.find_or_create_main_vbox();
        self.main_vbox = Some(vbox.clone());
        vbox
    }
}

impl DiffInspector {
    fn find_or_create_main_vbox(&mut self) -> Gd<VBoxContainer> {
        let existing = self
            .base()
            .get_node_or_null("%DifferMainVBox")
            .or_else(|| self.base().get_node_or_null("DifferMainVBox"))
            .and_then(|node: Gd<Node>| node.try_cast::<VBoxContainer>().ok());
        if let Some(vbox) = existing {
            return vbox;
        }

        let mut vbox = VBoxContainer::new_alloc();
        vbox.set_name("DifferMainVBox");
        vbox.set_h_size_flags(SizeFlags::EXPAND_FILL);
        vbox.set_v_size_flags(SizeFlags::EXPAND_FILL);
        self.base_mut().add_child(&vbox);
        vbox
    }
}
