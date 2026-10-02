use backstitch::diff::text_differ::{TextDiff, TextDiffHunk, TextDiffLine};
use backstitch::helpers::utils::ChangeType;
use godot::builtin::{Array, GString, StringName, VarDictionary};
use godot::classes::{
    DisplayServer, EditorInterface, IRichTextLabel, InputEvent, InputEventMouseButton, PopupMenu,
    RichTextLabel, Theme,
};
use godot::global::MouseButton;
use godot::prelude::*;

use crate::interop::godot_helpers::ToGodotExt;

#[derive(GodotClass)]
#[class(tool, base=RichTextLabel)]
pub struct TextDifferView {
    #[base]
    base: Base<RichTextLabel>,
    popup_menu: Gd<PopupMenu>,
    diff_file: TextDiff,
    split_view: bool,
}

trait TextDiffFromDict: Sized {
    fn from_dict(dict: &VarDictionary) -> Option<Self>;
}

impl TextDiffFromDict for TextDiffLine {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        Some(Self {
            new_line_no: dict.get("new_line_no")?.to::<i64>(),
            old_line_no: dict.get("old_line_no")?.to::<i64>(),
            content: dict.get("content")?.to::<GString>().to_string(),
            status: dict.get("status")?.to::<GString>().to_string(),
        })
    }
}

impl TextDiffFromDict for TextDiffHunk {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        let diff_lines_array = dict.get("diff_lines")?.to::<Array<VarDictionary>>();
        let mut diff_lines = Vec::new();

        for line_dict in diff_lines_array.iter_shared() {
            if let Some(diff_line) = TextDiffLine::from_dict(&line_dict) {
                diff_lines.push(diff_line);
            }
        }

        Some(Self {
            new_start: dict.get("new_start")?.to::<i64>(),
            old_start: dict.get("old_start")?.to::<i64>(),
            new_lines: dict.get("new_lines")?.to::<i64>(),
            old_lines: dict.get("old_lines")?.to::<i64>(),
            diff_lines,
        })
    }
}

impl TextDiffFromDict for TextDiff {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        let diff_hunks_array = dict.get("diff_hunks")?.to::<Array<VarDictionary>>();
        let mut diff_hunks = Vec::new();

        for hunk_dict in diff_hunks_array.iter_shared() {
            if let Some(diff_hunk) = TextDiffHunk::from_dict(&hunk_dict) {
                diff_hunks.push(diff_hunk);
            }
        }

        Some(Self {
            path: dict.get("old_file")?.to::<GString>().to_string(),
            diff_hunks,
            change_type: ChangeType::Modified,
        })
    }
}

#[godot_api]
impl TextDifferView {
    const DISPLAY_UNIFIED_VIEW: i32 = 0;
    const DISPLAY_SPLIT_VIEW: i32 = 1;
    const COPY_AS_UNIFIED_DIFF: i32 = 2;

    fn create_instance(base: Base<RichTextLabel>, diff: TextDiff, split_view: bool) -> Self {
        Self {
            base,
            diff_file: diff,
            split_view,
            popup_menu: PopupMenu::new_alloc(),
        }
    }

    pub fn create_new(diff: TextDiff, split_view: bool) -> Gd<TextDifferView> {
        Gd::from_init_fn(|base| Self::create_instance(base, diff, split_view))
    }

    #[func]
    pub fn get_text_diff_view(diff: VarDictionary, split_view: bool) -> Option<Gd<TextDifferView>> {
        let diff_file = TextDiff::from_dict(&diff)
            .ok_or_else(|| godot_error!("Failed to create text diff"))
            .ok()?;
        let mut rich_text_label = TextDifferView::create_new(diff_file.clone(), split_view);
        rich_text_label.bind_mut().redraw();

        return Some(rich_text_label);
    }

