use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use godot::builtin::{GString, PackedStringArray, StringName, VarDictionary, Variant};
use godot::classes::resource_loader::CacheMode;
use godot::classes::{
    ConfigFile, IResourceFormatLoader, IResourceFormatSaver, ProjectSettings, Resource,
    ResourceFormatLoader, ResourceFormatSaver, ResourceLoader, ResourceUid,
};
use godot::global::Error;
use godot::prelude::*;
use uuid::Uuid;

use crate::fs::file_utils::FileContent;
use crate::helpers::history_path::HistoryRefPath;
use crate::helpers::history_ref::HistoryRef;
use crate::interop::godot_accessors::BackstitchEditorAccessor;
use crate::interop::godot_project::GodotProject;

/// This class allows us to load resources directly from backstitch history.
/// It is registered as a resource format loader with Godot.
/// Works on paths that are formatted as `backstitch://<doc_id>+<ChangeHash>/<actual_path>`
#[derive(GodotClass)]
#[class(base = ResourceFormatLoader)]
pub struct BackstitchResourceLoader {
    #[base]
    base: Base<ResourceFormatLoader>,
}

#[inline]
fn recognize_path(path: GString) -> bool {
    HistoryRefPath::recognize_path(&path.to_string())
}
impl BackstitchResourceLoader {
    fn get_content_at_ref_path_str(&self, ref_path_str: &str) -> Result<FileContent, Error> {
        let history_ref_path = match HistoryRefPath::from_str(ref_path_str) {
            Ok(history_ref_path) => history_ref_path,
            Err(_) => return Err(Error::ERR_FILE_UNRECOGNIZED),
        };
        self.get_content_at_history_ref_path(&history_ref_path)
    }

    fn get_content_at_history_ref_path(
        &self,
        history_ref_path: &HistoryRefPath,
    ) -> Result<FileContent, Error> {
        let Some(content) = GodotProject::get_singleton()
            .bind()
            .get_file_at_ref(&history_ref_path.path, &history_ref_path.ref_)
        else {
            return Err(Error::ERR_FILE_NOT_FOUND);
        };

        Ok(content)
    }

    fn get_content_and_import_file_content_at_history_ref_path(
        &self,
        history_ref_path: &HistoryRefPath,
    ) -> Result<(FileContent, Option<FileContent>), Error> {
        let import_path = format!("{}.import", history_ref_path.path);
        let Some(contents) = GodotProject::get_singleton().bind().get_files_at_ref(
            &history_ref_path.ref_,
            &HashSet::from([history_ref_path.path.clone(), import_path.clone()]),
        ) else {
            return Err(Error::ERR_FILE_NOT_FOUND);
        };
        let content = contents
            .get(&history_ref_path.path)
            .ok_or(Error::ERR_FILE_NOT_FOUND)?
            .to_owned();
        let import_content = contents.get(&import_path);
        let mut import_content = if import_content.is_none() {
            None
        } else {
            Some(import_content.unwrap().to_owned())
        };
        if import_content.is_none() {
            let current_ref = GodotProject::get_singleton().bind().get_current_ref();
            if let Some(current_ref) = current_ref {
                import_content = GodotProject::get_singleton()
                    .bind()
                    .get_file_at_ref(&import_path, &current_ref);
            }
        }
        Ok((content, import_content))
    }
    /// ext_resource path to the backstitch path at the same history ref and sets UIDs to -1
    /// (None) so Godot loads by path via this loader.
    fn content_bytes_for_temp(
        content: &FileContent,
        history_ref: &HistoryRef,
    ) -> Result<Vec<u8>, Error> {
        match content {
            FileContent::Scene(scene) => Ok(scene
                .serialize_with_ext_resource_override(Some(history_ref), true)
                .into_bytes()),
            FileContent::String(s) => Ok(s.as_bytes().to_vec()),
            FileContent::Binary(b) => Ok(b.clone()),
            FileContent::Deleted => Err(Error::ERR_FILE_NOT_FOUND),
        }
    }

    fn get_temp_path(history_ref_path: &HistoryRefPath, override_ext: Option<&str>) -> PathBuf {
        let path = history_ref_path
            .path
            .strip_prefix("res://")
            .unwrap_or(&history_ref_path.path);
        let ext = if let Some(override_ext) = override_ext {
            override_ext
        } else {
            Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("res")
        };
        let temp_name = format!("backstitch_{}.{}", Uuid::new_v4(), ext);
        let temp_path = std::env::temp_dir().join(&temp_name);
        temp_path
    }

