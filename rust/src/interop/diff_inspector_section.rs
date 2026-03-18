use godot::builtin::{Color, GString, Rect2, StringName, Variant, Vector2};
use godot::classes::notify::ContainerNotification;
use godot::classes::text_server::JustificationFlag;
use godot::classes::{
    Container, Control, EditorInspector, EditorProperty, IContainer, Input, InputEvent, InputEventMouseButton, Object, StyleBoxFlat, Texture2D, Timer, VBoxContainer
};
use godot::global::{HorizontalAlignment, MouseButton, PropertyHint};
use godot::prelude::*;

#[derive(GodotClass)]
#[class(base=Container)]
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

        let entered = self.foldable && self.get_header_rect()
            .contains_point(mouse_pos);

        if !entered {
            return;
        }

        self.entered = true;
        let section = self.section.clone();

        self.base_mut().emit_signal(
            "section_mouse_entered",
            &[section.to_variant()],
        );

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

        self.base_mut().emit_signal(
            "section_mouse_exited",
            &[section.to_variant()],
        );

        self.base_mut().queue_redraw();
    }
	#[func]
	pub fn instance_property_diff(object: Gd<Object>, path: String, wide: bool) -> Option<Gd<EditorProperty>> {

		let list = object.get_property_list();
		for property in list.iter_shared() {
			let name = property.get("name");
			if name.is_some() && name.unwrap().to::<String>() == path {
				let property_type = VariantType::from_ord(property.get("type")?.to::<i64>() as i32);
				let property_hint= PropertyHint::from_ord(property.get("hint")?.to::<i64>() as i32);
				let property_hint_string = property.get("hint_string")?.to::<GString>();
				let property_usage = property.get("usage")?.to::<i64>() as u32;
				return EditorInspector::instantiate_property_editor_ex(
					&object,
					property_type,
					&path,
					property_hint,
					&property_hint_string,
					property_usage)
					.wide(wide).done();
			}
		}
		None
	}


    #[func]
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
        let section_indent_size = self
            .base()
            .get_theme_constant_ex(&StringName::from("indent_size"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        let mut section_indent = 0;

        if self.indent_depth > 0 && section_indent_size > 0 {
            section_indent = self.indent_depth * section_indent_size;
        }

        let section_indent_style = self
            .base()
            .get_theme_stylebox_ex(&StringName::from("indent_box"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        if self.indent_depth > 0 {
            if let Some(ref style) = section_indent_style {
                if let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>() {
                    section_indent += (style_flat.get_margin(Side::LEFT)
                        + style_flat.get_margin(Side::RIGHT))
                        as i32; // LEFT + RIGHT
                }
            }
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
        let font = self
            .base()
            .get_theme_font_ex(&StringName::from("bold"))
            .theme_type(&StringName::from("EditorFonts"))
            .done();
        let font_size = self
            .base()
            .get_theme_font_size_ex(&StringName::from("bold_size"))
            .theme_type(&StringName::from("EditorFonts"))
            .done();

        let mut header_height = if let Some(ref font) = font {
            font.get_height_ex().font_size(font_size).done()
        } else {
            0.0
        };

        if let Some(arrow) = self.get_arrow() {
            header_height = header_height.max(arrow.get_height() as f32);
        }

        let v_separation = self
            .base()
            .get_theme_constant_ex(&StringName::from("v_separation"))
            .theme_type(&StringName::from("Tree"))
            .done();
        header_height += v_separation as f32;

        header_height
    }

    fn get_arrow(&self) -> Option<Gd<Texture2D>> {
        if !self.foldable {
            return None;
        }

        if self.unfolded {
            self.base()
                .get_theme_icon_ex(&StringName::from("arrow"))
                .theme_type(&StringName::from("Tree"))
                .done()
        } else {
            let rtl = self.base().is_layout_rtl();
            if rtl {
                self.base()
                    .get_theme_icon_ex(&StringName::from("arrow_collapsed_mirrored"))
                    .theme_type(&StringName::from("Tree"))
                    .done()
            } else {
                self.base()
                    .get_theme_icon_ex(&StringName::from("arrow_collapsed"))
                    .theme_type(&StringName::from("Tree"))
                    .done()
            }
        }
    }

    fn update_bg_color(&mut self) {
        let color_name = if self.type_name == "modified" {
            "prop_subsection_modified"
        } else if self.type_name == "added" {
            "prop_subsection_added"
        } else if self.type_name == "removed" {
            "prop_subsection_removed"
        } else {
            "prop_subsection"
        };

        self.bg_color = self
            .base()
            .get_theme_color_ex(color_name)
            .theme_type(&StringName::from("Editor"))
            .done();
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
        if let Some(Ok(control)) = node.map(|n| n.try_cast::<Control>()) {
            if !control.is_set_as_top_level() && control.is_visible_in_tree() {
                return Some(control);
            }
        }
        None
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
            vbox: vbox,
            object: None,
            arrow_position: Vector2::ZERO,
            entered: false,
            dropping_unfold_timer: dropping_unfold_timer,
            dropping: false,
            dropping_for_unfold: false,
            vbox_added: false,
        }
    }

    fn enter_tree(&mut self) {
        self.add_timer();
    }

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

        let font = self
            .base()
            .get_theme_font_ex(&StringName::from("font"))
            .theme_type(&StringName::from("Tree"))
            .done();
        let font_size = self
            .base()
            .get_theme_font_size_ex(&StringName::from("font_size"))
            .theme_type(&StringName::from("Tree"))
            .done();
        if let Some(ref font) = font {
            ms.y += font.get_height_ex().font_size(font_size).done()
                + self
                    .base()
                    .get_theme_constant_ex(&StringName::from("v_separation"))
                    .theme_type(&StringName::from("Tree"))
                    .done() as f32;
        }

        ms.x += self
            .base()
            .get_theme_constant_ex(&StringName::from("inspector_margin"))
            .theme_type(&StringName::from("Editor"))
            .done() as f32;

        let section_indent_size = self
            .base()
            .get_theme_constant_ex(&StringName::from("indent_size"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        if self.indent_depth > 0 && section_indent_size > 0 {
            ms.x += (self.indent_depth * section_indent_size) as f32;
        }

        let section_indent_style = self
            .base()
            .get_theme_stylebox_ex(&StringName::from("indent_box"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        if self.indent_depth > 0 {
            if let Some(ref style) = section_indent_style {
                if let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>() {
                    ms.x += (style_flat.get_margin(Side::LEFT) + style_flat.get_margin(Side::RIGHT))
                        as f32;
                }
            }
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
                        return;
                    } else {
                        self.accept_and_emit_box_clicked();
                        return;
                    }
                } else {
                    self.accept_and_emit_box_clicked();
                    return;
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

                let inspector_margin = self
                    .base()
                    .get_theme_constant_ex(&StringName::from("inspector_margin"))
                    .theme_type(&StringName::from("Editor"))
                    .done();
                let mut inspector_margin_val = inspector_margin as f32;

                let section_indent_size = self
                    .base()
                    .get_theme_constant_ex(&StringName::from("indent_size"))
                    .theme_type(&StringName::from("DiffInspectorSection"))
                    .done();
                if self.indent_depth > 0 && section_indent_size > 0 {
                    inspector_margin_val += (self.indent_depth * section_indent_size) as f32;
                }

                let section_indent_style = self
                    .base()
                    .get_theme_stylebox_ex(&StringName::from("indent_box"))
                    .theme_type(&StringName::from("DiffInspectorSection"))
                    .done();
                if self.indent_depth > 0 {
                    if let Some(ref style) = section_indent_style {
                        if let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>() {
                            inspector_margin_val += (style_flat.get_margin(Side::LEFT)
                                + style_flat.get_margin(Side::RIGHT))
                                as f32; // LEFT + RIGHT
                        }
                    }
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
        let section_indent_size = self
            .base()
            .get_theme_constant_ex(&StringName::from("indent_size"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        let mut section_indent = 0;

        if self.indent_depth > 0 && section_indent_size > 0 {
            section_indent = self.indent_depth * section_indent_size;
        }

        let section_indent_style = self
            .base()
            .get_theme_stylebox_ex(&StringName::from("indent_box"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        if self.indent_depth > 0 {
            if let Some(ref style) = section_indent_style {
                if let Ok(style_flat) = style.clone().try_cast::<StyleBoxFlat>() {
                    section_indent += (style_flat.get_margin(Side::LEFT)
                        + style_flat.get_margin(Side::RIGHT))
                        as i32; // LEFT + RIGHT
                }
            }
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
        let separation = self
            .base()
            .get_theme_constant_ex(&StringName::from("h_separation"))
            .theme_type(&StringName::from("DiffInspectorSection"))
            .done();
        let separation_val = separation as f32;

        let mut margin_start = section_indent as f32 + outer_margin as f32;
        let margin_end = outer_margin as f32;

        // Draw arrow
        if let Some(arrow) = self.get_arrow() {
            if rtl {
                self.arrow_position.x = self.base().get_size().x
                    - ((margin_start as f32 + arrow.get_width() as f32) as f32);
            } else {
                self.arrow_position.x = margin_start as f32;
            }
            self.arrow_position.y = (header_height as f32 - arrow.get_height() as f32) / 2.0;
            let arrow_position = self.arrow_position.clone();
            self.base_mut()
                .draw_texture_ex(&arrow, arrow_position)
                .done();
            margin_start += (arrow.get_width() as f32 + separation_val) as f32;
        }

        let available = self.base().get_size().x - (margin_start + margin_end);

		// TODO: Currently not able to use this due to the way that we construct the child controls.
        // Draw count (if folded)
        // let folded = self.foldable && !self.unfolded;
		// let child_count = self.base().get_child_count() - 1; // -1 for the vertical seperator
        // if folded && child_count > 0 {
        //     let font = self
        //         .base()
        //         .get_theme_font_ex(&StringName::from("bold"))
        //         .theme_type(&StringName::from("EditorFonts"))
        //         .done();
        //     let font_size = self
        //         .base()
        //         .get_theme_font_size_ex(&StringName::from("bold_size"))
        //         .theme_type(&StringName::from("EditorFonts"))
        //         .done();

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

        //         let light_font = self
        //             .base()
        //             .get_theme_font_ex(&StringName::from("main"))
        //             .theme_type(&StringName::from("EditorFonts"))
        //             .done();
        //         let light_font_size = self
        //             .base()
        //             .get_theme_font_size_ex(&StringName::from("main_size"))
        //             .theme_type(&StringName::from("EditorFonts"))
        //             .done();
        //         let light_font_color = self
        //             .base()
        //             .get_theme_color_ex(&StringName::from("font_disabled_color"))
        //             .theme_type(&StringName::from("Editor"))
        //             .done();

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
        let font = self
            .base()
            .get_theme_font_ex(&StringName::from("bold"))
            .theme_type(&StringName::from("EditorFonts"))
            .done();
        let font_size = self
            .base()
            .get_theme_font_size_ex(&StringName::from("bold_size"))
            .theme_type(&StringName::from("EditorFonts"))
            .done();
        let font_color = self
            .base()
            .get_theme_color_ex(&StringName::from("font_color"))
            .theme_type(&StringName::from("Editor"))
            .done();

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
            let accent_color = self
                .base()
                .get_theme_color_ex(&StringName::from("accent_color"))
                .theme_type(&StringName::from("Editor"))
                .done();
            let size = self.base().get_size();
            self.base_mut()
                .draw_rect_ex(Rect2::new(Vector2::ZERO, size), accent_color)
                .filled(false)
                .done();
        }

        // Draw section indentation
        if let Some(ref section_indent_style) = section_indent_style {
            if section_indent > 0 {
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
                            - (section_indent as f32 + style_flat.get_margin(Side::RIGHT) as f32); // RIGHT
                    } else {
                        indent_pos.x = style_flat.get_margin(Side::LEFT) as f32; // LEFT
                    }
                }
                let final_rect = Rect2::new(indent_pos, indent_rect.size);
                self.base_mut()
                    .draw_style_box(section_indent_style, final_rect);
            }
        }
    }
}
