use godot::builtin::{Color, Vector2};
use godot::classes::control::{LayoutDirection, SizeFlags};
use godot::classes::notify::ControlNotification;
use godot::classes::{ColorRect, IColorRect};
use godot::prelude::*;

use crate::interop::diff_inspector::change_type_ext::ChangeTypeExt;
use crate::interop::godot_helpers::ThemeGetter;
use backstitch::helpers::utils::ChangeType;

/// A small colored square that is tinted by the theme color for a [`ChangeType`].
#[derive(GodotClass)]
#[class(tool, base=ColorRect, init)]
pub struct DiffColorMarker {
    base: Base<ColorRect>,
    #[init(val = ChangeType::Modified)]
    kind: ChangeType,
}

#[godot_api]
impl IColorRect for DiffColorMarker {
    fn on_notification(&mut self, what: ControlNotification) {
        if what == ControlNotification::THEME_CHANGED {
            let color = self.get_change_color();
            self.base_mut().set_color(color);
        }
    }
}

impl DiffColorMarker {
    fn get_change_color(&self) -> Color {
        self.get_theme_color(self.kind.theme_color_name(), "Editor")
    }

    pub fn create(kind: ChangeType) -> Gd<Self> {
        let mut color_rect = Gd::from_init_fn(|base: Base<ColorRect>| Self { base, kind });
        let change_color = color_rect.bind().get_change_color();
        color_rect.set_color(change_color);
        color_rect.set_custom_minimum_size(Vector2::new(10.0, 10.0));
        color_rect.set_layout_direction(LayoutDirection::LTR);
        color_rect.set_h_size_flags(SizeFlags::SHRINK_CENTER);
        color_rect
    }
}
