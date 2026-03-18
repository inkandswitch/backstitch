@tool
class_name MonkeyTester
extends Node

@export var enabled: bool = false
@export var verbose_logging: bool = false
@export var max_position_delta: float = 16.0
@export var max_rotation_delta: float = 0.25
@export var max_scale_delta: float = 0.2

var action_attempts: int = 0
var action_successes: int = 0
var action_failures: int = 0
var save_successes: int = 0
var save_failures: int = 0

var _action_names: PackedStringArray = []
var _actions: Array[Callable] = []

func _ready() -> void:
	_action_names = [
		"tweak_property",
		"add_node",
		"delete_node",
		"reparent_or_move",
		"duplicate_node",
	]
	_actions = [
		Callable(self, "_action_tweak_property"),
		Callable(self, "_action_add_node"),
		Callable(self, "_action_delete_node"),
		Callable(self, "_action_reparent_or_move"),
		Callable(self, "_action_duplicate_node"),
	]

func _process(_delta: float) -> void:
	if not enabled or not Engine.is_editor_hint():
		return

	var edited_root: Node = EditorInterface.get_edited_scene_root()
	if edited_root == null or not is_instance_valid(edited_root):
		return

	var all_nodes: Array[Node] = _collect_nodes(edited_root)
	if all_nodes.is_empty() or _actions.is_empty():
		return

	var action_index: int = randi_range(0, _actions.size() - 1)
	var action_name: String = _action_names[action_index]
	action_attempts += 1

	var result: Dictionary = _actions[action_index].call(all_nodes, edited_root)
	var ok: bool = bool(result.get("ok", false))
	var details: String = String(result.get("details", ""))

	if ok:
		action_successes += 1
	else:
		action_failures += 1

	if verbose_logging:
		print("MonkeyTester action=", action_name, " ok=", ok, " details=", details)

	_save_current_scene()

func _action_tweak_property(all_nodes: Array[Node], _edited_root: Node) -> Dictionary:
	var node: Node = _pick_random_node(all_nodes)
	if node == null:
		return _fail("no node to tweak")

	var tweak_mode: int = randi_range(0, 4)
	match tweak_mode:
		0:
			_mutate_name(node)
			return _ok("renamed " + String(node.name))
		1:
			if node is Node2D:
				_mutate_node_2d(node)
				return _ok("mutated Node2D transform")
		2:
			if node is Node3D:
				_mutate_node_3d(node)
				return _ok("mutated Node3D transform")
		3:
			if node is CanvasItem:
				var canvas_item: CanvasItem = node
				canvas_item.visible = not canvas_item.visible
				return _ok("toggled visibility")
		4:
			node.set_process_priority(node.get_process_priority() + randi_range(-1, 1))
			return _ok("changed process priority")

	_mutate_name(node)
	return _ok("fallback rename " + String(node.name))

func _action_add_node(all_nodes: Array[Node], edited_root: Node) -> Dictionary:
	var parent: Node = _pick_random_node(all_nodes)
	if parent == null:
		return _fail("no parent candidate")

	var classnames: PackedStringArray = [
		"Node",
		"Node2D",
		"Node3D",
		"Marker2D",
		"Marker3D",
		"Control",
	]
	var classname: String = classnames[randi_range(0, classnames.size() - 1)]
	var instance: Variant = ClassDB.instantiate(classname)
	var new_node: Node = instance if instance is Node else Node.new()

	new_node.name = StringName("Monkey_" + classname + "_" + str(randi_range(1000, 9999)))
	parent.add_child(new_node)
	_set_owner_recursive(new_node, edited_root)
	_apply_initial_random_transform(new_node)
	return _ok("added " + classname + " under " + String(parent.name))

func _action_delete_node(all_nodes: Array[Node], edited_root: Node) -> Dictionary:
	var candidates: Array[Node] = _non_root_nodes(all_nodes, edited_root)
	if candidates.is_empty():
		return _fail("no deletable nodes")

	var victim: Node = _pick_random_node(candidates)
	if victim == null:
		return _fail("invalid victim")

	var parent: Node = victim.get_parent()
	if parent == null:
		return _fail("victim has no parent")

	parent.remove_child(victim)
	victim.queue_free()
	return _ok("deleted " + String(victim.name))

func _action_reparent_or_move(all_nodes: Array[Node], edited_root: Node) -> Dictionary:
	if randi() % 2 == 0:
		var move_result: Dictionary = _try_move_child(all_nodes)
		if bool(move_result.get("ok", false)):
			return move_result

	return _try_reparent_node(all_nodes, edited_root)

func _action_duplicate_node(all_nodes: Array[Node], edited_root: Node) -> Dictionary:
	var candidates: Array[Node] = _non_root_nodes(all_nodes, edited_root)
	if candidates.is_empty():
		return _fail("no duplicable nodes")

	var source: Node = _pick_random_node(candidates)
	if source == null:
		return _fail("invalid source")

	var parent: Node = source.get_parent()
	if parent == null:
		return _fail("source has no parent")

	var clone: Node = source.duplicate()
	if clone == null:
		return _fail("duplicate failed")

	clone.name = StringName(String(source.name) + "_Dup" + str(randi_range(10, 99)))
	parent.add_child(clone)
	_set_owner_recursive(clone, edited_root)
	return _ok("duplicated " + String(source.name))

