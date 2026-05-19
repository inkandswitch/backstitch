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
    helpers::{history_path::HistoryRefPath, history_ref::HistoryRef},
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

        return match ResourceLoader::singleton().load_threaded_request(&history_ref_path) {
            global::Error::OK => Ok(history_ref_path),
            e => Err(format!(
                "load_threaded_request failed ({})",
                e.as_str().to_string()
            )),
        };
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

        for (path, new_file_content) in &new_file_contents {
            let change_type = changed_files.get(path)?;
            let old_file_content = old_file_contents.get(path).unwrap_or(&FileContent::Deleted);

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
                    diffs.push(Diff::Scene(
                        self.get_scene_diff(&path, old_scene, new_scene, before, after)
                            .await,
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

        Some(ProjectDiff { file_diffs: diffs })
    }
}