    pub fn redraw(&mut self) {
        self.base_mut().clear();
        if self.diff_file.is_empty() {
            return;
        }

        let editor_interface = EditorInterface::singleton();
        let editor_theme = editor_interface.get_editor_theme();
        if editor_theme.is_none() {
            godot_error!("Editor theme is none");
            return;
        }

        let editor_theme = editor_theme.unwrap();

        // Add file header
        let Some(doc_bold_font) = editor_theme
            .get_font(
                &StringName::from("doc_bold"),
                &StringName::from("EditorFonts"),
            )
            .or_else(|| editor_theme.get_default_font())
        else {
            godot_error!("Doc bold font is none");
            return;
        };

        let accent_color = editor_theme.get_color(
            &StringName::from("accent_color"),
            &StringName::from("Editor"),
        );

        let diff_file = self.diff_file.clone();
        let split_view = self.split_view;
        let mut rich_text_label = self.base_mut();
        rich_text_label.push_font(&doc_bold_font);
        rich_text_label.push_color(accent_color);
        rich_text_label.add_text(&format!("File: {}", diff_file.path));
        rich_text_label.pop();
        rich_text_label.pop();

        let Some(status_source_font) = editor_theme
            .get_font(
                &StringName::from("status_source"),
                &StringName::from("EditorFonts"),
            )
            .or_else(|| editor_theme.get_default_font())
        else {
            godot_error!("Status source font is none");
            return;
        };
        rich_text_label.push_font(&status_source_font);

        for hunk in &diff_file.diff_hunks {
            rich_text_label.newline();
            let hunk_header = format!(
                "[center]@@ {},{} {},{} @@[/center]",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            );
            rich_text_label.append_text(&hunk_header);
            rich_text_label.newline();

            if split_view {
                Self::display_diff_split_view(
                    &mut rich_text_label,
                    &hunk.diff_lines,
                    &editor_theme,
                );
            } else {
                Self::display_diff_unified_view(
                    &mut rich_text_label,
                    &hunk.diff_lines,
                    &editor_theme,
                );
            }

            rich_text_label.newline();
        }

        rich_text_label.pop();
        rich_text_label.newline();
    }

