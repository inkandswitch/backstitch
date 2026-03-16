use crate::fs::file_utils::{FileContent, FileSystemEvent};
use crate::helpers::history_ref::HistoryRef;
use crate::interop::godot_accessors::{EditorFilesystemAccessor, PatchworkConfigAccessor, PatchworkEditorAccessor};
use crate::project::project::{GodotProjectSignal, Project};
use crate::project::project_api::{BranchViewModel, ProjectViewModel};
use automerge::ChangeHash;
use godot::classes::editor_plugin::DockSlot;
use ::safer_ffi::prelude::*;
use samod::{DocumentId};
use godot::classes::resource_loader::CacheMode;
use godot::classes::{ConfirmationDialog, Control};
use godot::classes::EditorInterface;
use godot::classes::ProjectSettings;
use godot::classes::ResourceLoader;
use godot::classes::{EditorPlugin, Engine, IEditorPlugin};
use godot::classes::{DirAccess};
use godot::prelude::*;
use tracing::instrument;
use std::collections::{HashSet};
use std::path::PathBuf;
use std::{collections::HashMap, str::FromStr};
use crate::interop::godot_helpers::{ToGodotExt, branch_view_model_to_dict, change_view_model_to_dict, diff_view_model_to_dict};

// This is the worst thing I've ever done
// Get the file system
// get the parent of the file system, that's the editor node
// look for the first Panel child of the editor node, that's the gui base
// look for ConfirmationDialog children of the gui base
// it's unique in that it has a vbox container with a tree child; just look for that
// if we find it, get the signals
// find the signals connected to the confirmed signal
// the first is the _reload_modified_scenes callable
// the second is the _reload_project_settings callable
// steal those, call _reload_modified_scenes
fn steal_editor_node_private_reload_methods_from_dialog_signal_handlers() -> Option<(Callable, Callable)> {
		// get the editor node
	let editor_file_system = EditorInterface::singleton().get_resource_filesystem();
	let editor_node = if let Some(editor_file_system) = editor_file_system {
		// get the parent of the editor file system, that's the editor node
		editor_file_system.get_parent()
	} else {
		return None;
	};
	if let Some(editor_node) = editor_node {
			// get the first Panel child of the editor node, that's the gui base
		let children = editor_node.get_children();
		// it should be the first panel
		if let Some(gui_base) = children.iter_shared().find(|c| c.get_class().to_string() == "Panel") {
			// find the disk_changed dialog child of the gui base
			let children = gui_base.get_children();
			if let Some(disk_changed_dialog_node) = children.iter_shared().find(|c|{
				if c.get_class().to_string() == "ConfirmationDialog" {
					// check that one of the children is a VBoxContainer
					let children = c.get_children();
					if let Some(vbox_container) = children.iter_shared().find(|c| c.get_class().to_string() == "VBoxContainer") {
						// check that one of the children is a Tree
						let children = vbox_container.get_children();
						if let Some(_) = children.iter_shared().find(|c| c.get_class().to_string() == "Tree") {
							return true;
						}
					}
				}
				false
			}) {
				let disk_changed_dialog = match disk_changed_dialog_node.try_cast::<ConfirmationDialog>() {
					Ok(dialog) => dialog,
					Err(_) => return None,
				};
				let signals = disk_changed_dialog.get_signal_connection_list("confirmed");
				if signals.len() >= 2 {
					// the first two should be the _reload_modified_scenes and _reload_project_settings signals
					let reload_modified_scenes_callable = signals.get(0).unwrap().get("callable").unwrap().to::<Callable>();
					let reload_project_settings_callable = signals.get(1).unwrap().get("callable").unwrap().to::<Callable>();
					return Some((reload_modified_scenes_callable, reload_project_settings_callable));
				} else {
					return None;
				}
			} else {
				return None;
			}
		} else {
			return None;
		}
	}
	None
}

/// Tracks updates that Godot may have made, to ensure we can track them and update the state accordingly
#[derive(Debug, Default)]
struct PendingEditorUpdate {
	added_files: HashSet<String>,
	deleted_files: HashSet<String>,
	scripts_to_reload: HashSet<String>,
	scenes_to_reload: HashMap<String, FileContent>,
	reimport_files: HashSet<String>,
	uids_to_add: HashMap<String, String>,
	reload_project_settings: bool
}

