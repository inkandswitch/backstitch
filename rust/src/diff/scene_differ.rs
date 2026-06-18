use std::collections::{HashMap, HashSet};

use crate::{
    diff::differ::Differ,
    helpers::{history_ref::HistoryRef, utils::ChangeType},
    parser::{
        godot_parser::{
            ExternalResourceNode, GodotNode, GodotScene, NodeId, SubResourceNode, TypeOrInstance,
        },
        parser_defs::OrderedProperty,
    },
};

/// Represents a diff of a scene, with a scene path and a list of changed nodes.
#[derive(Clone, Debug)]
pub struct SceneDiff {
    /// The path of the scene.
    pub path: String,
    /// The change type for the scene.
    pub change_type: ChangeType,
    /// The nodes changed in this diff.
    pub changed_nodes: Vec<NodeDiff>,
}

impl SceneDiff {
    fn new(path: String, change_type: ChangeType, changed_nodes: Vec<NodeDiff>) -> SceneDiff {
        SceneDiff {
            path,
            change_type,
            changed_nodes,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextResourceDiff {
    /// The path of the scene.
    pub path: String,
    /// PackedScene or other
    pub resource_type: String,
    /// The change type for the scene.
    pub change_type: ChangeType,
    /// The sub resources changed in this diff.
    pub changed_sub_resources: Vec<SubResourceDiff>,
    pub changed_main_resource: Option<SubResourceDiff>,
}

impl TextResourceDiff {
    fn new(
        path: String,
        resource_type: String,
        change_type: ChangeType,
        changed_sub_resources: Vec<SubResourceDiff>,
        changed_main_resource: Option<SubResourceDiff>,
    ) -> TextResourceDiff {
        TextResourceDiff {
            path,
            resource_type,
            change_type,
            changed_sub_resources,
            changed_main_resource,
        }
    }
}

/// Represents a diff of a single node within a scene, with a collection of changed properties.
#[derive(Clone, Debug)]
pub struct NodeDiff {
    /// How the node has been changed.
    pub change_type: ChangeType,
    /// The path of the node within the scene.
    pub node_path: String,
    /// The type of the node.
    pub node_type: Option<TypeOrInstance>,
    /// The changed properties of the node.
    pub changed_properties: HashMap<String, PropertyDiff>,
}

impl NodeDiff {
    pub fn new(
        change_type: ChangeType,
        node_path: String,
        node_type: Option<TypeOrInstance>,
        changed_properties: HashMap<String, PropertyDiff>,
    ) -> NodeDiff {
        NodeDiff {
            change_type,
            node_path,
            node_type,
            changed_properties,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubResourceDiff {
    pub change_type: ChangeType,
    pub sub_resource_id: String,
    pub resource_type: String,
    pub script_class: Option<String>,
    pub changed_properties: HashMap<String, PropertyDiff>,
}

impl SubResourceDiff {
    pub fn new(
        change_type: ChangeType,
        sub_resource_id: String,
        resource_type: String,
        script_class: Option<String>,
        changed_properties: HashMap<String, PropertyDiff>,
    ) -> SubResourceDiff {
        SubResourceDiff {
            change_type,
            sub_resource_id,
            resource_type,
            script_class,
            changed_properties,
        }
    }
}
/// Represents a diff of a single Property within a Node, within a Scene.
#[derive(Clone, Debug)]
pub struct PropertyDiff {
    /// The name of the changed property.
    pub name: String,
    /// The change type of the property.
    pub change_type: ChangeType,
    /// The old value of the property, if it existed.
    pub old_value: Option<VariantValue>,
    /// The new value of the property, if it still exists.
    pub new_value: Option<VariantValue>,
}

impl PropertyDiff {
    pub fn new(
        name: String,
        change_type: ChangeType,
        old_value: Option<VariantValue>,
        new_value: Option<VariantValue>,
    ) -> Self {
        PropertyDiff {
            name,
            change_type,
            old_value,
            new_value,
        }
    }
}

/// The different types of Godot-recognized string values that can be stored in a Variant.
#[derive(PartialEq, Debug)]
enum VariantStrValue {
    /// A normal string that doesn't refer to a resource.
    Variant(String),
    /// A Godot resource path string.
    ResourcePath(String),
    /// A Godot sub-resource identifier string.
    SubResourceID(String),
    /// A Godot external resource identifier string.
    ExtResourceID(String),
    /// A default value for a property
    DefaultValue(Option<TypeOrInstance>, String),
}

#[derive(Clone, Debug)]
pub enum VariantValue {
    /// A normal variant string
    Variant(String),
    /// Default value of a variant of shape (type/instance name, property name)
    DefaultValue(Option<TypeOrInstance>, String),
    /// Lazy loaded variant with of shape (original path, load path). Converted to a LazyLoadToken at usage time.
    LazyLoadData(String, String),
}

/// Implement the to_string method for this enum
impl std::fmt::Display for VariantStrValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariantStrValue::Variant(s) => write!(f, "{}", s),
            VariantStrValue::ResourcePath(s) => write!(f, "Resource({})", s),
            VariantStrValue::SubResourceID(s) => write!(f, "SubResource({})", s),
            VariantStrValue::ExtResourceID(s) => write!(f, "ExtResource({})", s),
            VariantStrValue::DefaultValue(_, _) => write!(f, "<default_value>"),
        }
    }
}

trait PropertyGetter {
    fn get_property(&self, prop: &str) -> Option<&OrderedProperty>;
    fn get_properties(&self) -> &HashMap<String, OrderedProperty>;
    fn get_type_or_instance(&self) -> Option<TypeOrInstance>;
}

impl PropertyGetter for GodotNode {
    fn get_property(&self, prop: &str) -> Option<&OrderedProperty> {
        self.properties.get(prop)
    }
    fn get_properties(&self) -> &HashMap<String, OrderedProperty> {
        &self.properties
    }
    fn get_type_or_instance(&self) -> Option<TypeOrInstance> {
        self.type_or_instance.clone()
    }
}

impl PropertyGetter for SubResourceNode {
    fn get_property(&self, prop: &str) -> Option<&OrderedProperty> {
        self.properties.get(prop)
    }
    fn get_properties(&self) -> &HashMap<String, OrderedProperty> {
        &self.properties
    }
    fn get_type_or_instance(&self) -> Option<TypeOrInstance> {
        Some(TypeOrInstance::Type(self.resource_type.clone()))
    }
}

/// Implement scene-related functions on the Differ
impl Differ {
    /// Generate a [SceneDiff] between the previous and current heads.
    pub(super) async fn get_scene_diff(
        &self,
        path: &str,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> SceneDiff {
        let mut node_ids = HashSet::new();
        let mut sub_resource_ids = HashSet::new();
        let mut ext_resource_ids = HashSet::new();

        // Collect all the relevant node IDs, sub resource IDs, and ext resource IDs from both scenes.
        if let Some(old_scene) = old_scene {
            Self::get_ids_from_scene(
                old_scene,
                &mut node_ids,
                &mut ext_resource_ids,
                &mut sub_resource_ids,
            );
        }
        if let Some(new_scene) = new_scene {
            Self::get_ids_from_scene(
                new_scene,
                &mut node_ids,
                &mut ext_resource_ids,
                &mut sub_resource_ids,
            );
        }

        let mut node_diffs = Vec::new();

        // Diff each node
        for node_id in &node_ids {
            let old_node = old_scene.as_ref().and_then(|s| s.get_node(node_id));
            let new_node = new_scene.as_ref().and_then(|s| s.get_node(node_id));

            let Some(diff) = self
                .get_node_diff(
                    node_id, old_node, new_node, old_scene, new_scene, before, after,
                )
                .await
            else {
                // If the node has no changes or is otherwise invalid, just skip this one.
                continue;
            };

            node_diffs.push(diff);
        }

        SceneDiff::new(
            path.to_owned(),
            match (old_scene, new_scene) {
                (None, Some(_)) => ChangeType::Created,
                (Some(_), None) => ChangeType::Deleted,
                (_, _) => ChangeType::Modified,
            },
            node_diffs,
        )
    }

    pub(super) async fn get_text_resource_diff(
        &self,
        path: &str,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> TextResourceDiff {
        let mut node_ids = HashSet::new();
        let mut sub_resource_ids = HashSet::new();
        let mut ext_resource_ids = HashSet::new();

        let mut resource_type: String = "".to_string();
        let mut script_class: Option<String> = None;
        // Collect all the relevant node IDs, sub resource IDs, and ext resource IDs from both scenes.
        if let Some(old_scene) = old_scene {
            resource_type = old_scene.resource_type.clone();
            script_class = old_scene.script_class.clone();
            Self::get_ids_from_scene(
                old_scene,
                &mut node_ids,
                &mut ext_resource_ids,
                &mut sub_resource_ids,
            );
        }
        if let Some(new_scene) = new_scene {
            resource_type = new_scene.resource_type.clone();
            script_class = new_scene.script_class.clone();
            Self::get_ids_from_scene(
                new_scene,
                &mut node_ids,
                &mut ext_resource_ids,
                &mut sub_resource_ids,
            );
        }

        if let Some(script_class) = script_class.as_ref() {
            tracing::warn!("!!! script_class: {}", script_class);
        }

        let mut changed_sub_resources = Vec::new();
        // Diff each node
        for sub_resource_id in &sub_resource_ids {
            let old_sub_resource = old_scene
                .as_ref()
                .and_then(|s| s.sub_resources.get(sub_resource_id));
            let new_sub_resource = new_scene
                .as_ref()
                .and_then(|s| s.sub_resources.get(sub_resource_id));

            let Some(diff) = self
                .get_sub_resource_diff(
                    sub_resource_id,
                    old_sub_resource,
                    new_sub_resource,
                    old_scene,
                    new_scene,
                    before,
                    after,
                )
                .await
            else {
                // If the node has no changes or is otherwise invalid, just skip this one.
                continue;
            };

            changed_sub_resources.push(diff);
        }
        let mut changed_main_resource = self
            .get_sub_resource_diff(
                "",
                old_scene.and_then(|s| s.main_resource.as_ref()),
                new_scene.and_then(|s| s.main_resource.as_ref()),
                old_scene,
                new_scene,
                before,
                after,
            )
            .await;

        if let Some(s) = changed_main_resource.as_mut() {
            s.script_class = script_class;
        }

        TextResourceDiff::new(
            path.to_string(),
            resource_type,
            match (old_scene, new_scene) {
                (None, Some(_)) => ChangeType::Created,
                (Some(_), None) => ChangeType::Deleted,
                (_, _) => ChangeType::Modified,
            },
            changed_sub_resources,
            changed_main_resource,
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn get_sub_resource_diff(
        &self,
        sub_resource_id: &str,
        old_node: Option<&SubResourceNode>,
        new_node: Option<&SubResourceNode>,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> Option<SubResourceDiff> {
        if old_node.is_none() && new_node.is_none() {
            return None;
        }

        let mut changed_properties = HashMap::new();
        let old_class_name = old_node.and_then(|n| n.get_type_or_instance().map(|t| t.to_string()));
        let new_class_name = new_node.and_then(|n| n.get_type_or_instance().map(|t| t.to_string()));

        // Collect all properties from new and old scenes
        let mut props: HashSet<String> = HashSet::new();
        if let Some(node) = old_node {
            for key in node.get_properties().keys() {
                let _ = props.insert(key.to_string());
            }
        }
        if let Some(node) = new_node {
            for key in node.get_properties().keys() {
                let _ = props.insert(key.to_string());
            }
        }
        for prop in &props {
            if let Some(prop_diff) = self
                .get_property_diff(
                    prop, old_node, new_node, old_scene, new_scene, before, after,
                )
                .await
            {
                changed_properties.insert(prop.clone(), prop_diff);
            }
        }
        Some(SubResourceDiff::new(
            match (old_node, new_node) {
                (None, Some(_)) => ChangeType::Created,
                (Some(_), None) => ChangeType::Deleted,
                (_, _) => ChangeType::Modified,
            },
            sub_resource_id.to_string(),
            old_class_name.or(new_class_name).unwrap_or_default(),
            None,
            changed_properties,
        ))
    }

    /// Generate a [NodeDiff] between two nodes.
    #[allow(clippy::too_many_arguments)]
    async fn get_node_diff(
        &self,
        node_id: &NodeId,
        old_node: Option<&impl PropertyGetter>,
        new_node: Option<&impl PropertyGetter>,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> Option<NodeDiff> {
        if old_node.is_none() && new_node.is_none() {
            return None;
        }

        let old_class_name = old_node
            .map(|n| n.get_type_or_instance())
            .unwrap_or_default();
        let new_class_name = new_node
            .map(|n| n.get_type_or_instance())
            .unwrap_or_default();

        let mut changed_properties = HashMap::new();

        // Collect all properties from new and old scenes
        let mut props: HashSet<String> = HashSet::new();
        if let Some(node) = old_node {
            for key in node.get_properties().keys() {
                let _ = props.insert(key.to_string());
            }
        }
        if let Some(node) = new_node {
            for key in node.get_properties().keys() {
                let _ = props.insert(key.to_string());
            }
        }

        // Iterate through the props
        for prop in &props {
            if let Some(prop_diff) = self
                .get_property_diff(
                    prop, old_node, new_node, old_scene, new_scene, before, after,
                )
                .await
            {
                changed_properties.insert(prop.clone(), prop_diff);
            }
        }

        // If there wasn't any real changes, there's no actual difference!
        if old_node.is_some()
            && new_node.is_some()
            && changed_properties.is_empty()
            && old_class_name == new_class_name
        {
            return None;
        }

        Some(NodeDiff::new(
            match (old_node, new_node) {
                (None, Some(_)) => ChangeType::Created,
                (Some(_), None) => ChangeType::Deleted,
                (_, _) => ChangeType::Modified,
            },
            // have to do something like this, because get_node_path panics if the node doesn't exist in the scene
            match (old_node, new_node) {
                (None, Some(_)) => new_scene?.get_node_path(node_id),
                (Some(_), None) => old_scene?.get_node_path(node_id),
                (_, _) => new_scene?.get_node_path(node_id),
            },
            new_class_name.or(old_class_name),
            changed_properties,
        ))
    }

    /// Returns the [VariantStrValue] of a property on a node, or the default value if the property doesn't
    /// exist on the node.
    /// If the node itself doesn't exist, returns [None].
    fn get_varstr_or_default(
        prop: &str,
        node: Option<&impl PropertyGetter>,
    ) -> Option<VariantStrValue> {
        // If this node never existed, don't provide a value.
        let node = node?;
        match node.get_property(prop) {
            Some(val) => Some(Self::get_varstr_value(val.get_value())),
            None => Some(VariantStrValue::DefaultValue(
                node.get_type_or_instance(),
                prop.to_string(),
            )),
        }
    }

    /// Returns a [PropertyDiff] comparing the old property value versus the new one.
    /// Returns [None] if neither node is valid, or if the value has not meaningfully changed.
    #[allow(clippy::too_many_arguments)]
    async fn get_property_diff(
        &self,
        prop: &str,
        old_node: Option<&impl PropertyGetter>,
        new_node: Option<&impl PropertyGetter>,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> Option<PropertyDiff> {
        // If neither node is valid, there's no valid property diff.
        if new_node.is_none() && old_node.is_none() {
            return None;
        };

        // Slightly weird hack: Diff against the default instead of the normal property.
        let old_value = Self::get_varstr_or_default(prop, old_node);
        let new_value = Self::get_varstr_or_default(prop, new_node);

        // Skip in case of no changes
        if !self.did_prop_change(old_value.as_ref(), new_value.as_ref(), old_scene, new_scene) {
            return None;
        }

        // Expensive: Load any ext resources and turn them into Variants
        let old = match &old_value {
            Some(v) => Some(
                self.get_prop_value(v, old_scene, true, prop == "script", before, after)
                    .await,
            ),
            None => None,
        };
        let new = match &new_value {
            Some(v) => Some(
                self.get_prop_value(v, new_scene, false, prop == "script", before, after)
                    .await,
            ),
            None => None,
        };

        Some(PropertyDiff::new(
            prop.to_string(),
            // We check for node add or remove intentionally, here, because otherwise we're just diffing a Modified prop against
            // the default value retrieved earlier.
            match (old_node, new_node) {
                (None, Some(_)) => ChangeType::Created,
                (Some(_), None) => ChangeType::Deleted,
                (_, _) => ChangeType::Modified,
            },
            old,
            new,
        ))
    }

    /// Check deeply to see if a subresource has changed.
    fn did_sub_resource_change(
        &self,
        old_resource: Option<&SubResourceNode>,
        new_resource: Option<&SubResourceNode>,
        old_scene: &GodotScene,
        new_scene: &GodotScene,
    ) -> bool {
        let (old_resource, new_resource) = match (old_resource, new_resource) {
            (None, None) => return false,         // subresource never existed
            (None, Some(_)) => return true,       // subresource added
            (Some(_), None) => return true,       // subresource removed
            (Some(old), Some(new)) => (old, new), // keep looking
        };

        // If type has changed, subresource has definitely changed.
        if old_resource.resource_type != new_resource.resource_type {
            return true;
        }

        for path in old_resource.properties.keys() {
            if !new_resource.properties.contains_key(path) {
                // prop removed
                return true;
            }
        }

        for (path, new_prop) in &new_resource.properties {
            let Some(old_prop) = old_resource.properties.get(path) else {
                // prop added
                return false;
            };

            let old_prop = Self::get_varstr_value(old_prop.get_value());
            let new_prop = Self::get_varstr_value(new_prop.get_value());
            if self.did_prop_change(
                Some(&old_prop),
                Some(&new_prop),
                Some(old_scene),
                Some(new_scene),
            ) {
                // prop changed
                return true;
            }
        }
        false
    }

    /// Check shallowly to see if an ext resource changed. Returns true if the path, type, etc has changed, but not
    /// the contents itself.
    fn did_ext_resource_reference_change(
        &self,
        old_resource: Option<&ExternalResourceNode>,
        new_resource: Option<&ExternalResourceNode>,
    ) -> bool {
        let (old_resource, new_resource) = match (old_resource, new_resource) {
            (None, None) => return false,         // resource never existed
            (None, Some(_)) => return true,       // resource added
            (Some(_), None) => return true,       // resource removed
            (Some(old), Some(new)) => (old, new), // keep looking
        };

        old_resource.resource_type != new_resource.resource_type
            || old_resource.path != new_resource.path
            || old_resource.uid != new_resource.uid
    }

    /// Check to see if a property has changed, including deep lookups of subresources and shallow lookup of extresources.
    fn did_prop_change(
        &self,
        old_value: Option<&VariantStrValue>,
        new_value: Option<&VariantStrValue>,
        old_scene: Option<&GodotScene>,
        new_scene: Option<&GodotScene>,
    ) -> bool {
        // If either are null, or both are none, easy exit
        let (old_value, new_value) = match (old_value, new_value) {
            (None, None) => return false,         // resource never existed
            (None, Some(_)) => return true,       // resource added
            (Some(_), None) => return true,       // resource removed
            (Some(old), Some(new)) => (old, new), // keep looking
        };

        // if either scene is null, we did change.
        let Some(old_scene) = old_scene else {
            return true;
        };
        let Some(new_scene) = new_scene else {
            return true;
        };
        match (old_value, new_value) {
            // Deeply lookup subresources
            (
                VariantStrValue::SubResourceID(old_value),
                VariantStrValue::SubResourceID(new_value),
            ) => self.did_sub_resource_change(
                old_scene.sub_resources.get(old_value),
                new_scene.sub_resources.get(new_value),
                old_scene,
                new_scene,
            ),
            // Shallowly lookup extresource references
            (
                VariantStrValue::ExtResourceID(old_value),
                VariantStrValue::ExtResourceID(new_value),
            ) => self.did_ext_resource_reference_change(
                old_scene.ext_resources.get(old_value),
                new_scene.ext_resources.get(new_value),
            ),
            // No special lookup needed for regular Variants (definitely) or ResourcePaths (I think?)
            (
                VariantStrValue::ResourcePath(old_value),
                VariantStrValue::ResourcePath(new_value),
            ) => old_value != new_value,
            (VariantStrValue::Variant(old_value), VariantStrValue::Variant(new_value)) => {
                old_value != new_value
            }
            // If the types are different, we've for sure changed
            _ => true,
        }
    }

    /// Returns the value of a given prop, within a given scene.
    /// Normally, it's a String. If it's a (non-script) ExtResource or ResourcePath,
    /// it loads and returns the resource content as a Variant.
    async fn get_prop_value(
        &self,
        prop_value: &VariantStrValue,
        scene: Option<&GodotScene>,
        is_old: bool,
        is_script: bool,
        before: &HistoryRef,
        after: &HistoryRef,
    ) -> VariantValue {
        // Prevent loading script files during the diff and creating issues for the editor
        if is_script {
            return VariantValue::Variant("<Script changed>".to_string());
        }

        let path = match prop_value {
            VariantStrValue::Variant(variant) => {
                return VariantValue::Variant(variant.clone());
            }
            VariantStrValue::SubResourceID(sub_resource_id) => {
                // TODO: add this for scene diffs; for scene diffs we want to display the subresource diffs as child nodes of the parent node.
                // We currently don't support displaying deep subresource diffs, so just inform of a change.
                return VariantValue::Variant(format!(
                    "\"<SubResource {} changed>\"",
                    sub_resource_id
                ));
            }
            VariantStrValue::ResourcePath(resource_path) => resource_path,
            VariantStrValue::ExtResourceID(ext_resource_id) => {
                let p = scene.and_then(|scene| {
                    scene
                        .ext_resources
                        .get(ext_resource_id)
                        .map(|ext_resource| &ext_resource.path)
                });
                let Some(p) = p else {
                    return VariantValue::Variant("\"<ExtResource not found>\"".to_string());
                };
                p
            }
            VariantStrValue::DefaultValue(type_or_instance, prop) => {
                return VariantValue::DefaultValue(type_or_instance.clone(), prop.clone());
            }
        };

        match self
            .start_load_ext_resource(path, if is_old { before } else { after })
            .await
        {
            Ok(load_path) => VariantValue::LazyLoadData(path.clone(), load_path),
            Err(e) => {
                VariantValue::Variant(format!("\"<ExtResource {} load failed ({})>\"", path, e))
            }
        }
    }

    /// Populates [node_ids], [ext_resource_ids], and [sub_resource_ids] from the
    /// given scene.
    fn get_ids_from_scene(
        scene: &GodotScene,
        node_ids: &mut HashSet<NodeId>,
        ext_resource_ids: &mut HashSet<String>,
        sub_resource_ids: &mut HashSet<String>,
    ) {
        for (ext_id, _) in scene.ext_resources.iter() {
            ext_resource_ids.insert(ext_id.clone());
        }
        for (node_id, _) in scene.nodes.iter() {
            node_ids.insert(node_id.clone());
        }
        for (sub_id, _) in scene.sub_resources.iter() {
            sub_resource_ids.insert(sub_id.clone());
        }
    }

    /// Parse a prop_value string into a [VariantStrValue] enum.
    // Ideally, the parser would do this for us... but for now, we're doing it ourselves.
    fn get_varstr_value(prop_value: String) -> VariantStrValue {
        if prop_value.starts_with("Resource(")
            || prop_value.starts_with("SubResource(")
            || prop_value.starts_with("ExtResource(")
        {
            let mut id = prop_value
                .split("(\"")
                .nth(1)
                .unwrap()
                .split("\")")
                .nth(0)
                .unwrap()
                .trim()
                .to_string();
            if prop_value.contains("SubResource(") {
                return VariantStrValue::SubResourceID(id);
            } else if prop_value.contains("ExtResource(") {
                return VariantStrValue::ExtResourceID(id);
            } else {
                // 4.5 and above started writing `Resource(uid, path)` instead of `Resource(path)`, so we need to handle that here.
                // if this is a Resource() with the format "Resource(uid, path)", we need to extract the path
                if id.contains("\", \"") {
                    // discard the uid
                    id = id.split("\", \"").nth(1).unwrap().trim().to_string();
                }
                return VariantStrValue::ResourcePath(id);
            }
        }
        // normal variant string
        VariantStrValue::Variant(prop_value)
    }
}
