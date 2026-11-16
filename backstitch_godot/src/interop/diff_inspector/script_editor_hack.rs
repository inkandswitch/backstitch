//! Opens arbitrary text files in the script editor.
//!
//! `ScriptEditor.open_file()` is not bound, so there is no way to open a non-script text file in
//! the script editor through the public API. The only thing that can do it is the "Open Recent"
//! menu of the script editor, so we dig the callback of that menu out of the editor's UI tree and
//! call it ourselves.
//!
//! This is fragile and depends on the layout of the script editor. Everything hacky is kept in
//! this file so that it can be replaced once <https://github.com/godotengine/godot/pull/117595>
//! lands.

use godot::classes::{EditorInterface, HBoxContainer, MenuButton, Node, PopupMenu, VBoxContainer};
use godot::obj::Singleton;
use godot::prelude::*;

struct OpenRecentHandle {
    /// The callable that the "Open Recent" menu is connected to; takes the id of the menu item.
    open_recent: Callable,
    recent_scripts_popup: Gd<PopupMenu>,
}

fn first_child_of_type<T: GodotClass + Inherits<Node>>(
    parent: &Gd<Node>,
    class_name: &str,
) -> Option<Gd<T>> {
    parent
        .find_children_ex("*")
        .type_(class_name)
        .recursive(false)
        .owned(false)
        .done()
        .iter_shared()
        .next()
        .and_then(|node| node.try_cast::<T>().ok())
}

fn find_open_recent_handle() -> Option<OpenRecentHandle> {
    let script_editor = EditorInterface::singleton().get_script_editor()?;
    let script_editor_node = script_editor.upcast::<Node>();

    let Some(main_vbox) =
        first_child_of_type::<VBoxContainer>(&script_editor_node, "VBoxContainer")
    else {
        tracing::error!("Couldn't find the script editor's main vbox");
        return None;
    };
    let Some(menu_hb) =
        first_child_of_type::<HBoxContainer>(&main_vbox.upcast::<Node>(), "HBoxContainer")
    else {
        tracing::error!("Couldn't find the script editor's menu bar");
        return None;
    };

    let file_button = menu_hb
        .get_children_ex()
        .include_internal(true)
        .done()
        .iter_shared()
        .find_map(|child| child.try_cast::<MenuButton>().ok());
    let Some(file_button) = file_button else {
        tracing::error!("Couldn't find the script editor's file button");
        return None;
    };
    // This should only fail if the translation locale is not english; it should be the first one in any case
    if file_button.get_text() != "File" {
        tracing::warn!("First script editor menu isn't File! Using it anyway...");
    }

    let file_menu_popup = file_button.get_popup()?;
    let Some(recent_scripts_popup) =
        first_child_of_type::<PopupMenu>(&file_menu_popup.upcast::<Node>(), "PopupMenu")
    else {
        tracing::error!("Couldn't find the script editor's recent scripts popup");
        return None;
    };

    let connections = recent_scripts_popup.get_signal_connection_list("id_pressed");
    let Some(connection) = connections.iter_shared().next() else {
        tracing::error!("The recent scripts popup's id_pressed signal has no connections");
        return None;
    };
    let open_recent = connection
        .get("callable")
        .and_then(|callable| callable.try_to::<Callable>().ok())?;

    Some(OpenRecentHandle {
        open_recent,
        recent_scripts_popup,
    })
}

/// Opens `file_path` in the script editor, even if it isn't a script. Returns whether it worked.
///
/// The handle is looked up on every call rather than cached: it holds Godot objects, which must not
/// be kept in a `thread_local` (they'd be dropped at thread exit, after the engine is gone).
pub fn open_text_file(file_path: &str) -> bool {
    let Some(handle) = find_open_recent_handle() else {
        return false;
    };
    let Some(settings) = EditorInterface::singleton().get_editor_settings() else {
        return false;
    };

    // Add the file to the most recent files at index 0.
    let mut recent_files = settings
        .get_project_metadata_ex("recent_files", "scripts")
        .default(&VarArray::new().to_variant())
        .done()
        .try_to::<VarArray>()
        .unwrap_or_default();
    let path_variant = file_path.to_variant();
    let mut idx = recent_files.find(&path_variant, None);
    if idx.is_none() {
        // Calling the open-recent callable with the last item of the popup (the "Clear Recent Files" entry)
        // clears the project metadata and calls _update_recent_files deferred, which is what we want.
        handle
            .open_recent
            .call(&[(handle.recent_scripts_popup.get_item_count() - 1).to_variant()]);
        recent_files.insert(0, &path_variant);
        // Re-set the project metadata so that it updates.
        let mut settings = settings.clone();
        settings.set_project_metadata("recent_files", "scripts", &recent_files.to_variant());
        idx = Some(0);
    }
    handle
        .open_recent
        .call(&[(idx.unwrap_or(0) as i64).to_variant()]);
    true
}