impl PendingEditorUpdate {
	/// Merges another PendingEditorUpdate into this one, combining their changes
	fn merge(&mut self, other: PendingEditorUpdate) {
		self.added_files.extend(other.added_files);
		self.deleted_files.extend(other.deleted_files);
		self.scripts_to_reload.extend(other.scripts_to_reload);
		for (path, content) in other.scenes_to_reload.into_iter() {
			self.scenes_to_reload.insert(path, content);
		}
		self.reimport_files.extend(other.reimport_files);
		for (path, uid) in other.uids_to_add.into_iter() {
			self.uids_to_add.insert(path, uid);
		}
		self.reload_project_settings = self.reload_project_settings || other.reload_project_settings;
	}

	/// Returns true if there are any added or deleted files
	fn added_or_deleted_files(&self) -> bool {
		self.added_files.len() > 0 || self.deleted_files.len() > 0
	}

	/// Returns true if there are any file changes to process
	fn any_changes(&self) -> bool {
		self.scripts_to_reload.len() > 0 || self.scenes_to_reload.len() > 0 || self.reimport_files.len() > 0 || self.uids_to_add.len() > 0 || self.added_or_deleted_files()
	}

	/// Clears all pending updates
	fn clear(&mut self) {
		self.added_files.clear();
		self.deleted_files.clear();
		self.scripts_to_reload.clear();
		self.scenes_to_reload.clear();
		self.reimport_files.clear();
		self.uids_to_add.clear();
		self.reload_project_settings = false;
	}
}

/// GodotProject is the main interface between Godot's API and the Patchwork Rust core.
/// It is intended to be a gdscript-visible lightweight wrapper around the GodotProjectImpl, which contains the actual logic.
/// It also handles signals and communication with Godot.
#[derive(GodotClass, Debug)]
#[class(base=Node)]
pub struct GodotProject {
	base: Base<Node>,
	project: Project,
	pending_editor_update: PendingEditorUpdate,
	reload_project_settings_callable: Option<Callable>,
	deferred_start: i32
}

// new API
/// This implementation binds as closely as possible to [GodotProjectViewModel].
#[godot_api]
impl GodotProject {
	#[signal]
	fn state_changed();

	#[func]
	fn has_user_name(&self) -> bool {
		self.project.has_user_name()
	}

	#[func]
	fn get_user_name(&self) -> String {
		self.project.get_user_name()
	}

	#[func]
	fn set_user_name(&self, name: String) {
		self.project.set_user_name(name);
	}

	#[func]
	fn clear_project(&mut self) {
		self.project.clear_project();
	}

	#[func]
	fn has_project(&self) -> bool {
		self.project.has_project()
	}

	#[func]
	fn get_project_id(&self) -> String {
		if let Some(id) = self.project.get_project_id() {
			return id.to_string();
		}
		"".to_string()
	}

	#[func]
	fn new_project(&mut self) {
		self.project.new_project()
	}

	#[func]
	fn load_project(&mut self, id: String) {
		if let Ok(id) = DocumentId::from_str(&id) {
			self.project.load_project(&id);
		}
	}

	#[func]
    fn get_sync_status(&self) -> VarDictionary {
		self.project.get_sync_status().to_godot()
	}

	#[func]
    fn print_sync_debug(&self) {
		self.project.print_sync_debug();
	}

	fn branch_to_variant(&self, branch: Option<impl BranchViewModel>) -> Variant{
		let Some(branch) = branch else {
			return Variant::nil();
		};
		Variant::from(branch_view_model_to_dict(&branch))
	}

	#[func]
	fn get_branch(&self, id: String) -> Variant {
		let Ok(id) = DocumentId::from_str(&id) else {
			return Variant::nil();
		};
		self.branch_to_variant(self.project.get_branch(&id))
	}

	#[func]
	fn get_main_branch(&self) -> Variant {
		self.branch_to_variant(self.project.get_main_branch())
	}

	#[func]
	fn get_checked_out_branch(&self) -> Variant {
		self.branch_to_variant(self.project.get_checked_out_branch())
	}

	#[func]
	fn dump_current_branch(&self) {
		self.project.dump_current_branch();
	}

	#[func]
	fn is_branch_loaded(&self, id: String) -> bool {
		let Ok(id) = DocumentId::from_str(&id) else {
			return false;
		};
		self.project.is_branch_loaded(&id)
	}

	#[func]
	fn create_branch(&mut self, name: String) {
		self.project.create_branch(name);
	}

	#[func]
	fn checkout_branch(&mut self, id: String) {
		if let Ok(id) = DocumentId::from_str(&id) {
		self.project.checkout_branch(&id);
		};
	}

