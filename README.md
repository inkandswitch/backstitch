# Backstitch

[Backstitch](https://backstitch.dev/) provides real-time version control for the [Godot](https://godotengine.org/) game engine. 

![A screenshot of Backstitch](backstitch-screenshot.png)

With Backstitch, you can:

- Sync your changes in real-time with collaborators
- Branch, merge, and revert change history
- Inspect your changes visually
- ... and more!

Learn more on our [website](https://backstitch.dev/).

## Disclaimer

**This is alpha-grade software** — please do not rely on it as the sole backup for your project. We strongly recommend:

- Maintaining separate backups while testing
- Only using it in low-risk situations

**About the sync server:** The plugin optionally syncs your data with our alpha sync server, located at `alpha.backstitch.dev:8085`. We cannot guarantee long-term data availability or privacy on this server — please use it for testing purposes only. However, the sync server is optional. Even without it, you can work offline and retain all your data locally, similar to Git.

**We'd love your feedback!** If you're testing the plugin with your project, please share your experience, bug reports, or suggestions at [paul@inkandswitch.com](mailto:paul@inkandswitch.com).

## Setup & Tutorial

To get started with Backstitch, check out the [docs on our website](https://backstitch.dev/docs). 

## Configuration

Plugin configuration is stored in your project as `res://backstitch.cfg/`. The following configuration options are available:

| Config | Description |
| --- | --- |
| project_doc_id | The ID of your project. Empty if there is no Backstitch project created.
| checked_out_branch_doc_id | Your current checked out branch inside your project. Empty for the main branch, or if there is no Backstitch project.
| server_url | The URL for the sync server. If empty or missing, uses the default testing sync server run by Ink & Switch. If the URL or IP address is prefixed with `ws://`, uses a WebSockets server; otherwise, it uses raw TCP from a `samod` server like the one [here](https://github.com/inkandswitch/backstitch-sync-server/).

## Contributing

We welcome pull requests and other contributions! Get started by reading our [Contributor's Guide](./CONTRIBUTING.md).

## Questions? Comments? Found a Bug?

If you have any questions or comments, please email us at [paul@inkandswitch.com](mailto:paul@inkandswitch.com)!

If you find a bug, please open an [issue!](https://github.com/inkandswitch/backstitch/issues/new)
