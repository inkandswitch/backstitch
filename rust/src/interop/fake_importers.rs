use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use godot::classes::audio_stream_wav::LoopMode;
use godot::classes::class_macros::private::virtuals::Os::{
    Array, Encoding, PackedByteArray, Rect2i, VarDictionary, Vector2i, vdict,
};
use godot::classes::gltf_state::HandleBinaryImageMode;
use godot::classes::text_server::{
    FixedSizeScaleMode, FontAntialiasing, Hinting, SubpixelPositioning,
};
use godot::classes::{
    AudioStreamMp3, AudioStreamOggVorbis, AudioStreamWav, Cubemap, CubemapArray, DpiTexture,
    FbxDocument, FbxState, Font, FontFile, GltfDocument, GltfState, Image, ImageTexture,
    ImageTexture3D, ImageTextureLayered, PackedScene, Resource, Texture2DArray,
};
use godot::meta::FromGodot;
use godot::obj::{EngineEnum, NewGd};
use godot::tools::try_load;
use godot::{builtin::GString, meta::ToGodot, obj::Gd};
use uuid::Uuid;

fn get_extension<P: AsRef<Path>>(path: P) -> String {
    path.as_ref()
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn get_temp_path<P: AsRef<Path>>(old_path: P, override_ext: Option<&str>) -> PathBuf {
    let path = old_path
        .as_ref()
        .strip_prefix("res://")
        .unwrap_or(old_path.as_ref());
    let ext = if let Some(override_ext) = override_ext {
        override_ext
    } else {
        path.extension().and_then(|e| e.to_str()).unwrap_or("res")
    };
    let basename = path
        .file_stem()
        .unwrap_or(OsStr::new("file_name"))
        .to_string_lossy()
        .to_string();
    let temp_name = format!("backstitch_{}_{}.{}", basename, Uuid::new_v4(), ext);

    std::env::temp_dir().join(&temp_name)
}

fn write_content_to_temp_file<P: AsRef<Path>>(
    temp_path: P,
    content: &[u8],
) -> Result<(), godot::global::Error> {
    let mut file = match File::create(&temp_path) {
        Ok(f) => f,
        Err(_) => return Err(godot::global::Error::ERR_CANT_CREATE),
    };
    if file.write_all(content).is_err() {
        return Err(godot::global::Error::ERR_CANT_CREATE);
    }
    drop(file);
    Ok(())
}

const DEFAULT_RECOGNIZED_IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "tga", "svg", "bmp", "dds", "hdr", "exr",
];

pub fn load_image_from_buffer<P: AsRef<Path>>(
    path: P,
    content: &[u8],
    scale: f32,
) -> Result<Gd<Image>, godot::global::Error> {
    let mut image = Image::new_gd();
    let ext = get_extension(&path);
    let result = match ext.as_str() {
        "png" => image.load_png_from_buffer(&PackedByteArray::from(content)),
        "jpg" => image.load_jpg_from_buffer(&PackedByteArray::from(content)),
        "bmp" => image.load_bmp_from_buffer(&PackedByteArray::from(content)),
        "webp" => image.load_webp_from_buffer(&PackedByteArray::from(content)),
        "tga" => image.load_tga_from_buffer(&PackedByteArray::from(content)),
        "ktx" => image.load_ktx_from_buffer(&PackedByteArray::from(content)),
        "dds" => image.load_dds_from_buffer(&PackedByteArray::from(content)),
        "svg" => image
            .load_svg_from_buffer_ex(&PackedByteArray::from(content))
            .scale(scale)
            .done(),
        _ => {
            let path = path.as_ref();
            // a file type without a buffer load function, like "hdr" or "exr"
            let result = if path.starts_with("patchwork") {
                // we need to save the file to a temporary file
                let temp_path = get_temp_path(path, Some("png"));
                write_content_to_temp_file(&temp_path, content)?;
                let res = Image::load_from_file(temp_path.to_str().unwrap_or_default());
                let _ = std::fs::remove_file(&temp_path);
                res
            } else {
                Image::load_from_file(path.to_str().unwrap_or_default())
            }
            .ok_or(godot::global::Error::ERR_FILE_CANT_OPEN)?;
            image = result;
            godot::global::Error::OK
        }
    };
    if result != godot::global::Error::OK {
        return Err(result);
    }
    Ok(image)
}

