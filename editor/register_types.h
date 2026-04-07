/*************************************************************************/
/*  register_types.h                                                     */
/*************************************************************************/

#ifndef BACKSTITCH_EDITOR_REGISTER_TYPES_H
#define BACKSTITCH_EDITOR_REGISTER_TYPES_H

#include "modules/register_module_types.h"

void initialize_backstitch_editor_module(ModuleInitializationLevel p_level);
void uninitialize_backstitch_editor_module(ModuleInitializationLevel p_level);
void init_ver_regex();
void free_ver_regex();
#endif // BACKSTITCH_EDITOR_REGISTER_TYPES_H