	#[func]
	fn can_create_merge_preview_branch(&self) -> bool {
		self.project.can_create_merge_preview_branch()
	}

	#[func]
	fn create_merge_preview_branch(&mut self) {
		self.project.create_merge_preview_branch();
	}

	#[func]
	fn can_create_revert_preview_branch(&self, head: String) -> bool {
		if let Ok(hash) = ChangeHash::from_str(&head) {
			return self.project.can_create_revert_preview_branch(hash);
		}
		false
	}

	#[func]
	fn create_revert_preview_branch(&mut self, head: String) {
		if let Ok(hash) = ChangeHash::from_str(&head) {
			self.project.create_revert_preview_branch(hash);
		}
	}

	#[func]
	fn is_revert_preview_branch_active(&self) -> bool {
		self.project.is_revert_preview_branch_active()
	}

	#[func]
	fn is_merge_preview_branch_active(&self) -> bool {
		self.project.is_merge_preview_branch_active()
	}

	#[func]
	fn is_safe_to_merge(&self) -> bool {
		self.project.is_safe_to_merge()
	}

	#[func]
	fn confirm_preview_branch(&mut self) {
		self.project.confirm_preview_branch();
	}

	#[func]
	fn discard_preview_branch(&mut self) {
		self.project.discard_preview_branch();
	}

	#[func]
	fn get_branch_history(&self) -> PackedStringArray {
		self.project.get_branch_history().to_godot()
	}

	#[func]
	fn get_change(&self, hash: String) -> Variant {
		let Ok(hash) = ChangeHash::from_str(&hash) else {
			return Variant::nil();
		};
		let Some(change) = self.project.get_change(hash) else {
			return Variant::nil();
		};
		Variant::from(change_view_model_to_dict(change))
	}

	#[func]
	fn get_diff(&self, selected_hash: String) -> Variant {
		let Ok(hash) = ChangeHash::from_str(&selected_hash) else {
			return Variant::nil();
		};
		let Some(diff) = ProjectViewModel::get_diff(&self.project, hash) else {
			return Variant::nil();
		};
		Variant::from(diff_view_model_to_dict(&diff))
	}

	#[func]
	fn get_default_diff(&self) -> Variant {
		let Some(diff) = self.project.get_default_diff() else {
			return Variant::nil();
		};
		Variant::from(diff_view_model_to_dict(&diff))
	}

	#[func]
	fn get_current_ref_string(&self) -> String {
		let Some(ref_) = self.project.get_current_ref() else {
			return "".to_string();
		};
		ref_.to_string()
	}

    #[func]
    pub fn get_singleton() -> Gd<Self> {
        Engine::singleton()
            .get_singleton(&StringName::from("GodotProject"))
            .unwrap()
            .cast::<Self>()
    }

	fn process_godot_updates(&self, events: Vec<FileSystemEvent>) -> PendingEditorUpdate {
		let mut pending_editor_update = PendingEditorUpdate::default();
		let mut files_changed = Vec::new();
        for event in events {
			let mut file_created = false;
            let (abs_path, content) = match event {
                FileSystemEvent::FileCreated(path, content) => {
					pending_editor_update.added_files.insert(ProjectSettings::singleton().localize_path(&path.to_string_lossy().to_string()).to_string());
					file_created = true;
					(path, content)
				},
                FileSystemEvent::FileModified(path, content) => (path, content),
                FileSystemEvent::FileDeleted(path) => {
					pending_editor_update.deleted_files.insert(ProjectSettings::singleton().localize_path(&path.to_string_lossy().to_string()).to_string());
					continue;
				},
            };
			files_changed.push(abs_path.to_string_lossy().to_string());
            let res_path = ProjectSettings::singleton().localize_path(&abs_path.to_string_lossy().to_string()).to_string();
            let extension = abs_path.extension().unwrap_or_default().to_string_lossy().to_string().to_ascii_lowercase();
            if extension == "gd" {
				pending_editor_update.scripts_to_reload.insert(res_path);
            } else if extension == "tscn" {
                pending_editor_update.scenes_to_reload.insert(res_path, content);
            } else if extension == "import" {
				let mut pb = PathBuf::from(res_path);
				pb.set_extension("");
				let base = pb.to_string_lossy().to_string();
				if !file_created {
					pending_editor_update.reimport_files.insert(base.clone());
				}
                if let FileContent::String(string) = content {
                    // go line by line, find the line that begins with "uid="
                    for line in string.lines() {
                        if line.starts_with("uid=") {
                            let uid = line.split("=").nth(1).unwrap_or_default().to_string();
                            pending_editor_update.uids_to_add.insert(base, uid);
                            break;
                        }
                    }
                }
            } else if extension == "uid" {
                if let FileContent::String(string) = content {
                    pending_editor_update.uids_to_add.insert(res_path.to_string(), string);
                }
			} else if extension == "godot" {
				pending_editor_update.reload_project_settings = true;
            // check if a file with .import added exists
            } else  {
                let mut import_path = abs_path.clone();
				import_path.set_extension(abs_path.extension().unwrap_or_default().to_string_lossy().to_string() + ".import");
                if import_path.exists() {
					if !file_created {
						pending_editor_update.reimport_files.insert(res_path.to_string());
					}
                }
            }
        }
		tracing::info!("---------- files_changed: {:?}", files_changed);
		return pending_editor_update;
	}

