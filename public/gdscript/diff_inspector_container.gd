@tool
class_name DiffInspectorContainer
extends EditorInspector

@export var added_icon: Texture2D
@export var removed_icon: Texture2D
@export var modified_icon: Texture2D

# signal section_mouse_entered(section: String)

signal node_hovered(file_path: String, node_path: Array)
signal node_unhovered(file_path: String, node_path: Array)


func get_change_theme_color_name(change_type: String) -> String:
	if change_type == "added":
		return "prop_subsection_added"
	elif change_type == "removed":
		return "prop_subsection_removed"
	elif change_type == "modified":
		return "prop_subsection_modified"
	return "prop_subsection"


func set_inspector_change_color(name: String, color: Color) -> void:
	# get the theme override for the given name for the Editor type
	var theme: Theme = get_theme()
	theme.set_color(get_change_theme_color_name(name), "Editor", color)
	self.theme_changed.emit()


func get_color_for_change_type(change_type: String) -> Color:
	var theme: Theme = get_theme()
	return theme.get_color(get_change_theme_color_name(change_type), "Editor")


#3fa62e
@export var added_color: Color = Color("#3fa62e"):
	set(value):
		added_color = value
		set_inspector_change_color(("added"), value)

#a55454
@export var removed_color: Color = Color("#a55454"):
	set(value):
		removed_color = value
		set_inspector_change_color(("removed"), value)

#e2be99
@export var modified_color: Color = Color("#e2be99"):
	set(value):
		modified_color = value
		set_inspector_change_color(("modified"), value)

@onready var main_vbox: VBoxContainer = %DifferMainVBox
var diff_result: Dictionary

var categories: Array = []
var sections: Array = []
var changed_nodes: Array = []
var added_nodes: Array = []
var deleted_nodes: Array = []
# this is really just to keep a reference to the resources that have been changed;
var changed_resources: Array = []
var changed_files: Array = []
var waiting_callables: Array = []
var last_inspected_resource: Object = null

func _ready() -> void:
	pass


# Called every frame. 'delta' is the elapsed time since the previous frame.
func _process(delta: float) -> void:
	var to_call = waiting_callables.duplicate()
	waiting_callables.clear()
	for callable in to_call:
		callable.call()

func _on_button_pressed() -> void:
	pass

func snake_case_to_human_readable(snake_case_string: String) -> String:
	var words = snake_case_string.split("_")
	var title_case_words = []
	for word in words:
		if word.length() > 0:
			title_case_words.append(word[0].to_upper() + word.substr(1))
	return " ".join(title_case_words)


func get_default_val_for_class(node_type: String, prop_name: String, script_class = null):
	# We can't get the default value for a script instance
	var ret = null
	if script_class != null and ClassDB.class_exists(script_class):
		ret = ClassDB.class_get_property_default_value(script_class, prop_name)
	if ret == null and not (node_type.begins_with("Resource(") or node_type.begins_with("ExtResource(")):
		ret = ClassDB.class_get_property_default_value(node_type, prop_name)
	if ret == null:
		return "<default_value>"
	return ret

func add_PropertyDiffResult(inspector_section: DiffInspectorSection, property_diff: Dictionary, node_type: String, script_class = null) -> void:
	var change_type = property_diff["change_type"]
	var prop_name = property_diff["name"]
	var prop_label = snake_case_to_human_readable(property_diff["name"])
	var prop_old = property_diff.get("old_value", null)
	var prop_new = property_diff.get("new_value", null)
	if node_type != "":
		if prop_old == null and change_type != "added":
			prop_old = get_default_val_for_class(node_type, prop_name, script_class)
		if prop_new == null and change_type != "removed":
			prop_new = get_default_val_for_class(node_type, prop_name, script_class)

	# print("!!! adding property diff result for ", prop_name, " with type ", change_type)
	# print("!!! prop_old: ", prop_old)
	# print("!!! prop_new: ", prop_new)

	inspector_section.add_old_and_new(change_type, prop_name, prop_old, prop_new, prop_label)

func get_flat_stylebox(color: Color) -> StyleBoxFlat:
	var stylebox: StyleBoxFlat = StyleBoxFlat.new()
	stylebox.bg_color = color
	return stylebox