    fn display_diff_split_view(
        rich_text_label: &mut godot::obj::BaseMut<TextDifferView>,
        diff_lines: &[TextDiffLine],
        theme: &Gd<Theme>,
    ) {
        // Parse diff lines into a format suitable for split view
        let mut parsed_diff: Vec<ParsedDiffLine> = Vec::new();

        for diff_line in diff_lines {
            let line = diff_line.content.trim_end().to_string();

            if diff_line.new_line_no >= 0 && diff_line.old_line_no >= 0 {
                // Unchanged line
                parsed_diff.push(ParsedDiffLine {
                    old_line_no: diff_line.old_line_no,
                    new_line_no: diff_line.new_line_no,
                    old_text: line.clone(),
                    new_text: line,
                    status: diff_line.status.clone(),
                });
            } else if diff_line.new_line_no == -1 {
                // Deleted line
                parsed_diff.push(ParsedDiffLine {
                    old_line_no: diff_line.old_line_no,
                    new_line_no: -1,
                    old_text: line,
                    new_text: String::new(),
                    status: diff_line.status.clone(),
                });
            } else if diff_line.old_line_no == -1 {
                // Added line - try to pair with previous deleted lines
                let mut j = parsed_diff.len() as i32 - 1;
                while j >= 0 && parsed_diff[j as usize].new_line_no == -1 {
                    j -= 1;
                }

                if j == parsed_diff.len() as i32 - 1 {
                    // No lines are modified
                    parsed_diff.push(ParsedDiffLine {
                        old_line_no: -1,
                        new_line_no: diff_line.new_line_no,
                        old_text: String::new(),
                        new_text: line,
                        status: diff_line.status.clone(),
                    });
                } else {
                    // Lines are modified - pair with the deleted line
                    let modified_line = &mut parsed_diff[(j + 1) as usize];
                    modified_line.new_text = line;
                    modified_line.new_line_no = diff_line.new_line_no;
                }
            }
        }

        // Create 6-column table: Old Line No | prefix | Old Code | New Line No | prefix | New Code
        rich_text_label.push_table(6);
        rich_text_label.set_table_column_expand(2, true);
        rich_text_label.set_table_column_expand(5, true);

        let error_color = theme.get_color(
            &StringName::from("error_color"),
            &StringName::from("Editor"),
        );
        let success_color = theme.get_color(
            &StringName::from("success_color"),
            &StringName::from("Editor"),
        );
        let font_color =
            theme.get_color(&StringName::from("font_color"), &StringName::from("Label"));
        let white =
            font_color * Color::from_rgb(1.0, 1.0, 1.0) * Color::from_rgba(1.0, 1.0, 1.0, 0.6);

        for diff_line in parsed_diff {
            let has_change = diff_line.status != " ";

            // Old side
            if diff_line.old_line_no >= 0 {
                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { error_color } else { white });
                rich_text_label.add_text(&diff_line.old_line_no.to_string());
                rich_text_label.pop();
                rich_text_label.pop();

                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { error_color } else { white });
                rich_text_label.add_text(if has_change { "-|" } else { " |" });
                rich_text_label.pop();
                rich_text_label.pop();

                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { error_color } else { white });
                rich_text_label.add_text(&diff_line.old_text);
                rich_text_label.pop();
                rich_text_label.pop();
            } else {
                rich_text_label.push_cell();
                rich_text_label.pop();
                rich_text_label.push_cell();
                rich_text_label.pop();
                rich_text_label.push_cell();
                rich_text_label.pop();
            }

            // New side
            if diff_line.new_line_no >= 0 {
                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { success_color } else { white });
                rich_text_label.add_text(&diff_line.new_line_no.to_string());
                rich_text_label.pop();
                rich_text_label.pop();

                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { success_color } else { white });
                rich_text_label.add_text(if has_change { "+|" } else { " |" });
                rich_text_label.pop();
                rich_text_label.pop();

                rich_text_label.push_cell();
                rich_text_label.push_color(if has_change { success_color } else { white });
                rich_text_label.add_text(&diff_line.new_text);
                rich_text_label.pop();
                rich_text_label.pop();
            } else {
                rich_text_label.push_cell();
                rich_text_label.pop();
                rich_text_label.push_cell();
                rich_text_label.pop();
                rich_text_label.push_cell();
                rich_text_label.pop();
            }
        }

        rich_text_label.pop();
    }

    fn display_diff_unified_view(
        rich_text_label: &mut godot::obj::BaseMut<TextDifferView>,
        diff_lines: &[TextDiffLine],
        theme: &Gd<Theme>,
    ) {
        // Create 4-column table: Old Line No | New Line No | status | code
        rich_text_label.push_table(4);
        rich_text_label.set_table_column_expand(3, true);

        let error_color = theme.get_color(
            &StringName::from("error_color"),
            &StringName::from("Editor"),
        );
        let success_color = theme.get_color(
            &StringName::from("success_color"),
            &StringName::from("Editor"),
        );
        let font_color =
            theme.get_color(&StringName::from("font_color"), &StringName::from("Label"));
        let default_color = font_color * Color::from_rgba(1.0, 1.0, 1.0, 0.6);

        for diff_line in diff_lines {
            let line = diff_line.content.trim_end().to_string();

            let color = if diff_line.status == "+" {
                success_color
            } else if diff_line.status == "-" {
                error_color
            } else {
                default_color
            };

            let mut diff_old_line_no = if diff_line.old_line_no >= 0 {
                diff_line.old_line_no.to_string()
            } else {
                String::new()
            };
            let diff_new_line_no = if diff_line.new_line_no >= 0 {
                diff_line.new_line_no.to_string()
            } else {
                String::new()
            };

            if diff_line.old_line_no >= 0 && diff_line.new_line_no >= 0 {
                diff_old_line_no.push('|');
            }

            // Old line number
            rich_text_label.push_cell();
            rich_text_label.push_color(color);
            rich_text_label.push_indent(1);
            rich_text_label.add_text(&diff_old_line_no);
            rich_text_label.pop();
            rich_text_label.pop();
            rich_text_label.pop();

            // New line number
            rich_text_label.push_cell();
            rich_text_label.push_color(color);
            rich_text_label.push_indent(1);
            rich_text_label.add_text(&diff_new_line_no);
            rich_text_label.pop();
            rich_text_label.pop();
            rich_text_label.pop();

            // Status
            rich_text_label.push_cell();
            rich_text_label.push_color(color);
            let status_text = if !diff_line.status.is_empty() {
                format!("{}|", diff_line.status)
            } else {
                " |".to_string()
            };
            rich_text_label.add_text(&status_text);
            rich_text_label.pop();
            rich_text_label.pop();

            // Code
            rich_text_label.push_cell();
            rich_text_label.push_color(color);
            rich_text_label.add_text(&line);
            rich_text_label.pop();
            rich_text_label.pop();
        }

        rich_text_label.pop();
    }

    pub fn set_diff_file(&mut self, diff: TextDiff) {
        self.diff_file = diff;
        self.redraw();
    }

    pub fn get_diff_file(&self) -> TextDiff {
        self.diff_file.clone()
    }

    #[func]
    pub fn get_diff_dict(&self) -> VarDictionary {
        self.get_diff_file().to_godot()
    }

    #[func]
    pub fn set_diff_dict(&mut self, diff: VarDictionary) {
        self.set_diff_file(TextDiff::from_dict(&diff).unwrap());
    }

    #[func]
    pub fn set_split_view(&mut self, split_view: bool) {
        if self.split_view != split_view {
            self.split_view = split_view;
            self.popup_menu
                .set_item_checked(Self::DISPLAY_UNIFIED_VIEW, !self.split_view);
            self.popup_menu
                .set_item_checked(Self::DISPLAY_SPLIT_VIEW, self.split_view);
            self.redraw();
        }
    }

    #[func]
    pub fn is_split_view(&self) -> bool {
        self.split_view
    }

    #[func]
    pub fn get_unified_diff_text(&self) -> String {
        self.diff_file.to_unified()
    }

    #[func]
    fn _on_popup_menu_id_pressed(&mut self, id: i64) {
        self.popup_menu.set_visible(false);
        match id as i32 {
            Self::DISPLAY_UNIFIED_VIEW => {
                self.set_split_view(false);
            }
            Self::DISPLAY_SPLIT_VIEW => {
                self.set_split_view(true);
            }
            Self::COPY_AS_UNIFIED_DIFF => {
                DisplayServer::singleton().clipboard_set(&self.get_unified_diff_text());
            }
            _ => {}
        }
    }
}

