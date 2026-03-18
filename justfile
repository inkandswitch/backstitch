set dotenv-load
set dotenv-filename := "build.env"
set dotenv-required

# Get the default architecture
default_arch := shell("rustc --version --verbose | grep host | awk '{print $2}'")

default:
  just --list

# Safely symlink src to dest
_symlink src dest:
    #!/usr/bin/env python3
    # Use python for a cross-platform safe symlink that isn't dependent on Git bash installation settings
    from pathlib import Path
    src = Path("{{src}}")
    dest = Path("{{dest}}")
    if dest.exists():
        if not dest.is_symlink():
            print(f"Destination {{dest}} already exists and is NOT a symlink!")
            exit(1)

        target = (dest.parent / dest.readlink()).resolve()
        
        if not target.samefile(src.resolve()):
            print(f"Destination {{dest}} already exists, but it points to {target} instead of {src.resolve()}.")
            exit(1)

        # symlink already exists and is valid
        exit(0)
    
    try:
        dest.symlink_to(src.resolve(), True)
        exit(0)
    except OSError as e:
        print((f"Failed to create symlink from {dest.resolve()} to {src.resolve()}. "
            "If you are on Windows, you MUST enable Developer Mode in system settings."))
        exit(1)


# Create build/
@_make-build-dir:
    mkdir -p build

# Create build/patchwork
@_make-plugin-dir: _make-build-dir
    mkdir -p build/patchwork

# Clone a repository to a directory and check out a commit.
_clone repo_url directory checkout:
    #!/usr/bin/env sh
    # set -euxo pipefail

    # if directory is empty (as a result of a previous clone that failed), remove it
    if [[ -n $(find "{{directory}}" -maxdepth 0 -type d -empty) ]]; then
        if rmdir "{{directory}}"; then
            echo "Removed directory: {{directory}}"
        else
            echo "Failed to clean directory: {{directory}}"
            exit 1
        fi
    fi

    # If the directory doesn't exist, freshly clone
    if [[ ! -d "{{directory}}" ]]; then
        git clone "git@github.com:{{repo_url}}" "{{directory}}" --no-checkout --filter=blob:none 
    fi

    # Require .git to exist
    if [[ ! -d "{{directory}}/.git" ]]; then
        echo "Not a git repository: {{directory}}"
        exit 1
    fi

    # Ensure the git repository is actually valid
    # Force Git to use THIS directory's metadata only, so the parent isn't grabbed.
    # (If the parent is grabbed, we could corrupt the enclosing repository!)
    if ! GIT_DIR="{{absolute_path(directory)}}/.git" GIT_WORK_TREE="{{absolute_path(directory)}}" \
            git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "Invalid git repository: {{directory}}"
        exit 1
    fi

    if git -C "{{directory}}" remote | grep -q '^origin$'; then
        git -C "{{directory}}" remote set-url origin "git@github.com:{{repo_url}}"
    else
        git -C "{{directory}}" remote add origin "git@github.com:{{repo_url}}"
    fi
    
    # try checkout, if we get an error fetch then try again.
    # this won't pull updates from the remote, but that's probably fine for now.
    # if we need to handle local changes, this will need to be refactored (to force reset?)
    git -C "{{directory}}"  fetch --depth 1 origin {{checkout}}
    if git -C "{{directory}}"  checkout "{{checkout}}" | grep -q '^fatal'; then
        git -C "{{directory}}"  checkout "{{checkout}}"
    fi

# Clone our desired project and checkout the proper commit.
[arg('project', pattern='moddable-platformer|threadbare')]
_acquire-project project: _make-build-dir
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ "{{project}}" = "moddable-platformer" ]]; then
        just _clone "$MODDABLE_PLATFORMER_REPO" "build/{{project}}" "$MODDABLE_PLATFORMER_REF"
    else
        just _clone "$THREADBARE_REPO" "build/{{project}}" "$THREADBARE_REF"
    fi

# Clone the Godot repository and checkout the proper commit.
_acquire-godot: _make-build-dir
    just _clone "$GODOT_REPO" "build/godot" "$GODOT_REF"

