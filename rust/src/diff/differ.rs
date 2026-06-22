use std::collections::{HashMap, HashSet};

use godot::{
    classes::ResourceLoader,
    global,
    obj::{EngineEnum, Singleton},
};
use tracing::instrument;

use crate::{
    diff::{
        resource_differ::BinaryResourceDiff,
        scene_differ::{SceneDiff, TextResourceDiff},
        text_differ::TextDiff,
    },
    fs::file_utils::FileContent,
    helpers::{history_path::HistoryRefPath, history_ref::HistoryRef, utils::ChangeType},
    parser::godot_parser::TypeOrInstance,
    project::branch_db::BranchDb,
};

/// A diff for a single file.
#[derive(Clone, Debug)]
pub enum Diff {
    /// A scene file diff.
    Scene(SceneDiff),
    /// A text resource diff.
    TextResourceDiff(TextResourceDiff),
    /// A resource file diff.
    BinaryResource(BinaryResourceDiff),
    /// A text file diff.
    Text(TextDiff),
}

/// A diff for an entire project.
#[derive(Clone, Default, Debug)]
pub struct ProjectDiff {
    /// The file diffs in the project diff.
    pub file_diffs: Vec<Diff>,
}

/// Computes diffs between two sets of heads in a project.
#[derive(Debug)]
pub struct Differ {
    /// The [BranchDb] we're working off.
    branch_db: BranchDb,
}

impl Differ {
    /// Creates a new [Differ].
    pub fn new(branch_db: BranchDb) -> Self {
        Self { branch_db }
    }

    /// Loads an ExtResource given a path.
    pub(super) async fn start_load_ext_resource(
        &self,
        path: &str,
        ref_: &HistoryRef,
    ) -> Result<String, String> {
        let history_ref_path = HistoryRefPath::make_path_string(ref_, path)
            .map_err(|_| "Invalid history ref path".to_string())?;

        match ResourceLoader::singleton().load_threaded_request(&history_ref_path) {
            global::Error::OK => Ok(history_ref_path),
            e => Err(format!("load_threaded_request failed ({})", e.as_str())),
        }
    }

