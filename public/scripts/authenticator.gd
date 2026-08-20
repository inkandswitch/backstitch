@tool
extends Control
class_name BackstitchAuthenticator

var current_url := ""

func _ready() -> void:
	if is_part_of_edited_scene():
		return
		
	%AuthenticatorDialog.canceled.connect(self._auth_canceled)
	%AuthenticatorDialog.confirmed.connect(self._reopen_url)

	%AuthenticatorDialog.visible = false
	
	GodotProject.auth_status_changed.connect(self._auth_status_changed)

func _auth_status_changed(status: Variant) -> void:
	match status.status:
		"idle":
			%AuthenticatorDialog.visible = false
		"needs_user_login":
			_open(status.url)
			%AuthenticatorDialog.popup_centered()
			%AuthenticatorDialog.dialog_text = "Waiting for sign-in to complete in your browser..." 
		"needs_user_logout":
			_open(status.url)
			%AuthenticatorDialog.popup_centered()
			%AuthenticatorDialog.dialog_text = "Waiting for sign-out to complete in your browser..." 


func _open(url) -> void:
	current_url = url
	OS.shell_open(url)

func _reopen_url() -> void:
	%AuthenticatorDialog.popup_centered()
	_open(current_url)

func _auth_canceled() -> void:
	GodotProject.cancel_authenticate()
