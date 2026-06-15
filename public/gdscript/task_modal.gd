@tool
class_name TaskModal
extends Window


var center_panel: PanelContainer
var main: VBoxContainer
var cancel_hb: HBoxContainer
var cancel: Button
var canceled: bool = false
var tasks: Dictionary = {}
var main_border_size: Vector2 = Vector2.ZERO
var queued_calls: Array[Callable] = []

class Task extends VBoxContainer:
	var description: String
	var steps: int
	var can_cancel: bool
	var canceled: bool = false
	var progress: ProgressBar
	var state: Label

	func add_margin_child(p_label: String, p_control: Control, expand: bool = false) -> void:
		var label: Label = Label.new()
		label.theme_type_variation = "HeaderSmall"
		label.text = p_label
		self.add_child(label)
		var mc: MarginContainer = MarginContainer.new()
		mc.add_child(p_control, true)
		self.add_child(mc)
		if expand:
			p_control.size_flags_vertical = Control.SIZE_EXPAND_FILL
		p_control.accessibility_name = p_label

	func _init(name: String, description: String, steps: int, indeterminate: bool = false, can_cancel: bool = false) -> void:
		self.name = name
		self.description = description
		self.steps = steps
		self.can_cancel = can_cancel
		var vb2 = VBoxContainer.new()
		self.add_margin_child(name, vb2)
		self.progress = ProgressBar.new()
		self.progress.set_process(true)
		self.progress.process_mode = ProgressBar.PROCESS_MODE_ALWAYS
		self.progress.indeterminate = indeterminate
		self.progress.theme_type_variation = "PopupProgressBar"
		if indeterminate:
			self.steps = 1
			self.progress.indeterminate = true
		self.progress.max_value = self.steps
		self.progress.value = self.steps
		vb2.add_child(self.progress)
		self.state = Label.new()
		self.state.clip_text = true
		vb2.add_child(self.state)

	func step(p_state: String, p_step: int) -> void:
		self.state.text = p_state
		if self.progress.indeterminate:
			return
		if p_step < 0:
			progress.value = progress.value + 1
		else:
			progress.value = p_step

	func cancel() -> void:
		if can_cancel and not canceled:
			canceled = true
			self.state.text = "Canceling..."
			self.progress.indeterminate = true
			self.progress.value = 0


func _update_ui() -> void:
	# no way to iterate the main loop in gdscript, so nothing to do here
	pass


func _on_parent_visibility_changed() -> void:
	if not Thread.is_main_thread():
		self.call_deferred("_on_parent_visibility_changed")
		return
	if not self.get_parent() or not self.get_parent().visible:
		_reparent_and_show()

func _reparent_and_show() -> void:
	var current_window: Window = get_tree().root.get_last_exclusive_window()

	while current_window != null and (current_window == self or not (current_window is Window) or not current_window.visible):
		current_window = current_window.get_parent()

	if not current_window:
		printerr("No current window found!!")
		return

	if current_window is FileDialog and current_window.use_native_dialog:
		# This is is a native file dialog and is likely about to close, so we need to wait for it to close before showing our modal
		self.call_deferred("_reparent_and_show")
		return

	if is_inside_tree():
		if current_window != self && get_parent() != current_window:
			reparent(current_window)

	if not is_inside_tree():
		popup_exclusive_centered(current_window, center_panel.custom_minimum_size)
	else:
		popup_centered(center_panel.custom_minimum_size)
	self.transient = false
	self.exclusive = true
	self.always_on_top = true
	if not current_window.visibility_changed.is_connected(_on_parent_visibility_changed):
		current_window.visibility_changed.connect(_on_parent_visibility_changed)

func _popup() -> void:
	# if not 
	var ms: Vector2 = main.get_combined_minimum_size();
	ms.x = max(500 * EditorInterface.get_editor_scale(), ms.x);
	ms += main_border_size;
	center_panel.custom_minimum_size = ms

	if self.is_node_ready():
		_reparent_and_show()
	else:
		self.call_deferred("_reparent_and_show")


func _on_cancel_pressed() -> void:
	canceled = true
	for task in tasks.values():
		task.cancel()

