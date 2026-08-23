#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

if [ "$#" -ne 1 ]; then
  echo "usage: fetch-kernel-source.sh ABSOLUTE_OUTPUT_TARBALL" >&2
  exit 64
fi

output=$1
case "$output" in
  /*) ;;
  *) echo "output must be absolute" >&2; exit 64 ;;
esac
if [ -e "$output" ] || [ -L "$output" ]; then
  echo "output must not already exist" >&2
  exit 73
fi
parent=$(dirname "$output")
if [ ! -d "$parent" ] || [ -L "$parent" ] ||
   [ "$(cd -- "$parent" && pwd -P)" != "$parent" ]; then
  echo "output parent must be an existing canonical directory" >&2
  exit 73
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
version=$(jq -er 'if .schemaVersion == 1 and .architecture == "arm64" then .kernelVersion else empty end' \
  "$script_dir/guest-source.lock.json")
url=$(jq -er '.sourceUrl' "$script_dir/guest-source.lock.json")
expected=$(jq -er '.sourceSha256' "$script_dir/guest-source.lock.json")
temporary=$(mktemp "$parent/.grokptah-linux-kernel.XXXXXX")
cleanup() {
  rm -f -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
  --output "$temporary" "$url"
observed=$(sha256sum "$temporary" | awk '{print $1}')
if [ "$observed" != "$expected" ]; then
  echo "Linux $version source digest mismatch" >&2
  exit 65
fi
install -m 0444 "$temporary" "$output"
trap - EXIT HUP INT TERM
rm -f -- "$temporary"
printf 'kernel_version=%s\nsource_sha256=%s\noutput=%s\n' "$version" "$observed" "$output"