    /// Computes the diff between the two sets of heads.
    #[instrument(skip_all, level = tracing::Level::DEBUG)]
    pub async fn get_diff(&self, before: &HistoryRef, after: &HistoryRef) -> Option<ProjectDiff> {
        if before == after {
            tracing::debug!("no changes");
            return None;
        }

        let changed_files = self
            .branch_db
            .get_changed_files_between_refs(Some(before), after)
            .await?;

        tracing::debug!("diffing {} changes...", changed_files.len());

        if changed_files.is_empty() {
            return None;
        }

        let changed_filter = changed_files.keys().cloned().collect();

        let new_file_contents = self
            .branch_db
            .get_files_at_ref(after, &changed_filter)
            .await?;
        let old_file_contents = self
            .branch_db
            .get_files_at_ref(before, &changed_filter)
            .await?;

        let mut diffs: Vec<Diff> = vec![];

        let mut old_scenes_to_load = HashSet::new();
        let mut new_scenes_to_load = HashSet::new();
        let mut diff_idx_to_needed_map: HashMap<usize, HashMap<usize, (bool, String)>> =
            HashMap::new();

        for (path, change_type) in &changed_files {
            let old_file_content = old_file_contents.get(path);
            let new_file_content = new_file_contents.get(path);
            if matches!(old_file_content, Some(FileContent::Scene(_)))
                || matches!(new_file_content, Some(FileContent::Scene(_)))
            {
                let old_scene = match old_file_content {
                    Some(FileContent::Scene(s)) => Some(s),
                    _ => None,
                };
                let new_scene = match new_file_content {
                    Some(FileContent::Scene(s)) => Some(s),
                    _ => None,
                };

                let resource_type = match (old_scene, new_scene) {
                    (None, Some(scene)) => scene.resource_type.clone(),
                    (Some(scene), None) => scene.resource_type.clone(),
                    (_, Some(scene)) => scene.resource_type.clone(),
                    (_, _) => "".to_string(),
                };
                if resource_type == "PackedScene" {
                    let mut scene_diff = self
                        .get_scene_diff(
                            path,
                            old_scene.map(|v| &**v),
                            new_scene.map(|v| &**v),
                            before,
                            after,
                        )
                        .await;
                    // For a scene diff, we need to do some extra work.
                    // Instanced scenes need their node type set properly for default values to work.
                    // This maps node index in the diff to -> (is_new, instance_path)
                    let mut idx_to_instance_path_needed = HashMap::new();
                    // Iterate through the indices of the changed nodes
                    for (idx, node_diff) in scene_diff.changed_nodes.iter_mut().enumerate() {
                        let Some(TypeOrInstance::Instance(instance_id)) =
                            node_diff.node_type.as_ref()
                        else {
                            continue;
                        };

                        // If we're a created node, we can use the new type.
                        if node_diff.change_type == ChangeType::Modified
                            || node_diff.change_type == ChangeType::Created
                        {
                            let Some(instance_path) = new_scene
                                .as_ref()
                                .unwrap()
                                .get_ext_resource_path(instance_id)
                            else {
                                continue;
                            };
                            // If we already have the contents of the instanced scene loaded, yay!
                            // We can set the type now.
                            if let Some(FileContent::Scene(new_file_content)) =
                                new_file_contents.get(&instance_path)
                            {
                                node_diff.node_type = new_file_content.get_root_node_type();
                            }
                            // Otherwise, we need to load it and check there (later).
                            else {
                                new_scenes_to_load.insert(instance_path.clone());
                                idx_to_instance_path_needed.insert(idx, (true, instance_path));
                            }
                        }
                        // Otherwise, we must check the old node for the type.
                        else if node_diff.change_type == ChangeType::Deleted {
                            let Some(instance_path) = old_scene
                                .as_ref()
                                .unwrap()
                                .get_ext_resource_path(instance_id)
                            else {
                                continue;
                            };
                            // If we already have the contents of the instanced scene loaded, yay!
                            // We can set the type now.
                            if let Some(FileContent::Scene(old_file_content)) =
                                old_file_contents.get(&instance_path)
                            {
                                node_diff.node_type = old_file_content.get_root_node_type();
                            }
                            // Otherwise, we need to load it and check there (later).
                            else {
                                old_scenes_to_load.insert(instance_path.clone());
                                idx_to_instance_path_needed.insert(idx, (false, instance_path));
                            }
                        }
                    }
                    if !idx_to_instance_path_needed.is_empty() {
                        diff_idx_to_needed_map.insert(diffs.len(), idx_to_instance_path_needed);
                    }
                    diffs.push(Diff::Scene(scene_diff));
                } else {
                    diffs.push(Diff::TextResourceDiff(
                        self.get_text_resource_diff(
                            path,
                            old_scene.map(|v| &**v),
                            new_scene.map(|v| &**v),
                            before,
                            after,
                        )
                        .await,
                    ));
                }
            } else if matches!(old_file_content, Some(FileContent::Binary(_)))
                || matches!(new_file_content, Some(FileContent::Binary(_)))
            {
                // This is a binary file, so use a resource diff
                diffs.push(Diff::BinaryResource(
                    self.get_binary_resource_diff(
                        path,
                        change_type.clone(),
                        old_file_content,
                        new_file_content,
                        before,
                        after,
                    )
                    .await,
                ));
            } else if matches!(old_file_content, Some(FileContent::String(_)))
                || matches!(new_file_content, Some(FileContent::String(_)))
            {
                // This is a text file, so do a text diff.
                diffs.push(Diff::Text(self.get_text_diff(
                    path,
                    change_type.clone(),
                    old_file_content,
                    new_file_content,
                )));
            } else {
                // We have no idea what type of file this is, so skip it
                continue;
            }
        }

        // This loads all the get_files_at_ref calls needed by the extra scene diff work, setting up instance types.
        let old_needed_scenes = self
            .branch_db
            .get_files_at_ref(before, &old_scenes_to_load)
            .await
            .unwrap_or(HashMap::new());
        let new_needed_scenes = self
            .branch_db
            .get_files_at_ref(after, &new_scenes_to_load)
            .await
            .unwrap_or(HashMap::new());

        // For all the diffs, setup the instance paths.
        for (diff_idx, idx_to_instance_path_needed) in diff_idx_to_needed_map.iter() {
            let Diff::Scene(scene_diff) = &mut diffs[*diff_idx] else {
                continue;
            };
            // For all the instanced nodes in the needed scene diffs, setup the instance paths.
            for (node_idx, (is_new, instance_path)) in idx_to_instance_path_needed.iter() {
                let contents = if *is_new {
                    &old_needed_scenes
                } else {
                    &new_needed_scenes
                };
                if let Some(FileContent::Scene(new_file_content)) = contents.get(instance_path) {
                    scene_diff.changed_nodes[*node_idx].node_type =
                        new_file_content.get_root_node_type();
                }
            }
        }
        Some(ProjectDiff { file_diffs: diffs })
    }
}
