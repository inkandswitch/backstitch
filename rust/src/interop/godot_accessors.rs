use std::collections::HashSet;

use godot::obj::Singleton;
use godot::{
    builtin::{GString, PackedStringArray},
    classes::{ClassDb, EditorInterface, Object},
    meta::ToGodot,
    obj::Gd,
};

use crate::interop::patchwork_config::PatchworkConfig;

/// Allows Rust code to easily get and set Patchwork configuration values via Godot's config system.
pub struct PatchworkConfigAccessor {}

impl PatchworkConfigAccessor {
    pub fn get_project_doc_id() -> String {
        PatchworkConfigAccessor::get_project_value("project_doc_id", "")
    }

    pub fn get_project_value(name: &str, default: &str) -> String {
        PatchworkConfig::singleton()
            .bind()
            .get_project_value(GString::from(name), default.to_variant())
            .to::<String>()
    }

    pub fn set_project_value(name: &str, value: &str) {
        PatchworkConfig::singleton()
            .bind_mut()
            .set_project_value(GString::from(name), value.to_variant());
    }

    pub fn get_user_value(name: &str, default: &str) -> String {
        PatchworkConfig::singleton()
            .bind()
            .get_user_value(GString::from(name), default.to_variant())
            .to::<String>()
    }

    #[allow(dead_code)] // will be used later
    pub fn set_user_value(name: &str, value: &str) {
        PatchworkConfig::singleton()
            .bind_mut()
            .set_user_value(GString::from(name), value.to_variant());
    }
}

/// Allows Rust code to access the C++ PatchworkEditor editor module from Godot.
pub struct PatchworkEditorAccessor {}

#[allow(dead_code)] // entire API might not be used yet
impl PatchworkEditorAccessor {
    pub fn import_and_save_resource(
        path: &str,
        import_file_content: &str,
        import_base_path: &str,
    ) -> godot::global::Error {
        // TODO: Depends on https://github.com/godotengine/godot/pull/116861; if this doesn't make it into 4.7, we'll have to figure out something else
        ClassDb::singleton()
            .class_call_static(
                "EditorInterface",
                "import_and_save_resource",
                &[
                    path.to_variant(),
                    import_file_content.to_variant(),
                    import_base_path.to_variant(),
                ],
            )
            .to::<godot::global::Error>()
    }

    pub fn is_editor_importing() -> bool {
        return EditorInterface::singleton()
            .get_resource_filesystem()
            .map(|mut fs| return fs.call("is_importing", &[]).to::<bool>())
            .unwrap_or(false);
    }

    // TODO: This should never be true now because of reload scene changes, but we need to test it
    pub fn is_changing_scene() -> bool {
        let result = ClassDb::singleton()
            .class_call_static("PatchworkEditor", "is_changing_scene", &[])
            .to::<bool>();
        if result {
            tracing::warn!("************** is_changing_scene is TRUE?!");
        }
        result
    }

    // TODO: Confirm that we no longer need this; if not, then we need to PR this to Godot
    // pub fn force_refresh_editor_inspector() {
    //     ClassDb::singleton().class_call_static(
    //         "PatchworkEditor",
    //         "force_refresh_editor_inspector",
    //         &[],
    //     );
    // }

    // TODO: Remove the progress dialog stuff entirely and replace it with something else, like our own modal progress dialog
    pub fn progress_add_task(task: &str, label: &str, steps: i32, can_cancel: bool) {
        ClassDb::singleton().class_call_static(
            "PatchworkEditor",
            "progress_add_task",
            &[
                task.to_variant(),
                label.to_variant(),
                steps.to_variant(),
                can_cancel.to_variant(),
            ],
        );
    }

    pub fn progress_task_step(task: &str, state: &str, step: i32, force_refresh: bool) {
        ClassDb::singleton().class_call_static(
            "PatchworkEditor",
            "progress_task_step",
            &[
                task.to_variant(),
                state.to_variant(),
                step.to_variant(),
                force_refresh.to_variant(),
            ],
        );
    }

    pub fn progress_end_task(task: &str) {
        ClassDb::singleton().class_call_static(
            "PatchworkEditor",
            "progress_end_task",
            &[task.to_variant()],
        );
    }

    pub fn get_unsaved_scripts() -> PackedStringArray {
        let Some(mut script_editor) = EditorInterface::singleton().get_script_editor() else {
            tracing::error!("No script editor found?!");
            return PackedStringArray::new();
        };
        // TODO: when 4.7 is released, use the bound method instead
        script_editor
            .call("get_unsaved_files", &[])
            .to::<PackedStringArray>()
    }

    pub fn unsaved_files_open() -> bool {
        if Self::get_unsaved_scripts().len() > 0 {
            return true;
        }
        // TODO: when 4.7 is released, use the bound method instead
        let unsaved_scenes = EditorInterface::singleton()
            .call("get_unsaved_scenes", &[])
            .to::<PackedStringArray>();
        if unsaved_scenes.len() > 0 {
            return true;
        }
        false
    }

