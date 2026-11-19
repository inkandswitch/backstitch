//! The diff inspector's view-model.
//!
//! This is a plain-data description of what the inspector shows, built from a [`ProjectDiff`]:
//! labels, change kinds, collapsed node trees and the node paths each section covers are all
//! resolved here.
//!
//! Nothing in this module (or its children) should import `godot`, so that it's unit-testable without an editor.

pub mod labels;
pub mod node_tree;

use std::collections::HashMap;

use backstitch::diff::differ::{Diff, ProjectDiff};
use backstitch::diff::resource_differ::BinaryResourceDiff;
use backstitch::diff::scene_differ::{
    PropertyDiff, SceneDiff, SubResourceDiff, TextResourceDiff, VariantValue,
};
use backstitch::diff::text_differ::TextDiff;

use super::change_type_ext::ChangeTypeExt;
use backstitch::helpers::utils::ChangeType;
use labels::{simplify_path, snake_case_to_capital_case};
pub use node_tree::NodeSectionViewModel;

/// Everything the inspector shows for one [`ProjectDiff`].
#[derive(Debug, Default)]
pub struct DiffViewModel {
    pub files: Vec<FileViewModel>,
}

impl DiffViewModel {
    pub fn from_project_diff(diff: &ProjectDiff) -> Self {
        Self {
            files: diff
                .file_diffs
                .iter()
                .map(FileViewModel::from_diff)
                .collect(),
        }
    }
}

/// The diff of one file; shown as one top-level section.
#[derive(Debug)]
pub struct FileViewModel {
    /// The (simplified) path of the file. Also the identifier of the section.
    pub path: String,
    /// The label of the section's header.
    pub label: String,
    pub kind: ChangeType,
    pub body: FileBody,
}

/// What a file's section contains.
#[derive(Debug)]
pub enum FileBody {
    /// A binary resource file; shows the old and new resource side by side.
    Resource {
        old: Option<VariantValue>,
        new: Option<VariantValue>,
        /// The label of the property row the resource is shown in.
        row_label: String,
    },
    /// A plain text file.
    Text(TextDiff),
    /// A text resource (`.tres`): its main resource and sub-resources.
    TextResource {
        sections: Vec<ResourceSectionViewModel>,
    },
    /// A scene (`.tscn`): a tree of changed nodes.
    Scene { roots: Vec<NodeSectionViewModel> },
}

impl FileViewModel {
    fn from_diff(diff: &Diff) -> Self {
        match diff {
            Diff::Scene(scene) => Self::from_scene(scene),
            Diff::TextResourceDiff(resource) => Self::from_text_resource(resource),
            Diff::BinaryResource(resource) => Self::from_binary_resource(resource),
            Diff::Text(text) => Self::from_text(text),
        }
    }

    fn new(path: &str, kind: ChangeType, body: FileBody) -> Self {
        let path = simplify_path(path);
        let label = format!("{} {}", path, kind.file_label_suffix());
        Self {
            path,
            label,
            kind,
            body,
        }
    }

    fn from_scene(scene: &SceneDiff) -> Self {
        Self::new(
            &scene.path,
            scene.change_type,
            FileBody::Scene {
                roots: node_tree::build_node_tree(&scene.changed_nodes),
            },
        )
    }

    fn from_text_resource(resource: &TextResourceDiff) -> Self {
        let mut sections = Vec::new();
        if let Some(main) = &resource.changed_main_resource {
            sections.extend(ResourceSectionViewModel::from_sub_resource(main, true));
        }
        for sub_resource in &resource.changed_sub_resources {
            sections.extend(ResourceSectionViewModel::from_sub_resource(
                sub_resource,
                false,
            ));
        }
        Self::new(
            &resource.path,
            resource.change_type,
            FileBody::TextResource { sections },
        )
    }

    fn from_binary_resource(resource: &BinaryResourceDiff) -> Self {
        let path = simplify_path(&resource.path);
        Self::new(
            &resource.path,
            resource.change_type,
            FileBody::Resource {
                old: resource.old_resource.clone(),
                new: resource.new_resource.clone(),
                row_label: snake_case_to_capital_case(&path),
            },
        )
    }

    fn from_text(text: &TextDiff) -> Self {
        Self::new(&text.path, text.change_type, FileBody::Text(text.clone()))
    }

    /// The node paths a hover over this file's section should highlight in the scene.
    pub fn hover_paths(&self) -> Vec<String> {
        match &self.body {
            FileBody::Scene { roots } => node_tree::hover_paths(roots),
            _ => Vec::new(),
        }
    }
}

