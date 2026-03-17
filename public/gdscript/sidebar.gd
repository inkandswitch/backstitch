@tool
class_name PatchworkSidebar
extends MarginContainer

const diff_inspector_script = preload("res://addons/patchwork/public/gdscript/diff_inspector_container.gd")
const branch_icon_history = preload("res://addons/patchwork/public/icons/Branch24.svg")

# Status bar
@onready var sync_button: Button = %SyncButton
@onready var copy_project_id_button: Button = %CopyProjectIdButton
@onready var share_button: Button = %ShareButton
@onready var action_menu_button: MenuButton = %ActionMenuButton

# Branch bar
@onready var branch_picker: PatchworkBranchPicker = %BranchPicker
@onready var fork_button: Button = %ForkButton
@onready var merge_button: Button = %MergeButton

# History panel
@onready var history_tree: Tree = %HistoryTree
@onready var history_list_popup: PopupMenu = %HistoryListPopup

# Changes panel
@onready var inspector: DiffInspectorContainer = %BigDiffer

# Footer
@onready var user_button: Button = %UserButton

# Merge/revert preview
@onready var merge_preview_modal: Control = %MergePreviewModal
@onready var cancel_merge_button: Button = %CancelMergeButton
@onready var confirm_merge_button: Button = %ConfirmMergeButton
@onready var merge_preview_title: Label = %MergePreviewTitle
@onready var merge_preview_source_label: Label = %MergePreviewSourceLabel
@onready var merge_preview_target_label: Label = %MergePreviewTargetLabel
@onready var merge_preview_diff_container: MarginContainer = %MergePreviewDiffContainer
@onready var revert_preview_modal: Control = %RevertPreviewModal
@onready var cancel_revert_button: Button = %CancelRevertButton
@onready var confirm_revert_button: Button = %ConfirmRevertButton
@onready var revert_preview_title: Label = %RevertPreviewTitle
@onready var revert_preview_message_label: Label = %RevertPreviewMessageLabel
@onready var revert_preview_message_icon: TextureRect = %RevertPreviewMessageIcon
@onready var revert_preview_diff_container: MarginContainer = %RevertPreviewDiffContainer
@onready var main_diff_container: MarginContainer = %MainDiffContainer
@onready var merge_preview_message_label: Label = %MergePreviewMessageLabel
@onready var merge_preview_message_icon: TextureRect = %MergePreviewMessageIcon

# collapsible sections
@onready var main_v_split: VSplitContainer = %MainVSplit
@onready var history_section_header: Button = %HistorySectionHeader
@onready var history_section_body: Control = %HistorySectionBody
@onready var diff_section_header: Control = %DiffSectionHeader
@onready var diff_section_button: Button = %DiffSectionButton
@onready var diff_section_body: Control = %DiffSectionBody

# Defines the column indices for the history tree.
class HistoryColumns:
	const HASH = 0
	const TEXT = 1 
	const TIME = 2
	const COUNT = 3
	const HASH_META = 0
	const ENABLED_META = 1

# Maps the action menu enums to IDs
class ActionMenuItems:
	const RELOAD_UI = 0
	const DUMP_BRANCH = 1
	const CLEAR_PROJECT = 2
	const DEV_MODE = 3

var task_modal: TaskModal = TaskModal.new()
var item_context_menu_icon: Texture2D = preload("../icons/GuiTabMenuHlHorizontal.svg")
var highlight_changes = false
var waiting_callables: Array = []
var deferred_highlight_update = null

var all_changes_count = 0
var history_item_count = 0
var history_saved_selection = null # hash string

const CREATE_BRANCH_IDX = 1
const MERGE_BRANCH_IDX = 2

signal reload_ui()
signal user_name_dialog_closed()

var last_seen_branch: String = ""
# emitted when we've noticed a new branch is checked out
# or if a branch we're waiting on was checked out
signal branch_checked_out

static var instance: PatchworkSidebar

func _update_ui_on_state_change():
	print("Patchwork: Updating UI due to state change...")
	update_ui()

func _on_reload_ui_button_pressed():
	reload_ui.emit()

func _is_dev_mode() -> bool:
	var idx = action_menu_button.get_popup().get_item_index(ActionMenuItems.DEV_MODE)
	var checked = action_menu_button.get_popup().is_item_checked(idx)
	return checked