# Clone the GodotFormatters repository and checkout the proper commit.
_acquire-formatters: _make-build-dir
    just _clone "$GODOT_FORMATTERS_REPO" "build/GodotFormatters" "$GODOT_FORMATTERS_REF"

# Link our plugin build directory to the desired project.
_link-project project: (_acquire-project project) _make-plugin-dir
    mkdir -p "build/{{project}}/addons"
    just _symlink "build/patchwork" "build/{{project}}/addons/patchwork"

# Link our custom Godot editor module
[arg('skip_godot_clone', pattern='yes|no')]
_link-godot skip_godot_clone="no":
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ "{{skip_godot_clone}}" = "no" ]] ; then
        echo "**** Cloning Godot... ****"
        just _acquire-godot
    else
        echo "**** Skipping Godot clone... ****"
    fi
    just _symlink "editor" "build/godot/modules/patchwork_editor"

# Link the assets directory for our plugin
_link-public: _make-plugin-dir
    just _symlink "public" "build/patchwork/public"

# Build the Godot editor with our editor module linked in. Available profiles are release, debug, or sani (for use_asan=yes)
[arg('profile', pattern='release|debug|sani')]
[arg('skip_godot_clone', pattern='yes|no')]
build-godot profile skip_godot_clone="no": (_link-godot skip_godot_clone)
    #!/usr/bin/env sh
    # set -euxo pipefail
    cd "build/godot"
    EXTRA_BUILD_FLAGS=""
    # check for macos; if yes, add `generate_bundle=yes`
    if [[ "{{os()}}" = "macos" ]] ; then
        EXTRA_BUILD_FLAGS="generate_bundle=yes"
        # check for the .cargo/.devidentity file; if it exists, add `bundle_sign_identity=<contents>`
        if [ -f .cargo/.devidentity ]; then
            EXTRA_BUILD_FLAGS="$EXTRA_BUILD_FLAGS bundle_sign_identity=$(cat .cargo/.devidentity)"
        fi
    fi
    # TODO: figure out a way to see if scons actually needs a run, since this takes forever even when built
    if [[ {{profile}} = "release" ]] ; then
        scons dev_build=no debug_symbols=no target=editor deprecated=yes minizip=yes compiledb=yes metal=no module_text_server_fb_enabled=yes $EXTRA_BUILD_FLAGS
    elif [[ {{profile}} = "sani" ]] ; then
        scons dev_build=yes target=editor compiledb=yes deprecated=yes minizip=yes tests=yes use_asan=yes metal=no module_text_server_fb_enabled=yes $EXTRA_BUILD_FLAGS
    else
        scons dev_build=yes target=editor compiledb=yes deprecated=yes minizip=yes tests=yes metal=no module_text_server_fb_enabled=yes $EXTRA_BUILD_FLAGS
    fi

# Build the Rust plugin binaries.
_build-plugin architecture profile tracing_support:
    #!/usr/bin/env sh
    if [[ {{tracing_support}} = "tokio-console" ]] ; then
        export RUSTFLAGS="--cfg tokio_unstable"
        cargo build --profile="{{profile}}" --target="{{architecture}}" --features "tokio-console"
    else
        cargo build --profile="{{profile}}" --target="{{architecture}}"
    fi

