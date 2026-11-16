use godot::builtin::{Color, GString, Rect2, Vector2};
use godot::classes::class_macros::private::virtuals::Xrvrs::Side;
use godot::classes::control::{LayoutDirection, SizeFlags};
use godot::classes::notify::{ContainerNotification, ControlNotification};
use godot::classes::text_server::JustificationFlag;
use godot::classes::{
    ColorRect, Container, Control, EditorInspector, EditorProperty, IColorRect, IContainer,
    IPanelContainer, Input, InputEvent, InputEventMouseButton, Label, MarginContainer,
    MissingResource, Object, PanelContainer, StyleBoxFlat, Texture2D, Timer, VBoxContainer,
};
use godot::global::{HorizontalAlignment, MouseButton};
use godot::prelude::*;
use godot::register::info::PropertyHint;

use crate::interop::godot_helpers::ThemeGetter;
use crate::interop::lazy_load_editor_property::LazyLoadTokenEditorProperty;
use crate::interop::lazy_load_token::LazyLoadToken;

fn theme_name_for_change_type(change_type: &str) -> &str {
    match change_type {
        "modified" => "prop_subsection_modified",
        "added" => "prop_subsection_added",
        "removed" => "prop_subsection_removed",
        _ => "prop_subsection_modified",
    }
}

fn snake_case_to_human_readable(snake_case_string: &str) -> String {
    let words = snake_case_string.split("_");
    let title_case_words = words
        .map(|word| {
            if word.is_empty() {
                return String::new();
            }
            word.chars().nth(0).unwrap().to_uppercase().to_string() + &word[1..]
        })
        .collect::<Vec<String>>();
    title_case_words.join(" ")
}

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

#[derive(GodotClass)]
#[class(tool, base=ColorRect, init)]
pub struct DiffColorMarker {
    base: Base<ColorRect>,
    type_name: String, // "modified", "added", "removed", "changed"
}

#[godot_api]
impl IColorRect for DiffColorMarker {
    fn on_notification(&mut self, what: ControlNotification) {
        match what {
            ControlNotification::THEME_CHANGED => {
                let color = self.get_change_color();
                self.base_mut().set_color(color);
            }
            _ => (),
        }
    }
}

#[godot_api]
impl DiffColorMarker {
    fn get_change_color(&self) -> Color {
        self.get_theme_color(theme_name_for_change_type(&self.type_name), "Editor")
    }

    #[func]
    pub fn new(type_name: String) -> Gd<Self> {
        let mut color_rect = Gd::from_init_fn(|base: Base<ColorRect>| Self { base, type_name });
        let change_color = color_rect.bind().get_change_color();
        color_rect.set_color(change_color);
        color_rect.set_custom_minimum_size(Vector2::new(10.0, 10.0));
        color_rect.set_layout_direction(LayoutDirection::LTR);
        color_rect.set_h_size_flags(SizeFlags::SHRINK_CENTER);
        color_rect
    }
}

#[derive(GodotClass)]
#[class(tool, base=PanelContainer)]
#[allow(unused)]
pub struct DiffEditorPropertyContainer {
    base: Base<PanelContainer>,
    type_name: String,
    object: Gd<Object>,
    prop_editor: Gd<EditorProperty>,
    label: Gd<Label>,
    color_marker_container: Gd<MarginContainer>,
}

#[godot_api]
impl IPanelContainer for DiffEditorPropertyContainer {
    fn init(base: Base<PanelContainer>) -> Self {
        Self {
            base,
            type_name: "modified".to_string(),
            object: MissingResource::new_gd().upcast::<Object>(),
            prop_editor: EditorProperty::new_alloc(),
            label: Label::new_alloc(),
            color_marker_container: Self::create_color_marker_container("modified"),
        }
    }
}

