use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use godot::builtin::{GString, PackedStringArray, StringName, VarDictionary, Variant};
use godot::classes::resource_loader::CacheMode;
use godot::classes::{
    ClassDb, ConfigFile, IResourceFormatLoader, IResourceFormatSaver, ProjectSettings, Resource, ResourceFormatLoader, ResourceFormatSaver, ResourceLoader, ResourceUid
};
use godot::global::Error;
use godot::prelude::*;
use uuid::Uuid;

use crate::fs::file_utils::FileContent;
use crate::helpers::history_path::HistoryRefPath;
use crate::helpers::history_ref::HistoryRef;
use crate::interop::fake_importers::FakeImporter;
use crate::interop::godot_project::GodotProject;

/// This class allows us to load resources directly from backstitch history.
/// It is registered as a resource format loader with Godot.
/// Works on paths that are formatted as `backstitch://<doc_id>+<ChangeHash>/<actual_path>`
#[derive(GodotClass)]
#[class(base = ResourceFormatLoader)]
pub struct BackstitchResourceLoader {
    #[base]
    base: Base<ResourceFormatLoader>,
    fake_importer: FakeImporter,
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
        let temp_text;
        let buf: &[u8] = match content {
			FileContent::String(text) => {
				text.as_bytes()
			}
			FileContent::Binary(data) => {
				data
			}
            FileContent::Scene(scene) => {
                // ext_resource path to the backstitch path at the same history ref and sets UIDs to -1
                // (None) so Godot loads by path via this loader.
                temp_text = Some(scene.serialize_with_ext_resource_override(Some(history_ref), true));
				temp_text.as_ref().unwrap().as_bytes()
            }
            FileContent::Deleted => {
                return Err(Error::ERR_FILE_NOT_FOUND);
            }
        };
        let mut file = match File::create(&temp_path) {
            Ok(f) => f,
            Err(_) => return Err(Error::ERR_CANT_CREATE),
        };
        if file.write_all(buf).is_err() {
            return Err(Error::ERR_CANT_CREATE);
        }
        drop(file);
        Ok(())
    }

    fn get_importer_and_params_from_import_file_content(
        &self,
        import_file_content: &str,
    ) -> (GString, VarDictionary) {
        let mut import_file = ConfigFile::new_gd();
        import_file.parse(import_file_content);
        let importer = import_file.get_value("remap", "importer");
        let keys: PackedStringArray = import_file.get_section_keys("params");
        let mut params: VarDictionary = vdict!{};
        for key in keys.as_slice().iter() {
            params.set(key.to_variant(), import_file.get_value("params", key));
        }
        (importer.to::<GString>(), params)
    }

    fn set_resource_path(resource: &mut Gd<Resource>, path: GString, cache_mode: CacheMode) {
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
    }

    fn get_content_as_textfile_resource(content: &FileContent) -> Option<Gd<Resource>> {
        // For some reason, TextFile isn't bound in Rust, so we instantiate it manually.
        if !content.is_text() && !content.is_scene() {
            tracing::error!("Do not try to save a non-text or scene file as a TextFile");
            return None;
        }
        let resource = ClassDb::singleton().instantiate(&StringName::from("TextFile"));
        if resource.is_nil(){
            tracing::error!("Error instantiating TextFile");
            return None;
        }
        let mut resource = resource.try_to::<Gd<Resource>>().unwrap();
        match content {
            FileContent::String(text) => {
                resource.call("set_text", &[text.to_variant()]);
            }
            FileContent::Scene(scene) => {
                resource.call("set_text", &[scene.serialize().to_variant()]);
            }
            _ => return None,
        }
        Some(resource)
    }
}

#[godot_api]
impl IResourceFormatLoader for BackstitchResourceLoader {
    fn init(base: Base<ResourceFormatLoader>) -> Self {
        Self { base, fake_importer: FakeImporter::default() }
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
        // _original_path should match the path passed to load() because the resource loader should fail to remap the path due to the `backstitch` prefix.
        // If it doesn't, log it so we know our assumptions were wrong.
        debug_assert!(_original_path == path, "Original path {} does not match path {}, we should never get here!", _original_path, path);
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

        if content.is_deleted() {
            tracing::error!("File is deleted at ref {} path {}", history_ref_path.ref_, history_ref_path.path);
            return Variant::nil();
        }
        let mut maybe_import = import_file_content.is_some();
        if !maybe_import {
            let extensions = ResourceLoader::singleton().get_recognized_extensions_for_type("");
            let ext = Path::new(&history_ref_path.path).extension().unwrap_or_default().to_string_lossy().to_string().to_lowercase();
            if !extensions.contains(&GString::from(&ext)) && !content.is_scene() {
                maybe_import = true;
            }
        }

        if maybe_import {
            let (_importer, _params) = match import_file_content {
                Some(FileContent::String(import_file_content)) => {
                    let (importer, params) = self.get_importer_and_params_from_import_file_content(&import_file_content);
                    (Some(importer.to_string()), params)
                },
                _ => (None, vdict!{}),
            };    
            if self.fake_importer.recognize(&history_ref_path.path, _importer.as_deref()) {
                let content_bytes = match &content {
                    FileContent::String(t) => t.as_bytes(),
                    FileContent::Binary(bytes) => bytes.as_slice(),
                    _ => {
                        tracing::error!("Content is not a string or binary, we should never get here");
                        return Variant::nil();
                    }
                };
                let resource = self.fake_importer.import_file(&history_ref_path.to_string(), _importer.as_deref(), content_bytes, &_params);
                if let Err(e) = resource {
                    if content.is_text() {
                        let resource = Self::get_content_as_textfile_resource(&content);
                        return resource.unwrap_or_default().to_variant();
                    }
                    // don't log error if the importer is not implemented
                    if e != Error::ERR_UNAVAILABLE {
                        tracing::error!("Error importing file: {}", e.as_str());
                    }
                    return Variant::nil();
                }
                let mut resource = resource.unwrap();
                Self::set_resource_path(&mut resource, path, cache_mode);
                return resource.to_variant();
            }
            // else continue with the normal flow
        }

        let temp_path = Self::get_temp_path(&history_ref_path, None);

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

        Self::set_resource_path(&mut resource, path, cache_mode);
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
