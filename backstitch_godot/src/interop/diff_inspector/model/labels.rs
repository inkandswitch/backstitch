//! Small string helpers used to build labels. These mirror the Godot `String` methods of the same
//! name so that the view-model can stay free of Godot types.

/// Turns `snake_case_words` into `Capitalized Case Words`.
pub fn snake_case_to_capital_case(snake_case: &str) -> String {
    snake_case
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The part of the path after the last `/` (like Godot's `String.get_file()`).
pub fn get_file(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}

/// The part of the path before the last `/` (like Godot's `String.get_base_dir()`, for the paths we deal with).
pub fn get_base_dir(path: &str) -> &str {
    let (prefix_len, rest) = match path.find("://") {
        Some(idx) => (idx + 3, &path[idx + 3..]),
        None => (0, path),
    };
    match rest.rfind('/') {
        Some(idx) => &path[..prefix_len + idx],
        None => &path[..prefix_len],
    }
}

/// Collapses `.`, `..` and repeated `/` in a path (like Godot's `String.simplify_path()`).
pub fn simplify_path(path: &str) -> String {
    let (prefix, rest) = match path.find("://") {
        Some(idx) => (&path[..idx + 3], &path[idx + 3..]),
        None if path.starts_with('/') => ("/", &path[1..]),
        None => ("", path),
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in rest.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if matches!(segments.last(), Some(last) if *last != "..") {
                    segments.pop();
                } else if prefix.is_empty() {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    format!("{}{}", prefix, segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_readable() {
        assert_eq!(
            snake_case_to_capital_case("some_prop_name"),
            "Some Prop Name"
        );
        assert_eq!(
            snake_case_to_capital_case("_leading__double"),
            "Leading Double"
        );
        assert_eq!(snake_case_to_capital_case("Ünicode_start"), "Ünicode Start");
        assert_eq!(snake_case_to_capital_case(""), "");
    }

    #[test]
    fn file_and_base_dir() {
        assert_eq!(get_file("./A/B"), "B");
        assert_eq!(get_file("."), ".");
        assert_eq!(get_base_dir("./A/B"), "./A");
        assert_eq!(get_base_dir("./A"), ".");
        assert_eq!(get_base_dir("."), "");
        assert_eq!(get_base_dir("res://a/b.tscn"), "res://a");
        assert_eq!(get_base_dir("res://b.tscn"), "res://");
    }

    #[test]
    fn simplify() {
        assert_eq!(simplify_path("res://a/./b/../c.tscn"), "res://a/c.tscn");
        assert_eq!(simplify_path("res://a/../c.tscn"), "res://c.tscn");
        assert_eq!(simplify_path("res://a//b.tscn"), "res://a/b.tscn");
        assert_eq!(simplify_path("a/b"), "a/b");
        assert_eq!(simplify_path("../a"), "../a");
    }
}
