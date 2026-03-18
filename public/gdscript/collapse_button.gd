@tool
extends Button

# This script makes a button collapse if it's too big, but otherwise
# shrink to minimum size. Godot UI is awful and doesn't let us do this without
# a script...

func _ready():
	resized.connect(_update_mode)
	if get_parent():
		get_parent().resized.connect(_update_mode)
	_update_mode()

# This is the weird part... we always the content to no trimming, then measure size.
# Afterwards, we restore the old behavior.
# If we want this to work in an HBox with other stuff, this needs refactoring.
func _update_mode():
	if !get_parent():
		return

	var container_width = get_parent().size.x
	
	var old_behavior = text_overrun_behavior
	text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
	minimum_size_changed.emit()

	var width = get_combined_minimum_size().x

	if width <= container_width - 1:
		text_overrun_behavior = TextServer.OVERRUN_NO_TRIMMING
		size_flags_horizontal = Control.SIZE_SHRINK_END ^ Control.SIZE_EXPAND
	else:
		text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
		size_flags_horizontal = Control.SIZE_EXPAND_FILL
