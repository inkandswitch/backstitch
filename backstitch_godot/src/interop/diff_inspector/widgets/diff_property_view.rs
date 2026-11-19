//! A property diff: the old value (if any) above the new value (if any).
//!
//! TODO: This handles too much logic of how the property is stored in the object, which will
//! likely be relevant if and when we start handling individual property revert.
//! Change this to use PropertyHost and handle it there.

use godot::builtin::GString;
use godot::classes::control::LayoutPreset;
use godot::classes::{
    EditorInspector, EditorProperty, IPanelContainer, Label, MarginContainer, MissingResource,
    Object, PanelContainer, VBoxContainer,
};
use godot::prelude::*;
use godot::register::info::PropertyHint;

use crate::interop::diff_inspector::property_host::set_host_property;
use crate::interop::diff_inspector::widgets::change_marker::DiffColorMarker;
use crate::interop::lazy_load_editor_property::LazyLoadTokenEditorProperty;
use crate::interop::lazy_load_token::LazyLoadToken;
use backstitch::helpers::utils::ChangeType;

pub trait UpdatePropEditor {
    fn update(&mut self);
}

impl UpdatePropEditor for Gd<EditorProperty> {
    fn update(&mut self) {
        self.set_read_only(true);
        self.update_property();
        self.call("_update_editor_property_status", &[]);
    }
}

/// One half (old or new) of a property diff: a label, a color marker, and the editor for the value.
#[derive(GodotClass)]
#[class(tool, base=PanelContainer)]
#[allow(unused)]
pub struct DiffEditorPropertyContainer {
    base: Base<PanelContainer>,
    kind: ChangeType,
    object: Gd<Object>,
    prop_name: String,
    prop_editor: Gd<EditorProperty>,
    label: Gd<Label>,
    color_marker_container: Gd<MarginContainer>,
}

#[godot_api]
impl IPanelContainer for DiffEditorPropertyContainer {
    fn init(base: Base<PanelContainer>) -> Self {
        Self {
            base,
            kind: ChangeType::Modified,
            object: MissingResource::new_gd().upcast::<Object>(),
            prop_name: "".to_string(),
            prop_editor: EditorProperty::new_alloc(),
            label: Label::new_alloc(),
            color_marker_container: Self::create_color_marker_container(ChangeType::Modified),
        }
    }
}

#[godot_api]
impl DiffEditorPropertyContainer {
    /// Swaps the placeholder editor for the real editor once a lazily-loaded resource has loaded.
    #[func]
    fn update_to_real_editor_property(&mut self, resource: Gd<Resource>) {
        let res_variant = resource.to_variant();
        set_host_property(&self.object, &self.prop_name, &res_variant);

        let real_editor_property =
            Self::instance_property_diff(self.object.clone(), &self.prop_name, true);
        if let Some(mut real_editor_property) = real_editor_property {
            real_editor_property.set_anchors_preset(LayoutPreset::FULL_RECT);
            real_editor_property.set_object_and_property(&self.object, &self.prop_name);
            real_editor_property.update();
            let mut old_prop_editor = self.prop_editor.clone();
            self.base_mut().remove_child(&old_prop_editor);
            self.base_mut().add_child(&real_editor_property);
            self.prop_editor = real_editor_property;
            old_prop_editor.queue_free();
        }
    }
}

impl DiffEditorPropertyContainer {
    /// Instantiates the editor that the inspector would use for the property `path` on `object`.
    pub fn instance_property_diff(
        object: Gd<Object>,
        path: &str,
        wide: bool,
    ) -> Option<Gd<EditorProperty>> {
        let list = object.get_property_list();
        for property in list.iter_shared() {
            let name = property.get("name");
            if name.is_some() && name.unwrap().to::<String>() == path {
                let property_type = VariantType::from_ord(property.get("type")?.to::<i64>() as i32);
                let property_hint =
                    PropertyHint::from_ord(property.get("hint")?.to::<i64>() as i32);
                let property_hint_string = property.get("hint_string")?.to::<GString>();
                let property_usage = property.get("usage")?.to::<i64>() as u32;
                return EditorInspector::instantiate_property_editor_ex(
                    &object,
                    property_type,
                    path,
                    property_hint,
                    &property_hint_string,
                    property_usage,
                )
                .wide(wide)
                .done();
            }
        }
        None
    }

