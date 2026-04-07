use std::{fmt::Display, str::FromStr};
use serde::{Deserialize, Serialize};

use crate::helpers::history_ref::HistoryRef;

/// References a file at a particular [HistoryRef]. Converts to a custom URL format referencable by a resource loader.
/// This allows us to identify any file in history with a URL!
#[derive(Debug, Clone, Serialize, Deserialize, Hash)]
pub struct HistoryRefPath {
    pub ref_: HistoryRef,
    pub path: String,
}

impl HistoryRefPath {
    pub const REF_DIVIDER: char = '-';

    pub fn recognize_path(path: &str) -> bool {
        HistoryRefPath::from_str(path).is_ok()
    }

    pub fn make_path_string(ref_: &HistoryRef, path: &str) -> Result<String, std::fmt::Error> {
        if !ref_.is_valid() {
            return Err(std::fmt::Error);
        }
        Ok(format!(
            "{}{}{}",
            ref_.to_uri_scheme_prefix(),
            HistoryRefPath::REF_DIVIDER,
            path
        ))
    }
}

impl Display for HistoryRefPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = Self::make_path_string(&self.ref_, &self.path)?;
        write!(f, "{}", path)
    }
}

trait UriSchemeChar {
    fn is_valid_uri_scheme_char(&self) -> bool;
}

// See https://www.rfc-editor.org/rfc/rfc3986#section-3.1
// scheme      = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )
impl UriSchemeChar for char {
    fn is_valid_uri_scheme_char(&self) -> bool {
        self.is_ascii_alphanumeric() || matches!(*self, '-' | '.' | '+')
    }
}

fn is_valid_uri_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => chars.all(|c| c.is_valid_uri_scheme_char()),
        _ => false,
    }
}

impl FromStr for HistoryRefPath {
    type Err = &'static str;
    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let path = path
            .strip_prefix(HistoryRef::BACKSTITCH_SCHEME_PREFIX)
            .ok_or_else(|| "Invalid path")?;
        let (history_ref_part, path) = path
            .split_once(HistoryRefPath::REF_DIVIDER)
            .ok_or_else(|| "Invalid path")?;
        // `simplify_path()` ends up mangling the uri identifier (e.g. `res://foo.gd` -> `res:/foo.gd`) so we need to check for that
        // TODO: remove this when this PR is merged and we rebase on that: (https://github.com/godotengine/godot/pull/115660)
        let path = if let Some(pos) = path.find(":/") {
            let uri_scheme = &path[..pos];
            // check if the previous characters before this were valid alphanumeric characters
            if is_valid_uri_scheme(uri_scheme)
                && path.len() >= pos + 2
                && &path[pos + 2..pos + 3] != "/"
            {
                // otherwise fix the path
                format!(
                    "{}://{}",
                    uri_scheme.to_string(),
                    path[pos + 2..].to_string()
                )
            } else {
                path.to_string()
            }
        } else {
            path.to_string()
        };
        let ref_ = HistoryRef::from_str(history_ref_part)?;
        Ok(HistoryRefPath { ref_, path })
    }
}
