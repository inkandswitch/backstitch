use godot::builtin::{Rect2, Vector2};

/// Horizontal layout numbers for a [`DiffInspectorSection`](super::foldable_section::DiffInspectorSection),
/// derived from the section's indent depth and its theme.
///
/// This is computed once per use so that the header rect, the minimum size, child sorting and
/// drawing all agree on the same numbers and we save compute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionMetrics {
    /// The theme's `indent_size` constant.
    pub indent_size: i32,
    /// The total indent of the section: `indent_depth * indent_size`, plus the horizontal
    /// margins of the indent box style, if any. Zero if the section isn't indented.
    pub indent: i32,
}

impl SectionMetrics {
    /// `indent_box_margins` is the sum of the left and right margins of the `indent_box` style, if it has one.
    pub fn new(indent_depth: i32, indent_size: i32, indent_box_margins: Option<f32>) -> Self {
        let mut indent = 0;
        if indent_depth > 0 {
            if indent_size > 0 {
                indent = indent_depth * indent_size;
            }
            if let Some(margins) = indent_box_margins {
                indent += margins as i32;
            }
        }
        Self {
            indent_size,
            indent,
        }
    }

    /// The rect of the header, given the width of the whole section.
    pub fn header_rect(&self, section_width: f32, rtl: bool, header_height: f32) -> Rect2 {
        let header_width = section_width - self.indent as f32;
        let header_offset_x = if rtl { 0.0 } else { self.indent as f32 };
        Rect2::new(
            Vector2::new(header_offset_x, 0.0),
            Vector2::new(header_width, header_height),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_indent_at_depth_zero() {
        let metrics = SectionMetrics::new(0, 8, Some(4.0));
        assert_eq!(metrics.indent, 0);
    }

    #[test]
    fn indent_includes_margins() {
        let metrics = SectionMetrics::new(2, 8, Some(4.0));
        assert_eq!(metrics.indent, 20);
        let metrics = SectionMetrics::new(2, 0, Some(4.0));
        assert_eq!(metrics.indent, 4);
    }

    #[test]
    fn header_rect_respects_direction() {
        let metrics = SectionMetrics::new(1, 10, None);
        let ltr = metrics.header_rect(100.0, false, 20.0);
        assert_eq!(ltr.position.x, 10.0);
        assert_eq!(ltr.size.x, 90.0);
        let rtl = metrics.header_rect(100.0, true, 20.0);
        assert_eq!(rtl.position.x, 0.0);
        assert_eq!(rtl.size.x, 90.0);
    }
}