#[godot_api]
impl IRichTextLabel for TextDifferView {
    fn init(base: Base<RichTextLabel>) -> Self {
        Self {
            base,
            diff_file: TextDiff {
                path: String::new(),
                diff_hunks: Vec::new(),
                change_type: ChangeType::Modified,
            },
            split_view: false,
            popup_menu: PopupMenu::new_alloc(),
        }
    }

    fn ready(&mut self) {
        // setup popup menu
        let mut popup_menu = self.popup_menu.clone();
        popup_menu.set_visible(false);
        popup_menu
            .add_radio_check_item_ex("Display Unified View")
            .id(Self::DISPLAY_UNIFIED_VIEW)
            .done();
        popup_menu
            .add_radio_check_item_ex("Display Split View")
            .id(Self::DISPLAY_SPLIT_VIEW)
            .done();
        popup_menu.set_item_checked(Self::DISPLAY_UNIFIED_VIEW, !self.split_view);
        popup_menu.set_item_checked(Self::DISPLAY_SPLIT_VIEW, self.split_view);
        popup_menu.add_separator();
        popup_menu
            .add_item_ex("Copy As Unified Diff")
            .id(Self::COPY_AS_UNIFIED_DIFF)
            .done();

        self.base_mut().add_child(&popup_menu);
        let callable = Callable::from_object_method(&self.to_gd(), "_on_popup_menu_id_pressed");
        popup_menu.connect("id_pressed", &callable);
    }

    // handle right click
    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if let Ok(mb) = event.clone().try_cast::<InputEventMouseButton>() {
            let is_pressed = mb.is_pressed();
            let button_index = mb.get_button_index();
            if is_pressed && button_index == MouseButton::RIGHT {
                self.popup_menu
                    .set_position(DisplayServer::singleton().mouse_get_position());
                self.popup_menu.set_visible(true);
                self.base_mut().accept_event();
            }
        }
    }
}

// Helper struct for parsed diff lines in split view
struct ParsedDiffLine {
    old_line_no: i64,
    new_line_no: i64,
    old_text: String,
    new_text: String,
    status: String,
}