    // TODO: Confirm that we no longer need this; if not, then we need to PR this to Godot
    // pub fn clear_editor_selection() {
    //     ClassDb::singleton().class_call_static("PatchworkEditor", "clear_editor_selection", &[]);
    // }

    fn close_scene_file(path: &str) {
        EditorInterface::singleton().open_scene_from_path(path);
        EditorInterface::singleton().close_scene();
    }

    fn close_script_file(path: &str) {
        let Some(mut script_editor) = EditorInterface::singleton().get_script_editor() else {
            tracing::error!("No script editor found?!");
            return;
        };
        // TODO: when 4.7 is released, use the bound method instead
        script_editor.call("close_file", &[path.to_variant()]);
    }

    pub fn close_files_if_open(paths: &Vec<String>) {
        let open_scenes = EditorInterface::singleton().get_open_scenes();
        let mut script_editor = EditorInterface::singleton().get_script_editor().unwrap();

        let open_scripts = script_editor
            .get_open_scripts()
            .iter_shared()
            .map(|script| script.get_path().to_string())
            .collect::<HashSet<String>>();
        for path in paths {
            if open_scenes.contains(path) {
                EditorInterface::singleton().open_scene_from_path(path);
                EditorInterface::singleton().close_scene();
            }
            if open_scripts.contains(path) {
                // TODO: when https://github.com/godotengine/godot/pull/113772 is merged and 4.7 is released, use the bound method instead
                script_editor.call("close_file", &[path.to_variant()]);
            }
        }
    }

    pub fn refresh_after_source_change() -> bool {
        EditorFilesystemAccessor::scan_changes();
        let mut script_editor = EditorInterface::singleton().get_script_editor().unwrap();
        // TODO: when 4.7 is released, use the bound method instead
        script_editor.call("reload_scripts", &[]);

        let current_scene = EditorInterface::singleton()
            .get_edited_scene_root()
            .map(|scene| scene.get_scene_file_path());
        let open_scenes = EditorInterface::singleton().get_open_scenes();
        for scene in open_scenes.as_slice().iter() {
            if current_scene.is_some() && current_scene.as_ref().unwrap() == scene {
                continue;
            }
            EditorInterface::singleton().reload_scene_from_path(scene);
        }
        if current_scene.is_some() {
            EditorInterface::singleton().reload_scene_from_path(current_scene.as_ref().unwrap());
        }
        true
    }

    pub fn save_all_scripts() {
        // TODO: when 4.7 is released, use the bound method instead
        EditorInterface::singleton()
            .get_script_editor()
            .unwrap()
            .call("save_all_scripts", &[]);
    }

    pub fn save_all() {
        // TODO: no bound method to get shader_editor; I don't think we need it?
        // ShaderEditorPlugin *shader_editor = Object::cast_to<ShaderEditorPlugin>(EditorNode::get_editor_data().get_editor_by_name("Shader"));
        // if (shader_editor) {
        //     shader_editor->save_external_data();
        // }
        PatchworkEditorAccessor::save_all_scripts();
        EditorInterface::singleton().save_all_scenes();
    }
}

/// Allows Rust code to access the Godot EditorFilesystem API
pub struct EditorFilesystemAccessor {}

#[allow(dead_code)] // entire API might not be used yet
impl EditorFilesystemAccessor {
    pub fn is_scanning() -> bool {
        EditorInterface::singleton()
            .get_resource_filesystem()
            .map(|fs| return fs.is_scanning())
            .unwrap_or(false)
    }

    pub fn reimport_files(files: &Vec<String>) {
        let files_packed = files
            .iter()
            .map(|f| GString::from(f))
            .collect::<PackedStringArray>();
        EditorInterface::singleton()
            .get_resource_filesystem()
            .unwrap()
            .reimport_files(&files_packed);
    }

    pub fn reload_scene_from_path(path: &str) {
        EditorInterface::singleton().reload_scene_from_path(&GString::from(path));
    }

    pub fn scan() {
        EditorInterface::singleton()
            .get_resource_filesystem()
            .unwrap()
            .scan();
    }

    pub fn scan_changes() {
        EditorInterface::singleton()
            .get_resource_filesystem()
            .unwrap()
            .scan_sources();
    }

    pub fn get_inspector_edited_object() -> Option<Gd<Object>> {
        EditorInterface::singleton()
            .get_inspector()
            .unwrap()
            .get_edited_object()
    }

    pub fn clear_inspector_item() {
        let object = Gd::<Object>::null_arg();
        EditorInterface::singleton()
            .inspect_object_ex(object)
            .for_property("")
            .inspector_only(true)
            .done();
    }
}