# Display a "Loading Patchwork" modal until we notice the branch has changed, then initialize.
# Used when creating a new project, manually loading an existing project from ID, or auto-loading
# an existing project from the project.
func wait_for_checked_out_branch():
	if not GodotProject.get_checked_out_branch():
		task_modal.start_task("Loading Patchwork")
		await branch_checked_out
		task_modal.end_task("Loading Patchwork")
	init()

# Asks the user for their username, if there is none stored.
# If they cancel or close, returns false. If the username is confirmed, returns true.
func require_user_name() -> bool:
	if !GodotProject.has_user_name():
		_on_user_button_pressed(true)
		await user_name_dialog_closed
		return GodotProject.has_user_name()
	return true

func _on_init_button_pressed():
	if PatchworkUtils.create_unsaved_files_dialog(self, "Please save your unsaved files before initializing a new project."):
		return
	if not await require_user_name():
		return

	GodotProject.new_project();
	await wait_for_checked_out_branch()

func _on_load_project_button_pressed():
	if PatchworkUtils.create_unsaved_files_dialog(self, "Please save your unsaved files before loading an existing project."):
		return
	var doc_id = %ProjectIDBox.text.strip_edges()
	if doc_id.is_empty():
		PatchworkUtils.popup_box(self, $ErrorDialog, "Project ID is empty", "Error")
		return
	if not await require_user_name():
		return

	GodotProject.load_project(doc_id);
	await wait_for_checked_out_branch()

func update_init_panel():
	var visible = !GodotProject.has_project()
	%InitPanelContainer.visible = visible
	main_v_split.visible = !visible
	sync_button.disabled = visible
	branch_picker.disabled = visible
	fork_button.disabled = visible
	copy_project_id_button.disabled = visible
	share_button.disabled = visible
	_set_action_disabled(visible, ActionMenuItems.CLEAR_PROJECT)
	_set_action_disabled(visible, ActionMenuItems.DUMP_BRANCH)

func _set_action_disabled(disabled: bool, action: int):
	var popup = action_menu_button.get_popup()
	var index = popup.get_item_index(action)
	popup.set_item_disabled(index, disabled)

func _on_user_button_pressed(disable_cancel: bool = false):
	%UserNameEntry.text = GodotProject.get_user_name()
	%UserNameDialog.popup_centered()
	%UserNameDialog.get_cancel_button().visible = not disable_cancel

func _on_user_name_canceled():
	user_name_dialog_closed.emit()

func _on_user_name_confirmed():
	var new_user_name = %UserNameEntry.text.strip_edges()
	if new_user_name != "": GodotProject.set_user_name(new_user_name)
	user_name_dialog_closed.emit()
	print("Patchwork: Updating UI due to username confirmation...")
	update_ui()

func _on_clear_project_button_pressed():
	PatchworkUtils.popup_box(self, $ConfirmationDialog, "Are you sure you want to clear the project?", "Clear Project",
		func(): clear_project(), func(): pass)

func clear_project():
	GodotProject.clear_project()
	_on_reload_ui_button_pressed()

func get_history_item_enabled(item: TreeItem) -> bool:
	return item.get_metadata(HistoryColumns.ENABLED_META)

func set_history_item_enabled(item: TreeItem, value: bool) -> void:
	item.set_metadata(HistoryColumns.ENABLED_META, value)

func get_history_item_hash(item: TreeItem) -> String:
	return item.get_metadata(HistoryColumns.HASH_META)

func set_history_item_hash(item: TreeItem, value: String) -> void:
	item.set_metadata(HistoryColumns.HASH_META, value)

# TODO: It seems that Sidebar is being instantiated by the editor before the plugin does?
func _ready() -> void:
	if is_part_of_edited_scene():
		return

	# @Paul: I think somewhere besides the plugin sidebar gets instantiated. Is this something godot does?
	# to paper over this we check if plugin and godot_project are set
	# The singleton class accessor is still pointing to the old GodotProject singleton
	# if we're hot-reloading, so we check the Engine for the singleton instead.
	# The rest of the accessor uses outside of _ready() should be fine.
	var godot_project = Engine.get_singleton("GodotProject")
	if !godot_project: return

	bind_listeners(godot_project)
	setup_history_list_popup()

	print("Sidebar: ready!")

	# need to add task_modal as a child to the plugin otherwise process won't be called
	add_child(task_modal)
	if not EditorInterface.get_edited_scene_root() == self:
		waiting_callables.append(self._try_init)
	else:
		print("Sidebar: in editor!!!!!!!!!!!!")