pub trait FakeResourceImporter {
    fn recognize(&self, path: &str, importer_name: Option<&str>) -> bool {
        if let Some(importer_name) = importer_name {
            return self.get_recognized_importers().contains(&importer_name);
        }
        self.get_recognized_extensions()
            .contains(&get_extension(path).as_str())
    }

    // fn recognize_importer(importer_name: &str) -> bool;
    fn get_recognized_extensions(&self) -> Vec<&'static str>;
    fn get_recognized_importers(&self) -> Vec<&'static str>;
    fn import_file(
        &self,
        path: &str,
        importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error>;

    // fn save_file(path: &str, content: &Resource) -> Result<(), godot::global::Error>;
}

pub struct FakeResourceImporterTexture {}
pub struct FakeResourceImporterLayeredTexture {}
pub struct FakeResourceImporterMP3 {}
pub struct FakeResourceImporterOggVorbis {}
pub struct FakeResourceImporterWAV {}
pub struct FakeResourceImporterBMFont {}
pub struct FakeResourceImporterDynamicFont {}
pub struct FakeResourceImporterImageFont {}
pub struct FakeResourceImporterSVG {}
pub struct FakeResourceImporterOBJ {}
pub struct FakeResourceImporterScene {}
pub struct FakeResourceImporterTextureAtlas {}

fn get_or_default<T: ToGodot + FromGodot>(dict: &VarDictionary, key: &str, default: T) -> T {
    if let Some(value) = dict.get(key) {
        value.try_to_relaxed::<T>().unwrap_or(default)
    } else {
        default
    }
}

impl FakeResourceImporter for FakeResourceImporterTexture {
    fn recognize(&self, path: &str, importer_name: Option<&str>) -> bool {
        if let Some(importer_name) = importer_name {
            return self.get_recognized_importers().contains(&importer_name);
        }

        let ext = get_extension(path);
        if ext == "svg" {
            // default to FakeResourceImporterSVG for svg files
            return false;
        }
        self.get_recognized_extensions().contains(&ext.as_str())
    }

    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["texture"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        DEFAULT_RECOGNIZED_IMAGE_EXTENSIONS.to_vec()
    }

    fn import_file(
        &self,
        path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let image =
            load_image_from_buffer(path, content, get_or_default(params, "svg/scale", 1.0))?;
        // parameters aren't particularly relevant here
        let texture = ImageTexture::create_from_image(&image)
            .ok_or(godot::global::Error::ERR_INVALID_PARAMETER)?;
        Ok(texture.upcast::<Resource>())
    }
}

impl FakeResourceImporterLayeredTexture {
    fn get_image_slices_from_image(
        image: &Gd<Image>,
        x: i32,
        y: i32,
    ) -> Result<Vec<Gd<Image>>, godot::global::Error> {
        let mut slices = Vec::new();

        let slice_w = image.get_width() / x;
        let slice_h = image.get_height() / y;
        for i in 0..y {
            for j in 0..x {
                let Some(slice) = image.get_region(Rect2i::new(
                    Vector2i::new(j * slice_w, i * slice_h),
                    Vector2i::new(slice_w, slice_h),
                )) else {
                    return Err(godot::global::Error::ERR_INVALID_PARAMETER);
                };

                slices.push(slice);
            }
        }
        Ok(slices)
    }

