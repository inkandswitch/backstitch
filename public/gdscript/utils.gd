class_name PatchworkUtils

static func short_hash(hash: String) -> String:
	return hash.substr(0, 7)

static func add_listener_disable_button_if_text_is_empty(button: Button, line_edit: LineEdit):
	var listener = func(new_text: String):
		button.disabled = new_text.strip_edges().is_empty()
	line_edit.text_changed.connect(listener)
	listener.call(line_edit.text)


static var void_func = func(): return
static func popup_box(parent_window: Node, dialog: AcceptDialog, message: String, box_title: String, confirm_func: Callable = func(): pass, cancel_func: Callable = func(): pass):
	if (dialog == null):
		dialog = AcceptDialog.new()
	if (dialog.get_parent() != parent_window):
		if (dialog.get_parent() == null):
			parent_window.add_child(dialog)
		else:
			dialog.reparent(parent_window)
	dialog.reset_size()
	dialog.set_text(message)
	dialog.set_title(box_title)
	var _confirm_func: Callable
	var _cancel_func: Callable
	var arr = dialog.get_signal_connection_list("confirmed")
	for dict in arr:
		dialog.disconnect("confirmed", dict.callable)
	arr = dialog.get_signal_connection_list("canceled")
	for dict in arr:
		dialog.disconnect("canceled", dict.callable)
	dialog.connect("confirmed", confirm_func)
	dialog.connect("canceled", cancel_func)
	dialog.popup_centered()

static func unsaved_files_open() -> bool:
	if EditorInterface.get_script_editor().get_unsaved_files().size() > 0:
		return true
	if EditorInterface.get_unsaved_scenes().size() > 0:
		return true
	return false

static func create_unsaved_files_dialog(parent: Control, message: String):
	if unsaved_files_open():
		var dialog = AcceptDialog.new()
		dialog.title = "Unsaved Files"
		dialog.dialog_text = message
		dialog.get_ok_button().text = "OK"

		dialog.confirmed.connect(func():
			dialog.queue_free()
		)

		parent.add_child(dialog)
		dialog.popup_centered()
		return true
	return false