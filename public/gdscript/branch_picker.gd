@tool
extends OptionButton
class_name PatchworkBranchPicker

@export var override_icon: Texture2D
@export var max_char_length: int = -1

signal branch_selected(id: String)

func _ready():
	if is_part_of_edited_scene():
		return
	item_selected.connect(_on_item_selected)

# Populate the branch picker with branches.
func populate() -> void:
	if !GodotProject.has_project(): return
	clear()

	var main_branch = GodotProject.get_main_branch();
	var checked_out_branch = GodotProject.get_checked_out_branch()
	if !checked_out_branch or !main_branch:
		return

	_add_branch_to_picker(main_branch, checked_out_branch.id)
	_update_selected(checked_out_branch.name)

# Recursively add a branch and all of its child forks to the branch picker.
func _add_branch_to_picker(branch: Dictionary, selected_branch_id: String, indentation: String = "", is_last: bool = false) -> void:
	if !branch.is_available: return

	var label
	if !branch.parent:
		label = branch.name
	else:
		var connection = "└─ " if is_last else "├─ "
		label = indentation + connection + branch.name

	var branch_index = get_item_count()
	add_item(label, branch_index)

	if !GodotProject.is_branch_loaded(branch.id):
		set_item_icon(branch_index, load("res://addons/patchwork/public/icons/NodeWarning.svg"))

	set_item_metadata(branch_index, branch.id)

	if branch.id == selected_branch_id:
		select(branch_index)

	var new_indentation
	if !branch.parent:
		new_indentation = ""
	else:
		if is_last:
			new_indentation = indentation + "    "
		else:
			new_indentation = indentation + "│   "

	for i in range(branch.children.size()):
		var child = branch.children[i]
		var is_last_child = i == branch.children.size() - 1
		_add_branch_to_picker(GodotProject.get_branch(child), selected_branch_id, new_indentation, is_last_child)

		
func _on_item_selected(_index: int) -> void:
	var selected_branch = GodotProject.get_branch(get_item_metadata(_index))
	
	if (!selected_branch):
		PatchworkUtils.popup_box(self, $ErrorDialog, "Branch not found", "Error")
		populate()
		return

	if !GodotProject.is_branch_loaded(selected_branch.id):
		# Show warning dialog that branch is not synced correctly
		var dialog = AcceptDialog.new()
		dialog.title = "Branch Not Available"
		dialog.dialog_text = "Can't checkout branch because it is not synced yet"
		dialog.get_ok_button().text = "OK"
		dialog.canceled.connect(func(): dialog.queue_free())
		dialog.confirmed.connect(func(): dialog.queue_free())

		add_child(dialog)
		dialog.popup_centered()

		# Return early to prevent checkout attempt
		populate()
		return

	if not selected_branch:
		printerr("no selected branch")
		populate()
		return

	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before checking out another branch."):
		populate()
		return;

	_update_selected(selected_branch.name)
	branch_selected.emit(selected_branch.id)

func _update_selected(name: String):
	# since we want a different label without indentation for the selected branch,
	# just set it manually here.
	text = name.substr(0, max_char_length) if max_char_length > 0 else name
	if text != name:
		text += "..."
	tooltip_text = name
	if override_icon:
		icon = override_icon