func get_node_box(icon: Texture2D, text: String) -> PanelContainer:
	var panel_container: PanelContainer = PanelContainer.new()
	# HBox with two items: The removed icon and a label with the text "Node Deleted"
	var hbox: HBoxContainer = HBoxContainer.new()
	var icon_rect: TextureRect = TextureRect.new()
	icon_rect.texture = icon
	icon_rect.size.x = 60
	icon_rect.size.y = 60
	icon_rect.expand_mode = TextureRect.EXPAND_FIT_WIDTH_PROPORTIONAL
	hbox.add_child(icon_rect)
	var label: Label = Label.new()
	label.text = text
	hbox.add_child(label)
	panel_container.add_child(hbox)
	panel_container.size.y = 60
	return panel_container

func get_node_deleted_box(type: String) -> PanelContainer:
	return get_node_box(removed_icon, type + " Deleted")

func get_node_added_box(type: String) -> PanelContainer:
	return get_node_box(added_icon, type + " Added")



func _on_node_box_clicked(sec: DiffInspectorSection, file_path: String, section: String) -> void:
	do_node_box_click(sec, file_path, section, false)

func do_node_box_click(sec: DiffInspectorSection, file_path: String, section: String, changed_scene: bool = false) -> void:
	# print("!!! box clicked: ", section)
	if !section.begins_with("res://"):
		var node_path = section
		if section.begins_with("./"):
			node_path = node_path.substr(2)

		var node: Node = EditorInterface.get_edited_scene_root()
		if node.scene_file_path != file_path:
			EditorInterface.open_scene_from_path(file_path)
			waiting_callables.append(func():
				self.do_node_box_click(sec, file_path, section, true)
			)
			return
		var node_to_select: Node = node.get_node(node_path)
		if is_instance_valid(node_to_select):
			EditorInterface.edit_node(node_to_select)
			EditorInterface.set_main_screen_editor("2D")
		# if we changed the scene, emit the hovered signal so the sidebar can update the highlight changes
		if changed_scene:
			_on_parent_node_box_hovered(sec, file_path)

func _on_resource_box_clicked(section: String) -> void:
	do_resource_box_click(section)

func do_resource_box_click(section: String) -> bool:
	var file_path = section
	var extension = file_path.get_extension().to_lower()
	EditorInterface.get_file_system_dock().navigate_to_path(file_path)
	var changed_scene = false
	if extension == "tscn" or extension == "scn":
		changed_scene = true
		var curr_root = EditorInterface.get_edited_scene_root()
		if is_instance_valid(curr_root) && curr_root.scene_file_path == file_path:
			pass
			var curr_obj = EditorInterface.get_inspector().get_edited_object()
			var reedit = false
			if curr_obj != curr_root:
				# print("NOT THE SAME OBJECT")
				reedit = true
				if curr_obj is Node && curr_root.is_ancestor_of(curr_obj):
					reedit = false
			if reedit:
				EditorInterface.inspect_object(curr_root, "", false)
		else:
			EditorInterface.open_scene_from_path(file_path)
		# last_inspected_resource = null
		EditorInterface.set_main_screen_editor("2D")
	else:
		var ff = ResourceLoader.load(file_path)
		if is_instance_valid(ff):
			EditorInterface.edit_resource(ff)
	return changed_scene

func _on_scene_resource_box_clicked(sec: DiffInspectorSection, section: String) -> void:
	if do_resource_box_click(section):
		waiting_callables.append(func():
			self._on_parent_node_box_hovered(sec, section)
		)

# This rigamarole is necessary because, for some GODFORSAKEN REASON, ScriptEditor.open_file() is not bound.
# There is literally no other way to open up a non-script text file in the script editor.
# TODO: Push on https://github.com/godotengine/godot/pull/117595 to be merged.
var recent_scripts_popup: PopupMenu = null