    fn remove_temp_path(temp_path: &PathBuf) -> Result<(), Error> {
        if std::fs::remove_file(temp_path).is_err() {
            return Err(Error::ERR_CANT_CREATE);
        }
        Ok(())
    }

    fn write_content_to_temp_file(
        content: &FileContent,
        history_ref: &HistoryRef,
        temp_path: &PathBuf,
    ) -> Result<(), Error> {
        let bytes = match Self::content_bytes_for_temp(&content, history_ref) {
            Ok(b) => b,
            Err(e) => return Err(e),
        };

        let mut file = match File::create(&temp_path) {
            Ok(f) => f,
            Err(_) => return Err(Error::ERR_CANT_CREATE),
        };
        if file.write_all(&bytes).is_err() {
            return Err(Error::ERR_CANT_CREATE);
        }
        drop(file);
        Ok(())
    }

    fn get_path_and_type_from_import_file_content(
        &self,
        import_file_content: &str,
    ) -> (GString, GString) {
        let mut import_file = ConfigFile::new_gd();
        import_file.parse(import_file_content);
        let mut path = import_file.get_value("remap", "path");
        let mut type_name = import_file.get_value("remap", "type");
        if path.is_nil() {
            let remap_keys = import_file.get_section_keys("remap");
            for i in 0..remap_keys.len() {
                let remap_key = remap_keys.get(i);
                let remap_key_str = remap_key.unwrap_or_default().to_string();
                if remap_key_str.starts_with("path") {
                    path = import_file.get_value("remap", &remap_key_str);
                    break;
                }
            }
        }
        if type_name.is_nil() {
            type_name = import_file.get_value("remap", "type");
        }
        (path.to::<GString>(), type_name.to::<GString>())
    }
}

#[godot_api]
impl IResourceFormatLoader for BackstitchResourceLoader {
    fn init(base: Base<ResourceFormatLoader>) -> Self {
        Self { base }
    }

    fn get_recognized_extensions(&self) -> PackedStringArray {
        // NOTE!: There is no `get_recognized_extensions_for_type()` in the extension api;
        // the default implmentation for ResourceFormatLoader::get_recognized_extensions_for_type() is this:
        // ```c++
        //	if (p_type.is_empty() || handles_type(p_type)) {
        //        get_recognized_extensions(p_extensions);
        //  }
        // ```
        // so when the classdb starts calling ResourceFormatLoader::get_recognized_extensions_for_type() for every type,
        // we end up polluting the extension list for all types when this is called.
        // This isn't called during loading if `recognize_path()` is implemented, so it's not necessary to implement it
        return PackedStringArray::new();
    }

    fn recognize_path(&self, path: GString, _type: StringName) -> bool {
        recognize_path(path)
    }

    fn handles_type(&self, _type_name: StringName) -> bool {
        return true; // handles everything
    }

    fn get_resource_type(&self, path: GString) -> GString {
        let ext = path.get_extension().to_string().to_lowercase();
        match ext.as_str() {
            "scn" => return GString::from("PackedScene"),
            "tscn" => return GString::from("PackedScene"),
            "gd" => return GString::from("GDScript"),
            "cs" => return GString::from("CSharpScript"),
            "txt" => return GString::from("TextFile"),
            "md" => return GString::from("MarkdownFile"),
            "cfg" => return GString::from("ConfigFile"),
            "ini" => return GString::from("IniFile"),
            "log" => return GString::from("LogFile"),
            "json" => return GString::from("JsonFile"),
            "yml" => return GString::from("YamlFile"),
            "yaml" => return GString::from("YamlFile"),
            "tres" => {}                // break, we handle it below
            _ => return GString::new(), // let the other loaders handle it
        }
        let content = match self.get_content_at_ref_path_str(&path.to_string()) {
            Ok(content) => content,
            Err(_) => return GString::new(),
        };
        if let FileContent::Scene(scn) = content {
            return GString::from(&scn.resource_type);
        }
        return GString::new();
    }

    fn get_resource_script_class(&self, _path: GString) -> GString {
        let content = match self.get_content_at_ref_path_str(&_path.to_string()) {
            Ok(content) => content,
            Err(_) => return GString::new(),
        };
        if let FileContent::Scene(scn) = content {
            return GString::from(&scn.script_class.unwrap_or_default());
        }
        return GString::new();
    }

