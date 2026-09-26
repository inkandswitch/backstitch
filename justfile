set dotenv-load
set dotenv-filename := "build.env"
set dotenv-required

export RUST_BACKTRACE := "full"

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

# Create build/backstitch
@_make-plugin-dir: _make-build-dir
    mkdir -p build/backstitch

# Clone a repository to a directory and check out a commit.
_clone repo_url directory checkout:
    #!/usr/bin/env sh
    # set -euxo pipefail

    # if directory is empty (as a result of a previous clone that failed), remove it
    if [[ -n $(find "{{directory}}" -maxdepth 0 -type d -empty) ]]; then
        if rmdir "{{directory}}"; then
            echo "Removed directory: {{directory}}"
        else
            echo "\033[31m***CLONE: Failed to clean directory: {{directory}}\033[0m"
            exit 1
        fi
    fi

    # If the directory doesn't exist, freshly clone
    if [[ ! -d "{{directory}}" ]]; then
        git clone "git@github.com:{{repo_url}}" "{{directory}}" --no-checkout --filter=blob:none 
    fi

    # Require .git to exist
    if [[ ! -d "{{directory}}/.git" ]]; then
        echo "\033[31m***CLONE: Not a git repository: {{directory}}\033[0m"
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

    DEPTH_ARG="--depth=1"
    # if this doesn't fail, then the repo has history
    if git -C "{{directory}}" rev-parse --abbrev-ref HEAD >/dev/null 2>&1; then
        if [[ $(git -C "{{directory}}" rev-parse --is-shallow-repository) = "false" ]]; then
            DEPTH_ARG=""
        fi
    fi


    # check if the HEAD is the same as the checkout
    if [[ $(git -C "{{directory}}" rev-parse --abbrev-ref HEAD) = "{{checkout}}" ]]; then
        echo "*** CLONE: {{directory}}: Pulling updates from origin/{{checkout}}*******************"
        # if `pull --ff-only` fails, try to reset the branch to the checkout ref at origin
        if ! git -C "{{directory}}" pull $DEPTH_ARG --ff-only --quiet origin "{{checkout}}" >/dev/null 2>&1; then
            # don't do this if there are local changes
            if git -C "{{directory}}" status --porcelain | grep -q '^[^?][^ ]'; then
                echo "\033[33m*** CLONE: {{directory}}: Local changes detected, skipping pull...\033[0m"
                exit 0
            fi
            echo "*** CLONE: {{directory}}: {{checkout}} is out-of-sync with origin/{{checkout}}, fetching and resetting..."
            git -C "{{directory}}" fetch $DEPTH_ARG origin {{checkout}}
            git -C "{{directory}}" reset --hard "origin/{{checkout}}"
        fi
    else
        echo "*** CLONE: {{directory}}: Fetching updates from origin/{{checkout}}*******************"
        git -C "{{directory}}" fetch $DEPTH_ARG origin {{checkout}}
    fi

    echo "*** CLONE: {{directory}}: Checking out origin/{{checkout}}*******************"
    # try checkout, if we get an error fetch then try again.
    # this won't pull updates from the remote, but that's probably fine for now.
    # if we need to handle local changes, this will need to be refactored (to force reset?)
    if git -C "{{directory}}"  checkout "{{checkout}}" | grep -q '^fatal'; then
        git -C "{{directory}}"  checkout "{{checkout}}"
    fi

# Clone our desired project and checkout the proper commit.
[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
_acquire-project project: _make-build-dir
    #!/usr/bin/env sh
    # set -euxo pipefail
    case "{{project}}" in
        "moddable-platformer")
            just _clone "$MODDABLE_PLATFORMER_REPO" "build/{{project}}" "$MODDABLE_PLATFORMER_REF"
            ;;
        "moddable-pong")
            just _clone "$MODDABLE_PONG_REPO" "build/{{project}}" "$MODDABLE_PONG_REF"
            ;;
        "threadbare")
            just _clone "$THREADBARE_REPO" "build/{{project}}" "$THREADBARE_REF"
            ;;
    esac

# Clone the Godot repository and checkout the proper commit.
_acquire-godot: _make-build-dir
    just _clone "$GODOT_REPO" "build/godot" "$GODOT_REF"