func _enter_tree():
	if is_part_of_edited_scene():
		return
	instance = self

func bind_listeners(godot_project):
	%InitializeButton.pressed.connect(self._on_init_button_pressed)
	%LoadExistingButton.pressed.connect(self._on_load_project_button_pressed)
	PatchworkUtils.add_listener_disable_button_if_text_is_empty(%UserNameDialog.get_ok_button(), %UserNameEntry)
	PatchworkUtils.add_listener_disable_button_if_text_is_empty(%LoadExistingButton, %ProjectIDBox)
	user_button.pressed.connect(_on_user_button_pressed)

	%UserNameDialog.canceled.connect(_on_user_name_canceled)
	%UserNameDialog.confirmed.connect(_on_user_name_confirmed)

	godot_project.state_changed.connect(self._update_ui_on_state_change);

	merge_button.pressed.connect(create_merge_preview_branch)
	fork_button.pressed.connect(create_new_branch)
	%ClearDiffButton.pressed.connect(_on_clear_diff_button_pressed)

	cancel_merge_button.pressed.connect(cancel_merge_preview)
	confirm_merge_button.pressed.connect(confirm_merge_preview)

	cancel_revert_button.pressed.connect(cancel_revert_preview)
	confirm_revert_button.pressed.connect(confirm_revert_preview)

	sync_button.pressed.connect(_on_sync_button_pressed)

	branch_picker.branch_selected.connect(_on_branch_selected)

	history_section_header.pressed.connect(func(): toggle_section(history_section_header, history_section_header, history_section_body))
	diff_section_button.pressed.connect(func(): toggle_section(diff_section_header, diff_section_button, diff_section_body))
	history_tree.item_selected.connect(_on_history_list_item_selected)
	history_tree.button_clicked.connect(_on_history_tree_button_clicked)
	history_tree.empty_clicked.connect(_on_history_tree_empty_clicked)
	history_tree.item_mouse_selected.connect(_on_history_tree_mouse_selected)
	history_tree.allow_rmb_select = true
	inspector.node_hovered.connect(_on_node_hovered)
	inspector.node_unhovered.connect(_on_node_unhovered)
	copy_project_id_button.pressed.connect(_on_copy_project_id_button_pressed)
	share_button.pressed.connect(_on_share_button_pressed)
	action_menu_button.get_popup().id_pressed.connect(_on_action_menu_item_selected)

	_style_button(sync_button)
	_style_button(copy_project_id_button)
	_style_button(share_button)
	_style_button(action_menu_button)
	_style_button(fork_button)
	_style_button(merge_button)
	_style_button(%ClearDiffButton)


func _style_button(button: Button):
	var theme = EditorInterface.get_editor_theme()
	button.theme_type_variation = "FlatButton"
	button.theme = theme


func _try_init():
	var godot_project = Engine.get_singleton("GodotProject")
	if godot_project:
		if !godot_project.has_project():
			print("Not initialized, showing init panel")
			print("Patchwork: Updating UI due to init...")
			update_ui()
			return
		else:
			print("Initialized, hiding init panel")
			wait_for_checked_out_branch()
	else:
		print("No GodotProject singleton!!!!!!!!")

func _process(delta: float) -> void:
	if is_part_of_edited_scene():
		return

	var checked_out_branch = GodotProject.get_checked_out_branch()
	if checked_out_branch && checked_out_branch.id != last_seen_branch:
		last_seen_branch = checked_out_branch.id
		branch_checked_out.emit()
		
	elif !checked_out_branch && last_seen_branch != "":
		last_seen_branch = ""
		branch_checked_out.emit()

	if deferred_highlight_update:
		var c = deferred_highlight_update
		deferred_highlight_update = null
		c.call()

	if waiting_callables.size() > 0:
		var callables = waiting_callables.duplicate();
		for callable in callables:
			callable.call()
		waiting_callables.clear()

