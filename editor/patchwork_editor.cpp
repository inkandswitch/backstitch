#include "patchwork_editor.h"

#include "editor/debugger/editor_debugger_node.h"
#include "editor/shader/shader_editor_plugin.h"
#include "scene/gui/box_container.h"
#include <core/io/json.h>
#include <core/io/missing_resource.h>
#include <core/object/class_db.h>
#include <core/os/os.h>
#include <core/variant/callable.h>
#include <core/variant/callable_bind.h>
#include <core/variant/variant.h>
#include <core/version_generated.gen.h>
#include <editor/editor_interface.h>
#include <editor/editor_undo_redo_manager.h>
#include <editor/file_system/editor_file_system.h>
#include <editor/inspector/editor_inspector.h>
#include <editor/script/script_editor_plugin.h>
#include <main/main.h>
#include <modules/gdscript/gdscript.h>
#include <scene/resources/packed_scene.h>

PatchworkEditor::PatchworkEditor() {
}

PatchworkEditor::~PatchworkEditor() {
}

PatchworkEditor *PatchworkEditor::get_singleton() {
	return singleton;
}


bool PatchworkEditor::unsaved_files_open() {
	if (get_unsaved_scripts().size() > 0) {
		return true;
	}
	auto opened_scenes = EditorNode::get_editor_data().get_edited_scenes();
	for (int i = 0; i < opened_scenes.size(); i++) {
		auto id = opened_scenes[i].history_id;
		if (EditorUndoRedoManager::get_singleton()->is_history_unsaved(id)) {
			return true;
		}
	}
	// Not bound
	if (EditorUndoRedoManager::get_singleton()->is_history_unsaved(EditorUndoRedoManager::GLOBAL_HISTORY)) {
		return true;
	}

	return false;
}

Vector<String> PatchworkEditor::get_unsaved_files() {
	auto files = get_unsaved_scripts();
	auto opened_scenes = EditorNode::get_editor_data().get_edited_scenes();
	for (auto &scene : opened_scenes) {
		if (EditorUndoRedoManager::get_singleton()->is_history_unsaved(scene.history_id)) {
			files.append(scene.path);
		}
	}
	return files;
}


void PatchworkEditor::progress_add_task(const String &p_task, const String &p_label, int p_steps, bool p_can_cancel) {
	EditorNode::get_singleton()->progress_add_task(p_task, p_label, p_steps, p_can_cancel);
}

bool PatchworkEditor::progress_task_step(const String &p_task, const String &p_state, int p_step, bool p_force_refresh) {
	return EditorNode::get_singleton()->progress_task_step(p_task, p_state, p_step, p_force_refresh);
}

void PatchworkEditor::progress_end_task(const String &p_task) {
	EditorNode::get_singleton()->progress_end_task(p_task);
}
void PatchworkEditor::progress_add_task_bg(const String &p_task, const String &p_label, int p_steps) {
	EditorNode::get_singleton()->progress_add_task_bg(p_task, p_label, p_steps);
}
void PatchworkEditor::progress_task_step_bg(const String &p_task, int p_step) {
	EditorNode::get_singleton()->progress_task_step_bg(p_task, p_step);
}
void PatchworkEditor::progress_end_task_bg(const String &p_task) {
	EditorNode::get_singleton()->progress_end_task_bg(p_task);
}
String PatchworkEditor::get_resource_script_class(const String &p_path) {
	return ResourceLoader::get_resource_script_class(p_path);
}

Ref<ResourceImporter> PatchworkEditor::get_importer_by_name(const String &p_name) {
	return ResourceFormatImporter::get_singleton()->get_importer_by_name(p_name);
}

inline Vector<String> _get_section_keys(const Ref<ConfigFile> &p_config_file,
                                        const String &p_section) {
  return p_config_file->get_section_keys(p_section);
}