# This is the second-worst thing I've ever done.
func get_script_editor_private_opem_recent_method():
	var script_editor = EditorInterface.get_script_editor()
	if script_editor != null:
		var children = script_editor.find_children("*", "VBoxContainer", false, false)
		if children.size() == 0:
			printerr("Couldn't find main vbox!!!!")
			return Callable()
		var vbox_children = children[0].find_children("*", "HBoxContainer", false, false)
		if vbox_children.size() == 0:
			printerr("Could not find menu_hb!!")
			return Callable()
		var menu_hb: HBoxContainer = vbox_children[0]
		var file_button: MenuButton
		for child in menu_hb.get_children(true):
			if child is MenuButton:
				if child.get_text() == "File":
					file_button = child
				else:
					# this should only fail if the translation locale is not english; it should be the first one in any case
					printerr("First file menu isn't File! setting anyway...")
					file_button = child
				break
		if not file_button:
			printerr("NO FILE BUTTON!!!!!")
			return Callable()
		var file_menu_popup: PopupMenu = file_button.get_popup()
		var popup_children = file_menu_popup.find_children("*", "PopupMenu", false, false)
		if popup_children.size() == 0:
			printerr("Could not find recent scripts popup!!!!")
			return Callable()
		recent_scripts_popup = popup_children[0]
		var list = recent_scripts_popup.get_signal_connection_list("id_pressed")
		if list.size() == 0:
			printerr("id_pressed signal connection list is empty!!!!")
			return Callable()
		var dict: Dictionary = list.get(0)
		var callable: Callable = dict.get("callable", Callable())
		return callable
	return Callable()
var private_script_editor_open_recent_scripts_method: Callable = get_script_editor_private_opem_recent_method()

func open_text_file(file_path: String) -> void:
	# add it to the most recent files at index 0
	var arr = EditorInterface.get_editor_settings().get_project_metadata("recent_files", "scripts")
	var idx = arr.find(file_path)
	if idx == -1:
		# we have to clear the recent scripts menu by calling the private_script_editor_open_recent_scripts_method with the last popup menu item index
		# this will simply clear the project metadata and call _update_recent_files deferred, which is what we want
		private_script_editor_open_recent_scripts_method.call(recent_scripts_popup.item_count - 1)
		idx = 0
		arr.insert(0, file_path)
		# re-set the project metadata so that it updates
		EditorInterface.get_editor_settings().set_project_metadata("recent_files", "scripts", arr)
	# call the private method with the index
	private_script_editor_open_recent_scripts_method.call(idx)

func get_text_file_extensions() -> PackedStringArray:
	return EditorInterface.get_editor_settings().get_setting("docks/filesystem/textfile_extensions").split(",")

func open_scene_or_resource_from_path(file_path: String) -> void:
	# don't open import files
	if file_path.get_extension().to_lower() == "import":
		return
	var extension = file_path.get_extension().to_lower()
	if extension == "tscn" or extension == "scn":
		EditorInterface.open_scene_from_path(file_path)
	elif extension == "gd" or extension == "cs" or get_text_file_extensions().has(extension):
		open_text_file(file_path)
	else:
		EditorInterface.edit_resource(ResourceLoader.load(file_path))

func _on_text_box_clicked(section: String) -> void:
	var file_path = section
	var extension = file_path.get_extension().to_lower()
	EditorInterface.get_file_system_dock().navigate_to_path(file_path)
	open_scene_or_resource_from_path(file_path)
	EditorInterface.set_main_screen_editor("Script")


func node_has_editor_properties(node: DiffInspectorSection) -> bool:
	for c in node.get_vbox().get_children():
		if not(c is DiffInspectorSection or c is HSeparator or c is VSeparator):
			return true
	return false

func get_children_paths(node: DiffInspectorSection) -> Array:
	# var children = node_to_children_map[node.get_section()]
	var children = node.get_vbox().get_children()
	var child_paths = []
	if node_has_editor_properties(node):
		child_paths.append(NodePath(node.get_section()))
	for child in children:
		if child is DiffInspectorSection:
			var new_paths = get_children_paths(child)
			if node_has_editor_properties(child):
				child_paths.append(NodePath(child.get_section()))
			child_paths.append_array(new_paths)

	child_paths.sort()
	return child_paths


func _on_parent_node_box_hovered(node: DiffInspectorSection, file_path: String) -> void:
	var child_paths = get_children_paths(node)
	node_hovered.emit(file_path, child_paths)
	# print("!!! parent node box hovered: ", child_paths)

func _on_parent_node_box_unhovered(node: DiffInspectorSection, file_path: String) -> void:
	var child_paths = get_children_paths(node)
	node_unhovered.emit(file_path, child_paths)
	# print("!!! parent node box unhovered: ", child_paths)