    fn get_slice_arrangement(params: &VarDictionary) -> (i32, i32) {
        let layout: i32 = get_or_default(params, "slices/arrangement", 1);
        match layout {
            0 => (1, 6),
            1 => (2, 3),
            2 => (3, 2),
            3 => (6, 1),
            _ => (2, 3),
        }
    }
    fn get_width_and_height_from_params(importer_name: &str, params: &VarDictionary) -> (i32, i32) {
        if importer_name == "cubemap_texture" {
            Self::get_slice_arrangement(params)
        } else if importer_name == "2d_array_texture" || importer_name == "3d_texture" {
            let x: i32 = get_or_default(params, "slices/horizontal", 1);
            let y: i32 = get_or_default(params, "slices/vertical", 1);
            (x, y)
        } else if importer_name == "cubemap_array_texture" {
            let (hslices, vslices) = Self::get_slice_arrangement(params);
            let layout: i32 = get_or_default(params, "slices/layout", 1);
            let amount: i32 = get_or_default(params, "slices/amount", 1);
            match layout {
                0 => (hslices * amount, vslices), // horizontal
                1 => (hslices, vslices * amount), // vertical
                _ => (hslices, vslices * amount),
            }
        } else {
            (1, 1)
        }
    }
}

impl FakeResourceImporter for FakeResourceImporterLayeredTexture {
    fn recognize(&self, _path: &str, importer_name: Option<&str>) -> bool {
        if let Some(importer_name) = importer_name {
            return self.get_recognized_importers().contains(&importer_name);
        }

        // We want to default to FakeResourceImporterTexture for image types, so we return false here
        false
        // self.get_recognized_extensions().contains(&get_extension(path).as_str())
    }

    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec![
            "cubemap_texture",
            "2d_array_texture",
            "cubemap_array_texture",
            "3d_texture",
        ]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        DEFAULT_RECOGNIZED_IMAGE_EXTENSIONS.to_vec()
    }

    fn import_file(
        &self,
        path: &str,
        importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let image = load_image_from_buffer(path, content, 1.0)?;
        let (x, y) = Self::get_width_and_height_from_params(importer_name, params);
        let slices = Self::get_image_slices_from_image(&image, x, y)?;
        let tex: Gd<Resource> = if importer_name == "3d_texture" {
            let width = image.get_width() / x;
            let height = image.get_height() / y;
            let depth = x * y;
            let mipmap_limit = params
                .get("mipmaps/limit")
                .map(|s| s.to::<i32>())
                .unwrap_or(-1);
            let has_mipmaps = mipmap_limit != 0;
            let mut tex_3d: Gd<ImageTexture3D> = ImageTexture3D::new_gd();
            let err = tex_3d.create(
                image.get_format(),
                width,
                height,
                depth,
                has_mipmaps,
                &slices.into_iter().collect::<Array<Gd<Image>>>(),
            );
            if err != godot::global::Error::OK {
                return Err(err);
            }
            tex_3d.upcast::<Resource>()
        } else {
            let mut tex_layered: Gd<ImageTextureLayered> = match importer_name {
                "cubemap_texture" => Cubemap::new_gd().upcast::<ImageTextureLayered>(),
                "2d_array_texture" => Texture2DArray::new_gd().upcast::<ImageTextureLayered>(),
                "cube_array_texture" => CubemapArray::new_gd().upcast::<ImageTextureLayered>(),
                _ => return Err(godot::global::Error::ERR_INVALID_PARAMETER),
            };
            let err =
                tex_layered.create_from_images(&slices.into_iter().collect::<Array<Gd<Image>>>());
            if err != godot::global::Error::OK {
                return Err(err);
            }
            tex_layered.upcast::<Resource>()
        };

        Ok(tex)
    }
}

impl FakeResourceImporterMP3 {
    fn get_mp3_from_buffer(content: &[u8]) -> Result<Gd<AudioStreamMp3>, godot::global::Error> {
        let Some(mp3) = AudioStreamMp3::load_from_buffer(&PackedByteArray::from(content)) else {
            return Err(godot::global::Error::ERR_INVALID_PARAMETER);
        };
        Ok(mp3)
    }
}

