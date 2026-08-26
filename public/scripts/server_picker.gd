@tool
extends Control
class_name BackstitchServerPicker

var _selection := ""

const status_warning_icon = preload("res://addons/backstitch/public/icons/StatusWarning.svg")
const status_sync_icon = preload("res://addons/backstitch/public/icons/StatusSync.svg")
const status_success_icon = preload("res://addons/backstitch/public/icons/StatusSuccess.svg")
const status_error_icon = preload("res://addons/backstitch/public/icons/StatusError.svg")

func _ready() -> void:
	if is_part_of_edited_scene():
		return

	GodotProject.server_status_changed.connect(self._update_status)

	print("Initializing server picker...")
	%AddServerButton.pressed.connect(self._on_add_server_button_pressed)
	%RemoveServerButton.pressed.connect(self._on_remove_server_button_pressed)
	%ServerPicker.item_selected.connect(self._on_server_picker_item_selected)
	print("connecting retry")
	%RetryButton.pressed.connect(self._on_retry_button_pressed)
	%LoginButton.pressed.connect(self._on_login_button_pressed)

	%AlertPopup.visible = false
	%AddServerDialog.visible = false
	%AddServerDialog.confirmed.connect(self._on_add_server_confirmed)
	%LoginButton.visible = false
	# %RetryButton.visible = false
	BackstitchUtils.style_button(%AddServerButton)
	BackstitchUtils.style_button(%RemoveServerButton)
	# BackstitchUtils.style_button(%RetryButton)
	self._update_server_picker()

func _process(_delta: float) -> void:
	if is_part_of_edited_scene():
		return

	if _selection != "":
		%Status.visible = true
	else:
		%Status.visible = false
		

func set_selection(selection: String) -> void:
	_selection = selection
	_update_server_picker()

func get_selection() -> String:
	return _selection

func _update_status() -> void:
	var status = GodotProject.ping_server(_selection, false)
	%Status.visible = true
	var image := status_sync_icon
	var text := "Checking server status..."
	
	match status.status:
		"auth_needed":
			%LoginButton.visible = true
			%RetryButton.visible = false
			image = status_warning_icon
			text = "You need to sign in to %s before you can use this server." % status.provider
		"failed":
			%LoginButton.visible = false
			%RetryButton.visible = true
			image = status_error_icon
			text = "The server can't be reached."
		"ready":
			%RetryButton.visible = false
			%LoginButton.visible = false
			image = status_success_icon
			if status.authenticated:
				text = "You're signed-in to the server as %s!" % status.username
			else:
				text = "The server is up and reachable!"
		_:
			%LoginButton.visible = false
			%RetryButton.visible = false


	%StatusIcon.texture = image
	%StatusText.text = text
	
func _on_retry_button_pressed() -> void:
	GodotProject.ping_server(_selection, true)
	_update_status()

func _on_login_button_pressed() -> void:
	GodotProject.authenticate_server(_selection)

func _on_add_server_button_pressed() -> void:
	%AddServerDialog.popup_centered()

func _on_remove_server_button_pressed() -> void:
	var text = %ServerPicker.get_item_text(%ServerPicker.selected).strip_edges()
	GodotProject.remove_server(text)
	_update_server_picker()

func _on_server_picker_item_selected(item: int) -> void:
	var text = %ServerPicker.get_item_text(%ServerPicker.selected).strip_edges()
	if text == "(No server)": text = ""
	_selection = text

	_update_server_picker()

func _on_add_server_confirmed() -> void:
	var server = %AddServerEntry.text.strip_edges()
	%AddServerEntry.text = ""
	var validated = GodotProject.validate_server(server)

	if !validated:
		%AlertPopup.popup_centered()
		%AlertPopup.dialog_text = "The server %s isn't valid. It must look like \"https://example.com/\"." % server
	else:
		_selection = validated
		GodotProject.add_server(validated)
		_update_server_picker()
		

func _update_server_picker() -> void:
	%ServerPicker.clear()
	var index := 0
	%ServerPicker.add_item("(No server)", index)
	%ServerPicker.select(index)
	
	for server in GodotProject.get_available_servers():
		index += 1
		%ServerPicker.add_item(server, index)
		if _selection == server:
			%ServerPicker.select(index)

	%RemoveServerButton.visible = _selection != ""
	%AlphaWarning.visible = _selection.contains("alpha.backstitch.dev")
	_update_status()