func create_node_diff_section(file_section: DiffInspectorSection, node_diff: Dictionary, parent_file_path: String, node_label: String):
	var node_name: String = node_diff["node_path"] # remove the leading "./"
	var change_type: String = node_diff["change_type"]
	# print("!!! adding node diff result for ", node_name, " with type ", change_type)

	var prop_diffs: Dictionary
	var inspector_section: DiffInspectorSection = DiffInspectorSection.new()
	var vbox = inspector_section.get_vbox()
	var fake_node = MissingResource.new()

	var node_type: String = node_diff.get("type", "")
	if (node_type == ""):
		pass
		# print(node_diff)
	var color: Color = added_color
	if change_type == "added":
		color = added_color
		node_label += " (Added)"
		# TODO: make rust code do this
		# print("adding node added box")
		added_nodes.append(fake_node)
	elif change_type == "removed":
		color = removed_color
		node_label += " (Deleted)"
		# print("adding node deleted box")
		deleted_nodes.append(fake_node)
	else:
		color = modified_color
		node_label += " (Modified)"
		changed_nodes.append(fake_node)
	prop_diffs = node_diff.get("changed_props", {})
	if prop_diffs.size() == 0 and change_type == "modified":
		return null

	inspector_section.setup(node_name, node_label, fake_node, color, true, 1, 2)
	inspector_section.set_type(change_type)
	# fake_node.original_class = node_type
	var i = 0
	# get the length of the prop_diffs dictionary
	for prop_name in prop_diffs.keys():
		if i > 0:
			var divider = HSeparator.new()
			vbox.add_child(divider)
		add_PropertyDiffResult(inspector_section, prop_diffs[prop_name], node_type)
		i += 1
	inspector_section.unfold()
	inspector_section.connect("box_clicked", func(section): self._on_node_box_clicked(inspector_section, parent_file_path, section))
	inspector_section.connect("section_mouse_entered", func(_section): self._on_parent_node_box_hovered(inspector_section, parent_file_path))
	inspector_section.connect("section_mouse_exited", func(_section): self._on_parent_node_box_unhovered(inspector_section, parent_file_path))
	sections.append(inspector_section)
	# file_section.get_vbox().add_child(inspector_section)
	return inspector_section

func add_text_diff(inspector_section: DiffInspectorSection, unified_diff: Dictionary) -> void:
	# print("adding text diff")
	var text_diff = TextDifferView.get_text_diff_view(unified_diff, false)
	text_diff.custom_minimum_size = Vector2(100, 500)
	inspector_section.get_vbox().add_child(text_diff)


#we're going to group them together by their parent node(s)
# e.g.: Node1/Node2/Node3/Node4
# 		Node1/Node2/Node3/Node4/Node5
# 		Node1/Node2/Node3/Node4/Node5/Node6
#  -> Node1/Node2/Node3->
# 	  -	Node4/ ->
# 		 - Node5/ ->
# 			 - Node6/ ->
# start by popping off the lefthand side of the node paths

func count_children(sec: DiffInspectorSection) -> int:
	var count = 0
	for child in sec.get_vbox().get_children():
		if child is DiffInspectorSection or child is PanelContainer:
			count += 1
	return count

func pop_node_sections(name: String, map: Dictionary, parent_section: DiffInspectorSection, file_path: String, override_label = null):
		var sec: DiffInspectorSection = null
		override_label = override_label if override_label != null else name.get_file()
		# print("!!! pop_node_sections: ", name)
		# merge the child sections into the parent section
		if map.size() == 1 and not map.has("_diff"):
			var child_key = map.keys()[0]
			var child_diff = map[child_key]
			var new_label = child_key.trim_prefix(name.get_base_dir() + "/")
			pop_node_sections(child_key, child_diff, parent_section, file_path, new_label)
			return

		var added_diff = false
		if map.has("_diff"):
			sec = create_node_diff_section(parent_section, map["_diff"], file_path, override_label)
			if sec != null:
				added_diff = true
				parent_section.get_vbox().add_child(sec)
			elif map.size() == 1:
				return
		if sec == null:
			var fake_node: MissingResource = MissingResource.new()
			sec = DiffInspectorSection.new()
			sec.setup(name, override_label, fake_node, modified_color, true, 0, 1)
			sec.set_type("modified")
			sec.get_vbox().add_child(HSeparator.new())
			changed_nodes.append(fake_node)

			sec.connect("box_clicked", func(section): self._on_node_box_clicked(sec, file_path, section))
			sec.connect("section_mouse_entered", func(_section): self._on_parent_node_box_hovered(sec, file_path))
			sec.connect("section_mouse_exited", func(_section): self._on_parent_node_box_unhovered(sec, file_path))

		for key in map.keys():
			if key == "_diff":
				continue
			else:
				pop_node_sections(key, map[key], sec, file_path, null)
		var count = count_children(sec)
		# merge the child sections into the parent section if there are no diffs
		if not added_diff:
			if count > 1:
				added_diff = true
				parent_section.get_vbox().add_child(sec)
			elif count == 1:
				var child_sec: DiffInspectorSection = sec.get_vbox().get_child(1)
				if child_sec is DiffInspectorSection:
					child_sec.set_label(sec.get_label() + "/" + child_sec.get_label())
					child_sec.reparent(parent_section.get_vbox())
				else:
					added_diff = true
					parent_section.get_vbox().add_child(sec)
		if not added_diff:
			sec.queue_free()