impl FakeResourceImporter for FakeResourceImporterMP3 {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["mp3"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["mp3"]
    }

    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let mut mp3 = Self::get_mp3_from_buffer(content)?;
        mp3.set_loop(get_or_default(params, "loop", false));
        mp3.set_loop_offset(get_or_default(params, "loop_offset", 0.0));
        mp3.set_bpm(get_or_default(params, "bpm", 0.0));
        mp3.set_beat_count(get_or_default(params, "beat_count", 0));
        mp3.set_bar_beats(get_or_default(params, "bar_beats", 4));
        Ok(mp3.upcast::<Resource>())
    }
}

impl FakeResourceImporterOggVorbis {
    fn get_ogg_vorbis_from_buffer(
        content: &[u8],
    ) -> Result<Gd<AudioStreamOggVorbis>, godot::global::Error> {
        let Some(ogg_vorbis) =
            AudioStreamOggVorbis::load_from_buffer(&PackedByteArray::from(content))
        else {
            return Err(godot::global::Error::ERR_INVALID_PARAMETER);
        };
        Ok(ogg_vorbis)
    }
}

impl FakeResourceImporter for FakeResourceImporterOggVorbis {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["oggvorbisstr"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["ogg"]
    }

    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let mut ogg_vorbis = Self::get_ogg_vorbis_from_buffer(content)?;
        ogg_vorbis.set_loop(get_or_default(params, "loop", false));
        ogg_vorbis.set_loop_offset(get_or_default(params, "loop_offset", 0.0));
        ogg_vorbis.set_bpm(get_or_default(params, "bpm", 0.0));
        ogg_vorbis.set_beat_count(get_or_default(params, "beat_count", 0));
        ogg_vorbis.set_bar_beats(get_or_default(params, "bar_beats", 4));
        Ok(ogg_vorbis.upcast::<Resource>())
    }
}

impl FakeResourceImporterWAV {
    fn get_wav_from_buffer(content: &[u8]) -> Result<Gd<AudioStreamWav>, godot::global::Error> {
        let Some(wav) = AudioStreamWav::load_from_buffer(&PackedByteArray::from(content)) else {
            return Err(godot::global::Error::ERR_INVALID_PARAMETER);
        };
        Ok(wav)
    }
}

impl FakeResourceImporter for FakeResourceImporterWAV {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["wav"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["wav"]
    }

    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let mut wav = Self::get_wav_from_buffer(content)?;
        let mode: i32 = get_or_default(params, "loop_mode", 0);
        if mode > 0 {
            wav.set_loop_mode(LoopMode::try_from_ord(mode - 1).unwrap_or(LoopMode::DISABLED));
        }
        wav.set_loop_begin(get_or_default(params, "loop_begin", 0));
        wav.set_loop_end(get_or_default(params, "loop_end", -1));
        Ok(wav.upcast::<Resource>())
    }
}

impl FakeResourceImporterBMFont {
    fn get_bmfont_from_buffer(
        path: &str,
        content: &[u8],
    ) -> Result<Gd<FontFile>, godot::global::Error> {
        let mut font_file = FontFile::new_gd();
        // save it to a temporary file
        let temp_path = get_temp_path(PathBuf::from(path), Some("font"));
        write_content_to_temp_file(&temp_path, content)?;
        let result = match font_file.load_bitmap_font(temp_path.to_str().unwrap_or_default()) {
            godot::global::Error::OK => Ok(font_file),
            e => Err(e),
        };
        let _ = std::fs::remove_file(&temp_path);
        result
    }
}

impl FakeResourceImporter for FakeResourceImporterBMFont {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["font_data_bmfont"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["fnt", "font"]
    }

