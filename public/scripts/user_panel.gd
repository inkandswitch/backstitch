@tool
extends Control
class_name BackstitchUserPanel

# Three possible states here:
# - The user is disconnected from a server, or the server doesn't have auth.
#   -> Show the user's saved name on the button; allow changing the name.
# - The user is connected to a server that requires log-in, and is unauthenticated:
#   -> Show the "Sign In" button
# - The user is connected to a server that requires log-in, and is authenticated:
#   -> Show the user's auth'd name on the button; clicking it shows info panel
#      with "Logout" button

func _ready() -> void:
	if is_part_of_edited_scene():
		return

	%LoginButton.pressed.connect(_on_login_button_pressed)
	%LogoutButton.pressed.connect(_on_logout_button_pressed)
	%UserButton.pressed.connect(_on_user_button_pressed)

	%UserDialog.confirmed.connect(_on_user_name_finished)
	%UserDialog.canceled.connect(_on_user_name_finished)

	GodotProject.server_status_changed.connect(_update_self)
	GodotProject.auth_status_changed.connect(_update_self)
	# this maybe not super reliable -- we're assuming changing a server
	# always updates the sync status when we need to change servers.
	# Ideally a server change actually has a server_changed event or smth
	GodotProject.sync_status_changed.connect(_update_self)

	self._update_self()

func _process(_delta: float) -> void:
	if is_part_of_edited_scene():
		return
	visible = GodotProject.has_project()

func _update_self() -> void:
	var server = GodotProject.get_saved_server()
	if server:
		var status = GodotProject.ping_server(server, false)
		match status.status:
			"auth_needed":
				_set_logged_out_mode(status.provider)
			"ready":
				%UserButton.visible = true
				%LoginButton.visible = false
				%UserButton.text = status.username

				# If we're logged in, we have a different popup UI
				if status.authenticated:
					_set_logged_in_mode(status)
				else:
					_set_no_auth_mode()
			_:
				_set_no_auth_mode()
	else: 
		_set_no_auth_mode()

func _set_logged_in_mode(status: Variant):
	%UserDialog.title = "Signed-in as %s" % status.username
	%LoginButton.visible = false
	%UserButton.visible = true
	%UserButton.text = status.username
	%LogoutButton.visible = true
	%UserLabel.visible = true
	%UserLabel.text = "Name: %s" % status.username
	%EmailLabel.visible = true
	%EmailLabel.text = "Email: %s" % status.email
	%ProviderLabel.visible = true
	%ProviderLabel.text = "Provider: %s" % status.provider
	%UserNameEntry.visible = false

func _set_logged_out_mode(provider: String):
	%LoginButton.visible = true
	%UserButton.visible = false
	%LogoutButton.visible = false
	%UserNameEntry.visible = false
	%UserLabel.visible = false
	%EmailLabel.visible = false
	%ProviderLabel.visible = false
	%LoginButton.tooltip_text = "Sign in with %s" % provider

func _set_no_auth_mode():
	%UserDialog.title = "Set Username"
	%LoginButton.visible = false
	%UserButton.visible = true
	%LogoutButton.visible = false
	%UserLabel.visible = false
	%EmailLabel.visible = false
	%ProviderLabel.visible = false
	%UserNameEntry.visible = true
	var name = GodotProject.get_user_name()
	if name == "": name = "Anonymous"
	%UserButton.text = name

func _on_login_button_pressed():
	GodotProject.authenticate_server(GodotProject.get_saved_server())

func _on_logout_button_pressed() -> void:
	#GodotProject.deauthenticate_server(GodotProject.get_saved_server())
	%UserDialog.visible = false # close

func _on_user_button_pressed():
	%UserNameEntry.text = GodotProject.get_user_name()
	%UserDialog.popup_centered()

func _on_user_name_finished():
	if %UserNameEntry.visible:
		var new_user_name = %UserNameEntry.text.strip_edges()
		if new_user_name != "": GodotProject.set_user_name(new_user_name)
	_update_self()
