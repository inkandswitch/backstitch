use godot::builtin::{Array, GString, StringName, VarDictionary};
use godot::classes::{EditorInterface, RichTextLabel, Theme};
use godot::prelude::*;

#[derive(GodotClass)]
#[class(base=Object)]
pub struct TextDifferView {
    #[base]
    base: Base<Object>,
}

struct DiffLine {
    new_line_no: i64,
    old_line_no: i64,
    content: String,
    status: String,
}

struct DiffHunk {
    new_start: i64,
    old_start: i64,
    new_lines: i64,
    old_lines: i64,
    diff_lines: Vec<DiffLine>,
}

struct DiffFile {
    new_file: String,
    old_file: String,
    diff_hunks: Vec<DiffHunk>,
}

impl DiffLine {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        Some(Self {
            new_line_no: dict.get("new_line_no")?.to::<i64>(),
            old_line_no: dict.get("old_line_no")?.to::<i64>(),
            content: dict.get("content")?.to::<GString>().to_string(),
            status: dict.get("status")?.to::<GString>().to_string(),
        })
    }
}

impl DiffHunk {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        let diff_lines_array = dict.get("diff_lines")?.to::<Array<VarDictionary>>();
        let mut diff_lines = Vec::new();

        for line_dict in diff_lines_array.iter_shared() {
            if let Some(diff_line) = DiffLine::from_dict(&line_dict) {
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

impl DiffFile {
    fn from_dict(dict: &VarDictionary) -> Option<Self> {
        let diff_hunks_array = dict.get("diff_hunks")?.to::<Array<VarDictionary>>();
        let mut diff_hunks = Vec::new();

        for hunk_dict in diff_hunks_array.iter_shared() {
            if let Some(diff_hunk) = DiffHunk::from_dict(&hunk_dict) {
                diff_hunks.push(diff_hunk);
            }
        }

        Some(Self {
            new_file: dict.get("new_file")?.to::<GString>().to_string(),
            old_file: dict.get("old_file")?.to::<GString>().to_string(),
            diff_hunks,
        })
    }
}

#[godot_api]
impl TextDifferView {
    #[func]
    pub fn get_text_diff_view(diff: VarDictionary, split_view: bool) -> Option<Gd<RichTextLabel>> {
        let editor_interface = EditorInterface::singleton();
        let editor_theme = editor_interface.get_editor_theme();
        if editor_theme.is_none() {
            godot_error!("Editor theme is none");
            return None;
        }
        let editor_theme = editor_theme.unwrap();

        // Parse dictionary into structured format
        let diff_file = DiffFile::from_dict(&diff)?;

        let mut rich_text_label = RichTextLabel::new_alloc();

        // Add file header
        let doc_bold_font = editor_theme.get_font(
            &StringName::from("doc_bold"),
            &StringName::from("EditorFonts"),
        )?;
        let accent_color = editor_theme.get_color(
            &StringName::from("accent_color"),
            &StringName::from("Editor"),
        );

        rich_text_label.push_font(&doc_bold_font);
        rich_text_label.push_color(accent_color);
        if diff_file.old_file != diff_file.new_file {
            rich_text_label.add_text(&format!(
                "File: {} -> {}",
                diff_file.old_file, diff_file.new_file
            ));
        } else {
            rich_text_label.add_text(&format!("File: {}", diff_file.new_file));
        }
        rich_text_label.pop();
        rich_text_label.pop();

        let status_source_font = editor_theme.get_font(
            &StringName::from("status_source"),
            &StringName::from("EditorFonts"),
        )?;
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

        Some(rich_text_label)
    }

    fn display_diff_split_view(
        rich_text_label: &mut Gd<RichTextLabel>,
        diff_lines: &[DiffLine],
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
        rich_text_label: &mut Gd<RichTextLabel>,
        diff_lines: &[DiffLine],
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
                diff_old_line_no.push_str("|");
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
}

#[godot_api]
impl IObject for TextDifferView {
    fn init(base: Base<Object>) -> Self {
        Self { base }
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
