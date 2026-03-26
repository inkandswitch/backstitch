#!/bin/sh

# fail if any command fails
set -e
# set echo to print commands
set -x



SCRIPT_PATH=$(dirname "$0")
PW_PATH=$(realpath "$SCRIPT_PATH/..")
GODOT_PATH=$(realpath "$SCRIPT_PATH/../build/godot")
GITCMD="git -C $GODOT_PATH"


# function to check if a remote exists, and if not, add it
function _add_remote {
    local remote=$1
    local url=$2
	if ! $GITCMD remote | grep -q "$remote"; then
		echo "Adding remote $remote"
        $GITCMD remote add $remote $url
    fi
}

function _delete_branch {
    local branch=$1
    if $GITCMD branch | grep -q "$branch"; then
		echo "Deleting branch $branch"
        $GITCMD branch -D $branch
	fi
}

_add_remote "upstream" "git@github.com:godotengine/godot.git"
$GITCMD fetch --all
$GITCMD checkout master
_delete_branch "upstream-master"
# create a new branch from upstream/master and name it 'upstream-master'
$GITCMD checkout -b upstream-master --track upstream/master


HEAD=$($GITCMD rev-parse HEAD)
SHORT_HASH=$($GITCMD rev-parse --short HEAD)
NEW_BRANCH_NAME="pw-wb-$SHORT_HASH"

# source the build.env
source "$PW_PATH/build.env"

# check for the existence of our 'GODOT_REMOTE' remote
_add_remote "$GODOT_REMOTE" "git@github.com:$GODOT_REMOTE/godot.git"
_add_remote "KoBeWi" "git@github.com:KoBeWi/godot.git"

# check if the branch already exists
_delete_branch $NEW_BRANCH_NAME

$GITCMD branch -c $NEW_BRANCH_NAME

$GITCMD checkout $NEW_BRANCH_NAME
$GITCMD reset --hard $HEAD

BRANCHES_TO_MERGE=(
	bind-get-unsaved-scripts
	add-reload_all_scenes
	import_and_save_resource
	add-close-file
	bind-open_scene_or_resource_path
	bind-resourceloader-get_resource_script_class
)

KOBEWI_BRANCHES=(
	slashtableflip
)

#unset -e, we don't want to exit on error here
set +e
# set fail on error
for branch in "${BRANCHES_TO_MERGE[@]}"; do
    # merge branch, use a merge commit
    $GITCMD merge $GODOT_REMOTE/$branch -m "Merge branch '$branch'"
	if [ $? -ne 0 ]; then
		$GITCMD merge --abort
		echo "Error: Failed to merge branch '$branch'"
		exit 1
	fi
done

for branch in "${KOBEWI_BRANCHES[@]}"; do
    # merge branch, use a merge commit
    $GITCMD merge KoBeWi/$branch -m "Merge branch '$branch'"
	if [ $? -ne 0 ]; then
		$GITCMD merge --abort
		echo "Error: Failed to merge branch '$branch'"
		exit 1
	fi
done

set -e

# detect OS for cross-platform sed compatibility
# macOS (BSD sed) requires -i '' while Linux (GNU sed) uses -i
if [ "$(uname)" = "Darwin" ]; then
    sed_in_place() { sed -i '' "$@"; }
else
    sed_in_place() { sed -i "$@"; }
fi


# update the GODOT_REF in build.env
sed_in_place "s/GODOT_REF=.*/GODOT_REF=$NEW_BRANCH_NAME/" "$PW_PATH/build.env"

$GITCMD push $GODOT_REMOTE $NEW_BRANCH_NAME --set-upstream
