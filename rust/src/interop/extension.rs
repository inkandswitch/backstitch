use godot::obj::Singleton;
use godot::{
    classes::{Engine, ResourceLoader, ResourceSaver},
    init::{EditorRunBehavior, ExtensionLibrary, InitStage, gdextension},
    obj::{Gd, NewAlloc, NewGd},
};

use crate::{
    helpers::tracing::initialize_tracing,
    interop::{
        backstitch_resource_loader::{BackstitchResourceFormatSaver, BackstitchResourceLoader},
        godot_project::GodotProject,
    },
};

struct MyExtension;
static mut BACKSTITCH_RESOURCE_LOADER: Option<Gd<BackstitchResourceLoader>> = None;
static mut BACKSTITCH_RESOURCE_FORMAT_SAVER: Option<Gd<BackstitchResourceFormatSaver>> = None;

#[gdextension]
unsafe impl ExtensionLibrary for MyExtension {
    fn editor_run_behavior() -> EditorRunBehavior {
        EditorRunBehavior::ToolClassesOnly
    }

    fn on_stage_init(level: InitStage) {
        if level == InitStage::Scene {
            initialize_tracing();
            #[cfg(target_os = "android")]
            initialize_android_cert_dir();
            tracing::info!("** on_level_init: Scene");
            Engine::singleton().register_singleton("GodotProject", &GodotProject::new_alloc());
            let loader = BackstitchResourceLoader::new_gd();
            let saver = BackstitchResourceFormatSaver::new_gd();
            ResourceLoader::singleton()
                .add_resource_format_loader_ex(&loader)
                .at_front(true)
                .done();
            ResourceSaver::singleton()
                .add_resource_format_saver_ex(&saver)
                .at_front(true)
                .done();
            unsafe {
                BACKSTITCH_RESOURCE_LOADER = Some(loader);
                BACKSTITCH_RESOURCE_FORMAT_SAVER = Some(saver);
            }
        } else if level == InitStage::Editor {
            tracing::info!("** on_level_init: Editor");
        }
    }

    fn on_stage_deinit(level: InitStage) {
        if level == InitStage::Editor {
            tracing::info!("** on_level_deinit: Editor");
        }
        if level == InitStage::Scene {
            // TODO: Figure out how to safely have a static mut pointer to a Gd<T>
            #[allow(clippy::deref_addrof)]
            let loader = unsafe { &*(&raw mut BACKSTITCH_RESOURCE_LOADER) };
            #[allow(clippy::deref_addrof)]
            let saver = unsafe { &*(&raw mut BACKSTITCH_RESOURCE_FORMAT_SAVER) };
            if let Some(loader) = loader {
                ResourceLoader::singleton().remove_resource_format_loader(loader);
            }
            if let Some(saver) = saver {
                ResourceSaver::singleton().remove_resource_format_saver(saver);
            }
            unsafe {
                BACKSTITCH_RESOURCE_LOADER = None;
                BACKSTITCH_RESOURCE_FORMAT_SAVER = None;
            }
            tracing::info!("** on_level_deinit: Scene");
            unregister_singleton("GodotProject");
        }
    }
}

/// Point OpenSSL at Android's CA store.
///
/// We build OpenSSL from source for Android, so its compiled-in `OPENSSLDIR` refers to a path on
/// the build machine, and `openssl-probe` only looks for Termux and generic Linux locations. Left
/// alone, neither finds any root certificates and every TLS handshake fails to verify the peer.
/// Both directories below use OpenSSL's hashed-`CApath` layout, so `SSL_CERT_DIR` is enough.
#[cfg(target_os = "android")]
fn initialize_android_cert_dir() {
    use std::path::Path;

    const CERT_DIRS: [&str; 2] = [
        // Updatable CA store, present from API 34 on and preferred when available.
        "/apex/com.android.conscrypt/cacerts",
        "/system/etc/security/cacerts",
    ];

    if std::env::var_os("SSL_CERT_DIR").is_some() || std::env::var_os("SSL_CERT_FILE").is_some() {
        return;
    }

    let Some(cert_dir) = CERT_DIRS
        .iter()
        .copied()
        .find(|dir| Path::new(dir).is_dir())
    else {
        tracing::warn!(
            "no Android CA store found; TLS connections will not be able to verify peers"
        );
        return;
    };

    // Sound because extension init runs before we spawn any threads that could read the env.
    unsafe { std::env::set_var("SSL_CERT_DIR", cert_dir) };
    tracing::info!("using Android CA store at {cert_dir}");
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