    fn get_resource_uid(&self, path: GString) -> i64 {
        let ext = path.get_extension().to_string().to_lowercase();
        let mut history_ref_path = match HistoryRefPath::from_str(&path.to_string()) {
            Ok(history_ref_path) => history_ref_path,
            Err(_) => return -1,
        };

        // TODO: more robust detection of this, godot doesn't expose `has_custom_uid` function in the extension api.
        let has_custom_uid = !(ext == "gd" || ext == "cs" || ext == "gdextension");

        if !has_custom_uid {
            history_ref_path.path = history_ref_path.path + ".uid";
        }
        let content = match self.get_content_at_history_ref_path(&history_ref_path) {
            Ok(content) => content,
            Err(_) => return -1,
        };
        if !has_custom_uid {
            if let FileContent::String(string) = content {
                return ResourceUid::singleton().text_to_id(&string);
            }
            return -1;
        }
        if let FileContent::Scene(scn) = content {
            return ResourceUid::singleton().text_to_id(&scn.uid);
        }
        return -1;
    }

    fn get_dependencies(&self, _path: GString, _add_types: bool) -> PackedStringArray {
        let content = match self.get_content_at_ref_path_str(&_path.to_string()) {
            Ok(content) => content,
            Err(_) => return PackedStringArray::new(),
        };
        if let FileContent::Scene(scn) = content {
            return PackedStringArray::from_iter(
                scn.ext_resources
                    .iter()
                    .map(|(_id, ext_resource)| GString::from(&ext_resource.path)),
            );
        }
        return PackedStringArray::new();
    }

    fn rename_dependencies(&self, _path: GString, _renames: VarDictionary) -> Error {
        // Backstitch resources are loaded from history and are read-only; we don't support renaming deps.
        Error::ERR_UNAVAILABLE
    }

    fn exists(&self, path: GString) -> bool {
        return self.recognize_path(path, StringName::default());
    }

    fn get_classes_used(&self, _path: GString) -> PackedStringArray {
        PackedStringArray::new()
    }

    fn load(
        &self,
        path: GString,
        _original_path: GString,
        _use_sub_threads: bool,
        cache_mode_ord: i32,
    ) -> Variant {
        let cache_mode = CacheMode::try_from_ord(cache_mode_ord).unwrap_or(CacheMode::IGNORE);
        let path_str = path.to_string();
        let history_ref_path = match HistoryRefPath::from_str(&path_str) {
            Ok(p) => p,
            Err(err) => {
                tracing::error!("Error getting history ref path {}: {}", path_str, err);
                return Variant::nil();
            }
        };

        let (content, import_file_content) =
            match self.get_content_and_import_file_content_at_history_ref_path(&history_ref_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Error getting content at history ref path: {}", e.as_str());
                    return Variant::nil();
                }
            };
        let mut temp_path = Self::get_temp_path(&history_ref_path, None);

        match Self::write_content_to_temp_file(&content, &history_ref_path.ref_, &temp_path) {
            Ok(_) => (),
            Err(e) => {
                tracing::error!(
                    "Error writing content to temp at path {}: {}",
                    path_str,
                    e.as_str()
                );
                return Variant::nil();
            }
        };

        if let Some(FileContent::String(import_file_content)) = import_file_content {
            let (import_base_path, _type_name) =
                self.get_path_and_type_from_import_file_content(&import_file_content);
            let ext = import_base_path.get_extension().to_string().to_lowercase();
            let temp_imported_path = Self::get_temp_path(&history_ref_path, Some(&ext));
            // temp_imported_path minus the extension
            let temp_base_name = temp_imported_path.to_string_lossy().to_string().strip_suffix(&format!(".{}", ext)).unwrap_or(&temp_imported_path.to_string_lossy().to_string()).to_string();
            let err = BackstitchEditorAccessor::import_and_save_resource(
                &temp_path.to_string_lossy().to_string(),
                &import_file_content,
                &temp_base_name,
            );
            let _ = Self::remove_temp_path(&temp_path);

            if err != Error::OK {
                tracing::error!(
                    "Error importing and saving resource at path {}: {}",
                    temp_path.to_string_lossy().to_string(),
                    err.as_str()
                );
                return Variant::nil();
            }
            temp_path = temp_imported_path.clone();
        }

