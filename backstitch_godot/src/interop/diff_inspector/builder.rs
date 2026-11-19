//! Builds the widget tree for a [`DiffModel`].
//!
//! Widgets are created from resolved model data, and the intents they emit
//! (hovered, clicked) are wired to the inspector's signals and the [`EditorNavigator`].
//!
//! This is seperated out from the main diff inspector module to avoid circular dependencies, and to
//! prevent the diff inspector from being re-binded when instantiating all the nodes.
//! It will also help with future refactors which will enable this to run on a background thread.
//! (TODO: Waiting on upstream gdext changes that would allow this (e.g. `Unique<T>`))

use std::rc::Rc;

use godot::builtin::{GString, NodePath, Vector2};
use godot::classes::texture_rect::ExpandMode;
use godot::classes::{
    HBoxContainer, HSeparator, Label, Object, PanelContainer, Texture2D, TextureRect, VBoxContainer,
};
use godot::prelude::*;

use super::model::{
    DiffViewModel, FileBody, FileViewModel, NodeSectionViewModel, PropertyRow,
    ResourceSectionViewModel,
};
use super::navigator::{EditorNavigator, NavTarget};
use super::property_host::PropertyHost;
use super::value_resolver::resolve_property_values;
use super::widgets::diff_property_view::DiffPropertyView;
use super::widgets::foldable_section::DiffInspectorSection;
use crate::interop::godot_helpers::ToGodotExt;
use crate::interop::text_differ_view::TextDifferView;
use backstitch::helpers::utils::ChangeType;

pub const SIGNAL_NODE_HOVERED: &str = "node_hovered";
pub const SIGNAL_NODE_UNHOVERED: &str = "node_unhovered";

/// Primarily here to avoid a re-entrancy panic. A weak reference to the inspector, which
/// closures connected to widget signals can use to emit the inspector's signals without
/// keeping the inspector borrowed.
#[derive(Clone)]
pub struct InspectorHandle {
    id: InstanceId,
}

impl InspectorHandle {
    pub fn new(inspector: &Gd<impl Inherits<Object>>) -> Self {
        Self {
            id: inspector.instance_id(),
        }
    }

    /// Emits `signal` (`node_hovered` or `node_unhovered`) with the file and the node paths it concerns.
    pub fn emit(&self, signal: &str, file_path: &str, node_paths: &[String]) {
        // NOTE: `try_from_instance_id` avoids rebinding the inspector by trying from the instance ID;
        // I manually checked the code paths to ensure that there are no rebinds, but if we start getting re-entrancy panics,
        // re-evaluate this.
        let Ok(mut inspector) = Gd::<Object>::try_from_instance_id(self.id) else {
            return;
        };
        let node_paths: VarArray = node_paths
            .iter()
            .map(|path| NodePath::from(path.as_str()).to_variant())
            .collect();
        inspector.emit_signal(
            signal,
            &[
                GString::from(file_path).to_variant(),
                node_paths.to_variant(),
            ],
        );
    }
}

/// The icons shown for added and removed files.
#[derive(Clone, Default)]
pub struct Icons {
    pub added: Option<Gd<Texture2D>>,
    pub removed: Option<Gd<Texture2D>>,
}

pub struct DiffWidgetBuilder {
    inspector: InspectorHandle,
    icons: Icons,
}

impl DiffWidgetBuilder {
    pub fn new(inspector: InspectorHandle, icons: Icons) -> Self {
        Self { inspector, icons }
    }

    /// Builds the top-level sections of the model, one per file.
    pub fn build(&self, model: &DiffViewModel) -> Vec<Gd<DiffInspectorSection>> {
        // HACK: if we have too many sections, we just punt on displaying the whole diff.
        // TODO: Figure out why exponentially increasing number of redraws are happening when adding the sections to the inspector.
        if model.files.len() > 200 {
            let fake_section = DiffInspectorSection::create(
                "",
                "Diff is too large to display",
                ChangeType::Modified,
                0,
                1,
            );
            return vec![fake_section];
        }

        model
            .files
            .iter()
            .map(|file| self.build_file_section(file))
            .collect()
    }