func _pop_child_map(node_diffs: Array) -> Dictionary:
	var child_map: Dictionary = {}
	var to_remove: Array = []
	# node_diffs.sort_custom(func(a, b): return a["node_path"] > b["node_path"])
	for node_diff in node_diffs:
		var node_path: String = node_diff["node_path"]
		# skip temporary nodes created by the instance
		if (node_path.contains("@")):
			continue

		var parts = node_path.split("/")
		if (parts.size() > 2):
			pass
			# print("!!! parts: ", parts)

		for i in range(parts.size() - 1, -1, -1):
			var parent = "/".join(parts.slice(0, i + 1))
			if !child_map.has(parent):
				child_map[parent] = {}
			if i == parts.size() - 1:
				child_map[parent]["_diff"] = node_diff
			else:
				var child = "/".join(parts.slice(0, i + 2))
				child_map[parent][child] = child_map[child]
			child_map[parent].sort()
			if i != 0:
				to_remove.append(parent)

	for key in to_remove:
		child_map.erase(key)
	child_map.sort()
	return child_map



func add_node_diff(file_section: DiffInspectorSection, file_path: String, node_diffs: Array) -> void:
	file_section.get_vbox().add_child(HSeparator.new())
	file_section.connect("section_mouse_entered", func(section): self._on_parent_node_box_hovered(file_section, file_path))
	file_section.connect("section_mouse_exited", func(section): self._on_parent_node_box_unhovered(file_section, file_path))

	file_section.unfold()
	var child_map = _pop_child_map(node_diffs)
	for key in child_map.keys():
		pop_node_sections(key, child_map[key], file_section, file_path)


func add_text_resource_diff(inspector_section: DiffInspectorSection, changed_sub_resources: Array, changed_main_resource: Dictionary) -> void:
	inspector_section.get_vbox().add_child(HSeparator.new())
	if changed_main_resource is Dictionary and changed_main_resource.size() > 0:
		add_sub_resource_diff(inspector_section, changed_main_resource["change_type"], changed_main_resource["sub_resource_id"], changed_main_resource["resource_type"], changed_main_resource["changed_props"], changed_main_resource.get("script_class", null))

	for sub_resource in changed_sub_resources:
		var change_type = sub_resource["change_type"]
		var sub_resource_id = sub_resource["sub_resource_id"]
		var sub_resource_type = sub_resource["resource_type"]
		var changed_properties = sub_resource["changed_props"]
		add_sub_resource_diff(inspector_section, change_type, sub_resource_id, sub_resource_type, changed_properties)

func add_sub_resource_diff(inspector_section: DiffInspectorSection, change_type: String, sub_resource_id: String, sub_resource_type: String, changed_properties: Dictionary, script_class = null) -> void:
	if (changed_properties.size() == 0 and change_type == "modified"):
		# print("!!! no prop diffs for ", sub_resource_id, " with type ", change_type)
		return
	var color: Color = modified_color
	var subresource_label = sub_resource_id
	if subresource_label.is_empty():
		subresource_label = "Main (" + sub_resource_type + ")"
	if change_type == "added":
		color = added_color
		subresource_label += " (Added)"
	elif change_type == "removed":
		color = removed_color
		subresource_label += " (Deleted)"
	elif change_type == "modified":
		color = modified_color
		subresource_label += " (Modified)"

	var i = 0
	var child_section: DiffInspectorSection = DiffInspectorSection.new()
	var fake_node: MissingResource = MissingResource.new()
	fake_node.original_class = sub_resource_type
	child_section.setup(sub_resource_id, subresource_label, fake_node, color, true, 1, 2)
	child_section.set_type(change_type)
	var vbox = child_section.get_vbox()
	vbox.add_child(HSeparator.new())
	# get the length of the prop_diffs dictionary
	for prop_name in changed_properties.keys():
		if i > 0:
			var divider = HSeparator.new()
			vbox.add_child(divider)
		add_PropertyDiffResult(child_section, changed_properties[prop_name], sub_resource_type, script_class)
		i += 1
	inspector_section.get_vbox().add_child(child_section)



