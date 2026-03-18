#!/bin/sh

SCRIPT_PATH=$(dirname "$0")
PW_PATH=$(realpath "$SCRIPT_PATH/..")
GODOT_PATH=$(realpath "$SCRIPT_PATH/../build/godot")

git -C "$GODOT_PATH" fetch --all
git -C "$GODOT_PATH" checkout master
git -C "$GODOT_PATH" pull

HEAD=$(git -C "$GODOT_PATH" rev-parse HEAD)
SHORT_HASH=$(git -C "$GODOT_PATH" rev-parse --short HEAD)
NEW_BRANCH_NAME="pw-wb-$SHORT_HASH"

# check for the existence of the 'nikitalita' remote
if ! git -C "$GODOT_PATH" remote | grep -q "nikitalita"; then
    git -C "$GODOT_PATH" remote add nikitalita git@github.com:nikitalita/godot.git
	git -C "$GODOT_PATH" fetch nikitalita
fi

# check if the branch already exists
if git -C "$GODOT_PATH" branch -a | grep -q $NEW_BRANCH_NAME; then
    git -C "$GODOT_PATH" branch -D $NEW_BRANCH_NAME
fi

git -C "$GODOT_PATH" branch -c $NEW_BRANCH_NAME

git -C "$GODOT_PATH" checkout $NEW_BRANCH_NAME
git -C "$GODOT_PATH" reset --hard $HEAD

BRANCHES_TO_MERGE=(
	bind-get-unsaved-scripts
	add-reload_all_scenes
	import_and_save_resource
	add-close-file
)

# set fail on error
for branch in "${BRANCHES_TO_MERGE[@]}"; do
    # merge branch, use a merge commit
    git -C "$GODOT_PATH" merge nikitalita/$branch -m "Merge branch '$branch'"
	if [ $? -ne 0 ]; then
		echo "Error: Failed to merge branch '$branch'"
		exit 1
	fi
done

# detect OS for cross-platform sed compatibility
# macOS (BSD sed) requires -i '' while Linux (GNU sed) uses -i
if [ "$(uname)" = "Darwin" ]; then
    sed_in_place() { sed -i '' "$@"; }
else
    sed_in_place() { sed -i "$@"; }
fi


# update the GODOT_REF in build.env
sed_in_place "s/GODOT_REF=.*/GODOT_REF=$NEW_BRANCH_NAME/" "$PW_PATH/build.env"

git -C "$GODOT_PATH" push nikitalita $NEW_BRANCH_NAME --set-upstream
