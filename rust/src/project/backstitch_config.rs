use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use indexmap::IndexMap;

const CONFIG_FILE_NAME: &str = "backstitch.cfg";
const USER_DIR_NAME: &str = "backstitch_plugin";
const SECTION: &str = "backstitch";

/// Matches Godot's ConfigFile serialization format; double-quotes and escapes strings as Godot's VariantWriter writes them.
/// Uses the same escaping as Godot's `String::c_escape()` to escape control characters.
fn encode_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn decode_hex_escape(chars: &mut std::iter::Peekable<std::str::Chars>, len: usize) -> Option<char> {
    let mut digits = String::with_capacity(len);
    while digits.len() < len && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
        digits.push(chars.next().unwrap());
    }
    if digits.len() != len {
        return None;
    }
    u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
}

fn decode_value(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return raw.to_string();
    };

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('a') => out.push('\u{07}'),
            Some('b') => out.push('\u{08}'),
            Some('f') => out.push('\u{0c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('v') => out.push('\u{0b}'),
            Some(marker @ ('u' | 'U')) => {
                let len = if marker == 'u' { 4 } else { 6 };
                match decode_hex_escape(&mut chars, len) {
                    Some(c) => out.push(c),
                    None => out.push(marker),
                }
            }
            // Everything else, including `\\`, `\"` and `\'`, is its own literal.
            Some(escaped) => out.push(escaped),
            None => {}
        }
    }
    out
}

/// An INI document backed by a file on disk, covering the subset of Godot's `ConfigFile` format
/// that Backstitch uses: `[section]` headers and `key="value"` pairs. Sections and keys we don't
/// know about are preserved, but comments are dropped on save.
#[derive(Debug)]
struct IniFile {
    path: PathBuf,
    sections: IndexMap<String, IndexMap<String, String>>,
}

