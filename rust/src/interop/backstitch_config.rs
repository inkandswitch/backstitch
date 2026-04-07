use godot::classes::{ConfigFile, DirAccess, Engine, FileAccess, Os};
use godot::global::Error;
use godot::prelude::*;
use godot::builtin::{Variant};

#[derive(GodotClass)]
#[class(base=Object)]
pub struct BackstitchConfig {
    #[base]
    base: Base<Object>,

    project_config: Gd<ConfigFile>,
    user_config: Gd<ConfigFile>,
	user_config_path: GString,
}

const CONFIG_FILE_NAME: &str = "backstitch.cfg";
const USER_DIR_NAME: &str = "backstitch_plugin";
const CONFIG_PROJECT_FILE: &str = "res://backstitch.cfg";

#[godot_api]
impl BackstitchConfig {

	pub fn singleton() -> Gd<Self> {
		Engine::singleton().get_singleton("BackstitchConfig").unwrap().cast::<Self>()
	}

    #[func]
    pub fn get_project_value(&self, key: GString, default: Variant) -> Variant {
        self.project_config
            .get_value_ex("backstitch", &key).default(&default).done()
    }

    #[func]
    pub fn get_user_value(&self, key: GString, default: Variant) -> Variant {
        self.user_config
            .get_value_ex("backstitch", &key).default(&default).done()
    }

    #[func]
    pub fn set_project_value(&mut self, key: GString, value: Variant) {
        self.project_config.set_value("backstitch", &key, &value);
        if self.project_config.save(CONFIG_PROJECT_FILE) != Error::OK {
            godot_error!("Failed to save backstitch configuration");
        }
    }

    #[func]
    pub fn set_user_value(&mut self, key: GString, value: Variant) {
        self.user_config.set_value("backstitch", &key, &value);
        if self.user_config.save(&self.user_config_path) != Error::OK{
            godot_error!("Failed to save backstitch user configuration");
        }
    }
}

#[godot_api]
impl IObject for BackstitchConfig {
    fn init(_base: Base<Object>) -> Self {
		// user_data_dir points to "user://", which is project specific, so we need to get the base dir and join it with the plugin name
		let user_dir = Os::singleton().get_user_data_dir().get_base_dir().path_join(USER_DIR_NAME);
		let user_config_path = user_dir.path_join(CONFIG_FILE_NAME);

		let mut project_config = ConfigFile::new_gd();
		let mut user_config = ConfigFile::new_gd();
		if FileAccess::file_exists(CONFIG_PROJECT_FILE) {
			project_config.load(CONFIG_PROJECT_FILE);
		}
		if FileAccess::file_exists(&user_config_path) {
			user_config.load(&user_config_path);
		} else {
			if !DirAccess::dir_exists_absolute(&user_dir) {
				DirAccess::make_dir_recursive_absolute(&user_dir);
			}
		}
		Self {
			base: _base,
			project_config,
			user_config,
			user_config_path,
		}
	}
}