#[godot_api]
impl DiffEditorPropertyContainer {
    #[func]
    pub fn instance_property_diff(
        object: Gd<Object>,
        path: String,
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
                    &path,
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
            Err(_) => Self::instance_property_diff(object.clone(), prop_name.to_string(), false)?,
        };
        editor_property.set_object_and_property(object, prop_name);
        editor_property.update();
        Some(editor_property)
    }

    fn set_property(object: &Gd<Object>, prop_name: &str, prop_value: &Variant) {
        let mut object = object.clone();
        if let Some(mut fake_object) = object.clone().try_cast::<MissingResource>().ok() {
            fake_object.set_recording_properties(true);
            fake_object.set(prop_name, &prop_value);
            fake_object.set_recording_properties(false);
        } else {
            object.set(prop_name, prop_value);
        }
    }

    fn create_color_marker_container(change_type: &str) -> Gd<MarginContainer> {
        let color_rect = DiffColorMarker::new(change_type.to_string());
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
        change_type: &str,
        prop_label: &str,
    ) -> Option<Gd<DiffEditorPropertyContainer>> {
        Self::set_property(&object, prop_name, &prop_value);

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
        let color_rect = Self::create_color_marker_container(change_type);
        let mut _self = Gd::from_init_fn(|base: Base<PanelContainer>| Self {
            base,
            type_name: change_type.to_string(),
            object,
            prop_editor: editor_property.clone(),
            label: label.clone(),
            color_marker_container: color_rect.clone(),
        });
        _self.add_child(&label);
        _self.add_child(&color_rect);
        _self.add_child(&editor_property);
        Some(_self)
    }
}

#[derive(GodotClass)]
#[class(tool, base=VBoxContainer, init)]
#[allow(unused)]
pub struct DiffPropertyView {
    base: Base<VBoxContainer>,
    type_name: String, // "modified", "added", "removed", "changed"
    prop_name: String,
    old_prop_editor: Option<Gd<DiffEditorPropertyContainer>>,
    new_prop_editor: Option<Gd<DiffEditorPropertyContainer>>,
}

