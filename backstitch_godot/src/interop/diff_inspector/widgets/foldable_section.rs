use godot::builtin::{Color, GString, Rect2, Vector2};
use godot::classes::class_macros::private::virtuals::Xrvrs::Side;
use godot::classes::notify::ContainerNotification;
use godot::classes::text_server::JustificationFlag;
use godot::classes::{
    Container, Control, IContainer, Input, InputEvent, InputEventMouseButton, StyleBox,
    StyleBoxFlat, Texture2D, Timer, VBoxContainer,
};
use godot::global::{HorizontalAlignment, MouseButton};
use godot::prelude::*;

use crate::interop::diff_inspector::change_type_ext::ChangeTypeExt;
use crate::interop::diff_inspector::widgets::section_metrics::SectionMetrics;
use crate::interop::godot_helpers::ThemeGetter;
use backstitch::helpers::utils::ChangeType;

const THEME_TYPE: &str = "DiffInspectorSection";

/// A foldable section with a colored header and a vertical box for its content.
///
/// This widget used to have much of the logic for the diff inspector, but it's been removed;
/// now it just draws a header, folds and unfolds, and reports what the user does with it through its signals.
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
    kind: ChangeType,
    unfolded: bool,

    // UI components
    vbox: Gd<VBoxContainer>,

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
}

impl DiffInspectorSection {
    /// Creates a foldable section.
    ///
    /// `section` is an identifier that is passed along with the signals. `indent_depth` and `level` control how
    /// indented the section is and how faded its header is.
    pub fn create(
        section: &str,
        label: &str,
        kind: ChangeType,
        indent_depth: i32,
        level: i32,
    ) -> Gd<Self> {
        let mut gd = Self::new_alloc();
        gd.bind_mut().setup(section, label, indent_depth, level);
        gd.bind_mut().set_kind(kind);
        gd
    }

    fn setup(&mut self, section: &str, label: &str, indent_depth: i32, level: i32) {
        self.section = GString::from(section);
        self.label = GString::from(label);
        self.foldable = true;
        self.indent_depth = indent_depth;
        self.level = level;

        self.test_unfold();
        if self.unfolded {
            self.vbox.show();
        } else {
            self.vbox.hide();
        }
    }

    pub fn get_vbox(&self) -> Gd<VBoxContainer> {
        self.vbox.clone()
    }

    pub fn set_kind(&mut self, kind: ChangeType) {
        self.kind = kind;
        self.update_bg_color();
    }

    #[allow(dead_code)]
    pub fn is_folded(&self) -> bool {
        !self.unfolded
    }

    fn metrics(&self) -> SectionMetrics {
        let margins = self
            .get_theme_stylebox("indent_box", THEME_TYPE)
            .and_then(|style| style.try_cast::<StyleBoxFlat>().ok())
            .map(|flat| flat.get_margin(Side::LEFT) + flat.get_margin(Side::RIGHT));
        SectionMetrics::new(
            self.indent_depth,
            self.get_theme_constant("indent_size", THEME_TYPE),
            margins,
        )
    }

    // This is currently only used in process to handle input, to mouse over the header.
    fn get_header_rect(&self) -> Rect2 {
        let rtl = self.base().is_layout_rtl();
        self.metrics()
            .header_rect(self.base().get_size().x, rtl, self.get_header_height())
    }

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
        self.bg_color = self.get_theme_color(self.kind.theme_color_name(), "Editor");
        self.base_mut().queue_redraw();
    }

    fn add_timer(&mut self) {
        // The section can enter the tree more than once; only set up the timer the first time.
        if self.dropping_unfold_timer.get_parent().is_some() {
            return;
        }
        let callable = Callable::from_object_method(&self.to_gd(), "unfold");

        let timer = self.dropping_unfold_timer.clone();
        self.base_mut().add_child(&timer);

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

    fn draw(&mut self) {
        let metrics = self.metrics();
        let section_indent = metrics.indent;
        let rtl = self.base().is_layout_rtl();

        let header_height = self.get_header_height();
        let header_rect = metrics.header_rect(self.base().get_size().x, rtl, header_height);

        // Draw header background
        let mut c = self.bg_color;
        c.a *= 0.4;

        if self.entered {
            if Input::singleton().is_mouse_button_pressed(MouseButton::LEFT) {
                c = c.lightened(-0.05);
            } else {
                c = c.lightened(0.2);
            }
        }

        self.base_mut().draw_rect(header_rect, c);

        // Draw header content (arrow, label)
        let outer_margin = (2.0 * self.base().get_theme_default_base_scale()).round();
        let separation = self.get_theme_constant("h_separation", THEME_TYPE);
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

        // TODO: draw a "(N changes)" count when folded. Currently not possible due to the way that
        // we construct the child controls.

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
        let indent_style: Option<Gd<StyleBox>> = self.get_theme_stylebox("indent_box", THEME_TYPE);
        if let Some(ref indent_style) = indent_style
            && section_indent > 0
        {
            let indent_rect = Rect2::new(
                Vector2::ZERO,
                Vector2::new(
                    self.indent_depth as f32 * metrics.indent_size as f32,
                    self.base().get_size().y,
                ),
            );
            let mut indent_pos = indent_rect.position;
            if let Ok(style_flat) = indent_style.clone().try_cast::<StyleBoxFlat>() {
                if rtl {
                    indent_pos.x = self.base().get_size().x
                        - (section_indent as f32 + style_flat.get_margin(Side::RIGHT));
                } else {
                    indent_pos.x = style_flat.get_margin(Side::LEFT);
                }
            }
            let final_rect = Rect2::new(indent_pos, indent_rect.size);
            self.base_mut().draw_style_box(indent_style, final_rect);
        }
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
            foldable: true,
            indent_depth: 0,
            level: 1,
            kind: ChangeType::Modified,
            unfolded: true,
            vbox,
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
        ms.x += self.metrics().indent as f32;

        ms
    }

    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if let Ok(mb) = event.clone().try_cast::<InputEventMouseButton>() {
            if mb.is_pressed() && mb.get_button_index() == MouseButton::LEFT {
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
            ContainerNotification::THEME_CHANGED => {
                self.base_mut().update_minimum_size();
                self.update_bg_color();
                self.bg_color.a /= self.level as f32;
            }
            ContainerNotification::SORT_CHILDREN => {
                if !self.vbox_added {
                    return;
                }

                let inspector_margin = self.get_theme_constant("inspector_margin", "Editor");
                let inspector_margin_val = inspector_margin as f32 + self.metrics().indent as f32;

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
            ContainerNotification::DRAW => {
                self.draw();
            }
            ContainerNotification::DRAG_BEGIN => {
                self.dropping_for_unfold = true;
            }
            ContainerNotification::DRAG_END => {
                self.dropping_for_unfold = false;
            }
            ContainerNotification::MOUSE_ENTER => {
                if self.dropping || self.dropping_for_unfold {
                    self.dropping_unfold_timer.start();
                }
                self.base_mut().queue_redraw();
            }
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