# Clone the GodotFormatters repository and checkout the proper commit.
_acquire-formatters: _make-build-dir
    just _clone "$GODOT_FORMATTERS_REPO" "build/GodotFormatters" "$GODOT_FORMATTERS_REF"

# Link our plugin build directory to the desired project.
_link-project project: (_acquire-project project) _make-plugin-dir
    mkdir -p "build/{{project}}/addons"
    just _symlink "build/backstitch" "build/{{project}}/addons/backstitch"

# Link our custom Godot editor module
[arg('profile', pattern='release|debug|sani')]
[arg('skip_godot_clone', pattern='yes|no')]
_link-godot profile="debug" skip_godot_clone="no":
    #!/usr/bin/env sh
    # set -euxo pipefail
    if [[ "{{ profile }}" = 'release' ]] ; then
        echo "**** Skipping Godot clone for release build ****"
    elif [[ "{{ skip_godot_clone }}" = "no" ]] ; then
        echo "**** Cloning Godot... ****"
        just _acquire-godot
    else
        echo "**** Skipping Godot clone... ****"
    fi
    rm -f "build/godot/modules/backstitch_editor"

# Link the assets directory for our plugin
_link-public: _make-plugin-dir
    just _symlink "public" "build/backstitch/public"

_download-godot godot_dir godot_path architecture slug platform:
    #!/usr/bin/env python3
    import os
    import subprocess
    from pathlib import Path
    from urllib.request import urlopen, Request
    from zipfile import ZipFile
    import shutil
    import stat
    import tempfile

    # RECOMMENDED_GODOT is expected to be <version number>-<flavor>, such as 4.7.1-stable
    recommended_version = str(os.getenv("RECOMMENDED_GODOT"))
    version, flavor = recommended_version.split("-")

    godot_dir = Path("{{ godot_dir }}")
    godot_path = Path("{{ godot_path }}")
    godot_versionfile = Path("{{ godot_path }}" + ".version.txt")

    print("Ensuring godot folder exists")
    godot_dir.mkdir(parents=True, exist_ok=True)

    if godot_path.exists():
        print("Godot exists - checking version")
        current_version = godot_versionfile.read_text().strip() if godot_versionfile.exists() else ""
        if current_version == recommended_version:
            print("Godot is already at recommended version")
            exit(0)
        else:
            print(f"""Godot exists at the wrong version.
                Current:'{current_version}'
                Recommended:'{recommended_version}'
                Clearing dir and downloading.""")
            godot_versionfile.unlink(missing_ok=True)
            if godot_path.is_dir():
                shutil.rmtree(godot_path)
            elif godot_path.is_file():
                godot_path.unlink()
    else:
        print("Godot doesn't exist yet")

    url = f"https://downloads.godotengine.org/?version={version}&flavor={flavor}&slug={{ slug }}.zip&platform={{ platform }}"
    print("Downloading Godot from " + url)

    with tempfile.TemporaryDirectory() as tmpdir:
        zip_path = Path(tmpdir) / "downloaded.zip"
        extract_path = Path(tmpdir) / "extract"
        from urllib.request import Request, urlopen

        req = Request(
            url,
            headers={
                "User-Agent": "Mozilla/5.0",
            },
        )

        with urlopen(req) as response:
            zip_path.write_bytes(response.read())

        with ZipFile(zip_path) as zip:
            zip.extractall(path=extract_path)
            
        if "{{ platform }}" == "macos":
            for child in extract_path.glob("*.app"):
                (child / "Contents" / "MacOS" / "Godot").chmod(child.stat().st_mode | stat.S_IEXEC)
                child.rename(extract_path / "godot_macos_editor.app")
        else:
            # For windows, exclude the console binary.
            child = next(c for c in extract_path.glob("Godot_v*") if "console" not in c.name)
            child.chmod(child.stat().st_mode | stat.S_IEXEC)
            child.rename(extract_path / godot_path.name)
    
        shutil.copytree(extract_path, godot_dir, dirs_exist_ok=True)
    
    godot_versionfile.write_text(recommended_version)

