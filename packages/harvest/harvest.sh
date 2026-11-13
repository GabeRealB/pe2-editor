#!/bin/bash
# harvest.sh [recipe...]: the ports' host files made again from what libraries' own builds do (every recipe of
# `recipes/order` when none is named; a recipe needs those before it).
#
# A ported library compiles what its own build compiles, which differs by host: for each host (linux, macos, windows)
# the harvest runs the library's own build - its configure and make, CMake or meson - with zig's compilers behind
# wrappers that record each compile, and writes the port's `hosts/<host>/`: `sources.zon`, the sources of each of its
# libraries in groups compiled alike, and `build/`, the files its build generated that the sources read
# (`tables.py`). It then installs the library where the next ones' builds find it, as their consumers would. FFmpeg's
# port is written from its own build's log (`ffmpeg.py`), x264's configuration from its configure; binutils' from its
# parts' configure, and CPython's from a reference build of each host (`cpython.py`).
#
# It runs on Linux, and needs bash, curl, git, make, cmake, ninja, patch, pkg-config, nm, python3 (with venv) and the zig
# `cargo xtask packages` fetches (`target/packages/zig/`; $ZIG names another). Its work is kept in
# `target/packages/harvest` ($HARVEST_WORK names another); $JOBS caps its parallel jobs.
#
# A recipe (`recipes/<name>.sh`) is sourced with this folder's `common.sh`. It sets URL (what the port's manifest
# fetches), PORT (its folder under `ports/`), ARCHIVES (`tables.py`'s archive arguments, relative to the build folder)
# and HOSTS when not every host's, and defines `configure`, `make_it` and `install`, run in the build folder with $SRC
# the sources; or it defines `run`, which does all of it its own way.

set -e
HERE=$(cd "$(dirname "$0")" && pwd)
PACKAGES=$(dirname "$HERE")
EDITOR=$(dirname "$PACKAGES")
ZIG_VERSION=$(sed -n '/^\[zig\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$PACKAGES/packages.toml")
ZIG=${ZIG:-$EDITOR/target/packages/zig/zig-x86_64-linux-$ZIG_VERSION/zig}
WORK=${HARVEST_WORK:-$EDITOR/target/packages/harvest}
JOBS=${JOBS:-$(nproc)}
export ZIG_GLOBAL_CACHE_DIR=${ZIG_GLOBAL_CACHE_DIR:-$EDITOR/target/packages/global}
[ -x "$ZIG" ] || { echo "no zig at $ZIG: run cargo xtask packages once, or name one with ZIG" >&2; exit 1; }
mkdir -p "$WORK"
. "$HERE/tools.sh"

recipes=${*:-$(grep -v '^#' "$HERE/recipes/order")}
for recipe in $recipes; do
  [ -f "$HERE/recipes/$recipe.sh" ] || { echo "no recipe $recipe" >&2; exit 1; }
  (
    HOSTS="linux macos windows"
    . "$HERE/common.sh"
    . "$HERE/recipes/$recipe.sh"
    echo "== $recipe"
    if declare -F run > /dev/null; then
      run
      exit
    fi
    for host in $HOSTS; do
      . "$HERE/env.sh" $host
      SRC=$WORK/src/$recipe-$host; B=$WORK/build/$recipe-$host
      unpack "$URL" "$SRC"
      rm -rf "$B"; mkdir -p "$B"; cd "$B"
      unset HARVEST_LOG
      configure > configure.log 2>&1 || { echo "$recipe $host: configure failed (see $B/configure.log)"; tail -20 configure.log; exit 1; }
      export HARVEST_LOG=$B/harvest.log; rm -f "$HARVEST_LOG"
      make_it > make.log 2>&1 || { echo "$recipe $host: make failed (see $B/make.log)"; grep -E "error" make.log | head -10; exit 1; }
      unset HARVEST_LOG
      eval "archives=\"$ARCHIVES\""
      WORK=$WORK python3 "$HERE/tables.py" "$B/harvest.log" "$SRC" "$B" "$PACKAGES/ports/$PORT/hosts/$host" $archives | sed "s/^/  $host: /"
      install > install.log 2>&1 || { echo "$recipe $host: install failed (see $B/install.log)"; tail -10 install.log; exit 1; }
    done
  )
done
