@tool
extends Node2D
class_name StickyNote

@export_multiline var note: String:
	set(value):
		note = value
		if content:
			content.text = value

@onready var content: Label = $Content

func _ready():
	content.text = note

	# Hide the note during gameplay
	if not Engine.is_editor_hint():
		hide()