# Build the Godot editor with our editor module linked in. Available profiles are release, debug, or sani (for use_asan=yes)
[arg('profile', pattern='release|debug|sani')]
[arg('skip_godot_clone', pattern='yes|no')]
build-godot profile skip_godot_clone="no": (_link-godot profile skip_godot_clone)
    #!/usr/bin/env sh
    # set -euxo pipefail
    EXTRA_BUILD_FLAG=""
    EXTRA_ID_FLAG=""
    if [[ "{{ profile }}" = 'release' ]] ; then
        godot_path=""
        arch={{ arch() }}
        godot_dir="./build/godot/bin"
        slug=""
        platform=""
        ext=""
        case "{{ os() }}" in
            "windows")
                platform="windows"
                if [[ "{{ arch() }}" == "x86_64" ]] ; then
                    slug="win64.exe"
                else
                    slug="windows_arm64.exe"
                fi
                ext=".exe" ;;
            "linux")
                platform="linuxbsd"
                if [[ "{{ arch() }}"  == "x86_64" ]] ; then
                    slug="linux.x86_64"
                else
                    slug="linux.aarch64"
                fi
                ext="" ;;
            "macos")
                platform="macos"
                slug="macos.universal"
                ext="" ;;
            *)
                echo "Unsupported OS for development: {{ os() }}."
                echo "If you think this OS should be supported, please open an issue on Github with your use-case and system details."
                exit 1 ;;
        esac
        if [[ "{{ os() }}" = "macos" ]] ; then
            godot_path="./build/godot/bin/godot_macos_editor.app/Contents/MacOS/Godot"
        else
            godot_path="./build/godot/bin/godot.$platform.editor.$arch$ext"
        fi
        just _download-godot $godot_dir $godot_path $arch $slug $platform
    else
        # check for macos; if yes, add `generate_bundle=yes`
        if [[ "{{os()}}" = "macos" ]] ; then
            EXTRA_BUILD_FLAG="generate_bundle=yes"
            # check for the .cargo/.devidentity file; if it exists, add `bundle_sign_identity=<contents>`
            if [ -f .cargo/.devidentity ]; then
                DEV_ID="$(cat .cargo/.devidentity)"
                EXTRA_ID_FLAG="bundle_sign_identity=$DEV_ID"
                echo "signing godot with identity: $DEV_ID"
            else
                echo "**** No development identity file found; if you want to enable code signing, create a .cargo/.devidentity file with your dev ID."
                echo "**** Example: echo 'Developer ID Application: Your Name (TEAMID)' > .cargo/.devidentity"
                echo "**** HINT: use 'security find-identity -p codesigning -v' to find your dev ID."
            fi
        fi
        cd "build/godot"

        if [[ {{ profile }} = "sani" ]] ; then
            scons dev_build=yes target=editor compiledb=yes deprecated=yes minizip=yes tests=yes use_asan=yes metal=no module_text_server_fb_enabled=yes "$EXTRA_BUILD_FLAG" "$EXTRA_ID_FLAG"
        else
            scons dev_build=yes target=editor compiledb=yes deprecated=yes minizip=yes tests=yes metal=no module_text_server_fb_enabled=yes "$EXTRA_BUILD_FLAG" "$EXTRA_ID_FLAG"
        fi
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
    framework="build/backstitch/bin/libbackstitch_godot.macos.framework"
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
    mkdir -p build/backstitch/bin

    # Copy the entire macos directory to get the Resources framework directory
    rm -rf "build/backstitch/bin/libbackstitch_godot.macos.framework"
    cp -r "backstitch/macos/libbackstitch_godot.macos.framework" "build/backstitch/bin/libbackstitch_godot.macos.framework"

    # Rather than copying the generated .dylibs, we combine them into a single one.
    lipo -create -output build/backstitch/bin/libbackstitch_godot.macos.framework/libbackstitch_godot.dylib \
        target/aarch64-apple-darwin/{{profile}}/libbackstitch_godot.dylib \
        target/x86_64-apple-darwin/{{profile}}/libbackstitch_godot.dylib

    just _sign-macos-plugin