    fn import_file(
        &self,
        path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let mut font_file = Self::get_bmfont_from_buffer(path, content)?;
        let fallbacks: Array<Gd<Font>> = get_or_default(params, "fallbacks", Array::new());
        let smode: i32 = get_or_default(params, "scaling_mode", 2);
        font_file.set_allow_system_fallback(false);
        // TODO: This means that we need to make sure that the config file loads the fonts from the correct backstitch reference, but we don't currently do that;
        // this is unlikely to be an issue, since we're just doing this for the diff, but something to keep in mind.
        font_file.set_fallbacks(&fallbacks);
        font_file.set_fixed_size_scale_mode(
            FixedSizeScaleMode::try_from_ord(smode).unwrap_or(FixedSizeScaleMode::ENABLED),
        );
        Ok(font_file.upcast::<Resource>())
    }
}

impl FakeResourceImporterDynamicFont {
    fn get_dynamic_font_from_buffer(
        path: &str,
        content: &[u8],
    ) -> Result<Gd<FontFile>, godot::global::Error> {
        let mut dynamic_font = FontFile::new_gd();
        let temp_path = get_temp_path(PathBuf::from(path), Some("font"));
        write_content_to_temp_file(&temp_path, content)?;
        let result = match dynamic_font.load_dynamic_font(temp_path.to_str().unwrap_or_default()) {
            godot::global::Error::OK => Ok(dynamic_font),
            e => Err(e),
        };
        let _ = std::fs::remove_file(&temp_path);
        result
    }
}

const DEFAULT_RECOGNIZED_FONT_EXTENSIONS: &[&str] =
    &["ttf", "ttc", "otf", "otc", "woff", "woff2", "pfb", "pfm"];
impl FakeResourceImporter for FakeResourceImporterDynamicFont {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["font_data_dynamic"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        DEFAULT_RECOGNIZED_FONT_EXTENSIONS.to_vec()
    }

    fn import_file(
        &self,
        path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let mut dynamic_font = Self::get_dynamic_font_from_buffer(path, content)?;

        let antialiasing: i32 = get_or_default(params, "antialiasing", 0);
        let generate_mipmaps: bool = get_or_default(params, "generate_mipmaps", false);
        let disable_embedded_bitmaps: bool =
            get_or_default(params, "disable_embedded_bitmaps", false);
        let msdf: bool = get_or_default(params, "multichannel_signed_distance_field", false);
        let px_range: i32 = get_or_default(params, "msdf_pixel_range", 0);
        let px_size: i32 = get_or_default(params, "msdf_size", 0);
        let ot_ov: VarDictionary = get_or_default(params, "opentype_features", vdict! {});
        let autohinter: bool = get_or_default(params, "force_autohinter", false);
        let modulate_color_glyphs: bool = get_or_default(params, "modulate_color_glyphs", false);
        let allow_system_fallback: bool = get_or_default(params, "allow_system_fallback", false);
        let hinting: i32 = get_or_default(params, "hinting", 0);
        let subpixel_positioning: i32 = get_or_default(params, "subpixel_positioning", 0);
        let keep_rounding_remainders: bool =
            get_or_default(params, "keep_rounding_remainders", false);
        let oversampling: f32 = get_or_default(params, "oversampling", 0.0);
        // TODO: This means that we need to make sure that the config file loads the fonts from the correct backstitch reference, but we don't currently do that;
        // this is unlikely to be an issue, since we're just doing this for the diff, but something to keep in mind.
        let fallbacks: Array<Gd<Font>> = get_or_default(params, "fallbacks", Array::new());

        dynamic_font.set_antialiasing(
            FontAntialiasing::try_from_ord(antialiasing).unwrap_or(FontAntialiasing::NONE),
        );
        dynamic_font.set_disable_embedded_bitmaps(disable_embedded_bitmaps);
        dynamic_font.set_generate_mipmaps(generate_mipmaps);
        dynamic_font.set_multichannel_signed_distance_field(msdf);
        dynamic_font.set_msdf_pixel_range(px_range);
        dynamic_font.set_msdf_size(px_size);
        dynamic_font.set_opentype_feature_overrides(&ot_ov);
        dynamic_font.set_force_autohinter(autohinter);
        dynamic_font.set_modulate_color_glyphs(modulate_color_glyphs);
        dynamic_font.set_allow_system_fallback(allow_system_fallback);
        dynamic_font.set_hinting(Hinting::try_from_ord(hinting).unwrap_or(Hinting::NONE));
        dynamic_font.set_subpixel_positioning(
            SubpixelPositioning::try_from_ord(subpixel_positioning)
                .unwrap_or(SubpixelPositioning::DISABLED),
        );
        dynamic_font.set_keep_rounding_remainders(keep_rounding_remainders);
        dynamic_font.set_oversampling(oversampling);
        dynamic_font.set_fallbacks(&fallbacks);

        Ok(dynamic_font.upcast::<Resource>())
    }
}
impl FakeResourceImporter for FakeResourceImporterImageFont {
    fn recognize(&self, _path: &str, importer_name: Option<&str>) -> bool {
        if let Some(importer_name) = importer_name {
            return self.get_recognized_importers().contains(&importer_name);
        }

        // We want to default to FakeResourceImporterTexture for image types, so we return false here
        false
        // self.get_recognized_extensions().contains(&get_extension(path).as_str())
    }

    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["font_data_image"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        DEFAULT_RECOGNIZED_FONT_EXTENSIONS.to_vec()
    }

