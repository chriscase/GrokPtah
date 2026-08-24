#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

if [ "$#" -ne 1 ]; then
  echo "usage: build-helper.sh ABSOLUTE_OUTPUT" >&2
  exit 64
fi

output=$1
case "$output" in
  /*) ;;
  *) echo "output must be absolute" >&2; exit 64 ;;
esac
if [ -L "$output" ] || [ -e "$output" ]; then
  echo "output must not already exist" >&2
  exit 73
fi
output_parent=$(dirname "$output")
if [ ! -d "$output_parent" ] || [ -L "$output_parent" ] ||
   [ "$(unset CDPATH; cd -- "$output_parent" && pwd -P)" != "$output_parent" ]; then
  echo "output parent must be an existing non-symlink directory" >&2
  exit 73
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
temporary=$(mktemp "$output_parent/.grokptah-isolated-helper.XXXXXX")
module_cache=$(mktemp -d /private/tmp/grokptah-clang-module-cache.XXXXXX)
cleanup() {
  rm -f -- "$temporary"
  rm -rf -- "$module_cache"
}
trap cleanup EXIT HUP INT TERM

CLANG_MODULE_CACHE_PATH="$module_cache"
export CLANG_MODULE_CACHE_PATH
xcrun clang \
  -fobjc-arc \
  -fblocks \
  -fvisibility=hidden \
  -mmacosx-version-min=14.0 \
  -Os \
  -Wall \
  -Wextra \
  -Werror \
  -Wl,-dead_strip \
  -framework Foundation \
  -framework Security \
  -framework Virtualization \
  "$script_dir/main.m" \
  -o "$temporary"
chmod 0555 "$temporary"
mv "$temporary" "$output"
trap - EXIT HUP INT TERM