[parallel]
_build-plugin-single-arch architecture profile tracing_support: (_build-plugin architecture profile tracing_support) _make-plugin-dir
    #!/usr/bin/env sh
    # set -euo pipefail
    mkdir -p build/backstitch/bin

    # Copy the entire macos directory to get the Resources framework directory
    rm -rf "build/backstitch/bin/libbackstitch_godot.macos.framework"
    cp -r "backstitch/macos/libbackstitch_godot.macos.framework" "build/backstitch/bin/libbackstitch_godot.macos.framework"

    if [ -f "target/{{architecture}}/{{profile}}/backstitch_godot.dll" ] ; then
        cp "target/{{architecture}}/{{profile}}/backstitch_godot.dll" \
            build/backstitch/bin/backstitch_godot.windows.{{architecture}}.dll
    fi

    if [ -f "target/{{architecture}}/{{profile}}/libbackstitch_godot.so" ] ; then
        cp "target/{{architecture}}/{{profile}}/libbackstitch_godot.so" \
            build/backstitch/bin/backstitch_godot.linux.{{architecture}}.so
    fi

    if [ -f "target/{{architecture}}/{{profile}}/libbackstitch_godot.dylib" ] ; then
        cp "target/{{architecture}}/{{profile}}/libbackstitch_godot.dylib" \
            build/backstitch/bin/libbackstitch_godot.macos.framework/libbackstitch_godot.dylib
        just _sign-macos-plugin
    fi

    if [ -f "target/{{architecture}}/{{profile}}/backstitch_godot.pdb" ] ; then
        cp "target/{{architecture}}/{{profile}}/backstitch_godot.pdb" \
            build/backstitch/bin/backstitch_godot.pdb
    fi

# Write plugin.cfg and Backstitch.gdextension
_configure-backstitch: _make-plugin-dir
    #!/usr/bin/env python3
    import os
    import subprocess

    # load the version from git
    print(f"Current directory: {os.getcwd()}")
    print(os.listdir())

    git_describe_raw = subprocess.run(["git", "describe", "--tags", "--abbrev=6"], capture_output=True)
    git_describe = ""
    if git_describe_raw.returncode == 0:
        git_describe = git_describe_raw.stdout.decode("utf-8").strip()

    # if it has more than two `-` in the version, replace all the subsequent `-` with `+`
    if git_describe.count("-") >= 2:
        first_index = git_describe.find("-")
        if first_index != -1:
            git_describe = git_describe[:first_index] + "-" + git_describe[first_index + 1 :].replace("-", "+")

    print(f"Loaded version from Git repository: {git_describe}")
    # remove the `v` prefix if it exists and remove any trailing prerelease or build metadata
    minimum_godot = str(os.getenv("MINIMUM_GODOT")).lstrip("v").split("-")[0].split("+")[0]
    # check if minimum_godot matches <MAJOR>.<MINOR> or <MAJOR>.<MINOR>.<PATCH>
    split_minimum_godot = minimum_godot.split(".")
    if (not (len(split_minimum_godot) >= 2 and len(split_minimum_godot) <= 3)) or (not all(part.isdigit() for part in split_minimum_godot)):
        print(f"**** Minimum Godot version {minimum_godot} is not a valid version!")
        exit(1)

    with open("build/backstitch/plugin.cfg", "w") as file:
        file.write(f"""[plugin]
    name="Backstitch"
    description="Version control for Godot"
    author="Ink & Switch"
    version="{git_describe}"
    script=""
    """)

    with open("build/backstitch/Backstitch.gdextension", "w") as file:
        file.write(f"""[configuration]
    entry_symbol = "gdext_rust_init"
    compatibility_minimum = {minimum_godot}
    reloadable = true

    [libraries]
    linux.editor.x86_64 =        "bin/backstitch_godot.linux.x86_64-unknown-linux-gnu.so"
    linux.editor.arm64 =         "bin/backstitch_godot.linux.aarch64-unknown-linux-gnu.so"
    linux.editor.arm32 =         "bin/backstitch_godot.linux.armv7-unknown-linux-gnueabihf.so"
    windows.editor.x86_64 =      "bin/backstitch_godot.windows.x86_64-pc-windows-msvc.dll"
    windows.editor.arm64 =       "bin/backstitch_godot.windows.aarch64-pc-windows-msvc.dll"
    macos.editor =               "bin/libbackstitch_godot.macos.framework/libbackstitch_godot.dylib"
    """)

