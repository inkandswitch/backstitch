@tool
class_name HighlightChangesLayer
extends Node2D

var overlay_size: Vector2
var overlay_position: Vector2
var scene_node: Node
var color_rect: ColorRect
var shader_material: ShaderMaterial

func _ready():
	# Ensure we're visible in the editor
	if Engine.is_editor_hint():
		set_process(true)
		set_notify_transform(true)

	color_rect = ColorRect.new()
	color_rect.position = Vector2(0, 0)
	color_rect.color = Color(1.0, 0.0, 0.0, 0.75)
	color_rect.size = Vector2(1000, 1000)
	color_rect.name = "BackstitchColorRect"
	add_child(color_rect)

	# Create and assign the shader material
	shader_material = ShaderMaterial.new()
	var shader = load("res://addons/backstitch/public/gdscript/highlight_shader.gdshader")
	shader_material.shader = shader
	color_rect.material = shader_material

	# Set default shader parameters
	shader_material.set_shader_parameter("fill_color", Color(0.25, 0.25, 0.25, 0.85))


func _get_fallback_box(node: Node):
	if node is Node2D:
		var node_2d = node as Node2D
		var position = node_2d.global_position
		# A small default size for nodes that don't have a size
		var box = Rect2(position.x - 25, position.y - 25, 50, 50)
		return box
	return null


func update_overlay(scene_changes: Dictionary):
	color_rect.size = overlay_size
	color_rect.global_position = overlay_position

	var normalized_rects = []
	var rect_colors = []

	for node_change in scene_changes.changed_nodes:
		var node_path = node_change.node_path
		var node = scene_node.get_node_or_null(node_path)

		if node == null:
			print("node not found: ", node_path)
			continue

		var box = _get_node_bounding_box(node)

		if box == null or box == Rect2():
			box = _get_fallback_box(node)

		if box != null:
			# Convert to coordinates relative to our overlay
			var rel_pos = box.position - overlay_position

			# Normalize coordinates to 0-1 range
			var normalized_rect = Vector4(
				rel_pos.x / overlay_size.x,
				rel_pos.y / overlay_size.y,
				box.size.x / overlay_size.x,
				box.size.y / overlay_size.y
			)

			normalized_rects.append(normalized_rect)

			if node_change.change_type == "modified":
				rect_colors.append(Color(0.75, 0.75, 0.75, 1.0))
			else:
				rect_colors.append(Color(0.18, 0.8, 0.251, 1.0))


	shader_material.set_shader_parameter("rectangles", normalized_rects)
	shader_material.set_shader_parameter("rectangles_color", rect_colors)
	shader_material.set_shader_parameter("rectangle_count", normalized_rects.size())


static func highlight_changes(root: Node, scene_changes: Dictionary):
	var highlight_changes_layer_container = root.get_node_or_null("BackstitchHighlightChangesLayerContainer")

	if highlight_changes_layer_container == null:
		highlight_changes_layer_container = CanvasLayer.new()
		highlight_changes_layer_container.name = "BackstitchHighlightChangesLayerContainer"
		highlight_changes_layer_container.layer = 1025
		root.add_child(highlight_changes_layer_container)

	var diff_layer = highlight_changes_layer_container.get_node_or_null("BackstitchHighlightChangesLayer")
	var bounding_box = _get_node_bounding_box(root)

	if !bounding_box.is_finite():
		return

	if diff_layer == null:
		diff_layer = HighlightChangesLayer.new()
		diff_layer.name = "BackstitchHighlightChangesLayer"
		diff_layer.scene_node = root
		highlight_changes_layer_container.add_child(diff_layer)

	# bounding box calculation doesn't work perfectly for the root node so we scale it by three to make sure we cover the whole scene
	diff_layer.overlay_size = bounding_box.size * 3
	diff_layer.overlay_position = Vector2(bounding_box.position.x - bounding_box.size.x, bounding_box.position.y - bounding_box.size.y)
	diff_layer.update_overlay(scene_changes)


static func remove_highlight(root: Node):
	var highlight_changes_layer_container = root.get_node_or_null("BackstitchHighlightChangesLayerContainer")

	if highlight_changes_layer_container:
		root.remove_child(highlight_changes_layer_container)
		highlight_changes_layer_container.queue_free()


static func _get_node_bounding_box(node: Node) -> Rect2:
	var bounding_box = Rect2()

	# ignore HighlightChangesLayer
	if node is HighlightChangesLayer:
		return bounding_box

	# Special handling for collision shapes
	if node is CollisionShape2D:
		var shape = node.shape
		var rect = Rect2()

		if shape is CircleShape2D:
			var radius = shape.radius
			rect = Rect2(-radius, -radius, radius * 2, radius * 2)
		elif shape is RectangleShape2D:
			var extents = shape.extents
			rect = Rect2(-extents.x, -extents.y, extents.x * 2, extents.y * 2)
		elif shape is CapsuleShape2D:
			var radius = shape.radius
			var height = shape.height
			rect = Rect2(-radius, -height / 2 - radius, radius * 2, height + radius * 2)

		# Convert to global coordinates
		var transform = node.get_global_transform()
		rect = transform * rect
		bounding_box = rect

	# Check if the node has a get_rect method
	elif node.has_method("get_rect"):
		var rect = node.get_rect()
		# Convert to global coordinates
		if node.has_method("get_global_transform"):
			var transform = node.get_global_transform()
			rect = transform * rect
		bounding_box = rect
		
	# Recursively process all children
	for child in node.get_children():
		# Get the bounding box of the child
		var child_rect = _get_node_bounding_box(child)

		# Skip empty rects
		if child_rect == null or child_rect.size.x <= 0 or child_rect.size.y <= 0:
			continue

		# If this is the first valid rect, use it directly
		if bounding_box == null:
			bounding_box = child_rect
		else:
			bounding_box = bounding_box.merge(child_rect)

	return bounding_box