Error PatchworkEditor::import_and_save_resource(const String &p_path, const String &import_file_content, const String &import_base_path) {
	String base_dir = import_base_path.get_base_dir();
	HashMap<StringName, Variant> params;

	Ref<ConfigFile> import_file;
	import_file.instantiate();

	Error err = import_file->parse(import_file_content);
	ERR_FAIL_COND_V_MSG(err != OK, err, "Failed to parse import file content");
	String importer_name = import_file->get_value("remap", "importer");
	Vector<String> param_keys = _get_section_keys(import_file, "params");
	for (auto &param_key : param_keys) {
		auto param_value = import_file->get_value("params", param_key);
		params[param_key] = param_value;
	}

	// make dir recursive
	DirAccess::make_dir_recursive_absolute(base_dir);
	auto importer = get_importer_by_name(importer_name);
	List<String> import_variants;
	List<String> import_options;
	Variant metadata;
	// set the default values for the import options in case they are not present in the import file
	List<ResourceImporter::ImportOption> opts;
	importer->get_import_options(p_path, &opts);
	for (const ResourceImporter::ImportOption &E : opts) {
		if (!params.has(E.option.name)) { //this one is not present
			params[E.option.name] = E.default_value;
		}
	}

	return importer->import(ResourceUID::INVALID_ID, p_path, import_base_path, params, &import_variants, &import_options, &metadata);
}

void PatchworkEditor::save_all_scenes_and_scripts() {
	ShaderEditorPlugin *shader_editor = Object::cast_to<ShaderEditorPlugin>(EditorNode::get_editor_data().get_editor_by_name("Shader"));
	if (shader_editor) {
		shader_editor->save_external_data();
	}
	save_all_scripts();
	// save the scenes
	EditorInterface::get_singleton()->save_all_scenes();
	EditorNode::get_singleton()->trigger_menu_option(EditorNode::SCENE_SAVE_SCENE, true);
}

void PatchworkEditor::save_all_scripts() {
	EditorInterface::get_singleton()->get_script_editor()->save_all_scripts();
}

PackedStringArray PatchworkEditor::get_unsaved_scripts() {
	return EditorInterface::get_singleton()->get_script_editor()->get_unsaved_scripts();
}

void PatchworkEditor::reload_scripts(PackedStringArray p_scripts) {
	// Call deferred to make sure it runs on the main thread.
	print_line("Reloading scripts: " + String(", ").join(p_scripts));
	Array scripts;
	for (auto &script : p_scripts) {
		auto sc = ResourceLoader::load(script, "", ResourceFormatLoader::CACHE_MODE_REPLACE_DEEP);
		if (sc.is_valid()) {
			scripts.append(sc);
		}
	}
	// soft_reload = false means it will reload all the script instances too
	GDScriptLanguage::get_singleton()->reload_scripts(scripts, true);
	// now get all the open scripts in the editor
	auto script_editor = EditorInterface::get_singleton()->get_script_editor();
	for (auto &script : script_editor->get_open_scripts()) {
		// If it has one of these scripts open, we have to reload it
		if (p_scripts.has(script->get_path())) {
			// call it with refresh_only = false (Forces it to reload the text)
			script_editor->reload_scripts(false);
			break;
		}
	}
	EditorDebuggerNode::get_singleton()->reload_scripts(p_scripts);
	// EditorInterface::get_singleton()->get_script_editor()->reload_scripts(p_scripts);
}

void PatchworkEditor::open_script_file(const String &p_script) {
	EditorInterface::get_singleton()->get_script_editor()->open_file(p_script);
}

void PatchworkEditor::force_refresh_editor_inspector() {
	EditorInterface::get_singleton()->get_inspector()->update_tree();
}

// not bound
bool PatchworkEditor::is_editor_importing() {
	return EditorFileSystem::get_singleton()->is_importing();
}

bool PatchworkEditor::is_changing_scene() {
	return EditorNode::get_singleton()->is_changing_scene();
}

void PatchworkEditor::clear_editor_selection() {
	EditorNode::get_singleton()->get_editor_selection()->clear();
}

