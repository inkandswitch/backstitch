#include "patchwork_editor.h"

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

PatchworkEditor::PatchworkEditor() {}

PatchworkEditor::~PatchworkEditor() {}

bool PatchworkEditor::is_changing_scene() {
  return EditorNode::get_singleton()->is_changing_scene();
}

void PatchworkEditor::progress_add_task(const String &p_task,
                                        const String &p_label, int p_steps,
                                        bool p_can_cancel) {
  EditorNode::get_singleton()->progress_add_task(p_task, p_label, p_steps,
                                                 p_can_cancel);
}

bool PatchworkEditor::progress_task_step(const String &p_task,
                                         const String &p_state, int p_step,
                                         bool p_force_refresh) {
  return EditorNode::get_singleton()->progress_task_step(
      p_task, p_state, p_step, p_force_refresh);
}

void PatchworkEditor::progress_end_task(const String &p_task) {
  EditorNode::get_singleton()->progress_end_task(p_task);
}

void PatchworkEditor::_bind_methods() {
  ClassDB::bind_static_method(get_class_static(), D_METHOD("is_changing_scene"),
                              &PatchworkEditor::is_changing_scene);
  ClassDB::bind_static_method(
      get_class_static(),
      D_METHOD("progress_add_task", "task", "label", "steps", "can_cancel"),
      &PatchworkEditor::progress_add_task);
  ClassDB::bind_static_method(
      get_class_static(),
      D_METHOD("progress_task_step", "task", "state", "step", "force_refresh"),
      &PatchworkEditor::progress_task_step);
  ClassDB::bind_static_method(get_class_static(),
                              D_METHOD("progress_end_task", "task"),
                              &PatchworkEditor::progress_end_task);
}