    /// Creates a small panel with an icon and a line of text, e.g. "File Added".
    fn create_node_box(icon: Option<&Gd<Texture2D>>, text: &str) -> Gd<PanelContainer> {
        let mut panel_container = PanelContainer::new_alloc();
        let mut hbox = HBoxContainer::new_alloc();

        let mut icon_rect = TextureRect::new_alloc();
        icon_rect.set_texture(icon);
        icon_rect.set_size(godot::builtin::Vector2::new(60.0, 60.0));
        icon_rect.set_expand_mode(ExpandMode::FIT_WIDTH_PROPORTIONAL);
        hbox.add_child(&icon_rect);

        let mut label = Label::new_alloc();
        label.set_text(text);
        hbox.add_child(&label);

        panel_container.add_child(&hbox);
        let width = panel_container.get_size().x;
        panel_container.set_size(godot::builtin::Vector2::new(width, 60.0));
        panel_container
    }

    fn build_file_section(&self, file: &FileViewModel) -> Gd<DiffInspectorSection> {
        let mut section = DiffInspectorSection::create(&file.path, &file.label, file.kind, 0, 1);
        let mut vbox = section.bind().get_vbox();

        match &file.body {
            FileBody::Resource {
                old,
                new,
                row_label,
            } => {
                self.build_resource_diff(&mut vbox, file, old, new, row_label);
                self.navigate_on_click(
                    &section,
                    NavTarget::Resource {
                        path: file.path.clone(),
                    },
                );
            }
            FileBody::Text(text_diff) => {
                let mut text_view = TextDifferView::create_new(text_diff.clone(), false);
                text_view.bind_mut().redraw();
                text_view.set_custom_minimum_size(Vector2::new(100.0, 500.0));
                vbox.add_child(&text_view);
                self.navigate_on_click(
                    &section,
                    NavTarget::Text {
                        path: file.path.clone(),
                    },
                );
            }
            FileBody::TextResource { sections } => {
                vbox.add_child(&HSeparator::new_alloc());
                for resource_section in sections {
                    vbox.add_child(&self.build_resource_section(resource_section));
                }
                self.navigate_on_click(
                    &section,
                    NavTarget::Resource {
                        path: file.path.clone(),
                    },
                );
            }
            FileBody::Scene { roots } => {
                let hover_paths: Rc<[String]> = file.hover_paths().into();
                self.connect_hover(&section, &file.path, hover_paths.clone());
                self.navigate_to_scene_on_click(&section, &file.path, hover_paths);

                vbox.add_child(&HSeparator::new_alloc());
                for root in roots {
                    vbox.add_child(&self.build_node_section(&file.path, root));
                }
            }
        }

        section.bind_mut().unfold();
        section
    }

    /// Shows a rich visual resource diff for a binary resource file. (e.g. if it's an image, displays the images side-by-side)
    fn build_resource_diff(
        &self,
        vbox: &mut Gd<VBoxContainer>,
        file: &FileViewModel,
        old: &Option<backstitch::diff::scene_differ::VariantValue>,
        new: &Option<backstitch::diff::scene_differ::VariantValue>,
        row_label: &str,
    ) {
        let old_value = old.as_ref().map(|v| v._to_godot()).unwrap_or_default();
        let new_value = new.as_ref().map(|v| v._to_godot()).unwrap_or_default();
        let is_object = |value: &Variant| value.try_to::<Gd<Object>>().is_ok();

        if !is_object(&old_value) && !is_object(&new_value) {
            // There is no resource we can show an editor for (e.g. it failed to load); just say what happened to the file.
            let (icon, text) = match file.kind {
                ChangeType::Created => (&self.icons.added, "File Added"),
                ChangeType::Deleted => (&self.icons.removed, "File Deleted"),
                ChangeType::Modified => return,
            };
            vbox.add_child(&Self::create_node_box(icon.as_ref(), text));
            return;
        }

        let host = PropertyHost::new(None);
        let view = DiffPropertyView::create(
            file.kind,
            host.object(),
            "Resource",
            old_value,
            new_value,
            row_label,
        );
        vbox.add_child(&view);
    }

    /// A sub-resource (or the main resource) of a text resource, with its changed properties.
    fn build_resource_section(
        &self,
        resource: &ResourceSectionViewModel,
    ) -> Gd<DiffInspectorSection> {
        let section =
            DiffInspectorSection::create(&resource.id, &resource.label, resource.kind, 1, 2);
        let mut vbox = section.bind().get_vbox();
        vbox.add_child(&HSeparator::new_alloc());

        let host = PropertyHost::new(Some(&resource.resource_type));
        self.build_property_rows(
            &mut vbox,
            &host,
            &resource.rows,
            &resource.resource_type,
            resource.script_class.as_deref(),
        );
        section
    }