func init() -> void:
	print("Sidebar initialized!")
	print("Patchwork: Updating UI due to init...")
	update_ui()

	# Here, the user could easily just hit X and remain anonymous. This can only happen in the case
	# of a project loaded from a file, where the user's config hasn't been set.
	# If we want to force the user to enter a username, we could do `while(!require_user_name()): pass`.
	# But that seems bad.
	require_user_name()

func _on_sync_button_pressed():
	var toaster = EditorInterface.get_editor_toaster()
	GodotProject.print_sync_debug()
	toaster.push_toast("Printed connection info to the console.");

func _on_branch_selected(branch_id: String):
	checkout_branch(branch_id)

func checkout_branch(branch_id: String) -> void:
	var branch = GodotProject.get_branch(branch_id)
	task_modal.do_task(
		"Checking out branch \"%s\"" % [branch.name],
		func():
			GodotProject.checkout_branch(branch_id)
			await branch_checked_out
	)

func create_new_branch() -> void:
	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before creating a new branch."):
		return

	var dialog = ConfirmationDialog.new()
	dialog.title = "Create New Branch"

	var branch_name_input = LineEdit.new()
	branch_name_input.placeholder_text = "Branch name"
	branch_name_input.text = GodotProject.get_user_name() + "'s branch"
	dialog.add_child(branch_name_input)

	# Not scaling these values because they display correctly at 1x-2x scale
	# Position line edit in dialog
	branch_name_input.position = Vector2(8, 8)
	branch_name_input.size = Vector2(200, 30)

	# Make dialog big enough for line edit
	dialog.size = Vector2(220, 100)

	dialog.get_ok_button().text = "Create"
	PatchworkUtils.add_listener_disable_button_if_text_is_empty(dialog.get_ok_button(), branch_name_input)

	dialog.canceled.connect(func(): dialog.queue_free())

	dialog.confirmed.connect(func():
		var new_branch_name = branch_name_input.text.strip_edges()
		dialog.queue_free()

		task_modal.do_task("Creating new branch \"%s\"" % new_branch_name, func():
			GodotProject.create_branch(new_branch_name)
			await branch_checked_out
		)
	)

	add_child(dialog)

	dialog.popup_centered()

	# focus on the branch name input
	branch_name_input.grab_focus()

func move_inspector_to(node: Node) -> void:
	if inspector and main_diff_container and node and inspector.get_parent() != node:
		inspector.reparent(node)
		inspector.visible = true

func create_merge_preview_branch():
	if PatchworkUtils.create_unsaved_files_dialog(self, "Please save your unsaved files before merging."):
		return

	# this shouldn't be possible due to UI disabling, but just in case
	if not GodotProject.can_create_merge_preview_branch():
		return

	task_modal.do_task("Creating merge preview", func():
		GodotProject.create_merge_preview_branch()
		await branch_checked_out
	)

func create_revert_preview_branch(head):
	if PatchworkUtils.create_unsaved_files_dialog(self, "Please save your unsaved files before reverting."):
		return
	# this shouldn't be possible due to UI disabling, but just in case
	if !GodotProject.can_create_revert_preview_branch(head): return

	task_modal.do_task("Creating revert preview", func():
		GodotProject.create_revert_preview_branch(head)
		await branch_checked_out
	)

func cancel_revert_preview():
	if !GodotProject.is_revert_preview_branch_active(): return

	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before cancelling your revert."):
		return

	task_modal.do_task("Cancel revert preview", func():
		GodotProject.discard_preview_branch()
		await branch_checked_out
	)

func confirm_revert_preview():
	if !GodotProject.is_revert_preview_branch_active(): return

	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before reverting."):
		return

	var target = PatchworkUtils.short_hash(GodotProject.get_checked_out_branch().reverted_to)

	PatchworkUtils.popup_box(self, $ConfirmationDialog, "Are you sure you want to revert to \"%s\" ?" % target, "Revert Branch", func():
		task_modal.do_task("Reverting to \"%s\"" % target, func():
			GodotProject.confirm_preview_branch()
			await branch_checked_out
		), func(): pass)

func cancel_merge_preview():
	if !GodotProject.is_merge_preview_branch_active(): return

	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before cancelling your merge."):
		return

	task_modal.do_task("Cancel merge preview", func():
		GodotProject.discard_preview_branch()
		await branch_checked_out
	)


