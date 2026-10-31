#!/usr/bin/env bash
# Copies the vfx-forge working tree to <dest> for parity runs, reading the
# source only. dist is excluded as a whole, then the three dist dirs a build
# mounts are copied back when they exist.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <dest>" >&2
  exit 2
fi

src=/home/zili/Projects/laptop/vfx-forge
dest=$1

mkdir -p "$dest"
rsync -a --delete --exclude .git --exclude node_modules --exclude /.worktrees --exclude /.claude --exclude /dist "$src/" "$dest/"

for dir in staging empty link; do
  if [ -d "$src/dist/$dir" ]; then
    mkdir -p "$dest/dist"
    rsync -a --delete "$src/dist/$dir/" "$dest/dist/$dir/"
  fi
done

echo "snapshot $dest $(find "$dest" -type f | wc -l)"
