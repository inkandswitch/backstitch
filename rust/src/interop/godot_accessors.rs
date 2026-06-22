use std::collections::HashSet;
use std::time::{Duration, Instant};

use godot::obj::Singleton;
use godot::{
    builtin::{GString, PackedStringArray},
    classes::{EditorInterface, Object},
    meta::ToGodot,
    obj::Gd,
};

use crate::interop::backstitch_config::BackstitchConfig;

/// Allows Rust code to easily get and set Backstitch configuration values via Godot's config system.
pub struct BackstitchConfigAccessor {}

impl BackstitchConfigAccessor {
    pub fn get_project_doc_id() -> String {
        BackstitchConfigAccessor::get_project_value("project_doc_id", "")
    }

    pub fn get_project_value(name: &str, default: &str) -> String {
        BackstitchConfig::singleton()
            .bind()
            .get_project_value(GString::from(name), default.to_variant())
            .to::<String>()
    }

    pub fn set_project_value(name: &str, value: &str) {
        BackstitchConfig::singleton()
            .bind_mut()
            .set_project_value(GString::from(name), value.to_variant());
    }

    pub fn get_user_value(name: &str, default: &str) -> String {
        BackstitchConfig::singleton()
            .bind()
            .get_user_value(GString::from(name), default.to_variant())
            .to::<String>()
    }

    #[allow(dead_code)] // will be used later
    pub fn set_user_value(name: &str, value: &str) {
        BackstitchConfig::singleton()
            .bind_mut()
            .set_user_value(GString::from(name), value.to_variant());
    }
}

/// Allows Rust code to access the C++ BackstitchEditor editor module from Godot.
pub struct BackstitchEditorAccessor {}

#[allow(dead_code)] // entire API might not be used yet
impl BackstitchEditorAccessor {
    pub fn is_editor_importing() -> bool {
        EditorInterface::singleton()
            .get_resource_filesystem()
            .map(|mut fs| fs.call("is_importing", &[]).to::<bool>())
            .unwrap_or(false)
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
        if !Self::get_unsaved_scripts().is_empty() {
            return true;
        }
        // TODO: when 4.7 is released, use the bound method instead
        let unsaved_scenes = EditorInterface::singleton()
            .call("get_unsaved_scenes", &[])
            .to::<PackedStringArray>();
        if !unsaved_scenes.is_empty() {
            return true;
        }
        false
    }

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

    fn get_current_scene_path() -> Option<GString> {
        EditorInterface::singleton()
            .get_edited_scene_root()
            .map(|scene| scene.get_scene_file_path())
    }

    pub fn close_files_if_open(paths: &Vec<String>) {
        let open_scenes = EditorInterface::singleton().get_open_scenes();
        let mut script_editor = EditorInterface::singleton().get_script_editor().unwrap();

        let open_scripts = script_editor
            .get_open_scripts()
            .iter_shared()
            .map(|script| script.get_path().to_string())
            .collect::<HashSet<String>>();
        let current_scene = Self::get_current_scene_path().map(|path| path.to_string());
        for path in paths {
            if open_scenes.contains(path) {
                if current_scene.is_some() && current_scene.as_ref().unwrap() == path {
                    EditorInterface::singleton().close_scene();
                } else {
                    // TODO: https://github.com/godotengine/godot/pull/116905 has a bug in it that causes a crash if the scene is not the current scene and then we close it; so we need to wait until that's fixed
                    // EditorInterface::singleton().open_scene_from_path(path);
                    // EditorInterface::singleton().close_scene();
                }
            } else if open_scripts.contains(path) {
                // TODO: when https://github.com/godotengine/godot/pull/113772 is merged and 4.7 is released, use the bound method instead
                script_editor.call("close_file", &[path.to_variant()]);
            }
        }
    }

    pub fn reload_scene_files() {
        let current_scene = Self::get_current_scene_path();
        let open_scenes = EditorInterface::singleton().get_open_scenes();
        for scene in open_scenes.as_slice().iter() {
            if current_scene.is_some() && current_scene.as_ref().unwrap() == scene {
                continue;
            }
            EditorInterface::singleton().reload_scene_from_path(scene);
        }
        if let Some(current_scene) = &current_scene {
            EditorInterface::singleton().reload_scene_from_path(current_scene);
        }
    }

    pub fn scan_fs_sync() -> bool {
        let mut fs = EditorInterface::singleton()
            .get_resource_filesystem()
            .unwrap();
        let time_start = Instant::now();
        let ten_secs = Duration::from_secs(30);
        fs.scan();
        let mut timed_out = false;
        while fs.is_scanning() {
            if Instant::now() - time_start >= ten_secs {
                timed_out = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            fs.notify(godot::classes::notify::NodeNotification::PROCESS)
        }
        return timed_out;
    }

    pub fn refresh_after_source_change() -> bool {
        if Self::scan_fs_sync() {
            tracing::warn!("Scanning filesystem timed out!");
            return false;
        }
        let mut script_editor = EditorInterface::singleton().get_script_editor().unwrap();
        // TODO: when 4.7 is released, use the bound method instead
        script_editor.call("reload_open_files", &[]);

        Self::reload_scene_files();
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
        BackstitchEditorAccessor::save_all_scripts();
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
            .map(|fs| fs.is_scanning())
            .unwrap_or(false)
    }

    pub fn reimport_files(files: &[String]) {
        let files_packed = files
            .iter()
            .map(GString::from)
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