# Build the plugin and output it to the plugin build dir. For MacOS multi-arch, use architecture=all-apple-darwin to build all architectures.
[parallel]
[arg('profile', pattern='release|debug')]
[arg('tracing_support', pattern='none|tokio-console')]
build-backstitch profile architecture=(default_arch) tracing_support="none": _configure-backstitch _link-public
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

# Remove any built Backstitch artifacts.
clean-backstitch:
    #!/usr/bin/env sh
    # set -euxo pipefail
    cargo clean
    if [[ ! -d "build/backstitch" ]]; then
        exit 0
    fi
    rm -rf "build/backstitch"

# Clean a single project, resetting the repository and unlinking Backstitch.
[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
clean-project project:
    #!/usr/bin/env sh
    # set -euxo pipefail
    case "{{project}}" in
        "moddable-platformer")
            checkout="$MODDABLE_PLATFORMER_REF"
            ;;
        "moddable-pong")
            checkout="$MODDABLE_PONG_REF"
            ;;
        "threadbare")
            checkout="$THREADBARE_REF"
            ;;
    esac

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

# Clean Backstitch, and the projects threadbare, moddable-platformer.
clean: (clean-project "threadbare") (clean-project "moddable-platformer") (clean-project "moddable-pong") clean-backstitch

# Write to the project .cfg with a new server url
[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
_write-url project url: (_link-project project)
    #!/usr/bin/env python3
    import os
    import subprocess
    from pathlib import Path

    # For now, if the URL is blank (default), don't touch the config.
    # When we have an actual serve command, we can then expect the user to always specify.
    if "{{url}}" == "":
        exit(0)

    path = "build/{{project}}/backstitch.cfg"

    try:
        f = open(path)
    except FileNotFoundError:
        lines = []
    else:
        with f: lines = f.readlines()

    new_lines: list[str] = []
    found_backstitch = False
    for line in lines:
        # place the server url immediately after backstitch
        if line.startswith("[backstitch]"):
            found_backstitch = True
            new_lines.append(line)
            new_lines.append('server_url="{{url}}"\n')
        # skip future server URLs
        elif not line.startswith("server_url="):
            new_lines.append(line)

    if not found_backstitch:
        new_lines = ['[backstitch]\n', 'server_url="{{url}}"']

    with open(path, "w") as file:
        file.writelines(new_lines)

[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
[arg('backstitch_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
echo-parms project="moddable-platformer" backstitch_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no":
    #!/usr/bin/env sh
    # set -euxo pipefail
    echo "backstitch_profile: {{backstitch_profile}}"
    echo "godot_profile:     {{godot_profile}}"
    echo "server_url:        {{server_url}}"
    echo "tracing_support:   {{tracing_support}}"
    echo "skip_godot_clone:  {{skip_godot_clone}}"

# Prepare a project for launch with Godot. Available projects are threadbare, moddable-platformer, moddable-pong.
[parallel]
[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
[arg('backstitch_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
prepare project="moddable-platformer" backstitch_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no": \
        (echo-parms project backstitch_profile godot_profile server_url tracing_support skip_godot_clone) (_link-project project) (build-godot godot_profile skip_godot_clone) (build-backstitch backstitch_profile default_arch tracing_support) (_write-url project server_url)


# Launch a project with Godot. Available projects are threadbare, moddable-platformer, moddable-pong.
[arg('project', pattern='moddable-platformer|moddable-pong|threadbare')]
[arg('backstitch_profile', pattern='release|debug')]
[arg('godot_profile', pattern='release|debug|sani')]
[arg('tracing_support', pattern='none|tokio-console')]
[arg('skip_godot_clone', pattern='yes|no')]
launch project="moddable-platformer" backstitch_profile="release" godot_profile="release" server_url="" tracing_support="none" skip_godot_clone="no": \
        (prepare project backstitch_profile godot_profile server_url tracing_support skip_godot_clone)
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

rebase-godot:
    #!/usr/bin/env sh
    # set -euxo pipefail
    .scripts/rebase_godot.sh
