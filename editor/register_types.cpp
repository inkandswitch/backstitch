/*************************************************************************/
/*  register_types.cpp                                                   */
/*************************************************************************/

#include "register_types.h"
#include "patchwork_editor.h"

#include "core/object/class_db.h"

void initialize_patchwork_editor_module(ModuleInitializationLevel p_level) {
	if (p_level == MODULE_INITIALIZATION_LEVEL_SCENE) {
		ClassDB::register_class<PatchworkEditor>();
	}
}

void uninitialize_patchwork_editor_module(ModuleInitializationLevel p_level) {
}
