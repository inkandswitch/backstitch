//! Drives the editor in response to the user clicking on parts of the diff inspector.
//!
//! This is the only part of the diff inspector that touches `EditorInterface`.

use godot::classes::object::ConnectFlags;
use godot::classes::{EditorInterface, Engine, Node, ResourceLoader, SceneTree};
use godot::obj::Singleton;
use godot::prelude::*;

use super::script_editor_hack;

/// Something in the editor the user wants to go to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavTarget {
    /// A node in a scene. `node_path` is relative to the scene root, optionally with a leading `./`.
    Node { file: String, node_path: String },
    /// A scene file. Opens the scene, or makes sure the scene root is inspected if it is already open.
    Scene { file: String },
    /// Any non-scene resource file.
    Resource { path: String },
    /// A text file; opened in the script editor.
    Text { path: String },
}

pub struct EditorNavigator;

impl EditorNavigator {
    /// Takes the editor to `target`.
    ///
    /// If this results in a scene being opened (or reselected), `scene_ready` is called a frame
    /// later, once the scene's nodes can be looked at. If the target is already shown,
    /// `scene_ready` is not called.
    pub fn go_to(target: NavTarget, scene_ready: impl FnOnce() + 'static) {
        match target {
            NavTarget::Node { file, node_path } => {
                Self::go_to_node(file, node_path, false, scene_ready)
            }
            NavTarget::Scene { file } | NavTarget::Resource { path: file } => {
                if Self::show_resource(&file) {
                    run_next_frame(scene_ready);
                }
            }
            NavTarget::Text { path } => Self::show_text_file(&path),
        }
    }

    fn go_to_node(
        file: String,
        node_path: String,
        changed_scene: bool,
        scene_ready: impl FnOnce() + 'static,
    ) {
        let node_path = node_path
            .strip_prefix("./")
            .unwrap_or(&node_path)
            .to_string();
        let mut editor = EditorInterface::singleton();

        let root = editor.get_edited_scene_root();
        let is_right_scene = root
            .as_ref()
            .is_some_and(|root| root.get_scene_file_path() == file.as_str());
        if !is_right_scene {
            if changed_scene {
                // We already tried to open the scene and it didn't work; don't loop forever.
                tracing::error!("Couldn't open scene {file} to select node {node_path}");
                return;
            }
            editor.open_scene_from_path(&file);
            run_next_frame(move || Self::go_to_node(file, node_path, true, scene_ready));
            return;
        }

        if let Some(node) = root.and_then(|root| root.get_node_or_null(&node_path)) {
            editor.edit_node(&node);
            editor.set_main_screen_editor("2D");
        }
        if changed_scene {
            // The scene changed, so listeners need to be told to update what they highlight.
            scene_ready();
        }
    }

    /// Shows a resource (or scene) file in the editor. Returns whether it was a scene.
    fn show_resource(path: &str) -> bool {
        let mut editor = EditorInterface::singleton();
        let extension = path.rsplit('.').next().unwrap_or("").to_lowercase();
        if let Some(mut dock) = editor.get_file_system_dock() {
            dock.navigate_to_path(path);
        }

        if extension == "tscn" || extension == "scn" {
            let current_root = editor.get_edited_scene_root();
            match current_root {
                Some(root) if root.get_scene_file_path() == path => {
                    let edited = editor
                        .get_inspector()
                        .and_then(|inspector| inspector.get_edited_object());
                    let is_root = edited
                        .as_ref()
                        .is_some_and(|object| object.instance_id() == root.instance_id());
                    // If something outside of this scene is being inspected, go back to the scene root.
                    let is_in_scene = edited
                        .and_then(|object| object.try_cast::<Node>().ok())
                        .is_some_and(|node| root.is_ancestor_of(&node));
                    if !is_root && !is_in_scene {
                        editor.inspect_object(&root);
                    }
                }
                _ => editor.open_scene_from_path(path),
            }
            editor.set_main_screen_editor("2D");
            true
        } else {
            if let Some(resource) = ResourceLoader::singleton().load(path) {
                editor.edit_resource(&resource);
            }
            false
        }
    }

    fn show_text_file(path: &str) {
        let mut editor = EditorInterface::singleton();
        if let Some(mut dock) = editor.get_file_system_dock() {
            dock.navigate_to_path(path);
        }
        Self::open_scene_or_resource(path);
        editor.set_main_screen_editor("Script");
    }

    fn open_scene_or_resource(path: &str) {
        let extension = path.rsplit('.').next().unwrap_or("").to_lowercase();
        // don't open import files
        if extension == "import" {
            return;
        }
        let mut editor = EditorInterface::singleton();
        if extension == "tscn" || extension == "scn" {
            editor.open_scene_from_path(path);
        } else if extension == "gd" || extension == "cs" || Self::is_text_file_extension(&extension)
        {
            if !script_editor_hack::open_text_file(path) {
                tracing::error!("Couldn't open {path} in the script editor");
            }
        } else if let Some(resource) = ResourceLoader::singleton().load(path) {
            editor.edit_resource(&resource);
        }
    }

    fn is_text_file_extension(extension: &str) -> bool {
        EditorInterface::singleton()
            .get_editor_settings()
            .map(|settings| {
                settings
                    .get_setting("docks/filesystem/textfile_extensions")
                    .to_string()
            })
            .is_some_and(|extensions| extensions.split(',').any(|ext| ext == extension))
    }
}

/// Runs `f` at the start of the next frame.
///
/// We do not want to use `call_deferred` here. Any deferred call that ends up loading resources can either:
/// * end up deadlocking the resource loader because the message queue can't flush, -or-
/// * the resource fails to load because of explicit checks for if the message queue can flush.
fn run_next_frame(f: impl FnOnce() + 'static) {
    let tree = Engine::singleton()
        .get_main_loop()
        .and_then(|main_loop| main_loop.try_cast::<SceneTree>().ok());
    let Some(mut tree) = tree else {
        f();
        return;
    };
    let mut f = Some(f);
    let callable = Callable::from_fn("diff_inspector_next_frame", move |_| {
        if let Some(f) = f.take() {
            f();
        }
    });
    tree.connect_flags("process_frame", &callable, ConnectFlags::ONE_SHOT);
}
