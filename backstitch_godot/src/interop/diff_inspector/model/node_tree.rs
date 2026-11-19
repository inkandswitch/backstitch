//! Builds the tree of changed nodes shown for a scene.
//!
//! Changed nodes are grouped by their parent nodes, e.g.:
//!
//! ```text
//! Node1/Node2/Node3/Node4
//! Node1/Node2/Node3/Node4/Node5
//! Node1/Node2/Node3/Node4/Node5/Node6
//!  -> Node1/Node2/Node3 ->
//!     - Node4 ->
//!        - Node5 ->
//!           - Node6
//! ```
//!
//! Ancestors that didn't change themselves are collapsed into their single child (`A/B/C`) or
//! dropped entirely if there is nothing to show underneath them.

use std::collections::BTreeMap;

use backstitch::diff::scene_differ::NodeDiff;

use super::labels::{get_base_dir, get_file};
use super::{ChangeType, PropertyRow};
use crate::interop::diff_inspector::change_type_ext::ChangeTypeExt;

/// A section in the node tree.
#[derive(Debug, Clone)]
pub struct NodeSectionViewModel {
    /// The identifier of the section. For sections of changed nodes, this is the path of the node.
    pub section: String,
    pub label: String,
    pub kind: ChangeType,
    /// The type (or instance) of the node, if known.
    pub node_type: Option<String>,
    /// Changed properties. Empty for sections that only group changed descendants.
    pub rows: Vec<PropertyRow>,
    /// `true` if this section groups changed descendants but is not a changed node itself.
    pub is_grouping: bool,
    pub children: Vec<NodeSectionViewModel>,
}

impl NodeSectionViewModel {
    /// The node paths a hover over this section should highlight: the paths of this section and all
    /// its descendants that have changed properties. Sorted and without duplicates.
    pub fn hover_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        self.collect_hover_paths(&mut paths);
        paths.sort();
        paths.dedup();
        paths
    }

    fn collect_hover_paths(&self, paths: &mut Vec<String>) {
        if !self.rows.is_empty() {
            paths.push(self.section.clone());
        }
        for child in &self.children {
            child.collect_hover_paths(paths);
        }
    }

    fn from_node_diff(diff: &NodeDiff, label: &str) -> Option<Self> {
        let kind: ChangeType = diff.change_type;
        let rows = PropertyRow::from_changed_properties(&diff.changed_properties);
        if rows.is_empty() && kind == ChangeType::Modified {
            return None;
        }
        Some(Self {
            section: diff.node_path.clone(),
            label: format!("{} {}", label, kind.label_suffix()),
            kind,
            node_type: diff.node_type.as_ref().map(|t| t.to_string()),
            rows,
            is_grouping: false,
            children: Vec::new(),
        })
    }

    fn grouping(name: &str, label: &str) -> Self {
        Self {
            section: name.to_string(),
            label: label.to_string(),
            kind: ChangeType::Modified,
            node_type: None,
            rows: Vec::new(),
            is_grouping: true,
            children: Vec::new(),
        }
    }
}

/// The node paths a hover over a set of sibling sections should highlight.
pub fn hover_paths(sections: &[NodeSectionViewModel]) -> Vec<String> {
    let mut paths: Vec<String> = sections.iter().flat_map(|s| s.hover_paths()).collect();
    paths.sort();
    paths.dedup();
    paths
}

/// A node of the intermediate prefix tree: the node at some path, and the nodes below it.
#[derive(Default)]
struct PathTrie<'a> {
    diff: Option<&'a NodeDiff>,
    /// Keyed by the full path of the child.
    children: BTreeMap<String, PathTrie<'a>>,
}

fn build_path_trie(node_diffs: &[NodeDiff]) -> BTreeMap<String, PathTrie<'_>> {
    let mut roots: BTreeMap<String, PathTrie> = BTreeMap::new();
    for diff in node_diffs {
        // skip temporary nodes created by the instance
        if diff.node_path.contains('@') {
            continue;
        }
        let parts: Vec<&str> = diff.node_path.split('/').collect();
        let mut level = &mut roots;
        for i in 0..parts.len() {
            let key = parts[..=i].join("/");
            let entry = level.entry(key).or_default();
            if i == parts.len() - 1 {
                entry.diff = Some(diff);
                break;
            }
            level = &mut entry.children;
        }
    }
    roots
}

/// Builds the section tree for the changed nodes of a scene.
pub fn build_node_tree(node_diffs: &[NodeDiff]) -> Vec<NodeSectionViewModel> {
    let mut sorted: Vec<&NodeDiff> = node_diffs.iter().collect();
    sorted.sort_by(|a, b| a.node_path.cmp(&b.node_path));
    let sorted: Vec<NodeDiff> = sorted.into_iter().cloned().collect();

    let trie = build_path_trie(&sorted);
    trie.iter()
        .filter_map(|(key, node)| build_section(key, node, None))
        .collect()
}

