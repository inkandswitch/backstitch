use godot::builtin::{Color, GString};
use godot::classes::control::SizeFlags;
use godot::classes::{EditorInspector, IEditorInspector, Node, Texture2D, VBoxContainer};
use godot::prelude::*;

use super::builder::{DiffWidgetBuilder, Icons, InspectorHandle};
use super::change_type_ext::ChangeTypeExt;
use super::godot_diff::GodotDiff;
use super::model::DiffModel;
use backstitch::helpers::utils::ChangeType;

/// Shows a diff as a tree of foldable sections with property editors.
///
/// This is the presenter of the diff inspector: it turns a [`GodotDiff`] into a [`DiffModel`],
/// builds widgets for it, and re-emits what happens in the widgets as its own signals. The
/// actual work lives in [`model`](super::model), [`widgets`](super::widgets) and
/// [`navigator`](super::navigator).
#[derive(GodotClass)]
#[class(tool, base=EditorInspector, rename=DiffInspectorContainer)]
pub struct DiffInspector {
    base: Base<EditorInspector>,

    #[export]
    added_icon: Option<Gd<Texture2D>>,
    #[export]
    removed_icon: Option<Gd<Texture2D>>,
    // Not used for anything yet; kept so that scenes that set it keep loading.
    #[export]
    modified_icon: Option<Gd<Texture2D>>,

    #[export]
    #[var(set = set_added_color)]
    added_color: Color,
    #[export]
    #[var(set = set_removed_color)]
    removed_color: Color,
    #[export]
    #[var(set = set_modified_color)]
    modified_color: Color,

    main_vbox: Option<Gd<VBoxContainer>>,
}

#[godot_api]
impl IEditorInspector for DiffInspector {
    fn init(base: Base<EditorInspector>) -> Self {
        Self {
            base,
            added_icon: None,
            removed_icon: None,
            modified_icon: None,
            added_color: Color::from_rgba8(0x3f, 0xa6, 0x2e, 255),
            removed_color: Color::from_rgba8(0xa5, 0x54, 0x54, 255),
            modified_color: Color::from_rgba8(0xe2, 0xbe, 0x99, 255),
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

    #[func]
    fn set_added_color(&mut self, color: Color) {
        self.added_color = color;
        self.set_inspector_change_color(ChangeType::Created, color);
    }

    #[func]
    fn set_removed_color(&mut self, color: Color) {
        self.removed_color = color;
        self.set_inspector_change_color(ChangeType::Deleted, color);
    }

    #[func]
    fn set_modified_color(&mut self, color: Color) {
        self.modified_color = color;
        self.set_inspector_change_color(ChangeType::Modified, color);
    }

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
            DiffModel::from_project_diff(project_diff)
        };

        let handle = InspectorHandle::new(&self.to_gd());
        let icons = Icons {
            added: self.added_icon.clone(),
            removed: self.removed_icon.clone(),
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

    /// Changes the color that things of the given kind are tinted with, by writing it into the inspector's theme.
    fn set_inspector_change_color(&mut self, kind: ChangeType, color: Color) {
        let Some(mut theme) = self.base().get_theme() else {
            return;
        };
        theme.set_color(kind.theme_color_name(), "Editor", color);
        if self.base().has_signal("theme_changed") {
            self.base_mut().emit_signal("theme_changed", &[]);
        }
    }
}
