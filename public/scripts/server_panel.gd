@tool
extends Control
class_name BackstitchServerPanel

func _ready() -> void:
	if is_part_of_edited_scene():
		return

	%ServerSelectorButton.pressed.connect(_on_server_selector_button_pressed)
	%ServerDialog.confirmed.connect(_on_server_selector_confirmed)
	_update_status()

func _process(_delta: float):
	if is_part_of_edited_scene():
		return
	if !GodotProject.has_project():
		_disable()
	else:
		# Just check for the current server every frame; that's easiest/cheapest for now I think
		_update_status()

func _on_server_selector_button_pressed() -> void:
	%ServerPicker.set_selection(GodotProject.get_saved_server())
	%ServerDialog.popup_centered()

func _on_server_selector_confirmed() -> void:
	GodotProject.change_server(%ServerPicker.get_selection())

func _disable():
	%ServerSelectorButton.visible = false

func _update_status() -> void:
	var server = GodotProject.get_saved_server()
	%ServerSelectorButton.visible = true
	
	if !server: 
		%ServerSelectorButton.text = "Connect..."
	else:
		%ServerSelectorButton.text = server
