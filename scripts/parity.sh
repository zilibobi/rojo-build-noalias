#!/usr/bin/env bash
# One-shot parity with v1: every case is built twice with OLD_BIN (old-a,
# old-b) and once with NEW_BIN, with SNAP (or a fixture copy) as cwd. A case
# passes when old-a equals old-b and the new output equals old-a.
#
# v1 keeps its workspaces in $TEMP or $TMPDIR/rojo-build-noalias, which a live
# v1 watcher may be using, so each tool runs with TEMP and TMPDIR pointed at a
# dir of its own inside this run's tmp. v1 exits 0 even when Rojo fails, so
# its outputs are checked for existence rather than trusting its status.
#
# Prints PASS <case>, DIFF <case> [reason] or SKIP <case> <reason> per case,
# then time <case> old=<ms> new=<ms>. Exits 1 on any DIFF. The run's tmp is
# removed when every case passes and kept (and printed) otherwise, or always
# with KEEP=1.
set -uo pipefail

: "${OLD_BIN:?set OLD_BIN to the v1 binary}"
: "${NEW_BIN:?set NEW_BIN to the new binary}"
: "${SNAP:?set SNAP to a vfx-forge snapshot}"

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/rbn-parity.XXXXXX")
failed=0

mkdir -p "$tmp/env/old-a" "$tmp/env/old-b" "$tmp/env/new" "$tmp/logs"

if ! cargo build --quiet --manifest-path "$repo/Cargo.toml" --example rbxm_diff; then
  echo "cannot build the rbxm_diff example" >&2
  exit 2
fi

if command -v jq >/dev/null 2>&1; then
  sort_json() { jq -S . "$1"; }
else
  sort_json() {
    python3 -I -c 'import json, sys; print(json.dumps(json.load(open(sys.argv[1])), indent=2, sort_keys=True))' "$1"
  }
fi

now_ms() {
  echo $(($(date +%s%N) / 1000000))
}

# run <tool> <dir> <args...>: runs one tool in <dir>, @OUT@ in the args
# replaced by the tool's own output path, logging to logs/<case>.<tool>.log.
# Sets `elapsed` (ms) and `status`.
run() {
  local tool=$1 dir=$2 bin
  shift 2

  if [ "$tool" = new ]; then
    bin=$NEW_BIN
  else
    bin=$OLD_BIN
  fi

  local args=()
  local arg

  for arg in "$@"; do
    args+=("${arg//@OUT@/$out_base.$tool$out_ext}")
  done

  local start
  start=$(now_ms)
  (cd "$dir" && TEMP="$tmp/env/$tool" TMPDIR="$tmp/env/$tool" "$bin" "${args[@]}") \
    >"$tmp/logs/$case.$tool.log" 2>&1
  status=$?
  elapsed=$(($(now_ms) - start))
}

fail() {
  echo "DIFF $case${1:+ $1}"
  failed=1
}

# build_case <case> <dir> <args...>: a build whose output is a model file.
build_case() {
  case=$1
  local dir=$2
  shift 2
  out_base=$tmp/$case
  out_ext=.rbxm

  local old_ms
  run old-a "$dir" "$@"
  old_ms=$elapsed
  run old-b "$dir" "$@"
  run new "$dir" "$@"
  local new_status=$status new_ms=$elapsed

  local a=$out_base.old-a.rbxm b=$out_base.old-b.rbxm n=$out_base.new.rbxm

  if [ ! -f "$a" ] || [ ! -f "$b" ]; then
    fail "old tool wrote no output (log $tmp/logs/$case.old-a.log)"
  elif ! cmp -s "$a" "$b"; then
    fail "old tool unstable"
  elif [ "$new_status" -ne 0 ] || [ ! -f "$n" ]; then
    fail "new tool failed with exit $new_status (log $tmp/logs/$case.new.log)"
  elif cmp -s "$a" "$n"; then
    echo "PASS $case"
  else
    fail
    cargo run --quiet --manifest-path "$repo/Cargo.toml" --example rbxm_diff -- "$a" "$n" | sed 's/^/  /'
  fi

  echo "time $case old=$old_ms new=$new_ms"
}

# convert_case <case> <dir>: --convert-to, compared as trees after sorting the
# keys of each tree's default.project.json.
convert_case() {
  case=$1
  local dir=$2
  out_base=$tmp/$case
  out_ext=

  local old_ms
  run old-a "$dir" --convert-to @OUT@
  old_ms=$elapsed
  run old-b "$dir" --convert-to @OUT@
  run new "$dir" --convert-to @OUT@
  local new_status=$status new_ms=$elapsed

  local tool tree
  for tool in old-a old-b new; do
    tree=$out_base.$tool

    if [ -f "$tree/default.project.json" ]; then
      sort_json "$tree/default.project.json" >"$tmp/sorted.json" && mv "$tmp/sorted.json" "$tree/default.project.json"
    fi
  done

  local a=$out_base.old-a b=$out_base.old-b n=$out_base.new

  if [ ! -d "$a" ] || [ ! -d "$b" ]; then
    fail "old tool wrote no output (log $tmp/logs/$case.old-a.log)"
  elif ! diff -r "$a" "$b" >/dev/null; then
    fail "old tool unstable"
  elif [ "$new_status" -ne 0 ] || [ ! -d "$n" ]; then
    fail "new tool failed with exit $new_status (log $tmp/logs/$case.new.log)"
  elif diff -r "$a" "$n" >"$tmp/logs/$case.diff"; then
    echo "PASS $case"
  else
    fail
    head -n 40 "$tmp/logs/$case.diff" | sed 's/^/  /'
  fi

  echo "time $case old=$old_ms new=$new_ms"
}

build_case dev "$SNAP" -o @OUT@ dev.project.json
build_case default "$SNAP" -o @OUT@

if [ -d "$SNAP/dist/staging" ] && [ -d "$SNAP/dist/link" ]; then
  build_case bundle "$SNAP" --process-orphans -o @OUT@ bundle.project.json
else
  echo "SKIP bundle (needs dist/staging and dist/link in the snapshot)"
fi

for fixture in "$repo"/tests/fixtures/*/; do
  name=$(basename "$fixture")

  if [ ! -f "$fixture/default.project.json" ]; then
    continue
  fi

  cp -R "$fixture" "$tmp/fixture-$name"
  build_case "fixture-$name" "$tmp/fixture-$name" -o @OUT@
done

convert_case convert-forge-vfx "$SNAP/pkg/forge-vfx"

if [ "$failed" -eq 0 ] && [ "${KEEP:-0}" != 1 ]; then
  rm -rf "$tmp"
else
  echo "kept $tmp"
fi

exit "$failed"