fn build_section(
    name: &str,
    node: &PathTrie,
    override_label: Option<String>,
) -> Option<NodeSectionViewModel> {
    let label = override_label.unwrap_or_else(|| get_file(name).to_string());

    // A node that didn't change, with a single changed descendant: merge into that descendant.
    if node.diff.is_none() && node.children.len() == 1 {
        let (child_key, child) = node.children.iter().next()?;
        let prefix = format!("{}/", get_base_dir(name));
        let new_label = child_key
            .strip_prefix(&prefix)
            .unwrap_or(child_key)
            .to_string();
        return build_section(child_key, child, Some(new_label));
    }

    let own_section = node
        .diff
        .and_then(|diff| NodeSectionViewModel::from_node_diff(diff, &label));
    if own_section.is_none() && node.diff.is_some() && node.children.is_empty() {
        // A modified node without any changed properties, and nothing below it.
        return None;
    }

    let has_own_diff = own_section.is_some();
    let mut section = own_section.unwrap_or_else(|| NodeSectionViewModel::grouping(name, &label));
    for (child_key, child) in &node.children {
        if let Some(child_section) = build_section(child_key, child, None) {
            section.children.push(child_section);
        }
    }

    if has_own_diff {
        return Some(section);
    }

    // Merge the child sections into the parent if there is no diff on this node.
    match section.children.len() {
        0 => None,
        1 => {
            let mut only_child = section.children.pop()?;
            only_child.label = format!("{}/{}", section.label, only_child.label);
            Some(only_child)
        }
        _ => Some(section),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backstitch::diff::scene_differ::{PropertyDiff, VariantValue};
    use backstitch::helpers::utils::ChangeType;
    use backstitch::parser::godot_parser::TypeOrInstance;

    fn node(path: &str, change_type: ChangeType, props: &[&str]) -> NodeDiff {
        NodeDiff::new(
            change_type,
            path.to_string(),
            Some(TypeOrInstance::Type("Node2D".to_string())),
            props
                .iter()
                .map(|name| {
                    (
                        name.to_string(),
                        PropertyDiff::new(
                            name.to_string(),
                            ChangeType::Modified,
                            Some(VariantValue::Variant("0".to_string())),
                            Some(VariantValue::Variant("1".to_string())),
                        ),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn single_changed_node_collapses_unchanged_root() {
        let tree = build_node_tree(&[node("./Child", ChangeType::Modified, &["visible"])]);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].section, "./Child");
        assert_eq!(tree[0].label, "./Child (Modified)");
        assert!(!tree[0].is_grouping);
        assert_eq!(tree[0].node_type.as_deref(), Some("Node2D"));
    }

    #[test]
    fn changed_root_with_changed_descendant_nests() {
        let tree = build_node_tree(&[
            node("./A/B", ChangeType::Created, &["position"]),
            node(".", ChangeType::Modified, &["visible"]),
            node("./A", ChangeType::Modified, &["scale"]),
        ]);
        assert_eq!(tree.len(), 1);
        let root = &tree[0];
        assert_eq!(root.section, ".");
        assert_eq!(root.children.len(), 1);
        let a = &root.children[0];
        assert_eq!(a.section, "./A");
        assert_eq!(a.label, "A (Modified)");
        assert_eq!(a.children.len(), 1);
        assert_eq!(a.children[0].label, "B (Added)");
        assert_eq!(a.children[0].kind, ChangeType::Created);
    }

    #[test]
    fn unchanged_chain_is_merged_into_one_label() {
        let tree = build_node_tree(&[
            node("./A/B/C", ChangeType::Modified, &["visible"]),
            node("./A/B/D", ChangeType::Modified, &["visible"]),
        ]);
        // "." has a single child "./A", which has a single child "./A/B", which has two changed children
        assert_eq!(tree.len(), 1);
        let grouping = &tree[0];
        assert!(grouping.is_grouping);
        assert_eq!(grouping.section, "./A/B");
        assert_eq!(grouping.label, "A/B");
        assert_eq!(grouping.children.len(), 2);
        assert_eq!(grouping.children[0].section, "./A/B/C");
        assert_eq!(grouping.children[0].label, "C (Modified)");
    }

    #[test]
    fn skips_modified_nodes_without_property_changes() {
        let tree = build_node_tree(&[node("./A", ChangeType::Modified, &[])]);
        assert!(tree.is_empty());
        // ...but keeps added nodes, even without properties
        let tree = build_node_tree(&[node("./A", ChangeType::Created, &[])]);
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn modified_parent_without_properties_still_groups_children() {
        let tree = build_node_tree(&[
            node("./A", ChangeType::Modified, &[]),
            node("./A/B", ChangeType::Modified, &["visible"]),
            node("./A/C", ChangeType::Modified, &["visible"]),
        ]);
        assert_eq!(tree.len(), 1);
        assert!(tree[0].is_grouping);
        assert_eq!(tree[0].children.len(), 2);
    }

    #[test]
    fn skips_instanced_temporary_nodes() {
        let tree = build_node_tree(&[
            node("./@Foo@2", ChangeType::Modified, &["visible"]),
            node("./@Foo@2/Child", ChangeType::Modified, &["visible"]),
        ]);
        assert!(tree.is_empty());
    }

    #[test]
    fn hover_paths_only_include_sections_with_properties() {
        let tree = build_node_tree(&[
            node("./A/B", ChangeType::Modified, &["position"]),
            node("./A/C", ChangeType::Modified, &["position"]),
            node("./D", ChangeType::Modified, &["position"]),
        ]);
        let all = hover_paths(&tree);
        assert_eq!(all, vec!["./A/B", "./A/C", "./D"]);
        // "." has two changed branches, so it stays as a grouping section with A and D below it.
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 2);
        // Hovering over the grouping section of A only covers B and C.
        let group_a = &tree[0].children[0];
        assert!(group_a.is_grouping);
        assert_eq!(group_a.hover_paths(), vec!["./A/B", "./A/C"]);
    }
}