func confirm_merge_preview():
	if !GodotProject.is_merge_preview_branch_active(): return

	if PatchworkUtils.create_unsaved_files_dialog(self, "You have unsaved files open. You need to save them before merging."):
		return

	var current_branch = GodotProject.get_checked_out_branch()
	var forked_from = GodotProject.get_branch(current_branch.parent).name
	var target = GodotProject.get_branch(current_branch.merge_into).name

	PatchworkUtils.popup_box(self, $ConfirmationDialog, "Are you sure you want to merge \"%s\" into \"%s\" ?" % [forked_from, target], "Merge Branch", func():
		task_modal.do_task("Merging \"%s\" into \"%s\"" % [forked_from, target], func():
			GodotProject.confirm_preview_branch()
			await branch_checked_out
		)
	)

func toggle_section(section_header: Control, section_button: Button, section_body: Control):
	var parent_vbox = section_header.get_parent()
	if section_body.visible:
		section_button.icon = load("res://addons/patchwork/public/icons/CollapsibleClosed.svg")
		section_body.visible = false
		parent_vbox.set_v_size_flags(Control.SIZE_FILL)
	else:
		section_button.icon = load("res://addons/patchwork/public/icons/CollapsibleOpen.svg")
		section_body.visible = true
		parent_vbox.set_v_size_flags(Control.SIZE_EXPAND_FILL)

func unfold_section(section_header: Button, section_body: Control):
	section_header.icon = load("res://addons/patchwork/public/icons/CollapsibleOpen.svg")
	section_body.visible = true

func fold_section(section_header: Button, section_body: Control):
	section_header.icon = load("res://addons/patchwork/public/icons/CollapsibleClosed.svg")
	section_body.visible = false

func update_history_tree():
	if !GodotProject.has_project(): return
	var history = GodotProject.get_branch_history()
	var dev_mode = _is_dev_mode()
	var column_offset = 0 if dev_mode else -1
	var hash_column = HistoryColumns.HASH + column_offset
	var text_column = HistoryColumns.TEXT + column_offset
	var time_column = HistoryColumns.TIME + column_offset

	history_tree.clear()
	history_item_count = 0

	# create root item
	var root = history_tree.create_item()
	var selection = null

	for i in range(history.size() - 1, -1, -1):
		var change = GodotProject.get_change(history[i])
		var item = history_tree.create_item(root)
		history_item_count += 1
		var editor_scale = EditorInterface.get_editor_scale()

		# if we're a dev, we need another column for the commit hash
		history_tree.columns = HistoryColumns.COUNT + column_offset
		if dev_mode:
			item.set_text(hash_column, PatchworkUtils.short_hash(change.hash))
			item.set_tooltip_text(hash_column, change.hash)
			item.set_selectable(hash_column, false)
			history_tree.set_column_expand(hash_column, true)
			history_tree.set_column_expand_ratio(hash_column, 0)
			history_tree.set_column_custom_minimum_width(hash_column, 80 * editor_scale)

		set_history_item_hash(item, change.hash)
		set_history_item_enabled(item, true)
		history_tree.set_column_expand(text_column, true)
		history_tree.set_column_expand_ratio(text_column, 2)
		history_tree.set_column_clip_content(text_column, true)
		item.set_selectable(text_column, true)

		var text_color = Color.WHITE

		if change.is_merge:
			var merged_branch = GodotProject.get_branch(change.merge_id)
			# Sometimes this is null while starting up, before the branch has loaded in.
			# If so the button will just appear later when we update UI.
			if merged_branch:
				item.add_button(text_column, branch_icon_history, 0,
					false, "Checkout branch " + merged_branch.name)

		item.set_text(text_column, change.summary)

		if !change.is_synced:
			text_color = Color(0.6, 0.6, 0.6)

		# disable initial commits
		if change.is_setup:
			set_history_item_enabled(item, false);

		var is_revertable = true;
		if change.is_setup && i == 0: is_revertable = false # we can't revert to the very first setup commit, because there's 2
		if i == history.size() - 1: is_revertable = false # we can't revert to the current commit
		if is_revertable:
			item.add_button(text_column, item_context_menu_icon, 1, false, "Open context menu")

		# timestamp
		item.set_text(time_column, change.human_timestamp)
		item.set_tooltip_text(time_column, change.exact_timestamp)
		item.set_selectable(time_column, false)
		history_tree.set_column_expand(time_column, true)
		history_tree.set_column_expand_ratio(time_column, 0)
		history_tree.set_column_custom_minimum_width(time_column, 75 * editor_scale)
		history_tree.set_column_clip_content(time_column, false)

		# apply the chosen color to all fields
		if dev_mode: item.set_custom_color(hash_column, text_color)
		item.set_custom_color(text_column, text_color)
		item.set_custom_color(time_column, text_color)

		if change.hash == history_saved_selection:
			selection = item

	# restore saved selection
	if selection != null:
		history_tree.set_selected(selection, text_column)
	# otherwise, ensure any invalid saved selection is reset
	else:
		history_saved_selection = null