func add_FileDiffResult(file_path: String, file_diff: Dictionary) -> void:
	file_path = file_path.simplify_path()
	var label = file_path
	var type = file_diff.get("diff_type", "added_or_removed")
	var change_type = file_diff["change_type"]
	# print("!!! adding file diff result for ", file_path, " with change_type ", change_type, " and type ", type)
	var color: Color
	if (change_type == "added"):
		color = added_color
		label += " (Added)"
	elif (change_type == "removed"):
		color = removed_color
		label += " (Removed)"
	elif (change_type == "modified"):
		color = modified_color
		label += " (Modified)"
	changed_files.append(file_path)
	var fake_node: MissingResource = MissingResource.new()
	changed_files.append(fake_node)
	var inspector_section: DiffInspectorSection = DiffInspectorSection.new()
	inspector_section.setup(file_path, label, fake_node, color, true, 0, 1)
	inspector_section.set_type(change_type)
	var vbox = inspector_section.get_vbox()
	if type == "added_or_removed":
		if change_type == "added":
			vbox.add_child(get_node_added_box("File"))
		elif change_type == "removed":
			vbox.add_child(get_node_deleted_box("File"))
	elif type == "resource_changed":
		var res_old = file_diff.get("old_resource", null)
		var res_new = file_diff.get("new_resource", null)
		inspector_section.add_resource_diff(change_type, file_path, res_old, res_new)
		inspector_section.connect("box_clicked", self._on_resource_box_clicked)
	elif type == "text_changed":
		var text_diff = file_diff["text_diff"]
		add_text_diff(inspector_section, text_diff)
		inspector_section.connect("box_clicked", self._on_text_box_clicked)
	elif type == "text_resource_changed":
		var changed_sub_resources = file_diff["changed_sub_resources"]
		var changed_main_resource = file_diff["changed_main_resource"]
		add_text_resource_diff(inspector_section, changed_sub_resources, changed_main_resource)
		inspector_section.connect("box_clicked", self._on_resource_box_clicked)
	elif type == "scene_changed":
		var node_diffs: Array = file_diff["changed_nodes"]
		node_diffs.sort_custom(func(a, b): return a["node_path"] < b["node_path"])
		inspector_section.connect("box_clicked", func(section): self._on_scene_resource_box_clicked(inspector_section, section))
		add_node_diff(inspector_section, file_path, node_diffs)
		# print("node_diff size: ", node_diffs.size())
		# for node in node_diffs:
		# 	var node_path: String = node["node_path"]
		# 	# skip temporary nodes created by the instance
		# 	if (node_path.contains("@")):
		# 		continue
		# 	add_NodeDiffResult(inspector_section, node, file_path)
	inspector_section.unfold()
	sections.append(inspector_section)
	main_vbox.add_child(inspector_section)

# defs for these are in editor/diff_result.h
func add_diff(diff: Dictionary) -> void:
	# print("ADDING DIFF!!!")
	diff_result = diff
	var size = diff_result.size()
	# print("Diff size: ", size)
	for file in diff_result.keys():
		# print("Adding file diff result for ", file)
		add_FileDiffResult(file, diff_result[file])


func reset() -> void:
	# disconnect from theme changed signal
	var all_signal_connections = self.theme_changed.get_connections()
	for connection in all_signal_connections:
		self.theme_changed.disconnect(connection.callable)
	for section in sections:
		section.queue_free()
	for child in main_vbox.get_children():
		child.queue_free()
	sections.clear()
	categories.clear()
	changed_nodes.clear()
	added_nodes.clear()
	deleted_nodes.clear()
	# changed_resources.clear()


func get_main_vbox() -> VBoxContainer:
	return main_vbox

func _init():
	pass
