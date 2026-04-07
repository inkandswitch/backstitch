/*************************************************************************/
/*  register_types.cpp                                                   */
/*************************************************************************/

#include "register_types.h"
#include "backstitch_editor.h"

#include "core/object/class_db.h"

void initialize_backstitch_editor_module(ModuleInitializationLevel p_level) {
	if (p_level == MODULE_INITIALIZATION_LEVEL_SCENE) {
		ClassDB::register_class<BackstitchEditor>();
	}
}

void uninitialize_backstitch_editor_module(ModuleInitializationLevel p_level) {
}