    fn get_editor_property(
        object: &Gd<Object>,
        prop_name: &str,
        prop_value: &Variant,
    ) -> Option<Gd<EditorProperty>> {
        let mut editor_property = match prop_value.try_to::<Gd<LazyLoadToken>>() {
            Ok(lazy_load_token) => {
                LazyLoadTokenEditorProperty::create(lazy_load_token).upcast::<EditorProperty>()
            }
            Err(_) => Self::instance_property_diff(object.clone(), prop_name, false)?,
        };
        editor_property.set_object_and_property(object, prop_name);
        editor_property.update();
        Some(editor_property)
    }

    fn create_color_marker_container(kind: ChangeType) -> Gd<MarginContainer> {
        let color_rect = DiffColorMarker::create(kind);
        let mut margin_container = MarginContainer::new_alloc();
        margin_container.call("_set_layout_mode", &[2.to_variant()]);
        margin_container.add_theme_constant_override("margin_right", 20);
        margin_container.add_child(&color_rect);
        margin_container
    }

    pub fn create(
        object: Gd<Object>,
        prop_name: &str,
        prop_value: Variant,
        kind: ChangeType,
        prop_label: &str,
    ) -> Option<Gd<DiffEditorPropertyContainer>> {
        set_host_property(&object, prop_name, &prop_value);

        let Some(editor_property) = Self::get_editor_property(&object, prop_name, &prop_value)
        else {
            tracing::error!(
                "Failed to get editor property for {} of type {}",
                prop_name,
                prop_value.get_type().godot_type_name()
            );
            return None;
        };
        let mut label = Label::new_alloc();
        label.set_text(prop_label);
        let color_rect = Self::create_color_marker_container(kind);
        let mut _self = Gd::from_init_fn(|base: Base<PanelContainer>| Self {
            base,
            kind,
            object,
            prop_name: prop_name.to_string(),
            prop_editor: editor_property.clone(),
            label: label.clone(),
            color_marker_container: color_rect.clone(),
        });
        if let Ok(mut lled) = editor_property
            .clone()
            .try_cast::<LazyLoadTokenEditorProperty>()
        {
            let thing = Callable::from_object_method(&_self, "update_to_real_editor_property");
            lled.connect("resource_loaded", &thing);
        }
        _self.add_child(&label);
        _self.add_child(&color_rect);
        _self.add_child(&editor_property);
        Some(_self)
    }
}

/// A property diff: the old value (if any) above the new value (if any).
#[derive(GodotClass)]
#[class(tool, base=VBoxContainer, init)]
#[allow(unused)]
pub struct DiffPropertyView {
    base: Base<VBoxContainer>,
    #[init(val = ChangeType::Modified)]
    kind: ChangeType,
    prop_name: String,
    old_prop_editor: Option<Gd<DiffEditorPropertyContainer>>,
    new_prop_editor: Option<Gd<DiffEditorPropertyContainer>>,
}

impl DiffPropertyView {
    /// Creates a view of the diff of one property. `label` is the human-readable property name.
    pub fn create(
        kind: ChangeType,
        object: Gd<Object>,
        prop_name: &str,
        old_prop_value: Variant,
        new_prop_value: Variant,
        label: &str,
    ) -> Gd<Self> {
        let old_property_editor = if kind != ChangeType::Created {
            DiffEditorPropertyContainer::create(
                object.clone(),
                &format!("{}_old", prop_name),
                old_prop_value,
                ChangeType::Deleted,
                label,
            )
        } else {
            None
        };
        let new_property_editor = if kind != ChangeType::Deleted {
            DiffEditorPropertyContainer::create(
                object.clone(),
                &format!("{}_new", prop_name),
                new_prop_value,
                ChangeType::Created,
                label,
            )
        } else {
            None
        };

        let mut _self = Gd::from_init_fn(|base: Base<VBoxContainer>| Self {
            base,
            kind,
            prop_name: prop_name.to_string(),
            old_prop_editor: old_property_editor.clone(),
            new_prop_editor: new_property_editor.clone(),
        });
        if let Some(old_prop_editor) = old_property_editor {
            _self.add_child(&old_prop_editor);
        }
        if let Some(new_prop_editor) = new_property_editor {
            _self.add_child(&new_prop_editor);
        }
        _self
    }
}
