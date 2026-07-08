use godot::{
    builtin::{Color, Vector2},
    classes::{
        AspectRatioContainer, EditorProperty, IEditorProperty, Material, MissingResource, Node,
        Object, Panel, Resource, StyleBoxFlat, aspect_ratio_container::StretchMode,
        control::LayoutPreset,
    },
    meta::ToGodot,
    obj::{Base, Gd, NewAlloc, NewGd, OnReady, WithBaseField},
    register::{GodotClass, godot_api},
};

use crate::interop::{
    diff_inspector_section::DiffInspectorSection, lazy_load_token::LazyLoadToken,
};

#[derive(GodotClass)]
#[class(tool, base=EditorProperty)]
pub struct LazyLoadTokenEditorProperty {
    #[base]
    base: Base<EditorProperty>,
    loading_rect: Option<Gd<Node>>,
    material: OnReady<Gd<Material>>,
    // Core properties
    token: Option<Gd<LazyLoadToken>>,
    resource: Option<Gd<Resource>>,
}

#[godot_api]
impl LazyLoadTokenEditorProperty {
    fn create_instance(base: Base<EditorProperty>, token: Option<Gd<LazyLoadToken>>) -> Self {
        Self {
            base,
            loading_rect: None,
            material: OnReady::from_loaded(
                // TODO: figure out how to create this ourselves once statically
                "res://addons/backstitch/public/gdscript/loading_circle.tres",
            ),
            token,
            resource: None,
        }
    }

    #[func]
    pub fn create(token: Gd<LazyLoadToken>) -> Gd<Self> {
        Gd::from_init_fn(|base| Self::create_instance(base, Some(token)))
    }

    fn create_loading_rect(&self) -> Gd<Node> {
        let mut aspect_ratio_container = AspectRatioContainer::new_alloc();
        aspect_ratio_container.set_stretch_mode(StretchMode::FIT);
        aspect_ratio_container.set_ratio(1.0);
        let mut panel = Panel::new_alloc();
        panel.set_material(&self.material.clone());
        panel.set_custom_minimum_size(Vector2::new(64.0, 64.0));
        let mut stylebox = StyleBoxFlat::new_gd();
        stylebox.set_bg_color(Color::from_rgba(1.0, 1.0, 1.0, 0.0));
        panel.add_theme_stylebox_override("panel", &stylebox);

        aspect_ratio_container.add_child(&panel);
        aspect_ratio_container.upcast::<Node>()
    }

    fn update_to_real_editor_property(&mut self, resource: Gd<Resource>) {
        let res_variant = resource.to_variant();
        self.resource = Some(resource);
        let prop_path = self.base().get_edited_property();
        let mut our_object = self
            .base()
            .get_edited_object()
            .unwrap_or(MissingResource::new_gd().upcast::<Object>());
        if let Ok(mut missing_resource) = our_object.clone().try_cast::<MissingResource>() {
            missing_resource.set_recording_properties(true);
            missing_resource.set(&prop_path, &res_variant);
            missing_resource.set_recording_properties(false);
        } else {
            our_object.set(&prop_path, &res_variant);
        }
        let real_editor_property = DiffInspectorSection::instance_property_diff(
            our_object.clone(),
            prop_path.to_string(),
            true,
        );
        if let Some(mut real_editor_property) = real_editor_property {
            real_editor_property.set_anchors_preset(LayoutPreset::FULL_RECT);
            real_editor_property.set_object_and_property(&our_object, &prop_path);
            DiffInspectorSection::update_property_editor(&mut real_editor_property);
            if let Some(mut loading_rect) = self.loading_rect.take() {
                self.base_mut().remove_child(&loading_rect);
                loading_rect.queue_free();
            }
            let parent = self.base_mut().get_parent();
            if let Some(mut parent) = parent {
                parent.add_child(&real_editor_property);
            }
            self.base_mut().hide();
        }
    }
}

#[godot_api]
impl IEditorProperty for LazyLoadTokenEditorProperty {
    fn init(base: Base<EditorProperty>) -> Self {
        Self::create_instance(base, None)
    }

    fn update_property(&mut self) {}

    fn set_read_only(&mut self, _read_only: bool) {}

    fn ready(&mut self) {
        let loading_rect = self.create_loading_rect();
        self.base_mut().add_child(&loading_rect);
        self.loading_rect = Some(loading_rect);
    }

    fn process(&mut self, _delta: f64) {
        if self.token.is_some() {
            if !self.token.as_ref().unwrap().bind().is_started() {
                self.token.as_mut().unwrap().bind_mut().start_load();
            }
            if self.token.as_ref().unwrap().bind().is_load_finished() {
                // NOTE: we need to keep a reference to the token until we've finished updating the property editor,
                // or it'll be freed while there are still dangling pointers to it
                let mut token = self.token.take().unwrap();
                let resource = token.bind_mut().get_resource();
                if let Some(resource) = resource {
                    self.update_to_real_editor_property(resource);
                }
            }
        }
    }
}