func update_action_buttons():
	if !GodotProject.has_project(): return
	var main_branch = GodotProject.get_main_branch()
	var current_branch = GodotProject.get_checked_out_branch()
	if !main_branch or !current_branch: return
	if main_branch.id == current_branch.id:
		merge_button.disabled = true
		merge_button.tooltip_text = "Can't merge main, because it's the root branch."
	else:
		var parent_branch = GodotProject.get_branch(current_branch.parent)
		merge_button.disabled = false
		merge_button.tooltip_text = "Merge \"%s\" into \"%s\"" % [current_branch.name, parent_branch.name]

func update_user_name():
	user_button.text = GodotProject.get_user_name()
	if user_button.text == "": user_button.text = "Anonymous"

func update_merge_preview():
	var active = GodotProject.is_merge_preview_branch_active()
	merge_preview_modal.visible = active
	if !active: return

	var current_branch = GodotProject.get_checked_out_branch()
	var source_branch = GodotProject.get_branch(current_branch.parent)
	var target_branch = GodotProject.get_branch(current_branch.merge_into)

	if !source_branch or !target_branch:
		printerr("Branch merge info invalid!")
		return;

	merge_preview_source_label.text = source_branch.name
	merge_preview_target_label.text = target_branch.name
	merge_preview_title.text = "Preview of merging \"" + target_branch.name + "\""
	merge_preview_title.tooltip_text = merge_preview_title.text

	if !GodotProject.is_safe_to_merge():
		merge_preview_message_label.text = "\"" + target_branch.name + "\" has changed since \"" + source_branch.name + "\" was created.\nBe careful and review your changes before merging."
		merge_preview_message_icon.texture = load("res://addons/patchwork/public/icons/StatusWarning32.svg")
	else:
		merge_preview_message_label.text = "This branch is safe to merge.\n \"" + target_branch.name + "\" hasn't changed since \"" + source_branch.name + "\" was created."
		merge_preview_message_icon.texture = load("res://addons/patchwork/public/icons/StatusSuccess32.svg")

func update_revert_preview():
	var active = GodotProject.is_revert_preview_branch_active()
	revert_preview_modal.visible = active
	if !active: return

	var current_branch = GodotProject.get_checked_out_branch()
	var parent_branch = GodotProject.get_branch(current_branch.parent)

	if !current_branch || !current_branch.reverted_to:
		printerr("Branch revert info invalid!")
		return

	var change_hash = PatchworkUtils.short_hash(current_branch.reverted_to)

	revert_preview_title.text = "Preview of reverting \"%s\" to %s" % [parent_branch.name, change_hash]
	revert_preview_title.tooltip_text = revert_preview_title.text

func update_inspector():
	if !GodotProject.has_project(): return
	if GodotProject.is_revert_preview_branch_active():
		move_inspector_to(revert_preview_diff_container)
	elif GodotProject.is_merge_preview_branch_active():
		move_inspector_to(merge_preview_diff_container)
	else:
		move_inspector_to(main_diff_container)

# Refresh the entire UI, rebinding all data.
func update_ui() -> void:
	update_init_panel()
	branch_picker.populate()
	update_history_tree()
	update_sync_status()
	update_action_buttons()
	update_user_name()
	update_inspector()
	update_revert_preview()
	update_merge_preview()
	update_diff()

