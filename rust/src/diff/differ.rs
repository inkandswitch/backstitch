use std::
    collections::{HashMap, HashSet}
;

use godot::{
    classes::ResourceLoader, global, obj::{EngineEnum, Singleton}
};
use tracing::instrument;

use crate::{
    diff::{resource_differ::BinaryResourceDiff, scene_differ::{SceneDiff, TextResourceDiff}, text_differ::TextDiff}, fs::file_utils::{FileContent, FileSystemEvent}, helpers::{history_path::HistoryRefPath, history_ref::HistoryRef}, parser::godot_parser::{GodotScene, TypeOrInstance}, project::branch_db::BranchDb
};

/// The type of change that occurred in a diff.
#[derive(Clone, Debug, PartialEq)]
pub enum ChangeType {
    /// The element was added.
    Added,
    /// The element was modified.
    Modified,
    /// The element was removed.
    Removed,
}

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
        Self {
            branch_db,
        }
    }

    /// Loads an ExtResource given a path.
    pub(super) async fn start_load_ext_resource(
        &self,
        path: &str,
        ref_: &HistoryRef
    ) -> Result<String, String> {
        let history_ref_path = HistoryRefPath::make_path_string(ref_, path).map_err(|_| "Invalid history ref path".to_string())?;

        return match ResourceLoader::singleton().load_threaded_request(&history_ref_path) {
            global::Error::OK => Ok(history_ref_path),
            e => Err(format!("load_threaded_request failed ({})", e.as_str().to_string())),
        };
    }

    /// Computes the diff between the two sets of heads.
    #[instrument(skip_all, level = tracing::Level::DEBUG)]
    pub async fn get_diff(&self, before: &HistoryRef, after: &HistoryRef) -> ProjectDiff {
        if before == after {
            tracing::debug!("no changes");
            return ProjectDiff::default();
        }

        // TODO: refactor `get_changed_file_content_between_refs` to not globalize the paths so we don't have to re-localize them here
        // Get the set of new file content that has changed
        let Some(new_file_contents) = self
            .branch_db
            .get_changed_file_content_between_refs(Some(before), after, false)
            .await
            .and_then(|events| Some(events.into_iter().map(|event| {
                match event {
                    FileSystemEvent::FileCreated(path, content) => (self.branch_db.localize_path(&path), (content, ChangeType::Added)),
                    FileSystemEvent::FileModified(path, content) => (self.branch_db.localize_path(&path), (content, ChangeType::Modified)),
                    FileSystemEvent::FileDeleted(path) => (self.branch_db.localize_path(&path), (FileContent::Deleted, ChangeType::Removed)),
                }
            }).collect::<HashMap<String, (FileContent, ChangeType)>>()))
        else {
            // Something went wrong
            return ProjectDiff::default();
        };
        if new_file_contents.is_empty() {
            return ProjectDiff::default();
        }

        let changed_filter: HashSet<String> = new_file_contents
            .iter()
            .map(|event| event.0.clone())
            .collect::<HashSet<String>>();

        // We do need to compare the new files to the old files, so grab the old contents with a filter
        let Some(old_file_contents) = self
            .branch_db
            .get_files_at_ref(before, &changed_filter)
            .await
        else {
            // Something went wrong
            return ProjectDiff::default();
        };

        let mut diffs: Vec<Diff> = vec![];

        let mut old_scenes_to_load: HashSet<String> = HashSet::new();
        let mut new_scenes_to_load: HashSet<String> = HashSet::new();
        let mut diff_idx_to_needed_map: HashMap<usize, HashMap<usize, (bool, String)>> = HashMap::new();

        for (path, (new_file_content, change_type)) in &new_file_contents {
            let old_file_content = old_file_contents
                .get(path)
                .unwrap_or(&FileContent::Deleted);

            if matches!(old_file_content, FileContent::Scene(_))
                || matches!(new_file_content, FileContent::Scene(_))
            {
                let old_scene = match old_file_content {
                    FileContent::Scene(s) => Some(s),
                    _ => None,
                };
                let new_scene = match new_file_content {
                    FileContent::Scene(s) => Some(s),
                    _ => None,
                };


                let resource_type = match (old_scene, new_scene) {
                    (None, Some(scene)) => scene.resource_type.clone(),
                    (Some(scene), None) => scene.resource_type.clone(),
                    (_, Some(scene)) => scene.resource_type.clone(),
                    (_, _) => "".to_string(),
                };
                if resource_type == "PackedScene" {
                    let mut idx_to_instance_path_needed: HashMap<usize, (bool, String)> = HashMap::new();
                    let mut scene_diff = self.get_scene_diff(&path, old_scene, new_scene, before, after).await;
                    for (idx, node_diff) in scene_diff.changed_nodes.iter_mut().enumerate() {
                        if let Some(TypeOrInstance::Instance(instance_id)) = node_diff.node_type.as_ref() {
                            if node_diff.change_type == ChangeType::Modified || node_diff.change_type == ChangeType::Added {
                                let Some(instance_path) = new_scene.as_ref().unwrap().get_ext_resource_path(instance_id) else {
                                    continue;
                                };
                                if let Some((FileContent::Scene(new_file_content), _)) = new_file_contents.get(&instance_path) {
                                    node_diff.node_type =  new_file_content.get_root_node_type();
                                } else {
                                    new_scenes_to_load.insert(instance_path.clone());
                                    idx_to_instance_path_needed.insert(idx, (true, instance_path));
                                }
                            } else if node_diff.change_type == ChangeType::Removed {
                                let Some(instance_path) = old_scene.as_ref().unwrap().get_ext_resource_path(instance_id) else {
                                    continue;
                                };
                                if let Some(FileContent::Scene(old_file_content)) = old_file_contents.get(&instance_path) {
                                    node_diff.node_type = old_file_content.get_root_node_type();
                                } else {
                                    old_scenes_to_load.insert(instance_path.clone());
                                    idx_to_instance_path_needed.insert(idx, (false, instance_path));
                                }
                            }
                        }
                    }
                    if !idx_to_instance_path_needed.is_empty() {
                        diff_idx_to_needed_map.insert(diffs.len(), idx_to_instance_path_needed);
                    }
                    diffs.push(Diff::Scene(
                        scene_diff,
                    ));
                } else {
                    diffs.push(Diff::TextResourceDiff(
                        self.get_text_resource_diff(&path, old_scene, new_scene, before, after)
                        .await,
                    ));
                }
            } else if matches!(old_file_content, FileContent::Binary(_))
                || matches!(new_file_content, FileContent::Binary(_))
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
            } else if matches!(old_file_content, FileContent::String(_))
                || matches!(new_file_content, FileContent::String(_))
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

        if !old_scenes_to_load.is_empty() || !new_scenes_to_load.is_empty() {
            let old_needed_content = self.branch_db.get_files_at_ref(before, &old_scenes_to_load).await.unwrap_or(HashMap::new());
            let new_needed_content = self.branch_db.get_files_at_ref(after, &new_scenes_to_load).await.unwrap_or(HashMap::new());
            for (diff_idx, idx_to_instance_path_needed) in diff_idx_to_needed_map.iter() {
                let Diff::Scene(scene_diff) = &mut diffs[*diff_idx] else {
                    continue;
                };
                for (node_idx, (is_new, instance_path)) in idx_to_instance_path_needed.iter() {
                    let contents = if *is_new {&old_needed_content } else {&new_needed_content};
                    if let Some(FileContent::Scene(new_file_content)) = contents.get(instance_path) {
                        scene_diff.changed_nodes[*node_idx].node_type = new_file_content.get_root_node_type();
                    }
                }
            }
        }


        ProjectDiff { file_diffs: diffs }
    }
}