func _init() -> void:
	self.always_on_top = true
	self.exclusive = true
	self.transient = false
	self.borderless = true
	self.keep_title_visible = false
	self.process_mode = Node.PROCESS_MODE_ALWAYS
	# title = "Backstitch Loading..."
	
	center_panel = PanelContainer.new()
	add_child(center_panel)
	center_panel.size_flags_horizontal = Control.SIZE_SHRINK_BEGIN
	center_panel.size_flags_vertical = Control.SIZE_SHRINK_END
	main = VBoxContainer.new()
	center_panel.add_child(main)
	cancel_hb = HBoxContainer.new()
	center_panel.add_child(cancel_hb)
	cancel_hb.hide()

	cancel = Button.new()
	cancel_hb.add_spacer(false)
	cancel_hb.add_child(cancel)
	cancel.text = "Cancel"
	cancel_hb.add_spacer(false)
	cancel.pressed.connect(_on_cancel_pressed)
	self.child_entered_tree.connect(_on_child_added)

func _on_child_added(child: Node) -> void:
	if child == center_panel:
		return
	# TODO: something to prevent ProgressDialog from covering the window?
	# Don't think this is necessary
	print("child added: %s (%s)" % [child.name, child.get_class()])
	pass

func _notification(what: int) -> void:
	match what:
		NOTIFICATION_THEME_CHANGED:
			var style: StyleBox = main.get_theme_stylebox("panel", "PopupMenu")
			main_border_size = style.get_minimum_size()
			main.set_offset(SIDE_LEFT, style.get_margin(SIDE_LEFT))
			main.set_offset(SIDE_RIGHT, -style.get_margin(SIDE_RIGHT))
			main.set_offset(SIDE_TOP, style.get_margin(SIDE_TOP))
			main.set_offset(SIDE_BOTTOM, -style.get_margin(SIDE_BOTTOM))
			center_panel.add_theme_stylebox_override("panel", get_theme_stylebox("panel", "PopupPanel"))

func _ready() -> void:
	pass

func _add_task(name: String, description: String, steps: int, indeterminate: bool = false, can_cancel: bool = false) -> void:
	canceled = false
	var task: Task = Task.new(name, description, steps, indeterminate, can_cancel)
	main.add_child(task)
	tasks[name] = task
	if can_cancel:
		cancel_hb.show()
	else:
		cancel_hb.hide()
	cancel_hb.move_to_front()
	canceled = false
	_popup()
	if (can_cancel):
		cancel.grab_focus()
	_update_ui()

func _task_step(name: String, state: String, step: int) -> bool:
	var task: Task = tasks.get(name, null)
	if not task:
		printerr("Task %s not found!!", name)
		return canceled
	task.step(state, step)
	_update_ui()
	return task.canceled

func _end_task(name: String) -> void:
	if not tasks.has(name):
		printerr("Task %s not found!!", name)
		return
	var task = tasks.get(name)
	task.queue_free()
	tasks.erase(name)

	if tasks.is_empty():
		hide()
	else:
		_popup()


func start_task(name: String, description: String = "", steps: int = -1, indeterminate: bool = true, can_cancel: bool = false):
	if description == "":
		description = name
	self.queued_calls.append(func():
		self._add_task(name, description, steps, indeterminate, can_cancel)
	)

func task_step(name: String, state: String, step: int):
	self.queued_calls.append(func():
		self._task_step(name, state, step)
	)

func end_task(name: String):
	self.queued_calls.append(func():
		self._end_task(name)
	)

# do_task is a helper function that adds a task to the queue and waits for it to finish
# don't use this if you need to wait for the task to finish, use start task and end task manually instead
func do_task(name: String, task: Callable):
	self.queued_calls.append(func():
		start_task(name)

		self.queued_calls.append(func():
			await task.call()

			end_task(name)
		)
	)

func _process(_delta: float) -> void:
	var calls = self.queued_calls.duplicate(false)
	self.queued_calls.clear()
	for call in calls:
		if call.is_valid():
			call.call()
		else:
			printerr("Invalid call: ", call)