func update_sync_status() -> void:
	var sync_status = GodotProject.get_sync_status()

	if sync_status.state == "unknown":
		sync_button.icon = load("res://addons/patchwork/public/icons/StatusWarning.svg")
		sync_button.tooltip_text = "Disconnected - might have unsynced changes"

	elif sync_status.state == "syncing":
		sync_button.icon = load("res://addons/patchwork/public/icons/StatusSync.svg")
		sync_button.tooltip_text = "Syncing"

	elif sync_status.state == "up_to_date":
		sync_button.icon = load("res://addons/patchwork/public/icons/StatusSuccess.svg")
		sync_button.tooltip_text = "Fully synced"

	elif sync_status.state == "disconnected":
		if sync_status.unsynced_changes == 0:
			sync_button.tooltip_text = "Disconnected - no unsynced local changes"
			sync_button.icon = load("res://addons/patchwork/public/icons/StatusWarning.svg")
		elif sync_status.unsynced_changes == 1:
			sync_button.icon = load("res://addons/patchwork/public/icons/StatusError.svg")
			sync_button.tooltip_text = "Disconnected - 1 local change that hasn't been synced"
		else:
			sync_button.icon = load("res://addons/patchwork/public/icons/StatusError.svg")
			sync_button.tooltip_text = "Disconnected - %s local changes that haven't been synced" % [sync_status.unsynced_changes]
	else: printerr("unknown sync status: " + sync_status.state)

func update_highlight_changes(diff: Dictionary) -> void:
	if (PatchworkEditor.is_changing_scene()):
		deferred_highlight_update = func(): update_highlight_changes(diff)
		return

	var edited_root = EditorInterface.get_edited_scene_root()

	# reflect highlight changes checkbox state

	if edited_root:
		if not (not diff || diff.is_empty()):
			var path = edited_root.scene_file_path
			var scene_changes = diff.dict.get(path)
			if scene_changes:
				HighlightChangesLayer.highlight_changes(edited_root, scene_changes)
		else:
			HighlightChangesLayer.remove_highlight(edited_root)

var last_diff = null

func _on_node_hovered(file_path: String, node_paths: Array) -> void:
	var node: Node = EditorInterface.get_edited_scene_root()
	if node.scene_file_path != file_path or !last_diff:
		# don't highlight changes for other files
		return
	var new_diff = {}
	new_diff.title = last_diff.title;
	new_diff.dict = {}
	# create a diff that only contains the changes for the hovered node
	for file in last_diff.dict.keys():
		if file == file_path:
			new_diff.dict[file] = last_diff.dict[file].duplicate()
			new_diff.dict[file]["changed_nodes"] = []
			for node_change in last_diff.dict[file]["changed_nodes"]:
				var np: String = node_change["node_path"]
				if node_paths.has(NodePath(np)):
					new_diff.dict[file]["changed_nodes"].append(node_change)
			break
	self.update_highlight_changes(new_diff)

func _on_node_unhovered(file_path: String, node_path: Array) -> void:
	self.update_highlight_changes({})

var context_menu_hash = null

enum HistoryListPopupItem {
	RESET_TO_COMMIT,
	CREATE_BRANCH_AT_COMMIT
}

func _on_history_list_popup_id_pressed(index: int) -> void:
	history_list_popup.hide()
	var item = history_list_popup.get_item_id(index)
	if context_menu_hash == null:
		printerr("no selected item")
		return
	if item == HistoryListPopupItem.RESET_TO_COMMIT:
		create_revert_preview_branch(context_menu_hash)
	elif item == HistoryListPopupItem.CREATE_BRANCH_AT_COMMIT:
		print("Create branch at change not implemented yet!")

func setup_history_list_popup() -> void:
	history_list_popup.clear()
	# TODO: adjust this when more items are added
	history_list_popup.max_size.y = 60 * EditorInterface.get_editor_scale()
	history_list_popup.id_pressed.connect(_on_history_list_popup_id_pressed)
	history_list_popup.add_icon_item(load("res://addons/patchwork/public/icons/UndoRedo.svg"), "Reset to here", HistoryListPopupItem.RESET_TO_COMMIT)
	# history_list_popup.add_item("Create branch from here", HistoryListPopupItem.CREATE_BRANCH_AT_COMMIT)

func _on_history_tree_mouse_selected(_at_position: Vector2, button_idx: int) -> void:
	if button_idx == MOUSE_BUTTON_RIGHT:
		# if the selected item is disabled, do not.
		if get_history_item_enabled(history_tree.get_selected()) == false: return
		show_contextmenu(get_history_item_hash(history_tree.get_selected()))