    /// A section for a changed node, or for a group of changed descendants of a node.
    fn build_node_section(
        &self,
        file_path: &str,
        node: &NodeSectionViewModel,
    ) -> Gd<DiffInspectorSection> {
        let (indent_depth, level) = if node.is_grouping { (0, 1) } else { (1, 2) };
        let mut section = DiffInspectorSection::create(
            &node.section,
            &node.label,
            node.kind,
            indent_depth,
            level,
        );
        let mut vbox = section.bind().get_vbox();

        if node.is_grouping {
            vbox.add_child(&HSeparator::new_alloc());
        } else {
            let host = PropertyHost::new(None);
            self.build_property_rows(
                &mut vbox,
                &host,
                &node.rows,
                node.node_type.as_deref().unwrap_or(""),
                None,
            );
            section.bind_mut().unfold();
        }

        for child in &node.children {
            vbox.add_child(&self.build_node_section(file_path, child));
        }

        let hover_paths: Rc<[String]> = node.hover_paths().into();
        self.connect_hover(&section, file_path, hover_paths.clone());
        self.navigate_to_node_on_click(&section, file_path, hover_paths);
        section
    }

    fn build_property_rows(
        &self,
        vbox: &mut Gd<VBoxContainer>,
        host: &PropertyHost,
        rows: &[PropertyRow],
        owner_type: &str,
        script_class: Option<&str>,
    ) {
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                vbox.add_child(&HSeparator::new_alloc());
            }
            let (old, new) = resolve_property_values(row, owner_type, script_class);
            let view =
                DiffPropertyView::create(row.kind, host.object(), &row.name, old, new, &row.label);
            vbox.add_child(&view);
        }
    }

    /// Emits the inspector's hover signals while the mouse is over the section.
    fn connect_hover(
        &self,
        section: &Gd<DiffInspectorSection>,
        file_path: &str,
        node_paths: Rc<[String]>,
    ) {
        let (inspector, file, paths) = (
            self.inspector.clone(),
            file_path.to_string(),
            node_paths.clone(),
        );
        section
            .signals()
            .section_mouse_entered()
            .connect(move |_section: GString| {
                inspector.emit(SIGNAL_NODE_HOVERED, &file, &paths);
            });

        let (inspector, file) = (self.inspector.clone(), file_path.to_string());
        section
            .signals()
            .section_mouse_exited()
            .connect(move |_section: GString| {
                inspector.emit(SIGNAL_NODE_UNHOVERED, &file, &node_paths);
            });
    }

    fn navigate_on_click(&self, section: &Gd<DiffInspectorSection>, target: NavTarget) {
        section
            .signals()
            .box_clicked()
            .connect(move |_section: GString| {
                EditorNavigator::go_to(target.clone(), || {});
            });
    }

    /// Clicking on a scene file's section opens the scene; once it's open, the inspector's hover
    /// signal is emitted so that listeners can update what they highlight.
    fn navigate_to_scene_on_click(
        &self,
        section: &Gd<DiffInspectorSection>,
        file_path: &str,
        node_paths: Rc<[String]>,
    ) {
        let (inspector, file) = (self.inspector.clone(), file_path.to_string());
        section
            .signals()
            .box_clicked()
            .connect(move |_section: GString| {
                let (inspector, file, paths) =
                    (inspector.clone(), file.clone(), node_paths.clone());
                EditorNavigator::go_to(NavTarget::Scene { file: file.clone() }, move || {
                    inspector.emit(SIGNAL_NODE_HOVERED, &file, &paths);
                });
            });
    }

    /// Clicking on a node's section selects the node in the scene.
    fn navigate_to_node_on_click(
        &self,
        section: &Gd<DiffInspectorSection>,
        file_path: &str,
        node_paths: Rc<[String]>,
    ) {
        let (inspector, file) = (self.inspector.clone(), file_path.to_string());
        section
            .signals()
            .box_clicked()
            .connect(move |node: GString| {
                let (inspector, file_for_hover, paths) =
                    (inspector.clone(), file.clone(), node_paths.clone());
                EditorNavigator::go_to(
                    NavTarget::Node {
                        file: file.clone(),
                        node_path: node.to_string(),
                    },
                    move || inspector.emit(SIGNAL_NODE_HOVERED, &file_for_hover, &paths),
                );
            });
    }
}