# Sign macOS plugin binaries using the identity from .cargo/.devidentity (one line, plain text).
@_sign-macos-plugin:
    #!/usr/bin/env sh
    # set -euxo pipefail

    # check for CI env variable, if it is set, skip signing
    if [ "$CI" = "1" ]; then
        echo "Skipping macOS plugin signing on CI"
        exit 0
    fi

    if [ ! -f .cargo/.devidentity ]; then
        echo "**** No development identity file found; if you want to enable code signing, create a .cargo/.devidentity file with your dev ID."
        echo "**** Example: echo 'Developer ID Application: Your Name (TEAMID)' > .cargo/.devidentity"
        echo "**** HINT: use 'security find-identity -p codesigning -v' to find your dev ID."
        exit 0
    fi
    identity=$(cat .cargo/.devidentity)
    framework="build/patchwork/bin/libpatchwork_rust_core.macos.framework"
    if [ ! -d "$framework" ]; then
        exit 0
    fi
    for dylib in "$framework"/*.dylib; do
        [ -f "$dylib" ] && codesign -s "$identity" -f "$dylib"
    done
    codesign --deep -s "$identity" -f "$framework"

# Build the multi-arch target for MacOS.
[parallel]
_build-plugin-all-macos profile tracing_support: (_build-plugin "aarch64-apple-darwin" profile tracing_support) \
        (_build-plugin "x86_64-apple-darwin" profile tracing_support) _make-plugin-dir
    mkdir -p build/patchwork/bin

    # Copy the entire macos directory to get the Resources framework directory
    rm -rf "build/patchwork/bin/libpatchwork_rust_core.macos.framework"
    cp -r "rust/macos/libpatchwork_rust_core.macos.framework" "build/patchwork/bin/libpatchwork_rust_core.macos.framework"

    # Rather than copying the generated .dylibs, we combine them into a single one.
    lipo -create -output build/patchwork/bin/libpatchwork_rust_core.macos.framework/libpatchwork_rust_core.dylib \
        target/aarch64-apple-darwin/{{profile}}/libpatchwork_rust_core.dylib \
        target/x86_64-apple-darwin/{{profile}}/libpatchwork_rust_core.dylib

    just _sign-macos-plugin

[parallel]
_build-plugin-single-arch architecture profile tracing_support: (_build-plugin architecture profile tracing_support) _make-plugin-dir
    #!/usr/bin/env sh
    # set -euo pipefail
    mkdir -p build/patchwork/bin

    # Copy the entire macos directory to get the Resources framework directory
    rm -rf "build/patchwork/bin/libpatchwork_rust_core.macos.framework"
    cp -r "rust/macos/libpatchwork_rust_core.macos.framework" "build/patchwork/bin/libpatchwork_rust_core.macos.framework"

    if [ -f "target/{{architecture}}/{{profile}}/patchwork_rust_core.dll" ] ; then
        cp "target/{{architecture}}/{{profile}}/patchwork_rust_core.dll" \
            build/patchwork/bin/patchwork_rust_core.windows.{{architecture}}.dll
    fi

    if [ -f "target/{{architecture}}/{{profile}}/libpatchwork_rust_core.so" ] ; then
        cp "target/{{architecture}}/{{profile}}/libpatchwork_rust_core.so" \
            build/patchwork/bin/patchwork_rust_core.linux.{{architecture}}.so
    fi

    if [ -f "target/{{architecture}}/{{profile}}/libpatchwork_rust_core.dylib" ] ; then
        cp "target/{{architecture}}/{{profile}}/libpatchwork_rust_core.dylib" \
            build/patchwork/bin/libpatchwork_rust_core.macos.framework/libpatchwork_rust_core.dylib
        just _sign-macos-plugin
    fi
    
    if [ -f "target/{{architecture}}/{{profile}}/patchwork_rust_core.pdb" ] ; then
        cp "target/{{architecture}}/{{profile}}/patchwork_rust_core.pdb" \
            build/patchwork/bin/patchwork_rust_core.pdb
    fi

# Write plugin.cfg and Patchwork.gdextension
_configure-patchwork: _make-plugin-dir
    #!/usr/bin/env python3
    import os
    import subprocess

    # load the version from git
    print(f"Current directory: {os.getcwd()}")
    print(os.listdir())

    git_describe = subprocess.check_output(["git", "describe", "--tags", "--abbrev=6"]).decode("utf-8").strip()

    # if it has more than two `-` in the version, replace all the subsequent `-` with `+`
    if git_describe.count("-") >= 2:
        first_index = git_describe.find("-")
        if first_index != -1:
            git_describe = git_describe[:first_index] + "-" + git_describe[first_index + 1 :].replace("-", "+")

    print(f"Loaded version from Git repository: {git_describe}")

    with open("build/patchwork/plugin.cfg", "w") as file:
        file.write(f"""[plugin]
    name="Patchwork"
    description="Version control for Godot"
    author="Ink & Switch"
    version="{git_describe}"
    script=""
    """)

    with open("build/patchwork/Patchwork.gdextension", "w") as file:
        file.write(f"""[configuration]
    entry_symbol = "gdext_rust_init"
    compatibility_minimum = 4.6
    reloadable = true

    [libraries]
    linux.debug.x86_64 =        "bin/patchwork_rust_core.linux.x86_64-unknown-linux-gnu.so"
    linux.release.x86_64 =      "bin/patchwork_rust_core.linux.x86_64-unknown-linux-gnu.so"
    linux.debug.arm64 =         "bin/patchwork_rust_core.linux.aarch64-unknown-linux-gnu.so"
    linux.release.arm64 =       "bin/patchwork_rust_core.linux.aarch64-unknown-linux-gnu.so"
    linux.debug.arm32 =         "bin/patchwork_rust_core.linux.armv7-unknown-linux-gnueabihf.so"
    linux.release.arm32 =       "bin/patchwork_rust_core.linux.armv7-unknown-linux-gnueabihf.so"
    windows.debug.x86_64 =      "bin/patchwork_rust_core.windows.x86_64-pc-windows-msvc.dll"
    windows.release.x86_64 =    "bin/patchwork_rust_core.windows.x86_64-pc-windows-msvc.dll"
    windows.debug.arm64 =       "bin/patchwork_rust_core.windows.aarch64-pc-windows-msvc.dll"
    windows.release.arm64 =     "bin/patchwork_rust_core.windows.aarch64-pc-windows-msvc.dll"
    macos.debug =               "bin/libpatchwork_rust_core.macos.framework/libpatchwork_rust_core.dylib"
    macos.release =             "bin/libpatchwork_rust_core.macos.framework/libpatchwork_rust_core.dylib"
    """)

# Build the plugin and output it to the plugin build dir. For MacOS multi-arch, use architecture=all-apple-darwin to build all architectures.
[parallel]
[arg('profile', pattern='release|debug')]
[arg('tracing_support', pattern='none|tokio-console')]
build-patchwork profile architecture=(default_arch) tracing_support="none": _configure-patchwork _link-public
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ "{{architecture}}" = "all-apple-darwin" ]] ; then
        just _build-plugin-all-macos "{{profile}}" "{{tracing_support}}"
        exit 0
    fi

    profile="release"
    if [[ "{{profile}}" = "debug" ]] ; then
        profile="release_debug"
    fi
    
    just _build-plugin-single-arch "{{architecture}}" "$profile" "{{tracing_support}}"

# Reset the Godot repository, removing the linked module and resetting the repo state.
clean-godot:
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ ! -d "build" ]]; then
        exit 0
    fi
    cd "build"
    
    # set -euxo pipefail
    if [[ ! -d "godot" ]]; then
        exit 0
    fi
    cd godot
    git checkout -f $GODOT_REF
    git clean -xdf

# Remove any built Patchwork artifacts.
clean-patchwork:
    #!/usr/bin/env sh
    # set -euxo pipefail
    cargo clean
    if [[ ! -d "build/patchwork" ]]; then
        exit 0
    fi
    rm -rf "build/patchwork"

# Clean a single project, resetting the repository and unlinking Patchwork.
[arg('project', pattern='moddable-platformer|threadbare')]
clean-project project:
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ "{{project}}" = "moddable-platformer" ]]; then
        checkout="$MODDABLE_PLATFORMER_REF"
    else
        checkout="$THREADBARE_REF"
    fi
    
    if [[ ! -d "build" ]]; then
        exit 0
    fi
    cd "build"
    
    if [[ ! -d "{{project}}" ]]; then
        exit 0
    fi
    cd {{project}}
    git checkout -f "$checkout"
    git clean -xdf

# Clean Patchwork, and the projects threadbare, moddable-platformer.
clean: (clean-project "threadbare") (clean-project "moddable-platformer") clean-patchwork

# Write to the project .cfg with a new server url
[arg('project', pattern='moddable-platformer|threadbare')]
_write-url project url: (_link-project project)
    #!/usr/bin/env python3
    import os
    import subprocess
    from pathlib import Path

    # For now, if the URL is blank (default), don't touch the config.
    # When we have an actual serve command, we can then expect the user to always specify. 
    if "{{url}}" == "":
        exit(0)

    path = "build/{{project}}/patchwork.cfg"

    try:
        f = open(path)
    except FileNotFoundError:
        lines = []
    else:
        with f: lines = f.readlines()

    new_lines: list[str] = []
    found_patchwork = False
    for line in lines:
        # place the server url immediately after patchwork
        if line.startswith("[patchwork]"):
            found_patchwork = True
            new_lines.append(line)
            new_lines.append('server_url="{{url}}"\n')
        # skip future server URLs
        elif not line.startswith("server_url="):
            new_lines.append(line)
    
    if not found_patchwork:
        new_lines = ['[patchwork]\n', 'server_url="{{url}}"']

    with open(path, "w") as file:
        file.writelines(new_lines)

[arg('project', pattern='moddable-platformer|threadbare')]
[arg('patchwork_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
echo-parms project="moddable-platformer" patchwork_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no":
    #!/usr/bin/env sh
    # set -euxo pipefail
    echo "patchwork_profile: {{patchwork_profile}}"
    echo "godot_profile:     {{godot_profile}}"
    echo "server_url:        {{server_url}}"
    echo "tracing_support:   {{tracing_support}}"
    echo "skip_godot_clone:  {{skip_godot_clone}}"

# Prepare a project for launch with Godot. Available projects are threadbare, moddable-platformer.
[parallel]
[arg('project', pattern='moddable-platformer|threadbare')]
[arg('patchwork_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
prepare project="moddable-platformer" patchwork_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no": \
        (echo-parms project patchwork_profile godot_profile server_url tracing_support skip_godot_clone) (_link-project project) (build-godot godot_profile skip_godot_clone) (build-patchwork patchwork_profile default_arch tracing_support) (_write-url project server_url)


# Launch a project with Godot. Available projects are threadbare, moddable-platformer.
[arg('project', pattern='moddable-platformer|threadbare')]
[arg('patchwork_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
launch project="moddable-platformer" patchwork_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no": \
        (prepare project patchwork_profile godot_profile server_url tracing_support skip_godot_clone)
    #!/usr/bin/env sh
    # set -euxo pipefail
    
    case "{{arch()}}" in
        "x86_64")
            arch=x86_64 ;;
        "aarch64")
            arch=arm64 ;;
        *)
            echo "Unsupported architecture for development: {{arch()}}."
            echo "If you think this architecture should be supported, please open an issue on Github with your use-case and system details."
            exit 1 ;;
    esac
    
    case "{{os()}}" in
        "windows")
            platform="windows"
            ext=".exe" ;;
        "linux")
            platform="linuxbsd"
            ext="" ;;
        "macos")
            platform="macos"
            ext="" ;;
        *)
            echo "Unsupported OS for development: {{os()}}."
            echo "If you think this OS should be supported, please open an issue on Github with your use-case and system details."
            exit 1 ;;
    esac
    if [[ "{{os()}}" = "macos" ]] ; then
        if [[ {{godot_profile}} = "release" ]] ; then
            godot_path="build/godot/bin/godot_macos_editor.app/Contents/MacOS/Godot"
        else
            godot_path="build/godot/bin/godot_macos_editor_dev.app/Contents/MacOS/Godot"
        fi
    else
        if [[ {{godot_profile}} = "release" ]] ; then
            godot_path="build/godot/bin/godot.$platform.editor.$arch$ext"
        else
            godot_path="build/godot/bin/godot.$platform.editor.dev.$arch$ext"
        fi
    fi

    $godot_path -e --path "build/{{project}}"