func _try_move_child(all_nodes: Array[Node]) -> Dictionary:
	var parents: Array[Node] = []
	for node in all_nodes:
		if node != null and node.get_child_count() > 1:
			parents.append(node)
	if parents.is_empty():
		return _fail("no movable child candidates")

	var parent: Node = _pick_random_node(parents)
	if parent == null:
		return _fail("invalid move parent")

	var child_index: int = randi_range(0, parent.get_child_count() - 1)
	var child: Node = parent.get_child(child_index)
	if child == null:
		return _fail("invalid child")

	var new_index: int = randi_range(0, parent.get_child_count() - 1)
	parent.move_child(child, new_index)
	return _ok("moved " + String(child.name) + " to index " + str(new_index))

func _try_reparent_node(all_nodes: Array[Node], edited_root: Node) -> Dictionary:
	var candidates: Array[Node] = _non_root_nodes(all_nodes, edited_root)
	if candidates.is_empty():
		return _fail("no reparentable nodes")

	for _attempt in 8:
		var node: Node = _pick_random_node(candidates)
		var new_parent: Node = _pick_random_node(all_nodes)
		if node == null or new_parent == null:
			continue
		if node == new_parent:
			continue
		if node.is_ancestor_of(new_parent):
			continue
		if node.get_parent() == new_parent:
			continue

		node.reparent(new_parent, true)
		_set_owner_recursive(node, edited_root)
		return _ok("reparented " + String(node.name) + " under " + String(new_parent.name))

	return _fail("no valid reparent target")

func _collect_nodes(root: Node) -> Array[Node]:
	var out: Array[Node] = []
	if root == null:
		return out

	var stack: Array[Node] = [root]
	while not stack.is_empty():
		var current: Node = stack.pop_back()
		if current == null or not is_instance_valid(current):
			continue

		out.append(current)
		for i in current.get_child_count():
			var child: Node = current.get_child(i)
			if child != null:
				stack.append(child)
	return out

func _non_root_nodes(all_nodes: Array[Node], root: Node) -> Array[Node]:
	var out: Array[Node] = []
	for node in all_nodes:
		if node != null and node != root:
			out.append(node)
	return out

func _pick_random_node(nodes: Array[Node]) -> Node:
	if nodes.is_empty():
		return null
	return nodes[randi_range(0, nodes.size() - 1)]

func _mutate_name(node: Node) -> void:
	node.name = StringName("MonkeyNode_" + str(randi_range(1000, 9999)))

func _mutate_node_2d(node: Node2D) -> void:
	node.position += Vector2(
		randf_range(-max_position_delta, max_position_delta),
		randf_range(-max_position_delta, max_position_delta)
	)
	node.rotation += randf_range(-max_rotation_delta, max_rotation_delta)
	node.scale += Vector2(
		randf_range(-max_scale_delta, max_scale_delta),
		randf_range(-max_scale_delta, max_scale_delta)
	)

func _mutate_node_3d(node: Node3D) -> void:
	node.position += Vector3(
		randf_range(-max_position_delta, max_position_delta),
		randf_range(-max_position_delta, max_position_delta),
		randf_range(-max_position_delta, max_position_delta)
	)
	node.rotation += Vector3(
		randf_range(-max_rotation_delta, max_rotation_delta),
		randf_range(-max_rotation_delta, max_rotation_delta),
		randf_range(-max_rotation_delta, max_rotation_delta)
	)
	node.scale += Vector3(
		randf_range(-max_scale_delta, max_scale_delta),
		randf_range(-max_scale_delta, max_scale_delta),
		randf_range(-max_scale_delta, max_scale_delta)
	)

func _apply_initial_random_transform(node: Node) -> void:
	if node is Node2D:
		_mutate_node_2d(node)
	elif node is Node3D:
		_mutate_node_3d(node)

func _set_owner_recursive(node: Node, owner: Node) -> void:
	if node == null or owner == null:
		return
	node.owner = owner
	for i in node.get_child_count():
		_set_owner_recursive(node.get_child(i), owner)

func _save_current_scene() -> void:
	var error_code: int = EditorInterface.save_scene()
	if error_code == OK:
		save_successes += 1
		return

	save_failures += 1
	if verbose_logging:
		printerr("MonkeyTester save_scene failed with code: ", error_code)

func get_stats() -> Dictionary:
	return {
		"action_attempts": action_attempts,
		"action_successes": action_successes,
		"action_failures": action_failures,
		"save_successes": save_successes,
		"save_failures": save_failures,
	}

func _ok(details: String = "") -> Dictionary:
	return {"ok": true, "details": details}

func _fail(details: String = "") -> Dictionary:
	return {"ok": false, "details": details}
