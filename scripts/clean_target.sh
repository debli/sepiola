#!/usr/bin/env bash
# Clean stale Rust build artifacts in target/debug (Plan 1):
#   1. Remove incremental compilation cache (fully rebuildable).
#   2. For each workspace crate, keep only the newest executable / .rlib / .rmeta
#      and delete older hash-suffixed copies.
# Third-party dependency artifacts are untouched.
#
# Usage:
#   ./clean_target.sh          # actually delete
#   ./clean_target.sh -n       # dry run, only list what would be deleted

set -euo pipefail

TARGET_DIR="/home/neu/.ts/Sepiola/target/debug"
DRY_RUN=false
for arg in "$@"; do
  case "$arg" in
    -n|--dry-run) DRY_RUN=true ;;
    *) TARGET_DIR="$arg" ;;
  esac
done

CRATES=(sepiola sepiola_gui sepiola_lib sepiola_eval sepiola_scene_eval sepiola_acoustid)

DEPS_DIR="$TARGET_DIR/deps"
INC_DIR="$TARGET_DIR/incremental"

before_kb=$(du -sk "$TARGET_DIR" | cut -f1)

rm_cmd() {
  if $DRY_RUN; then
    echo "[dry-run] would delete: $*"
  else
    rm -f -- "$@"
  fi
}

# 1. incremental cache
if [[ -d "$INC_DIR" ]]; then
  if $DRY_RUN; then
    echo "[dry-run] would delete dir: $INC_DIR ($(du -sh "$INC_DIR" | cut -f1))"
  else
    echo "removing $INC_DIR ($(du -sh "$INC_DIR" | cut -f1))"
    rm -rf "$INC_DIR"
  fi
fi

# 2. stale workspace-crate artifacts
cd "$DEPS_DIR"
for c in "${CRATES[@]}"; do
  # executables (no extension, not .d files): keep newest
  ls -t -- "$c"-* 2>/dev/null | grep -v '\.d$' | tail -n +2 | while read -r f; do
    rm_cmd "$f"
  done || true
  # rlib / rmeta: keep newest of each extension
  for e in rlib rmeta; do
    ls -t -- "lib$c"-*."$e" 2>/dev/null | tail -n +2 | while read -r f; do
      rm_cmd "$f"
    done || true
  done
done

after_kb=$(du -sk "$TARGET_DIR" | cut -f1)
freed_gb=$(awk -v a="$before_kb" -v b="$after_kb" 'BEGIN{printf "%.1f", (a-b)/1048576}')
echo "Done. Freed ~${freed_gb}G (target/debug: $((before_kb/1048576))G -> $((after_kb/1048576))G)"