bool PatchworkEditor::refresh_after_source_change() {
	EditorFileSystem::get_singleton()->scan_changes();
	// TODO: make this take in scripts to reload
	ScriptEditor::get_singleton()->reload_scripts();

	Main::iteration();

	constexpr int64_t MAX_SCAN_TIME = 30000;
	auto start_time = OS::get_singleton()->get_ticks_msec();
	while (EditorFileSystem::get_singleton()->is_scanning()) {
		// We're likely trapped in a `process` loop due to a bug with the process hack in the editor filesystem
		if (OS::get_singleton()->get_ticks_msec() - start_time > MAX_SCAN_TIME) {
			print_line("Scan timed out after " + itos(MAX_SCAN_TIME / 1000) + "s");
			return false;
		}
		OS::get_singleton()->delay_usec(10000);
		Main::iteration();
	}

	auto current_scene = EditorInterface::get_singleton()->get_edited_scene_root();

	auto open_scenes = EditorInterface::get_singleton()->get_open_scenes();
	for (auto &scene : open_scenes) {
		if (current_scene != nullptr && scene == current_scene->get_scene_file_path()) {
			continue;
		}
		while (is_changing_scene()) {
			OS::get_singleton()->delay_usec(10000);
			Main::iteration();
		}
		EditorInterface::get_singleton()->reload_scene_from_path(scene);
	}
	if (current_scene != nullptr) {
		// always iterate once if we must switch back, because sometimes is_changing_scene is false but we still need to iterate (?!)
		do {
			OS::get_singleton()->delay_usec(10000);
			Main::iteration();
		} while (is_changing_scene());
		EditorInterface::get_singleton()->reload_scene_from_path(current_scene->get_scene_file_path());
	}
	return true;
}

Callable PatchworkEditor::steal_close_current_script_tab_file_callback() {
	ScriptEditor *script_editor = EditorInterface::get_singleton()->get_script_editor();

	ERR_FAIL_COND_V_MSG(script_editor == nullptr, Callable(), "No script editor found");
	Callable new_callable;
	for (auto &child : script_editor->get_children()) {
		if (Object::cast_to<ConfirmationDialog>(child)) {
			auto confirmation_dialog = Object::cast_to<ConfirmationDialog>(child);
			bool found = false;
			for (auto &child : confirmation_dialog->get_children()) {
				if (Object::cast_to<HBoxContainer>(child)) {
					for (auto &subchild : Object::cast_to<HBoxContainer>(child)->get_children()) {
						if (Object::cast_to<Button>(subchild)) {
							auto button = Object::cast_to<Button>(subchild);
							if (button->get_text() == TTR("Discard")) {
								found = true;
								break;
							}
						}
					}
				}
				if (found) {
					break;
				}
			}
			ERR_FAIL_COND_V_MSG(!found, Callable(), "No discard button found");

			// we have to steal the signal handler for the "confirmed" signal
			List<Connection> connections;
			confirmation_dialog->get_signal_connection_list(SceneStringName(confirmed), &connections);
			ERR_FAIL_COND_V_MSG(connections.is_empty(), Callable(), "No connection found for the confirmed button");
			Callable confirm_callback = connections.front()->get().callable;
			// The signal handler is bound with arguments that we don't want, so we need to unbind it
			// we need "false, false" so that closing it does not save it and doesn't mess with the history
			auto custom = confirm_callback.get_custom();
			ERR_FAIL_COND_V_MSG(custom == nullptr, Callable(), "No callable found for the confirmed button");
			CallableCustomBind *custom_bind = dynamic_cast<CallableCustomBind *>(custom);
			ERR_FAIL_COND_V_MSG(custom_bind == nullptr, Callable(), "No callable found for the confirmed button");
			new_callable = custom_bind->get_callable().bind(false, false);
			ERR_FAIL_COND_V_MSG(new_callable.is_null(), Callable(), "Could not rebind the confirmed button");
			break;
		}
	}
	return new_callable;
}