#[godot_api]
impl DiffPropertyView {
    #[func]
    pub fn create(
        change_type: String,
        object: Gd<Object>,
        prop_name: String,
        old_prop_value: Variant,
        new_prop_value: Variant,
        label: String,
    ) -> Gd<Self> {
        let label = if label.is_empty() {
            snake_case_to_human_readable(&prop_name)
        } else {
            label
        };
        let old_property_editor = if change_type != "added" {
            DiffEditorPropertyContainer::create(
                object.clone(),
                &format!("{}_old", prop_name),
                old_prop_value,
                "removed",
                &label,
            )
        } else {
            None
        };
        let new_property_editor = if change_type != "removed" {
            DiffEditorPropertyContainer::create(
                object.clone(),
                &format!("{}_new", prop_name),
                new_prop_value,
                "added",
                &label,
            )
        } else {
            None
        };

        let mut _self = Gd::from_init_fn(|base: Base<VBoxContainer>| Self {
            base,
            type_name: change_type.to_string(),
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

#[derive(GodotClass)]
#[class(tool, base=Container)]
pub struct DiffInspectorSection {
    #[base]
    base: Base<Container>,

    // Core properties
    label: GString,
    section: GString,
    bg_color: Color,
    foldable: bool,
    indent_depth: i32,
    level: i32,
    type_name: String, // "modified", "added", "removed", "changed"
    unfolded: bool,

    // UI components
    vbox: Gd<VBoxContainer>,
    object: Option<Gd<Object>>,

    // State tracking
    arrow_position: Vector2,
    entered: bool,
    dropping_unfold_timer: Gd<Timer>,
    dropping: bool,
    dropping_for_unfold: bool,
    vbox_added: bool,
}

#[godot_api]
impl DiffInspectorSection {
    #[signal]
    pub fn section_mouse_entered(section: GString);
    #[signal]
    pub fn section_mouse_exited(section: GString);
    #[signal]
    pub fn box_clicked(section: GString);

    #[func]
    fn on_mouse_entered(&mut self) {
        if !self.foldable {
            return;
        }
        let mouse_pos = self.base().get_local_mouse_position();

        let entered = self.foldable && self.get_header_rect().contains_point(mouse_pos);

        if !entered {
            return;
        }

        self.entered = true;
        let section = self.section.clone();

        self.base_mut()
            .emit_signal("section_mouse_entered", &[section.to_variant()]);

        self.base_mut().queue_redraw();
    }

    #[func]
    fn on_mouse_exited(&mut self) {
        if !self.foldable {
            return;
        }

        if !self.entered {
            return;
        }

        self.entered = false;
        let section = self.section.clone();

        self.base_mut()
            .emit_signal("section_mouse_exited", &[section.to_variant()]);

        self.base_mut().queue_redraw();
    }
    #[func]
    pub fn instance_property_diff(
        object: Gd<Object>,
        path: String,
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
                    &path,
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

    #[func]
    #[allow(clippy::too_many_arguments)]
    pub fn setup(
        &mut self,
        p_section: GString,
        p_label: GString,
        p_object: Gd<Object>,
        p_bg_color: Color,
        p_foldable: bool,
        p_indent_depth: i32,
        p_level: i32,
    ) {
        self.section = p_section;
        self.label = p_label;
        self.object = Some(p_object);
        self.bg_color = p_bg_color;
        self.foldable = p_foldable;
        self.indent_depth = p_indent_depth;
        self.level = p_level;

        if !self.foldable && !self.vbox_added {
            let vbox = self.vbox.clone();
            self.base_mut().add_child(&vbox);
            self.base_mut().move_child(&vbox, 0);
            self.vbox_added = true;
        }

        if self.foldable {
            self.test_unfold();
            if self.unfolded {
                self.vbox.show();
            } else {
                self.vbox.hide();
            }
        }
    }

    #[func]
    pub fn get_vbox(&self) -> Gd<VBoxContainer> {
        self.vbox.clone()
    }

    #[func]
    pub fn unfold(&mut self) {
        if !self.foldable {
            return;
        }

        self.test_unfold();

        self.unfolded = true;
        self.vbox.show();
        self.base_mut().queue_redraw();
    }

    #[func]
    pub fn fold(&mut self) {
        if !self.foldable {
            return;
        }

        if !self.vbox_added {
            return;
        }

        self.unfolded = false;
        self.vbox.hide();
        self.base_mut().queue_redraw();
    }

    #[func]
    pub fn set_type(&mut self, p_type: GString) {
        self.type_name = p_type.to_string();
        self.update_bg_color();
    }

    #[func]
    pub fn get_type(&self) -> GString {
        GString::from(&self.type_name)
    }

    #[func]
    pub fn set_bg_color(&mut self, p_bg_color: Color) {
        self.bg_color = p_bg_color;
        self.base_mut().queue_redraw();
    }

    #[func]
    pub fn get_bg_color(&self) -> Color {
        self.bg_color
    }

    #[func]
    pub fn set_label(&mut self, p_label: GString) {
        self.label = p_label;
        self.base_mut().queue_redraw();
    }

    #[func]
    pub fn get_label(&self) -> GString {
        self.label.clone()
    }

    #[func]
    pub fn get_object(&self) -> Option<Gd<Object>> {
        self.object.clone()
    }

    #[func]
    pub fn is_folded(&self) -> bool {
        !self.unfolded
    }

    #[func]
    pub fn get_section(&self) -> GString {
        self.section.clone()
    }

    // This is currently only used in process to handle input, to mouse over the
    // header. It's the same code from draw() that computes the header rect.
    // Ideally we'd factor out the dimensions.
    fn get_header_rect(&self) -> Rect2 {
        let section_indent_size = self.get_theme_constant("indent_size", "DiffInspectorSection");
        let mut section_indent = 0;

        if self.indent_depth > 0 && section_indent_size > 0 {
            section_indent = self.indent_depth * section_indent_size;
        }

        let section_indent_style = self.get_theme_stylebox("indent_box", "DiffInspectorSection");
        if self.indent_depth > 0
            && let Some(ref style) = section_indent_style
            && let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>()
        {
            section_indent +=
                (style_flat.get_margin(Side::LEFT) + style_flat.get_margin(Side::RIGHT)) as i32; // LEFT + RIGHT
        }

        let header_width = self.base().get_size().x - section_indent as f32;
        let mut header_offset_x = 0.0;
        let rtl = self.base().is_layout_rtl();
        if !rtl {
            header_offset_x += section_indent as f32;
        }

        let header_height = self.get_header_height();
        Rect2::new(
            Vector2::new(header_offset_x, 0.0),
            Vector2::new(header_width, header_height),
        )
    }

    // Private helper methods
    fn test_unfold(&mut self) {
        if !self.vbox_added {
            let vbox = self.vbox.clone();
            self.base_mut().add_child(&vbox);
            self.base_mut().move_child(&vbox, 0);
            self.vbox_added = true;
        }
    }

    fn get_header_height(&self) -> f32 {
        let font = self.get_theme_font("bold", "EditorFonts");
        let font_size = self.get_theme_font_size("bold_size", "EditorFonts");

        let mut header_height = if let Some(ref font) = font {
            font.get_height_ex().font_size(font_size).done()
        } else {
            0.0
        };

        if let Some(arrow) = self.get_arrow() {
            header_height = header_height.max(arrow.get_height() as f32);
        }

        let v_separation = self.get_theme_constant("v_separation", "Tree");
        header_height += v_separation as f32;

        header_height
    }

    fn get_arrow(&self) -> Option<Gd<Texture2D>> {
        if !self.foldable {
            return None;
        }

        if self.unfolded {
            self.get_theme_icon("arrow", "Tree")
        } else {
            let rtl = self.base().is_layout_rtl();
            if rtl {
                self.get_theme_icon("arrow_collapsed_mirrored", "Tree")
            } else {
                self.get_theme_icon("arrow_collapsed", "Tree")
            }
        }
    }

    fn update_bg_color(&mut self) {
        self.bg_color = self.get_theme_color(theme_name_for_change_type(&self.type_name), "Editor");
        self.base_mut().queue_redraw();
    }

    fn add_timer(&mut self) {
        let callable = Callable::from_object_method(&self.to_gd(), "unfold");

        // Add timer as child (matching C++ implementation)
        let timer = self.dropping_unfold_timer.clone();
        self.base_mut().add_child(&timer);

        // Connect timer timeout to unfold
        self.dropping_unfold_timer.connect("timeout", &callable);
    }

    fn accept_and_emit_box_clicked(&mut self) {
        self.base_mut().accept_event();
        let section = self.section.clone();
        self.signals().box_clicked().emit(&section);
    }

    fn as_sortable_control(node: Option<Gd<Node>>) -> Option<Gd<Control>> {
        if let Some(Ok(control)) = node.map(|n| n.try_cast::<Control>())
            && !control.is_set_as_top_level()
            && control.is_visible_in_tree()
        {
            return Some(control);
        }
        None
    }

    #[func]
    fn add_old_and_new(
        &mut self,
        change_type: String,
        prop_name: String,
        old_prop_value: Variant,
        new_prop_value: Variant,
        label: String,
    ) {
        let diff_property_view = DiffPropertyView::create(
            change_type,
            self.get_object().unwrap(),
            prop_name,
            old_prop_value,
            new_prop_value,
            label,
        );
        self.vbox.add_child(&diff_property_view);
    }

    #[func]
    fn add_resource_diff(
        &mut self,
        change_type: String,
        file_path: String,
        old_resource: Variant,
        new_resource: Variant,
    ) {
        if old_resource.try_to::<Gd<Object>>().is_err()
            && new_resource.try_to::<Gd<Object>>().is_err()
        {
            return;
        }
        let prop_label = snake_case_to_human_readable(&file_path);
        let mut fake_node: Gd<MissingResource> = MissingResource::new_gd();
        fake_node.set_original_class("Resource");
        self.add_old_and_new(
            change_type,
            "Resource".to_string(),
            old_resource,
            new_resource,
            prop_label,
        );
    }
}

#[godot_api]
impl IContainer for DiffInspectorSection {
    fn ready(&mut self) {
        let mut base = self.base_mut();
        let entered = base.callable("on_mouse_entered");
        let exited = base.callable("on_mouse_exited");
        base.connect("mouse_entered", &entered);
        base.connect("mouse_exited", &exited);
    }

    fn init(base: Base<Container>) -> Self {
        let vbox = VBoxContainer::new_alloc();
        let mut dropping_unfold_timer = Timer::new_alloc();
        dropping_unfold_timer.set_wait_time(0.6);
        dropping_unfold_timer.set_one_shot(true);

        Self {
            base,
            label: GString::new(),
            section: GString::new(),
            bg_color: Color::default(),
            foldable: false,
            indent_depth: 0,
            level: 1,
            type_name: String::from("changed"),
            unfolded: true,
            vbox,
            object: None,
            arrow_position: Vector2::ZERO,
            entered: false,
            dropping_unfold_timer,
            dropping: false,
            dropping_for_unfold: false,
            vbox_added: false,
        }
    }

    fn enter_tree(&mut self) {
        self.add_timer();
    }

    // Solely here for the lazy load property editor to work
    fn process(&mut self, _delta: f64) {}

    fn get_minimum_size(&self) -> Vector2 {
        let mut ms = Vector2::ZERO;
        let child_count = self.base().get_child_count();
        for i in 0..child_count {
            if let Some(child) = Self::as_sortable_control(self.base().get_child(i)) {
                let minsize = child.get_combined_minimum_size();
                ms.x = ms.x.max(minsize.x);
                ms.y = ms.y.max(minsize.y);
            }
        }

        let font = self.get_theme_font("font", "Tree");
        let font_size = self.get_theme_font_size("font_size", "Tree");
        if let Some(ref font) = font {
            ms.y += font.get_height_ex().font_size(font_size).done()
                + self.get_theme_constant("v_separation", "Tree") as f32;
        }

        ms.x += self.get_theme_constant("inspector_margin", "Editor") as f32;

        let section_indent_size = self.get_theme_constant("indent_size", "DiffInspectorSection");
        if self.indent_depth > 0 && section_indent_size > 0 {
            ms.x += (self.indent_depth * section_indent_size) as f32;
        }

        let section_indent_style = self.get_theme_stylebox("indent_box", "DiffInspectorSection");
        if self.indent_depth > 0
            && let Some(ref style) = section_indent_style
            && let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>()
        {
            ms.x += style_flat.get_margin(Side::LEFT) + style_flat.get_margin(Side::RIGHT);
        }

        ms
    }

    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if let Ok(mb) = event.clone().try_cast::<InputEventMouseButton>() {
            if mb.is_pressed() && mb.get_button_index() == MouseButton::LEFT {
                // MouseButton::LEFT
                // Check the position of the arrow texture
                if self.foldable
                    && let Some(arrow) = self.get_arrow()
                {
                    const FUDGE_FACTOR: f32 = 10.0;
                    let bounding_width =
                        arrow.get_width() as f32 + self.arrow_position.x + FUDGE_FACTOR;
                    let bounding_height = self.base().get_size().y;
                    let bounding_box =
                        Rect2::new(Vector2::ZERO, Vector2::new(bounding_width, bounding_height));

                    if bounding_box.contains_point(mb.get_position()) {
                        if self.unfolded {
                            let header_height = self.get_header_height();
                            if mb.get_position().y >= header_height {
                                return;
                            }
                        }

                        self.base_mut().accept_event();

                        let should_unfold = !self.unfolded;
                        if should_unfold {
                            self.unfold();
                        } else {
                            self.fold();
                        }
                    } else {
                        self.accept_and_emit_box_clicked();
                    }
                } else {
                    self.accept_and_emit_box_clicked();
                }
            } else if !mb.is_pressed() {
                self.base_mut().queue_redraw();
            }
        }
    }

    fn on_notification(&mut self, what: ContainerNotification) {
        match what {
            // NOTIFICATION_THEME_CHANGED
            ContainerNotification::THEME_CHANGED => {
                self.base_mut().update_minimum_size();
                self.update_bg_color();
                self.bg_color.a /= self.level as f32;
            }
            // NOTIFICATION_SORT_CHILDREN
            ContainerNotification::SORT_CHILDREN => {
                if !self.vbox_added {
                    return;
                }

                let inspector_margin = self.get_theme_constant("inspector_margin", "Editor");
                let mut inspector_margin_val = inspector_margin as f32;

                let section_indent_size =
                    self.get_theme_constant("indent_size", "DiffInspectorSection");
                if self.indent_depth > 0 && section_indent_size > 0 {
                    inspector_margin_val += (self.indent_depth * section_indent_size) as f32;
                }

                let section_indent_style =
                    self.get_theme_stylebox("indent_box", "DiffInspectorSection");
                if self.indent_depth > 0
                    && let Some(ref style) = section_indent_style
                    && let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>()
                {
                    inspector_margin_val +=
                        style_flat.get_margin(Side::LEFT) + style_flat.get_margin(Side::RIGHT); // LEFT + RIGHT
                }

                let size = self.base().get_size() - Vector2::new(inspector_margin_val, 0.0);
                let header_height = self.get_header_height();
                let offset = Vector2::new(
                    if self.base().is_layout_rtl() {
                        0.0
                    } else {
                        inspector_margin_val
                    },
                    header_height,
                );

                let child_count = self.base().get_child_count();
                for i in 0..child_count {
                    if let Some(child) = Self::as_sortable_control(self.base().get_child(i)) {
                        self.base_mut()
                            .fit_child_in_rect(&child, Rect2::new(offset, size));
                    }
                }
            }
            // NOTIFICATION_DRAW
            ContainerNotification::DRAW => {
                self.draw();
            }
            // NOTIFICATION_DRAG_BEGIN
            ContainerNotification::DRAG_BEGIN => {
                self.dropping_for_unfold = true;
            }
            // NOTIFICATION_DRAG_END
            ContainerNotification::DRAG_END => {
                self.dropping_for_unfold = false;
            }
            // NOTIFICATION_MOUSE_ENTER
            ContainerNotification::MOUSE_ENTER => {
                if self.dropping || self.dropping_for_unfold {
                    self.dropping_unfold_timer.start();
                }
                self.base_mut().queue_redraw();
            }
            // NOTIFICATION_MOUSE_EXIT
            ContainerNotification::MOUSE_EXIT => {
                if self.dropping || self.dropping_for_unfold {
                    self.dropping_unfold_timer.stop();
                }
                self.base_mut().queue_redraw();
            }
            _ => {}
        }
    }
}

impl DiffInspectorSection {
    fn draw(&mut self) {
        let section_indent_size = self.get_theme_constant("indent_size", "DiffInspectorSection");
        let mut section_indent = 0;

        if self.indent_depth > 0 && section_indent_size > 0 {
            section_indent = self.indent_depth * section_indent_size;
        }

        let section_indent_style = self.get_theme_stylebox("indent_box", "DiffInspectorSection");
        if self.indent_depth > 0
            && let Some(ref style) = section_indent_style
            && let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>()
        {
            section_indent +=
                (style_flat.get_margin(Side::LEFT) + style_flat.get_margin(Side::RIGHT)) as i32; // LEFT + RIGHT
        }

        let header_width = self.base().get_size().x - section_indent as f32;
        let mut header_offset_x = 0.0;
        let rtl = self.base().is_layout_rtl();
        if !rtl {
            header_offset_x += section_indent as f32;
        }

        let header_height = self.get_header_height();
        let header_rect = Rect2::new(
            Vector2::new(header_offset_x, 0.0),
            Vector2::new(header_width, header_height),
        );

        // Draw header background
        let mut c = self.bg_color;
        c.a *= 0.4;

        if self.entered {
            if Input::singleton().is_mouse_button_pressed(MouseButton::LEFT) {
                // MouseButton::LEFT
                c = c.lightened(-0.05);
            } else {
                c = c.lightened(0.2);
            }
        }

        self.base_mut().draw_rect(header_rect, c);

        // Draw header content (arrow, label, revertable count)
        let outer_margin = (2.0 * self.base().get_theme_default_base_scale()).round();
        let separation = self.get_theme_constant("h_separation", "DiffInspectorSection");
        let separation_val = separation as f32;

        let mut margin_start = section_indent as f32 + outer_margin;
        let margin_end = outer_margin;

        // Draw arrow
        if let Some(arrow) = self.get_arrow() {
            if rtl {
                self.arrow_position.x =
                    self.base().get_size().x - (margin_start + arrow.get_width() as f32);
            } else {
                self.arrow_position.x = margin_start;
            }
            self.arrow_position.y = (header_height - arrow.get_height() as f32) / 2.0;
            let arrow_position = self.arrow_position;
            self.base_mut()
                .draw_texture_ex(&arrow, arrow_position)
                .done();
            margin_start += arrow.get_width() as f32 + separation_val;
        }

        let available = self.base().get_size().x - (margin_start + margin_end);

        // TODO: Currently not able to use this due to the way that we construct the child controls.
        // Draw count (if folded)
        // let folded = self.foldable && !self.unfolded;
        // let child_count = self.base().get_child_count() - 1; // -1 for the vertical seperator
        // if folded && child_count > 0 {
        //     let font = self.get_theme_font("bold", "EditorFonts");
        //     let font_size = self.get_theme_font_size("bold_size", "EditorFonts");

        //     if let Some(ref font) = font {
        //         // Use KASHIDA and CONSTRAIN_ELLIPSIS for label width calculation (matching C++)
        //         let label_width = font
        //             .get_string_size_ex(&self.label)
        //             .alignment(HorizontalAlignment::LEFT)
        //             .width(available)
        //             .font_size(font_size)
        //             .justification_flags(
        //                 JustificationFlag::KASHIDA | JustificationFlag::CONSTRAIN_ELLIPSIS,
        //             )
        //             .done()
        //             .x;

        //         let light_font = self.get_theme_font("main", "EditorFonts");
        //         let light_font_size = self.get_theme_font_size("main_size", "EditorFonts");
        //         let light_font_color = self.get_theme_color("font_disabled_color", "Editor");

        //         if let Some(ref light_font) = light_font {
        //             let count = child_count;
        //             let num_revertable_str = if count == 1 {
        //                 format!("({} change)", count)
        //             } else {
        //                 format!("({} changes)", count)
        //             };
        //             let mut num_revertable_width = light_font
        //                 .get_string_size_ex(&GString::from(&num_revertable_str))
        //                 .alignment(HorizontalAlignment::LEFT)
        //                 .width(-1.0)
        //                 .font_size(light_font_size)
        //                 .done()
        //                 .x;

        //             if label_width + outer_margin + num_revertable_width > available {
        //                 let short_str = format!("({})", count);
        //                 num_revertable_width = light_font
        //                     .get_string_size_ex(&GString::from(&short_str))
        //                     .alignment(HorizontalAlignment::LEFT)
        //                     .width(-1.0)
        //                     .font_size(light_font_size)
        //                     .done()
        //                     .x;

        //                 let text_offset_y =
        //                     light_font.get_ascent_ex().font_size(light_font_size).done()
        //                         + (header_height
        //                             - light_font.get_height_ex().font_size(light_font_size).done())
        //                             / 2.0;
        //                 let mut text_offset = Vector2::new(margin_end, text_offset_y).round();
        //                 if !rtl {
        //                     text_offset.x =
        //                         self.base().get_size().x - (text_offset.x + num_revertable_width);
        //                 }
        //                 self.base_mut()
        //                     .draw_string_ex(light_font, text_offset, &GString::from(&short_str))
        //                     .modulate(light_font_color)
        //                     .alignment(HorizontalAlignment::LEFT)
        //                     .width(-1.0)
        //                     .font_size(light_font_size)
        //                     .justification_flags(JustificationFlag::NONE)
        //                     .done();
        //                 margin_end += (num_revertable_width + outer_margin) as f32;
        //             } else {
        //                 let text_offset_y =
        //                     light_font.get_ascent_ex().font_size(light_font_size).done()
        //                         + (header_height
        //                             - light_font.get_height_ex().font_size(light_font_size).done())
        //                             / 2.0;
        //                 let mut text_offset = Vector2::new(margin_end, text_offset_y).round();
        //                 if !rtl {
        //                     text_offset.x =
        //                         self.base().get_size().x - (text_offset.x + num_revertable_width);
        //                 }
        //                 self.base_mut()
        //                     .draw_string_ex(
        //                         light_font,
        //                         text_offset,
        //                         &GString::from(&num_revertable_str),
        //                     )
        //                     .modulate(light_font_color)
        //                     .alignment(HorizontalAlignment::LEFT)
        //                     .width(-1.0)
        //                     .font_size(light_font_size)
        //                     .justification_flags(JustificationFlag::NONE)
        //                     .done();
        //                 margin_end += (num_revertable_width + outer_margin) as f32;
        //             }
        //             // Update available width (matching C++ line 231)
        //             available -= num_revertable_width + outer_margin;
        //         }
        //     }
        // }

        // Draw label
        let font = self.get_theme_font("bold", "EditorFonts");
        let font_size = self.get_theme_font_size("bold_size", "EditorFonts");
        let font_color = self.get_theme_color("font_color", "Editor");

        if let Some(ref font) = font {
            let text_offset_y = font.get_ascent_ex().font_size(font_size).done()
                + (header_height - font.get_height_ex().font_size(font_size).done()) / 2.0;
            let mut text_offset = Vector2::new(margin_start, text_offset_y).round();
            if rtl {
                text_offset.x = margin_end;
            }
            let text_align = if rtl {
                HorizontalAlignment::RIGHT
            } else {
                HorizontalAlignment::LEFT
            };
            let label = self.label.clone();
            // Use KASHIDA and CONSTRAIN_ELLIPSIS for label (matching C++ line 241)
            self.base_mut()
                .draw_string_ex(font, text_offset, &label)
                .modulate(font_color)
                .alignment(text_align)
                .width(available)
                .font_size(font_size)
                .justification_flags(
                    JustificationFlag::KASHIDA | JustificationFlag::CONSTRAIN_ELLIPSIS,
                )
                .done();
        }

        // Draw dropping highlight
        if self.dropping && !self.vbox.is_visible_in_tree() {
            let accent_color = self.get_theme_color("accent_color", "Editor");
            let size = self.base().get_size();
            self.base_mut()
                .draw_rect_ex(Rect2::new(Vector2::ZERO, size), accent_color)
                .filled(false)
                .done();
        }

        // Draw section indentation
        if let Some(ref section_indent_style) = section_indent_style
            && section_indent > 0
        {
            let indent_rect = Rect2::new(
                Vector2::ZERO,
                Vector2::new(
                    self.indent_depth as f32 * section_indent_size as f32,
                    self.base().get_size().y,
                ),
            );
            let mut indent_pos = indent_rect.position;
            if let Ok(style_flat) = section_indent_style.clone().try_cast::<StyleBoxFlat>() {
                if rtl {
                    indent_pos.x = self.base().get_size().x
                        - (section_indent as f32 + style_flat.get_margin(Side::RIGHT)); // RIGHT
                } else {
                    indent_pos.x = style_flat.get_margin(Side::LEFT); // LEFT
                }
            }
            let final_rect = Rect2::new(indent_pos, indent_rect.size);
            self.base_mut()
                .draw_style_box(section_indent_style, final_rect);
        }
    }
}
