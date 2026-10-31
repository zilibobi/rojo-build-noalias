#!/usr/bin/env bash
# Measures watch mode on a vfx-forge snapshot: edit-to-marker latency and idle
# CPU. Starts the release binary with --watch on dev.project.json in SNAPSHOT,
# waits for the first marker, then 5 times appends `-- latency probe <n>` to
# src/utility.luau and prints latency_ms=<n> from the write to the next marker
# line. Then idles IDLE_S seconds (default 60) and prints idle_cpu_ms=<n> (the
# watcher's user plus system time over that window, from /proc). Restores the
# file and stops the watcher on exit.
#
# Rojo must be on PATH as the real binary: the snapshot has no rokit manifest.
# The watcher's workspace goes in this run's own tmp, never in a shared /tmp
# dir another run may be using.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <snapshot>" >&2
  exit 2
fi

snap=$(cd "$1" && pwd)
repo=$(cd "$(dirname "$0")/.." && pwd)
bin=${BIN:-$repo/target/release/rojo-build-noalias}
idle_s=${IDLE_S:-60}
probe_file=$snap/src/utility.luau

if [ ! -x "$bin" ]; then
  echo "no release binary at $bin (cargo build --release)" >&2
  exit 2
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/rbn-latency.XXXXXX")
mkdir -p "$tmp/env"
cp "$probe_file" "$tmp/utility.luau.orig"
log=$tmp/watch.log
: >"$log"
pid=

cleanup() {
  cp "$tmp/utility.luau.orig" "$probe_file"

  if [ -n "$pid" ]; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi

  rm -rf "$tmp"
}
trap cleanup EXIT

now_ns() {
  date +%s%N
}

# stamps each line with the time it was read
stamp() {
  local line

  while IFS= read -r line; do
    printf '%s %s\n' "$(now_ns)" "$line"
  done
}

markers() {
  grep -c ' OK   Built project to ' "$log" || true
}

# wait_marker <count> <seconds>: waits until the log holds more than <count>
# markers and prints the time of the next one
wait_marker() {
  local want=$(($1 + 1)) limit=$(($2 * 100)) i=0

  while [ "$(markers)" -lt "$want" ]; do
    i=$((i + 1))

    if [ "$i" -gt "$limit" ]; then
      echo "no marker within $2 s; log:" >&2
      cat "$log" >&2
      exit 1
    fi

    sleep 0.01
  done

  grep ' OK   Built project to ' "$log" | sed -n "${want}p" | cut -d' ' -f1
}

cpu_ticks() {
  local stat rest
  stat=$(cat "/proc/$pid/stat")
  rest=${stat##*) }
  # utime and stime are fields 14 and 15, the 12th and 13th after the name
  awk '{ print $12 + $13 }' <<<"$rest"
}

cd "$snap"
TMPDIR=$tmp/env TEMP=$tmp/env TMP=$tmp/env "$bin" --color never --watch -o "$tmp/out.rbxm" dev.project.json > >(stamp >>"$log") &
pid=$!

start_ns=$(now_ns)
first=$(wait_marker 0 60)
echo "first_build_ms=$(((first - start_ns) / 1000000))"

for n in 1 2 3 4 5; do
  sleep 1
  before=$(markers)
  wrote=$(now_ns)
  printf '%s\n' "-- latency probe $n" >>"$probe_file"
  built=$(wait_marker "$before" 10)
  echo "latency_ms=$(((built - wrote) / 1000000))"
done

sleep 1
tick=$(getconf CLK_TCK)
cpu_before=$(cpu_ticks)
sleep "$idle_s"
cpu_after=$(cpu_ticks)
echo "idle_cpu_ms=$(((cpu_after - cpu_before) * 1000 / tick)) over ${idle_s}s"
echo "builds=$(markers)"
