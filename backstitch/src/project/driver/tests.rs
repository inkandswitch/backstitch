use super::*;
#[test]
fn test_should_ignore_windows_style_paths() {
    let dir = PathBuf::from("C:\\foo\\bar\\");
    let gitignore = Driver::build_gitignore(&dir);
    // ignores windows style paths
    assert!(
        gitignore
            .matched_path_or_any_parents(dir.join(".backstitch\\thingy.txt").as_path(), false)
            .is_ignore()
    );
    assert!(
        !gitignore
            .matched_path_or_any_parents(dir.join("blargh\\baz.txt").as_path(), false)
            .is_ignore()
    );
}
