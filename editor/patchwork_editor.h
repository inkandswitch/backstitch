#ifndef PATCHWORK_EDITOR_H
#define PATCHWORK_EDITOR_H

#include "core/io/resource_importer.h"
#include "core/object/ref_counted.h"
#include "core/variant/dictionary.h"
#include "core/variant/variant.h"
#include "editor/editor_node.h"
#include "scene/gui/control.h"
#include "scene/main/node.h"

class PatchworkEditor : public Object {
  GDCLASS(PatchworkEditor, Object);

private:
  static Callable steal_close_current_script_tab_file_callback();

protected:
  static void _bind_methods();

public:
  PatchworkEditor();
  ~PatchworkEditor();
  static bool is_changing_scene();
  static void progress_add_task(const String &p_task, const String &p_label,
                                int p_steps, bool p_can_cancel = false);
  static bool progress_task_step(const String &p_task, const String &p_state,
                                 int p_step = -1, bool p_force_refresh = true);
  static void progress_end_task(const String &p_task);
};

#endif // PATCHWORK_EDITOR_H