	fn reload_project_settings(&self) {
		if let Some(reload_project_settings_callable) = &self.reload_project_settings_callable {
			reload_project_settings_callable.call(&[]);
		}
	}

	// bit of a hack to clear the diff cache when UI is loaded, to facilitate debugging
	fn clear_diff_cache(&self) {
		self.project.clear_diff_cache();
	}

	pub fn get_current_ref(&self) -> Option<HistoryRef> {
		self.project.get_current_ref()
	}

	pub fn get_file_at_ref(&self, path: &String, ref_: &HistoryRef) -> Option<FileContent> {
		self.project.get_file_at_ref(path, ref_)
	}

	pub fn get_files_at_ref(&self, ref_: &HistoryRef, filters: &HashSet<String>) -> Option<HashMap<String, FileContent>> {
		self.project.get_files_at_ref(ref_, filters)
	}

}


#[godot_api]
impl INode for GodotProject {
    fn init(_base: Base<Node>) -> Self {
        GodotProject {
			base: _base,
			project: Project::new(ProjectSettings::singleton().globalize_path("res://").to_string().into()),
			pending_editor_update: PendingEditorUpdate::default(),
			reload_project_settings_callable: None,
			deferred_start: -1
		}
    }

    fn enter_tree(&mut self) {
		let callables = steal_editor_node_private_reload_methods_from_dialog_signal_handlers();
		if let Some((_, reload_project_settings_callable)) = callables {
			self.reload_project_settings_callable = Some(reload_project_settings_callable);
		} else {
			// if we rebase and this fails, we're going to have to do something else
			panic!("Failed to steal reload methods from dialog signal handlers");
		}
		let project_id = PatchworkConfigAccessor::get_project_doc_id();
		if project_id == "" {
			tracing::info!("Patchwork config has no project id, not autostarting...");
			return;
		}
		// for the autostart, we force save everything.
		PatchworkEditorAccessor::save_all();
		// wait some frames before starting
		self.deferred_start = 3;
    }

    fn exit_tree(&mut self) {
		if self.project.has_project() {
			self.project.stop();
		}
        // Perform typical plugin operations here.
    }

	#[instrument(target = "patchwork_rust_core::godot_project::outer_process", level = tracing::Level::DEBUG, skip_all)]
    fn process(&mut self, _delta: f64) {
		if self.deferred_start > 0 {
			self.deferred_start -= 1;
			if self.deferred_start == 0 {
				self.project.start();
			}
			return;
		}
		if !self.project.has_project() {
			return;
		}
		let (updates, signals) = self.project.process(_delta);
		if updates.len() > 0 {
			self.pending_editor_update.merge(self.process_godot_updates(updates));
		}
		for signal in signals {
			match signal {
				GodotProjectSignal::CheckedOutBranch => {
					// TODO: remove this signal
				}
				GodotProjectSignal::ChangesIngested => {
					self.base_mut().call_deferred("emit_signal", &["state_changed".to_variant()]);
				}
			}
		}
    }
}

/// An EditorPlugin to manage the GodotProject singleton and its UI.
#[derive(GodotClass)]
#[class(init, base=EditorPlugin, tool)]
pub struct GodotProjectPlugin {
    base: Base<EditorPlugin>,
	sidebar_scene: Option<Gd<PackedScene>>,
	sidebar: Option<Gd<Control>>,
	initialized: bool,
	ui_needs_update: bool,
}

#[godot_api]
impl GodotProjectPlugin {
	#[func]
	fn on_reload_ui(&mut self) {
		self.ui_needs_update = true;
		let proj = GodotProject::get_singleton();
		let b = proj.bind();
		b.clear_diff_cache();
	}

