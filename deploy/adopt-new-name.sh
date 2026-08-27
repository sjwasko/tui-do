#!/usr/bin/env bash
#
# Move a criax installation's own files to the tui-do name.
#
# The repository rename is a code change; this is the half that lives in the
# user's home directory and cannot be renamed by a commit: the config, the
# store, and the symlink on PATH. Run it once, after quitting every instance.
#
# Safe to re-run: anything already moved is left alone.

set -euo pipefail

readonly OLD=criax
readonly NEW=tui-do

config_dir="${XDG_CONFIG_HOME:-$HOME/.config}"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="$HOME/.local/bin"
repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# The store is SQLite in WAL mode. Moving it out from under a live process
# separates it from its -wal and would lose whatever has not been checkpointed.
if pgrep -x "$OLD" >/dev/null || pgrep -x "$NEW" >/dev/null; then
	echo "$OLD or $NEW is still running — quit every instance first." >&2
	echo "The store is SQLite in WAL mode; moving it live would lose the -wal." >&2
	exit 1
fi

move() { # src dst
	if [[ -e $1 && ! -e $2 ]]; then
		mkdir -p "$(dirname "$2")"
		mv "$1" "$2"
		echo "  $1 -> $2"
	fi
}

echo "config:"
move "$config_dir/$OLD" "$config_dir/$NEW"
# token_file is written as an absolute path, so it does not follow the rename.
if [[ -f $config_dir/$NEW/config.yaml ]]; then
	sed -i "s#$config_dir/$OLD/#$config_dir/$NEW/#g" "$config_dir/$NEW/config.yaml"
fi

echo "store:"
for suffix in "" -wal -shm; do
	move "$data_dir/$OLD/$OLD.db$suffix" "$data_dir/$NEW/$NEW.db$suffix"
done
rmdir "$data_dir/$OLD" 2>/dev/null || true

echo "PATH:"
if [[ -L $bin_dir/$OLD ]]; then
	rm "$bin_dir/$OLD"
	echo "  removed $bin_dir/$OLD"
fi
mkdir -p "$bin_dir"
ln -sfn "$repo/target/release/$NEW" "$bin_dir/$NEW"
echo "  $bin_dir/$NEW -> $repo/target/release/$NEW"

# A symlink to a path that is not there resolves to nothing, and the shell reports
# it as "command not found" -- which reads as "never installed" rather than
# "pointing at the wrong place". Two ways to get here: running this before the
# first release build, or running it and *then* moving the repository, which bakes
# in the old path.
if [[ ! -x $bin_dir/$NEW ]]; then
	echo
	echo "warning: $bin_dir/$NEW does not resolve to an executable." >&2
	echo "  It points at $repo/target/release/$NEW, which is not there." >&2
	echo "  Build it with 'cargo build --workspace --release', or -- if the" >&2
	echo "  repository has moved since -- re-run this script from its new home." >&2
	exit 1
fi

echo
echo "Done. Check with:  $NEW --version"
