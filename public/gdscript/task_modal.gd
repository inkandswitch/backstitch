@tool
extends Node
class_name TaskModal

# The reason we are queueing calls is because we need to wait for the editor to finish performing certain actions before we can manipulate gui elements
# We were getting crashes and weird behavior when trying to do everything synchronously
# Basically, if we need to do something that would induce the editor to perform actions, we need to queue it

var queued_calls = []

func start_task(name: String):
	queued_calls.append(func():
		BackstitchEditor.progress_add_task(name, name, 10, false)
	)

func end_task(name: String):
	queued_calls.append(func():
		BackstitchEditor.progress_end_task(name)
	)

# do_task is a helper function that adds a task to the queue and waits for it to finish
# don't use this if you need to wait for the task to finish, use start task and end task manually instead
func do_task(name: String, task: Callable):
	queued_calls.append(func():
		start_task(name)

		queued_calls.append(func():
			await task.call()

			end_task(name)
		)
	)

func _process(_delta: float) -> void:
	# we need to make a copy of the calls otherwise queued_calls can get called multiple times
	# I think this is a multi threading issue @nikita?
	var calls = []
	for queued_call in queued_calls:
		calls.append(queued_call)

	queued_calls.clear()

	for call in calls:
		if call.is_valid():
			call.call()
		else:
			print("Invalidcall: ", call)