/// One changed property of a node or resource.
#[derive(Debug, Clone)]
pub struct PropertyRow {
    pub name: String,
    /// The human-readable name of the property.
    pub label: String,
    pub kind: ChangeType,
    pub old: Option<VariantValue>,
    pub new: Option<VariantValue>,
}

impl PropertyRow {
    /// Turns the changed properties of a node or resource into rows, ordered by property name.
    pub fn from_changed_properties(props: &HashMap<String, PropertyDiff>) -> Vec<PropertyRow> {
        let mut rows: Vec<PropertyRow> = props
            .values()
            .map(|diff| PropertyRow {
                name: diff.name.clone(),
                label: snake_case_to_capital_case(&diff.name),
                kind: diff.change_type,
                old: diff.old_value.clone(),
                new: diff.new_value.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }
}

/// A changed (sub-)resource inside a text resource file.
#[derive(Debug)]
pub struct ResourceSectionViewModel {
    /// The id of the sub-resource; empty for the main resource.
    pub id: String,
    pub label: String,
    pub kind: ChangeType,
    /// The class the properties belong to.
    pub resource_type: String,
    /// The script class of the resource, if it has a script.
    pub script_class: Option<String>,
    pub rows: Vec<PropertyRow>,
}

impl ResourceSectionViewModel {
    /// Returns `None` for modified resources that have no changed properties.
    fn from_sub_resource(diff: &SubResourceDiff, is_main: bool) -> Option<Self> {
        let kind: ChangeType = diff.change_type;
        if diff.changed_properties.is_empty() && kind == ChangeType::Modified {
            return None;
        }
        let mut label = diff.sub_resource_id.clone();
        if label.is_empty() {
            label = format!("Main ({})", diff.resource_type);
        }
        label = format!("{} {}", label, kind.label_suffix());
        Some(Self {
            id: diff.sub_resource_id.clone(),
            label,
            kind,
            resource_type: diff.resource_type.clone(),
            // Only the main resource gets its script class resolved.
            script_class: if is_main {
                diff.script_class.clone()
            } else {
                None
            },
            rows: PropertyRow::from_changed_properties(&diff.changed_properties),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backstitch::diff::scene_differ::NodeDiff;
    use backstitch::helpers::utils::ChangeType;

    fn prop(name: &str, change_type: ChangeType) -> (String, PropertyDiff) {
        (
            name.to_string(),
            PropertyDiff::new(
                name.to_string(),
                change_type,
                None,
                Some(VariantValue::Variant("1".to_string())),
            ),
        )
    }

    #[test]
    fn rows_are_sorted_and_labelled() {
        let props: HashMap<_, _> = [
            prop("zeta_value", ChangeType::Created),
            prop("alpha_value", ChangeType::Modified),
        ]
        .into_iter()
        .collect();
        let rows = PropertyRow::from_changed_properties(&props);
        assert_eq!(rows[0].name, "alpha_value");
        assert_eq!(rows[0].label, "Alpha Value");
        assert_eq!(rows[0].kind, ChangeType::Modified);
        assert_eq!(rows[1].kind, ChangeType::Created);
    }

    #[test]
    fn skips_empty_modified_resources_and_labels_main() {
        let empty = SubResourceDiff::new(
            ChangeType::Modified,
            "Sub_1".to_string(),
            "Resource".to_string(),
            None,
            HashMap::new(),
        );
        assert!(ResourceSectionViewModel::from_sub_resource(&empty, false).is_none());

        let main = SubResourceDiff::new(
            ChangeType::Modified,
            String::new(),
            "Resource".to_string(),
            Some("MyScript".to_string()),
            [prop("a", ChangeType::Modified)].into_iter().collect(),
        );
        let section = ResourceSectionViewModel::from_sub_resource(&main, true).unwrap();
        assert_eq!(section.label, "Main (Resource) (Modified)");
        assert_eq!(section.script_class.as_deref(), Some("MyScript"));
        // Non-main resources don't resolve a script class.
        let section = ResourceSectionViewModel::from_sub_resource(&main, false).unwrap();
        assert_eq!(section.script_class, None);
    }

    #[test]
    fn scene_file_hover_paths() {
        let scene = SceneDiff {
            path: "res://a//scene.tscn".to_string(),
            change_type: ChangeType::Modified,
            changed_nodes: vec![NodeDiff::new(
                ChangeType::Modified,
                "./Child".to_string(),
                None,
                [prop("visible", ChangeType::Modified)]
                    .into_iter()
                    .collect(),
            )],
        };
        let view = FileViewModel::from_diff(&Diff::Scene(scene));
        assert_eq!(view.path, "res://a/scene.tscn");
        assert_eq!(view.label, "res://a/scene.tscn (Modified)");
        assert_eq!(view.hover_paths(), vec!["./Child".to_string()]);
    }
}
