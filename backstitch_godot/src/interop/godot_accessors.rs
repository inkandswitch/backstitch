use std::collections::HashSet;
use std::time::{Duration, Instant};

use godot::obj::Singleton;
use godot::{
    builtin::{GString, PackedStringArray},
    classes::{EditorInterface, Object},
    obj::Gd,
};

/// Various EditorInterface utility functions.
pub struct BackstitchEditorAccessor {}

impl BackstitchEditorAccessor {
    pub fn is_editor_importing() -> bool {
        EditorInterface::singleton()
            .get_resource_filesystem()
            .map(|fs| fs.is_importing())
            .unwrap_or(false)
    }

    pub fn get_unsaved_scripts() -> PackedStringArray {
        let Some(script_editor) = EditorInterface::singleton().get_script_editor() else {
            tracing::error!("No script editor found?!");
            return PackedStringArray::new();
        };
        script_editor.get_unsaved_files()
    }

    pub fn unsaved_files_open() -> bool {
        if !Self::get_unsaved_scripts().is_empty() {
            return true;
        }
        let unsaved_scenes = EditorInterface::singleton().get_unsaved_scenes();
        if !unsaved_scenes.is_empty() {
            return true;
        }
        false
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
                    EditorInterface::singleton().open_scene_from_path(path);
                    EditorInterface::singleton().close_scene();
                }
            } else if open_scripts.contains(path) {
                script_editor.close_file(path);
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

    pub fn reload_script_editor() {
        let mut script_editor = EditorInterface::singleton().get_script_editor().unwrap();
        script_editor.reload_open_files();
    }

    pub fn fs_scan_full_sync() -> bool {
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
        !timed_out
    }

    pub fn save_all_scripts() {
        EditorInterface::singleton()
            .get_script_editor()
            .unwrap()
            .save_all_scripts();
    }

    pub fn save_all() {
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