func show_contextmenu(item_hash):
	context_menu_hash = item_hash
	history_list_popup.popup_on_parent(Rect2(get_global_mouse_position(), Vector2.ZERO))

func _on_history_tree_button_clicked(item: TreeItem, _column : int, id: int, mouse_button_index: int) -> void:
	if mouse_button_index != MOUSE_BUTTON_LEFT: return

	if id == 0:
		var change_hash = get_history_item_hash(item)
		var change = GodotProject.get_change(change_hash)
		var merged_branch = change.merge_id
		if !merged_branch:
			print("Error: No matching change found.")
			return

		checkout_branch(merged_branch)
	elif id == 1:
		show_contextmenu(get_history_item_hash(item))

func _on_history_list_item_selected() -> void:
	var selected_item = history_tree.get_selected()
	if selected_item == null:
		history_saved_selection = null
		return

	# update the saved selection
	var change_hash = get_history_item_hash(selected_item)
	history_saved_selection = change_hash

	update_diff()

func _on_clear_diff_button_pressed():
	_on_history_tree_empty_clicked(null, 0)

func _on_history_tree_empty_clicked(_vec2, _idx):
	history_saved_selection = null
	history_tree.deselect_all()
	update_diff()

# Read the selection from the tree, and update the diff visualization accordingly.
func update_diff():
	if !GodotProject.has_project(): return
	var selected_item = history_tree.get_selected()
	var diff;

	if (selected_item == null
			or GodotProject.is_merge_preview_branch_active()
			or GodotProject.is_revert_preview_branch_active()):
		diff = GodotProject.get_default_diff()
		show_diff(diff, false)
	else:
		var hash = get_history_item_hash(selected_item)
		diff = GodotProject.get_diff(hash)
		if (!diff):
			show_invalid_diff()
			return
		show_diff(diff, true)

# Inspect the diff dictionary.
func show_diff(diff, is_change) -> void:
	if !diff:
		inspector.visible = false
		diff_section_button.text = "Changes"
		%ClearDiffButton.visible = false
		return
	last_diff = diff
	%ClearDiffButton.visible = is_change
	inspector.visible = true
	diff_section_button.text = diff.title
	inspector.reset()
	inspector.add_diff(diff.dict)

# Show an invalid diff for a commit with no valid diff (e.g. setup commits)
func show_invalid_diff() -> void:
	inspector.visible = false
	diff_section_button.text = "No diff available for selection"
	%ClearDiffButton.visible = true

func _on_copy_project_id_button_pressed() -> void:
	var toaster = EditorInterface.get_editor_toaster()
	var project_id = GodotProject.get_project_id()
	if not project_id.is_empty():
		DisplayServer.clipboard_set(project_id)
		toaster.push_toast("Project ID copied to clipboard.")
	else:
		toaster.push_toast("No Project ID found!", EditorToaster.Severity.SEVERITY_ERROR)

func _on_share_button_pressed() -> void:
	var toaster = EditorInterface.get_editor_toaster()
	var project_id = GodotProject.get_project_id()
	var branch_id = GodotProject.get_checked_out_branch().id;
	if not project_id.is_empty() && not branch_id.is_empty():
		DisplayServer.clipboard_set("https://godot-viewer.netlify.app/?project=%s&branch=%s" % [project_id, branch_id])
		toaster.push_toast("Share URL copied to clipboard.")
	else:
		toaster.push_toast("Couldn't create share URL!", EditorToaster.Severity.SEVERITY_ERROR)

func _on_action_menu_item_selected(id: int) -> void:
	var toaster = EditorInterface.get_editor_toaster()
	match id:
		ActionMenuItems.CLEAR_PROJECT:
			_on_clear_project_button_pressed()
		ActionMenuItems.RELOAD_UI:
			_on_reload_ui_button_pressed()
			toaster.push_toast("Reloaded UI.")
		ActionMenuItems.DEV_MODE:
			var idx = action_menu_button.get_popup().get_item_index(id)
			action_menu_button.get_popup().toggle_item_checked(idx)
			var checked = action_menu_button.get_popup().is_item_checked(idx)
			toaster.push_toast("Developer mode enabled." if checked else "Developer mode disabled.")
			update_ui()
		ActionMenuItems.DUMP_BRANCH:
			GodotProject.dump_current_branch()
			toaster.push_toast("Dumped current branch state to res://.patchwork/.")
			