    fn import_file(
        &self,
        path: &str,
        _importer_name: &str,
        content: &[u8],
        _params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let image = load_image_from_buffer(path, content, 1.0)?;

        // TODO: there's no way to create a font file from an image unless we reimplement the entire font file importing logic, so we're just going to return the image
        Ok(image.upcast::<Resource>())
    }
}
impl FakeResourceImporter for FakeResourceImporterSVG {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["svg"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["svg"]
    }

    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let contents = GString::try_from_bytes(content, Encoding::Utf8).unwrap_or_default();
        let mut texture = DpiTexture::create_from_string(&contents)
            .ok_or(godot::global::Error::ERR_INVALID_PARAMETER)?;
        let base_scale: f32 = get_or_default(params, "base_scale", 1.0);
        let saturation: f32 = get_or_default(params, "saturation", 1.0);
        let color_map: VarDictionary = get_or_default(params, "color_map", vdict! {});
        let fix_alpha_border: bool = get_or_default(params, "fix_alpha_border", false);
        let premult_alpha: bool = get_or_default(params, "premult_alpha", false);
        // Ignoring, only relevant if we save the resource to a file
        // let compress = params.get("compress").map(|s| s.to::<bool>()).unwrap_or(true);
        texture.set_base_scale(base_scale);
        texture.set_saturation(saturation);
        texture.set_color_map(&color_map);
        // TODO: These aren't bound yet in godot_rust, so just call the methods by using `call`
        texture.call("set_fix_alpha_border", &[fix_alpha_border.to_variant()]);
        texture.call("set_premult_alpha", &[premult_alpha.to_variant()]);
        Ok(texture.upcast::<Resource>())
    }
}

impl FakeResourceImporter for FakeResourceImporterOBJ {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["wavefront_obj"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["obj"]
    }

    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        _content: &[u8],
        _params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        // not implemented, it will be treated as a text file in the diff anyway
        Err(godot::global::Error::ERR_UNAVAILABLE)
    }
}

const DEFAULT_RECOGNIZED_SCENE_EXTENSIONS: &[&str] =
    &["escn", "glb", "gltf", "fbx", "blend", "dae"];
impl FakeResourceImporter for FakeResourceImporterScene {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["scene", "animation_library"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        DEFAULT_RECOGNIZED_SCENE_EXTENSIONS.to_vec()
    }

