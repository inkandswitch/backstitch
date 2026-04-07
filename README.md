# Godot Version Control Plugin

Native version control for Godot projects

![godot-plugin-features.webp](./assets/godot-plugin-features.webp)

## Disclaimer

This version control plugin currently requires our custom fork of Godot and is not yet compatible with the standard Godot engine.

We have long-term plans to upstream our changes into Godot, which will allow this to work as a normal plugin. Until then, you'll need to use our fork.

**This is alpha-grade software** — please do not rely on it as the sole backup for your project. We strongly recommend:

- Maintaining separate backups while testing
- Only using it in low-risk situations

**About the sync server:** The plugin syncs your data with our public sync server. We cannot guarantee long-term data availability on this server — please use it for testing purposes only. However, the sync server is optional. Even without it, you can work offline and retain all your data locally, similar to Git.

**We'd love your feedback!** If you're testing the plugin with your project, please share your experience, bug reports, or suggestions at [paul@inkandswitch.com](mailto:paul@inkandswitch.com).

## Installation

Download our fork of Godot for your platform

- [Mac OS](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/backstitch_editor-v0.0.1-alpha.20-macos.zip)
- [Windows](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/backstitch_editor-v0.0.1-alpha.20-windows.zip)
- [Linux](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/backstitch_editor-v0.0.1-alpha.20-linux.zip)

Then you can either:

Download example project with plugin already set up

- [Moddable Platformer](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/moddable-platformer-with-backstitch.zip) (recommended)
- [Threadbare](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/threadbare-with-backstitch.zip) (experimental)

*or* 

Download the plugin and copy it into an existing project

- [Plugin](https://github.com/inkandswitch/backstitch-godot-plugin/releases/download/v0.0.1-alpha.20/backstitch-godot-plugin.zip)

## Getting Started

Most of the Godot editor works as you're used to. To start using the collaboration features, click the Backstitch tab in the right sidebar:

![](./assets/plugin-tab.webp)

To collaborate with others, you need to be in the same Backstitch Project.

>💡 A Backstitch Project is a shared online session where everyone can see the same thing. Think of it sort of like a Google Doc. If you're familiar with Git, it's also similar to a Git repo with live synchronization.


You can either create a new project or load an existing one.

### Making a new project

To create a new project: click the “create a new project” button

To share a project with someone:

- You can click the "Copy Project ID" button in the bottom toolbar and then send the Project ID to another user
- The project ID is also written into the filesystem, so if you share your project folder with someone it will now contain the Backstitch Project ID in the `backstitch.cfg` file

**Joining an existing project**

- manually paste the ID into the Project ID box if someone has shared that
- You may already be in a Backstitch project, if someone shared a file with you that contains a project ID.

# Using Backstitch

**Set your username**

Start by entering a username in the bottom right corner. This will help identify you to other collaborators.

![](./assets/set-user-name.webp)

**Making changes directly on main**

When you start out in a Project, you're editing on the main copy.

Anytime you save (ctrl-s or cmd-s), your changes will now be shared with all collaborators in the project.


> ⚠️ If you're making a larger change or many people are working in the same project, then you should avoid editing directly on main, and instead work on a branch. See the next section.



You can see a log of recent changes by you and others in the History list.

![](./assets/history.webp)

**Making changes on a branch**

A branch is a separate copy of the game that you can edit independently from others. Later on, you can "merge" this copy back into the main copy if you want.

To make a new branch, click "remix" and select a name.

![](./assets/branch-picker.webp)

Now you can make changes privately on this branch without disrupting others.

The history list now shows just a list of changes on this branch.

The "changes" panel in the bottom of the Backstitch pane shows you the difference between this branch and the main branch. If you hover on different parts of the change list you can see highlights on the changed parts in the scene.

![](./assets/diff.webp)

If you want, you can make a branch off a branch by clicking Remix. Each branch starts where its parent left off.

**Merging a branch**

You can "merge" a branch, which adds any edits from that branch into its parent branch. For example, a branch created from the main branch can be merged into main.

To do so, click the Merge button. You'll see a preview pane. This shows what will happen once the two branches merge together. If things look good, hit Merge again. If you want to make adjustments, you can do those adjustments here before merging, or you can Cancel to make more edits on the original branch before attempting the merge again.

![](./assets/merge-preview.webp)

**How to use branches**

In a classroom setting, you could try:

- Each student makes their own branch
- Each team makes a branch, and different students collaborate in there. (Optionally, each team member could make their own branch off the team branch)

In a small collaborative team, you could try:

- Each major feature gets a branch, which gets merged when the feature is done
- Make a branch for a live jamming session on a game mechanic

## Configuration

Plugin configuration is stored in your project as `res://backstitch.cfg/`. The following configuration options are available:

| Config | Description |
| --- | --- |
| project_doc_id | The ID of your project. Empty if there is no Backstitch project created.
| checked_out_branch_doc_id | Your current checked out branch inside your project. Empty for the main branch, or if there is no Backstitch project.
| server_url | The URL for the sync server. If empty or missing, uses the default testing sync server run by Ink & Switch. If the URL or IP address is prefixed with `ws://`, uses a WebSockets server; otherwise, it uses raw TCP from a `samod` server like the one [here](https://github.com/paulsonnentag/automerge-rust-sync-server/).


## **Troubleshooting**

The plugin is still experimental software. Occasionally errors can happen.

As long as you're regularly saving your work, data loss is unlikely. All of your changes are still being saved to your computer's local filesystem.

If syncing is not working, you can try falling back to other methods of syncing data, like using a Git repository.

If the editor crashes or seems unresponsive, try restarting the editor.