impl IniFile {
    fn load(path: PathBuf) -> Self {
        let mut sections: IndexMap<String, IndexMap<String, String>> = IndexMap::new();
        match fs::read_to_string(&path) {
            Ok(text) => {
                // Anything before the first header goes into a nameless section, which is written
                // back out without a header.
                let mut section = String::new();
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                        continue;
                    }
                    if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                        section = name.trim().to_string();
                        sections.entry(section.clone()).or_default();
                        continue;
                    }
                    let Some((key, value)) = line.split_once('=') else {
                        tracing::warn!("Skipping malformed config line in {:?}: {line}", path);
                        continue;
                    };
                    sections
                        .entry(section.clone())
                        .or_default()
                        .insert(key.trim().to_string(), decode_value(value.trim()));
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => tracing::error!("Failed to read config at {:?}: {e}", path),
        }
        Self { path, sections }
    }

    fn save(&self) {
        let mut text = String::new();
        for (name, values) in &self.sections {
            if !name.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("[{name}]\n"));
            }
            for (key, value) in values {
                text.push_str(&format!("{key}={}\n", encode_value(value)));
            }
        }

        if let Some(parent) = self.path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::error!("Failed to create config directory {:?}: {e}", parent);
            return;
        }
        if let Err(e) = fs::write(&self.path, text) {
            tracing::error!("Failed to save config to {:?}: {e}", self.path);
        }
    }

    fn get(&self, key: &str, default: &str) -> String {
        self.sections
            .get(SECTION)
            .and_then(|values| values.get(key))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    fn set(&mut self, key: &str, value: &str) {
        self.sections
            .entry(SECTION.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.save();
    }
}

/// Backstitch's configuration, split between per-project settings that live alongside the project
/// and per-user settings that live outside of it.
#[derive(Debug)]
pub struct BackstitchConfig {
    project: IniFile,
    user: IniFile,
}

impl BackstitchConfig {
    /// The project config lives at `<project_dir>/backstitch.cfg`, and the user config at
    /// `<user_data_dir>/backstitch_plugin/backstitch.cfg`.
    pub fn new(project_dir: &Path, user_data_dir: &Path) -> Self {
        Self {
            project: IniFile::load(project_dir.join(CONFIG_FILE_NAME)),
            user: IniFile::load(user_data_dir.join(USER_DIR_NAME).join(CONFIG_FILE_NAME)),
        }
    }

    pub fn get_project_value(&self, key: &str, default: &str) -> String {
        self.project.get(key, default)
    }

    pub fn set_project_value(&mut self, key: &str, value: &str) {
        self.project.set(key, value);
    }

    pub fn get_user_value(&self, key: &str, default: &str) -> String {
        self.user.get(key, default)
    }

    pub fn set_user_value(&mut self, key: &str, value: &str) {
        self.user.set(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn load_config(dir: &TempDir) -> BackstitchConfig {
        BackstitchConfig::new(dir.path(), dir.path())
    }

    fn project_config_text(dir: &TempDir) -> String {
        fs::read_to_string(dir.path().join(CONFIG_FILE_NAME)).unwrap()
    }

    #[test]
    fn missing_files_read_as_empty() {
        let dir = TempDir::new().unwrap();
        let config = load_config(&dir);
        assert_eq!(
            config.get_project_value("server_url", "fallback"),
            "fallback"
        );
        assert_eq!(config.get_user_value("user_name", "Anonymous"), "Anonymous");
        assert_eq!(config.get_project_value("project_doc_id", ""), "");
    }

    #[test]
    fn writes_godot_config_file_format() {
        let dir = TempDir::new().unwrap();
        let mut config = load_config(&dir);
        config.set_project_value("server_url", "alpha.backstitch.dev:8085");
        assert_eq!(
            project_config_text(&dir),
            "[backstitch]\nserver_url=\"alpha.backstitch.dev:8085\"\n"
        );
    }

    #[test]
    fn reads_quoted_and_unquoted_values() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "; a comment\n[backstitch]\nserver_url=\"quoted.example.com\"\nproject_doc_id=bare\n",
        )
        .unwrap();
        let config = load_config(&dir);
        assert_eq!(
            config.get_project_value("server_url", ""),
            "quoted.example.com"
        );
        assert_eq!(config.get_project_value("project_doc_id", ""), "bare");
    }

    /// Every escape Godot's `String::c_escape()` produces, in the same order it lists them.
    #[test]
    fn round_trips_every_c_escape() {
        let dir = TempDir::new().unwrap();
        let mut config = load_config(&dir);
        let value = "\\\u{07}\u{08}\u{0c}\n\r\t\u{0b}'\"";
        config.set_project_value("weird", value);
        assert_eq!(
            project_config_text(&dir),
            "[backstitch]\nweird=\"\\\\\\a\\b\\f\\n\\r\\t\\v\\'\\\"\"\n"
        );
        assert_eq!(load_config(&dir).get_project_value("weird", ""), value);
    }

    /// Godot's Variant parser accepts hex escapes, so a hand-edited file may contain them.
    #[test]
    fn decodes_hex_escapes() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[backstitch]\nshort=\"caf\\u00e9\"\nlong=\"\\U01f600\"\nbroken=\"\\uzz\"\n",
        )
        .unwrap();
        let config = load_config(&dir);
        assert_eq!(config.get_project_value("short", ""), "café");
        assert_eq!(config.get_project_value("long", ""), "😀");
        assert_eq!(config.get_project_value("broken", ""), "uzz");
    }

    /// The file CI generates, and that the `_write-url` justfile recipe rewrites line by line,
    /// has to survive a load and save unchanged.
    #[test]
    fn round_trips_a_ci_generated_config() {
        let dir = TempDir::new().unwrap();
        let text = "[backstitch]\nserver_url=\"alpha.backstitch.dev:8085\"\navailable_servers=\"alpha.backstitch.dev:8085\"\n";
        fs::write(dir.path().join(CONFIG_FILE_NAME), text).unwrap();
        load_config(&dir).set_project_value("server_url", "alpha.backstitch.dev:8085");
        assert_eq!(project_config_text(&dir), text);
    }

    #[test]
    fn preserves_unknown_keys_and_sections() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE_NAME),
            "[backstitch]\nmystery=\"keep me\"\n\n[other]\nthing=\"also keep\"\n",
        )
        .unwrap();
        load_config(&dir).set_project_value("server_url", "example.com");
        assert_eq!(
            project_config_text(&dir),
            "[backstitch]\nmystery=\"keep me\"\nserver_url=\"example.com\"\n\n[other]\nthing=\"also keep\"\n"
        );
    }

    #[test]
    fn user_config_lives_in_its_own_directory() {
        let dir = TempDir::new().unwrap();
        let mut config = load_config(&dir);
        config.set_user_value("user_name", "Lilith");
        config.set_project_value("server_url", "example.com");
        assert_eq!(
            fs::read_to_string(dir.path().join(USER_DIR_NAME).join(CONFIG_FILE_NAME)).unwrap(),
            "[backstitch]\nuser_name=\"Lilith\"\n"
        );
        assert_eq!(config.get_user_value("server_url", ""), "");
    }

    #[test]
    fn parses_saved_project_doc_id() {
        let dir = TempDir::new().unwrap();
        let mut config = load_config(&dir);
        let id = "aa3ecb4c-1ee5-4a67-be9e-6ecf1a5c3f2b";
        config.set_project_value("project_doc_id", id);
        assert_eq!(config.get_project_value("project_doc_id", ""), id);

        config.set_project_value("project_doc_id", "not a doc id");
        assert_eq!(
            config.get_project_value("project_doc_id", ""),
            "not a doc id"
        );
    }
}
