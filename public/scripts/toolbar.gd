@tool
extends MarginContainer
class_name BackstitchToolbar

@onready var branch_picker: BackstitchBranchPicker = %BranchPicker

func _ready():
	if is_part_of_edited_scene():
		return
	branch_picker.branch_selected.connect(_on_branch_selected)
	GodotProject.state_changed.connect(_on_state_changed);
	branch_picker.populate()

func _on_branch_selected(branch_id: String):
	# this is awful... ideally we don't want UI components cross referencing each other like this.
	# we should abstract out the task modal instead.
	BackstitchSidebar.instance.checkout_branch(branch_id)

func _on_state_changed():
	branch_picker.populate()