void PatchworkEditor::close_script_file(const String &p_path) {
	auto scripts = EditorInterface::get_singleton()->get_script_editor()->get_open_scripts();
	Ref<Script> found;
	for (auto &script : scripts) {
		if (script->get_path() == p_path) {
			found = script;
			break;
		}
	}
	if (!found.is_valid()) {
		return;
	}
	Callable close_current_script_tab_callback = steal_close_current_script_tab_file_callback();
	ERR_FAIL_COND_MSG(close_current_script_tab_callback.is_null(), "No close callback found");
	// first, we have to load it
	EditorInterface::get_singleton()->get_script_editor()->edit(found, 0, 0, false);
	close_current_script_tab_callback.call();

	// we have to steal _close_tab from the signal handler on the erase_tab_confirm in the script editor
}

void PatchworkEditor::close_scene_file(const String &p_path) {
	auto open_scenes = EditorInterface::get_singleton()->get_open_scenes();
	if (!open_scenes.has(p_path)) {
		return; // nothing to do
	}
	// We have to LOAD the scene first (if it's already open, it'll just switch to the tab) and then close it by doing EditorNode::get_singleton()->trigger_menu_option()

	EditorNode::get_singleton()->load_scene(p_path);

        // Main::iteration();
        constexpr int CLOSE_MENU_OPTION = EditorNode::SCENE_CLOSE;

        // this needs to be bound to GDScript
        EditorNode::get_singleton()->trigger_menu_option(CLOSE_MENU_OPTION,
                                                         true);
}

void PatchworkEditor::close_files_if_open(const Vector<String> &p_paths) {
	for (auto &path : p_paths) {
		auto ext = path.get_extension().to_lower();
		if (ext == "tscn" || ext == "scn") {
			close_scene_file(path);
		} else if (ext == "gd") {
			close_script_file(path);
		}
	}
}

PatchworkEditor *PatchworkEditor::singleton = nullptr;

void PatchworkEditor::_bind_methods() {
	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_add_task", "task", "label", "steps", "can_cancel"), &PatchworkEditor::progress_add_task);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_task_step", "task", "state", "step", "force_refresh"), &PatchworkEditor::progress_task_step);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_end_task", "task"), &PatchworkEditor::progress_end_task);

	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_add_task_bg", "task", "label", "steps"), &PatchworkEditor::progress_add_task_bg);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_task_step_bg", "task", "step"), &PatchworkEditor::progress_task_step_bg);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("progress_end_task_bg", "task"), &PatchworkEditor::progress_end_task_bg);

	ClassDB::bind_static_method(get_class_static(), "unsaved_files_open", &PatchworkEditor::unsaved_files_open);

	ClassDB::bind_static_method(get_class_static(), D_METHOD("get_importer_by_name", "name"), &PatchworkEditor::get_importer_by_name);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("import_and_save_resource", "path", "import_file_content", "import_base_path"), &PatchworkEditor::import_and_save_resource);

	ClassDB::bind_static_method(get_class_static(), D_METHOD("save_all_scenes_and_scripts"), &PatchworkEditor::save_all_scenes_and_scripts);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("save_all_scripts"), &PatchworkEditor::save_all_scripts);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("get_unsaved_scripts"), &PatchworkEditor::get_unsaved_scripts);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("reload_scripts", "scripts"), &PatchworkEditor::reload_scripts);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("is_editor_importing"), &PatchworkEditor::is_editor_importing);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("is_changing_scene"), &PatchworkEditor::is_changing_scene);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("get_unsaved_files"), &PatchworkEditor::get_unsaved_files);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("force_refresh_editor_inspector"), &PatchworkEditor::force_refresh_editor_inspector);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("open_script_file", "script"), &PatchworkEditor::open_script_file);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("clear_editor_selection"), &PatchworkEditor::clear_editor_selection);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("get_resource_script_class", "path"), &PatchworkEditor::get_resource_script_class);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("close_scene_file", "path"), &PatchworkEditor::close_scene_file);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("close_script_file", "path"), &PatchworkEditor::close_script_file);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("close_files_if_open", "paths"), &PatchworkEditor::close_files_if_open);
	ClassDB::bind_static_method(get_class_static(), D_METHOD("refresh_after_source_change"), &PatchworkEditor::refresh_after_source_change);
}
