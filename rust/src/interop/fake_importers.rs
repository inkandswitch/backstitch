use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use godot::classes::{ConfigFile, Image, ImageTexture, Resource};
use godot::classes::class_macros::private::virtuals::Os::{PackedByteArray, VarDictionary, Variant, vdict};
use godot::obj::{NewGd, Singleton};
use godot::prelude::Var;
use godot::{
    builtin::{GString, PackedStringArray},
    classes::{ClassDb, EditorInterface, Object},
    meta::ToGodot,
    obj::Gd,
};
use uuid::Uuid;

use crate::fs::file_utils::FileContent;


pub trait FakeImporter {
    fn recognize_importer(importer_name: &str) -> bool;
    fn import_file(path: &str, content: &Vec<u8>, params: &VarDictionary) -> Result<Resource, godot::global::Error>;
    fn save_file(path: &str, content: &Resource) -> Result<(), godot::global::Error>;
}


pub struct FakeResourceImporterTexture{

}

fn get_temp_path(old_path: &PathBuf, override_ext: Option<&str>) -> PathBuf {
    let path = old_path
        .strip_prefix("res://")
        .unwrap_or(&old_path);
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

fn write_content_to_temp_file(
    temp_path: &PathBuf,
    content: &Vec<u8>,
) -> Result<(), godot::global::Error> {
    let mut file = match File::create(&temp_path) {
        Ok(f) => f,
        Err(_) => return Err(godot::global::Error::ERR_CANT_CREATE),
    };
    if file.write_all(content.as_slice()).is_err() {
        return Err(godot::global::Error::ERR_CANT_CREATE);
    }
    drop(file);
    Ok(())
}

pub fn load_image_from_buffer(path: &str, content: &Vec<u8>, scale: f32) -> Result<Gd<Image>, godot::global::Error> {
    let mut image = Image::new_gd();
    let mut path = PathBuf::from(path);
    let ext = path.extension().unwrap_or_default().to_string_lossy().to_lowercase();
    let result = match ext.as_str() {
        "png" => {
            image.load_png_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "jpg" => {
            image.load_jpg_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "bmp" => {
            image.load_bmp_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "webp" => {
            image.load_webp_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "tga" => {
            image.load_tga_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "ktx" => {
            image.load_ktx_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "dds" => {
            image.load_dds_from_buffer(&PackedByteArray::from(content.as_slice()))
        }
        "svg" => {
            image.load_svg_from_buffer_ex(&PackedByteArray::from(content.as_slice())).scale(scale).done()
        }
        _ => { // a file type without a buffer load function, like "hdr" or "exr"
            let mut did_create_temp_file = false; 
            if path.starts_with("patchwork") {
                // we need to save the file to a temporary file
                path = get_temp_path(&path, Some("png"));
                write_content_to_temp_file(&path, content)?;
                did_create_temp_file = true;
            }
            let result = Image::load_from_file(path.to_str().unwrap_or_default());
            // rimraf the temp file
            if did_create_temp_file {
                std::fs::remove_file(path).unwrap();
            }
            image = result.ok_or(godot::global::Error::ERR_FILE_NOT_FOUND)?;
            godot::global::Error::OK
        }
    };
    if result != godot::global::Error::OK {
        return Err(result);
    }
    Ok(image)
}

impl FakeResourceImporterTexture {
    fn recognize_importer(importer_name: &str) -> bool {
        importer_name == "texture"
    }

    fn import_file(path: &str, content: &Vec<u8>, params: &VarDictionary) -> Result<Gd<Resource>, godot::global::Error> {
        let scale = params.get("scale").map(|s| s.to::<f32>()).unwrap_or(1.0);
        let mut image = load_image_from_buffer(path, content, scale)?;
        let texture = ImageTexture::create_from_image(&image).ok_or(godot::global::Error::ERR_INVALID_PARAMETER)?;
        Ok(texture.to_variant().try_to::<Gd<Resource>>().unwrap())
    }
}