        let temp_path_godot =
            GString::from(&temp_path.to_string_lossy().to_string()).simplify_path();

        let mut loader = ResourceLoader::singleton();
        let sub_cache_mode = match cache_mode {
            CacheMode::IGNORE_DEEP => CacheMode::IGNORE_DEEP,
            CacheMode::REPLACE => CacheMode::REPLACE,
            CacheMode::REPLACE_DEEP => CacheMode::REPLACE_DEEP,
            // Loading with "IGNORE" will not cache the temp file,
            // but it will allow sub-resources to be re-used since the resource loader passes `REUSE` when the cache mode is `IGNORE`
            _ => CacheMode::IGNORE,
        };
        let ret = match loader
            .load_ex(&temp_path_godot)
            .cache_mode(sub_cache_mode)
            .done()
        {
            Some(resource) => resource.to_variant(),
            None => {
                tracing::error!("Error loading resource: {}", temp_path_godot.to_string());
                let _ = Self::remove_temp_path(&temp_path);
                return Variant::nil();
            }
        };

        let _ = Self::remove_temp_path(&temp_path);

        let mut resource = match ret.try_to::<Gd<Resource>>() {
            Ok(res) => res,
            Err(_) => {
                return Variant::nil();
            }
        };

        match cache_mode {
            CacheMode::IGNORE | CacheMode::IGNORE_DEEP => {
                resource.set_path_cache(&path);
            }
            CacheMode::REPLACE | CacheMode::REPLACE_DEEP => {
                resource.take_over_path(&path);
            }
            CacheMode::REUSE => {
                resource.set_path(&path);
            }
            _ => {
                // we should never get here
                tracing::error!("Invalid cache mode: {}", cache_mode.ord());
            }
        }
        resource.to_variant()
    }
}

#[derive(GodotClass)]
#[class(base = ResourceFormatSaver)]
pub struct BackstitchResourceFormatSaver {
    #[base]
    base: Base<ResourceFormatSaver>,
}

#[godot_api]
impl IResourceFormatSaver for BackstitchResourceFormatSaver {
    fn init(base: Base<ResourceFormatSaver>) -> Self {
        Self { base }
    }

    fn save(&mut self, _resource: Option<Gd<Resource>>, _path: GString, _flags: u32) -> Error {
        // TODO: Decide if and how we want to save resources loaded from backstitch history; right now this is just here to prevent saving loaded history resources
        Error::ERR_LOCKED // indicate read-only
    }

    fn set_uid(&mut self, _path: GString, _uid: i64) -> Error {
        // TODO: see above
        Error::ERR_LOCKED
    }

    fn recognize(&self, _resource: Option<Gd<Resource>>) -> bool {
        if let Some(resource) = _resource {
            return recognize_path(resource.get_path());
        }
        false
    }

    fn get_recognized_extensions(&self, _resource: Option<Gd<Resource>>) -> PackedStringArray {
        // get_all_recognized_extensions()
        // see note in BackstitchResourceLoader::get_recognized_extensions()
        PackedStringArray::new()
    }

    fn recognize_path(&self, _resource: Option<Gd<Resource>>, path: GString) -> bool {
        return recognize_path(path);
    }
}

// currently unused
#[allow(dead_code)]
fn get_all_recognized_extensions() -> PackedStringArray {
    // // prevent infinite recursion
    // thread_local! {
    //     pub static CALLING_ON_THIS_THREAD: Cell<bool> = Cell::new(false);
    // }
    // if CALLING_ON_THIS_THREAD.get() {
    //     return PackedStringArray::new();
    // }
    // CALLING_ON_THIS_THREAD.set(true);
    let mut arr = ResourceLoader::singleton().get_recognized_extensions_for_type("");
    // CALLING_ON_THIS_THREAD.set(false);
    arr.push("cs");

    let textfile_extensions = ProjectSettings::singleton()
        .get_setting_ex("docks/filesystem/textfile_extensions")
        .default_value(&"txt,md,cfg,ini,log,json,yml,yaml,toml,xml".to_variant())
        .done()
        .to::<GString>()
        .split(",");

    let other_file_extensions = ProjectSettings::singleton()
        .get_setting_ex("docks/filesystem/other_file_extensions")
        .default_value(&"ico,icns".to_variant())
        .done()
        .to::<GString>()
        .split(",");
    arr.extend_array(&textfile_extensions);
    arr.extend_array(&other_file_extensions);
    arr
}
