use godot::{classes::{Engine, ResourceLoader, ResourceSaver}, init::{EditorRunBehavior, ExtensionLibrary, InitLevel, gdextension}, obj::{Gd, NewAlloc, NewGd}};
use godot::obj::Singleton;

use crate::{helpers::tracing::initialize_tracing, interop::{godot_project::GodotProject, backstitch_config::BackstitchConfig, backstitch_resource_loader::{BackstitchResourceFormatSaver, BackstitchResourceLoader}}};


struct MyExtension;
static mut BACKSTITCH_RESOURCE_LOADER: Option<Gd<BackstitchResourceLoader>> = None;
static mut BACKSTITCH_RESOURCE_FORMAT_SAVER: Option<Gd<BackstitchResourceFormatSaver>> = None;

#[gdextension]
unsafe impl ExtensionLibrary for MyExtension {
    fn editor_run_behavior() -> EditorRunBehavior {
        EditorRunBehavior::ToolClassesOnly
    }

    fn on_level_init(level: InitLevel) {
        if level == InitLevel::Scene {
            initialize_tracing();
            tracing::info!("** on_level_init: Scene");
            Engine::singleton().register_singleton("BackstitchConfig", &BackstitchConfig::new_alloc());
            Engine::singleton().register_singleton("GodotProject", &GodotProject::new_alloc());
            let loader = BackstitchResourceLoader::new_gd();
            let saver = BackstitchResourceFormatSaver::new_gd();
            let _ = ResourceLoader::singleton().add_resource_format_loader_ex(&loader).at_front(true).done();
            let _ = ResourceSaver::singleton().add_resource_format_saver_ex(&saver).at_front(true).done();
            unsafe {
                BACKSTITCH_RESOURCE_LOADER = Some(loader);
                BACKSTITCH_RESOURCE_FORMAT_SAVER = Some(saver);
            }
        } else if level == InitLevel::Editor {
            tracing::info!("** on_level_init: Editor");
        }
    }

    fn on_level_deinit(level: InitLevel) {
        if level == InitLevel::Editor {
            tracing::info!("** on_level_deinit: Editor");
        }
        if level == InitLevel::Scene {
            // TODO: Figure out how to safely have a static mut pointer to a Gd<T>
            let loader = unsafe { &*(&raw mut BACKSTITCH_RESOURCE_LOADER) };
            let saver = unsafe { &*(&raw mut BACKSTITCH_RESOURCE_FORMAT_SAVER) };
            if let Some(loader) = loader {
                let _ = ResourceLoader::singleton().remove_resource_format_loader(loader);
            }
            if let Some(saver) = saver {
                let _ = ResourceSaver::singleton().remove_resource_format_saver(saver);
            }
            unsafe {
                BACKSTITCH_RESOURCE_LOADER = None;
                BACKSTITCH_RESOURCE_FORMAT_SAVER = None;
            }    
            tracing::info!("** on_level_deinit: Scene");
            unregister_singleton("GodotProject");
            unregister_singleton("BackstitchConfig");
        }
    }
}


fn unregister_singleton(singleton_name: &str) {
    if Engine::singleton().has_singleton(singleton_name) {
        let my_singleton = Engine::singleton().get_singleton(singleton_name);
        Engine::singleton().unregister_singleton(singleton_name);
        if let Some(my_singleton) = my_singleton {
            my_singleton.free();
        }
    }
}