    fn import_file(
        &self,
        path: &str,
        _importer_name: &str,
        content: &[u8],
        _params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let path = PathBuf::from(path);
        let ext = path
            .extension()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default();
        if ext == "escn" {
            // save it as a temporary file with "tscn"
            let temp_path = get_temp_path(&path, Some("tscn"));
            write_content_to_temp_file(&temp_path, content)?;
            return try_load(&GString::from(temp_path.to_str().unwrap_or_default()))
                .map_err(|_| godot::global::Error::ERR_CANT_ACQUIRE_RESOURCE);
        }

        let (mut gltf_document, mut gltf_state) = match ext {
            "glb" | "gltf" => (GltfDocument::new_gd(), GltfState::new_gd()),
            "fbx" => (
                // Fbx objects are derived from the GLTF objects, so we can use the same methods
                FbxDocument::new_gd().upcast::<GltfDocument>(),
                FbxState::new_gd().upcast::<GltfState>(),
            ),
            _ => {
                // TODO: the importer methods for blender and dae aren't public
                return Err(godot::global::Error::ERR_UNAVAILABLE);
            }
        };
        gltf_state.set_handle_binary_image_mode(HandleBinaryImageMode::EMBED_AS_UNCOMPRESSED);
        gltf_document
            .append_from_buffer(
                &PackedByteArray::from(content),
                &GString::from(path.to_str().unwrap_or_default()),
                &gltf_state,
            )
            .into_result()?;
        let scene_root = gltf_document
            .generate_scene(&gltf_state)
            .ok_or(godot::global::Error::ERR_CANT_CREATE)?;

        let mut packed_scene = PackedScene::new_gd();
        packed_scene.pack(&scene_root).into_result()?;

        return Ok(packed_scene.upcast::<Resource>());
    }
}

impl FakeResourceImporter for FakeResourceImporterTextureAtlas {
    fn get_recognized_importers(&self) -> Vec<&'static str> {
        vec!["texture_atlas"]
    }

    fn get_recognized_extensions(&self) -> Vec<&'static str> {
        vec!["texture_atlas"]
    }

    // TODO: Implement this
    fn import_file(
        &self,
        _path: &str,
        _importer_name: &str,
        _content: &[u8],
        _params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        Err(godot::global::Error::ERR_UNAVAILABLE)
    }
}
pub struct FakeImporter {
    importers: Vec<Box<dyn FakeResourceImporter + Send + Sync>>,
}

impl Default for FakeImporter {
    fn default() -> Self {
        Self {
            importers: vec![
                Box::new(FakeResourceImporterTexture {}),
                Box::new(FakeResourceImporterLayeredTexture {}),
                Box::new(FakeResourceImporterMP3 {}),
                Box::new(FakeResourceImporterOggVorbis {}),
                Box::new(FakeResourceImporterWAV {}),
                Box::new(FakeResourceImporterBMFont {}),
                Box::new(FakeResourceImporterDynamicFont {}),
                Box::new(FakeResourceImporterImageFont {}),
                Box::new(FakeResourceImporterSVG {}),
                Box::new(FakeResourceImporterOBJ {}),
                Box::new(FakeResourceImporterScene {}),
                Box::new(FakeResourceImporterTextureAtlas {}),
            ],
        }
    }
}

impl FakeImporter {
    pub fn recognize(&self, path: &str, importer_name: Option<&str>) -> bool {
        self.importers
            .iter()
            .any(|importer| importer.recognize(path, importer_name))
    }

    pub fn import_file(
        &self,
        path: &str,
        importer_name: Option<&str>,
        content: &[u8],
        params: &VarDictionary,
    ) -> Result<Gd<Resource>, godot::global::Error> {
        let importer = self
            .importers
            .iter()
            .find(|importer| importer.recognize(path, importer_name))
            .ok_or(godot::global::Error::ERR_CANT_CREATE)?;

        let importer_name = match importer_name {
            Some(importer_name) => importer_name,
            None => importer.get_recognized_importers()[0],
        };
        importer.import_file(path, importer_name, content, params)
    }
}
