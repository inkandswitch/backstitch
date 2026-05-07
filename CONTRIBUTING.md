# Backstitch: Contributor's Guide

We'd love your help developing Backstitch. Here's how to get started.

### Prerequisites

```bash
# 1. Git installed?
git --version

# 2. Rust installed? Must be at least 1.91.1
rustc --version
cargo --version

# 3. Python 3 + SCons installed?
python3 --version
scons --version

# 4. C++ compiler available?
# Windows: Check Visual Studio is installed
# macOS: xcode-select -p
# Linux: gcc --version

# 5. `just` installed? If not, `cargo install just`
just --version
```

If any are missing, see [Detailed Setup](#detailed-setup) below.

---


### `just` build system

We use [just](https://github.com/casey/just) as our command runner. 

To view a detailed list of targets, type `just`. 

### Quick start: Launching projects

If you want to get up and running as fast as possible, type `just launch`. It will launch Endless's `moddable-platformer` project with Backstitch installed, using a custom-built Godot editor with the Backstitch module.

Otherwise, you can specify arguments for `just launch`:

```bash
project=[moddable-platformer|moddable-pong|threadbare] # launch the given project
backstitch_profile=[release|debug] # whether we should build the rust code with release or debug configuration
godot_profile=[release|debug|sani] # whether we should build Godot with release, debug, or sani configuration
server_url=<url> # force embed a server URL into the project. By default, just keeps whatever server URL is already configured in the project.
tracing_support=[none|tokio-console] # allows a tokio-console to be connected at the default port for debugging
```

#### Using Visual Studio Code

A variety of helpful launch configurations are specified when you open the project in Visual Studio Code. These run `just` commands to prepare projects, and then attach an in-editor debugger.

When working with GDScript, you'll need to open `moddable-platformer`, `moddable-pong`, or `threadbare` directly in VSCode, and Godot must be running with `just launch`.


### Build structure

When you run `just launch`, the output generated files are copied to `build/`. There are several important directories, here:

- `build/backstitch`:
  + The built plugin.
  + `bin`: Rust binaries
  + `public`: Symlinked from `public/` in the repo root. For GDScript and assets we must ship directly with the plugin.
- `build/moddable-platformer`/`build/threadbare`/`build/moddable-pong`:
  + A clone of each project repository.
  + `addons/backstitch`: Symlinked from `build/backstitch`, so feel free to make GDScript or UI changes directly to `addons/backstitch/public`.
- `build/godot`:
  + A clone of the Godot repository
  + `modules/backstitch_editor`: Symlinked from `editor/` to form a new editor module.
  + `bin`: Contains the built Godot executable.
- `GodotFormatters`:
  + A special `lldb` formatter for Godot objects. Only cloned when running the project through VSCode.




### Understanding Backstitch's Architecture

Backstitch is a **hybrid Godot Engine C++ module + GDExtension**, not a traditional plugin:

- **Godot Engine C++ Module** (`editor/`) - Built INTO your custom Godot editor
  - Automatically active when you launch the custom editor
  - Registers the `BackstitchEditor` class
  - Only here to provide editor functionality that is not currently exposed to GDExtensions
    - Will eventually be removed once this functionality is upstreamed to Godot

- **GDExtension Component** (`public/` and `rust/`) - Actually runs the application
  - Contains the Rust plugin DLL/library
  - Contains public GDScript UI components
  - Located in your project's `addons/backstitch/` folder

Because the C++ module is compiled directly into Godot (see [register_types.cpp:11-14](register_types.cpp#L11-L14)), Backstitch automatically initializes when the editor starts. The `plugin.cfg` file exists for compatibility but has an empty `script=""` field because there's no GDScript plugin script to enable/disable.

**In summary:** When `just` builds Godot with Backstitch and symlinks the files to `addons/backstitch/`, the plugin is **always active** - you don't need to manually enable it in the Plugins menu. The Backstitch tab will appear automatically.

### Development Workflow

When developing manually, after making changes:

**For GDScript changes:**

Click the "Reload UI" button in the Backstitch tab to reload the UI.

**For Rust changes:**

Either run `just build-backstitch (release/debug)` in a terminal, or launch the `Hot reload backstitch` target in VSCode. Godot should reload the Rust binary automatically, but you may have to restart the editor if it explodes.

**For C++ module changes:**

Close the editor, and run `just launch` again (or launch from VSCode).

#### Auto-rebuild Rust changes (optional)

For faster Rust development iteration, use `watchexec` to automatically rebuild on file changes:

**1. Install watchexec:**

```bash
# macOS
brew install watchexec

# Linux (Ubuntu/Debian)
sudo apt install watchexec

# Windows (via Cargo)
cargo install watchexec-cli

# Or download from: https://github.com/watchexec/watchexec/releases
```

**2. Run auto-rebuild from the backstitch_editor root:**

```bash
cd godot/modules/backstitch_editor

# Auto-rebuild on any .rs or .toml file change
watchexec -e rs,toml just build-backstitch (release/debug)
```

This will watch for changes to `.rs` and `.toml` files and automatically run `just build-backstitch` when changes are detected.
Godot will automatically reload the plugin after the build is complete.

**3. macOS Code Signing (if needed):**

If you're on macOS and need code signing for the built library:

```bash
# In the rust directory, create the identity file
mkdir -p .cargo
echo "Apple Development: Your Name (TEAMID)" > .cargo/.devidentity

# Example:
echo "Apple Development: Nikita Zatkovich (RFTZV7M2RV)" > .cargo/.devidentity
```

**Tip:** Run `watchexec` in one terminal window and keep it running while you develop. Each time you save a Rust file, it will automatically rebuild!

---

## Detailed Setup

### 1. Install Rust

```bash
# Install via rustup (recommended)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Restart terminal, then verify
rustc --version  # Should be 1.91.1 or higher
cargo --version
```

**Windows users**: Download from <https://rustup.rs/>

### 2. Install Python 3 + SCons

#### Windows

1. Download Python 3.10+ from <https://www.python.org/downloads/>
2. **Important**: Check "Add Python to PATH" during installation
3. Open new terminal:

   ```bash
   pip install scons
   ```

#### macOS

```bash
# Install Python 3 (usually pre-installed)
brew install python3

# Install SCons
pip3 install scons
```

#### Linux (Ubuntu/Debian)

```bash
sudo apt-get update
sudo apt-get install python3 python3-pip
pip3 install scons
```

### 3. Install C++ Compiler

#### Windows

1. Download **Visual Studio 2019 or later**
2. During installation, select **"Desktop development with C++"**
3. Minimum components needed:
   - MSVC v142+ build tools
   - Windows 10 SDK

#### macOS

```bash
# Install Xcode Command Line Tools
xcode-select --install

# For this specific branch, Xcode 16+ is recommended
# Download from: https://developer.apple.com/xcode/
```

#### Linux (Ubuntu/Debian)

```bash
sudo apt-get install build-essential pkg-config libx11-dev libxcursor-dev \
    libxinerama-dev libgl1-mesa-dev libglu-dev libasound2-dev libpulse-dev \
    libudev-dev libxi-dev libxrandr-dev
```

### 4. Platform-Specific Setup

#### macOS: Vulkan SDK (Required for backstitch-4.6)

```bash
cd godot  # In the godot repository root
sh misc/scripts/install_vulkan_sdk_macos.sh
```

#### macOS: Code Signing

```bash
cd modules/backstitch_editor/rust/plugin

# Create identity file with your Apple Developer certificate
echo "Apple Development: Your Name (TEAMID)" > .cargo/.devidentity
```

Without this, macOS will show security warnings when loading the plugin.

---

### 5. Install Just

Once you have `cargo`, it's easiest to run:

```
cargo install just
```