	fn add_sidebar(&mut self) {
		self.sidebar_scene = Self
			::force_reload_resource("res://addons/patchwork/public/gdscript/sidebar.tscn")
			.map(|scene| scene.try_cast::<PackedScene>().ok())
			.flatten();
		self.sidebar = if let Some(Some(sidebar)) = self.sidebar_scene.as_ref().map(|scene| scene.instantiate()) {
			if let Ok(mut sidebar) = sidebar.try_cast::<Control>() {
				let _ = sidebar.connect("reload_ui", &Callable::from_object_method(&self.to_gd(), "on_reload_ui"));
				Some(sidebar)
			} else {
				None
			}
		} else {
			None
		};
		if let Some(sidebar) = self.sidebar.as_ref() {
			self.to_gd().add_control_to_dock(DockSlot::RIGHT_UL, sidebar);
		} else {
			panic!("Failed to instantiate sidebar");
		};
	}

	fn remove_sidebar(&mut self) {
		if let Some(sidebar) = self.sidebar.as_ref() {
			self.to_gd().remove_control_from_docks(sidebar);
			let mut sidebar = self.sidebar.take().unwrap();
			sidebar.queue_free();
		} else {
			tracing::warn!("no sidebar to remove");
		}
		self.sidebar_scene = None;
	}

	fn force_reload_resource(path: &str) -> Option<Gd<Resource>> {
		let scene = ResourceLoader::singleton()
			.load_ex(path)
			.cache_mode(CacheMode::REPLACE_DEEP)
			.done();
		scene
	}
	
	fn update_godot_after_source_change(&mut self) -> bool {
		let mut proj = GodotProject::get_singleton();
		let mut p = proj.bind_mut();
		if !p.pending_editor_update.any_changes() {
			return false;
		}
		if !Project::safe_to_update_godot() {
			return false;
		}
		self.base_mut().set_process(false);
		p.base_mut().set_process(false);
		PatchworkEditorAccessor::close_files_if_open(&p.pending_editor_update.deleted_files.iter().map(|path| path.clone()).collect::<Vec<String>>());
		p.pending_editor_update.deleted_files.clear();
		if p.pending_editor_update.reload_project_settings {
			p.reload_project_settings();
			p.pending_editor_update.reload_project_settings = false;
		}

		// make sure to explicitly have p dropped so that sidebar can update, then rebind
		drop(p);

		if PatchworkEditorAccessor::refresh_after_source_change() {
			let mut p = proj.bind_mut();
			p.pending_editor_update.clear();
		}
		let mut p = proj.bind_mut();
		p.base_mut().set_process(true);
		self.base_mut().set_process(true);
		return true;
	}

	#[func]
	fn on_scene_saved(&mut self, path: String) {
		if path == "res://addons/patchwork/public/gdscript/sidebar.tscn" {
			tracing::info!("Scene saved {path}; reloading sidebar");
			self.on_reload_ui();
		}
	}
}

#[godot_api]
impl IEditorPlugin for GodotProjectPlugin {
    fn enter_tree(&mut self) {
        tracing::debug!("** GodotProjectPlugin: enter_tree");
    }

	fn ready(&mut self) {
		{
			// When we save a scene, if it's sidebar.tscn, we want to reload the UI
			// This is for devs
			let callable = self.base().callable("on_scene_saved");
			let mut base = self.base_mut();
			base.connect("scene_saved", &callable);
		}
		self.process(0.0);
	}

	fn process(&mut self, _delta: f64) {
		// Don't initialize until the project is fully loaded and the editor is not importing
		if !self.initialized
			&& !EditorFilesystemAccessor::is_scanning()
			&& !PatchworkEditorAccessor::is_editor_importing()
			&& DirAccess::dir_exists_absolute("res://.godot") // This is at the end because DirAccess::dir_exists_absolute locks a global mutex
			{
			let godot_project_singleton: Gd<GodotProject> = GodotProject::get_singleton();
			self.base_mut().add_child(&godot_project_singleton);
			self.add_sidebar();
			self.initialized = true;
		}
		if self.ui_needs_update {
			self.ui_needs_update = false;
			self.remove_sidebar();
			self.add_sidebar();
		}

		self.update_godot_after_source_change();
	}
    fn exit_tree(&mut self) {
        tracing::debug!("** GodotProjectPlugin: exit_tree");
		if self.initialized {
			self.remove_sidebar();
			self.base_mut().remove_child(&GodotProject::get_singleton());
		} else {
			tracing::error!("*************** DID NOT INITIALIZE!!!!!!");
		}
    }
}
