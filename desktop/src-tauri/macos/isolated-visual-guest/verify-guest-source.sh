#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-guest-source.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
guest_source="$script_dir/guest-init.c"
fragment="$script_dir/kernel.config.fragment"
lock="$script_dir/guest-source.lock.json"
work=$(mktemp -d /private/tmp/grokptah-guest-source-proof.XXXXXX)
cleanup() {
  rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

sh -n "$script_dir/fetch-kernel-source.sh"
sh -n "$script_dir/build-guest-image.sh"
jq -e '
  .schemaVersion == 1 and
  .kernelVersion == "6.12.104" and
  .architecture == "arm64" and
  .sourceUrl == "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.104.tar.xz" and
  (.sourceSha256 | test("^[0-9a-f]{64}$"))
' "$lock" >/dev/null

clang -std=c11 -Wall -Wextra -Werror "$script_dir/protocol-selftest.c" \
  -o "$work/protocol-selftest"
"$work/protocol-selftest" | grep -Fx 'isolated guest bootstrap protocol self-test: ok'
clang --target=aarch64-linux-gnu -std=c11 -ffreestanding -fno-builtin \
  -fno-stack-protector -fsyntax-only -Wall -Wextra -Werror "$guest_source"

for required in \
  'CONFIG_INITRAMFS_SOURCE="grokptah-initramfs.cpio"' \
  'CONFIG_VSOCKETS=y' \
  'CONFIG_VIRTIO_VSOCKETS=y' \
  'CONFIG_DRM_VIRTIO_GPU=y' \
  'CONFIG_PCI_HOST_GENERIC=y' \
  'CONFIG_MODULES=n'; do
  grep -Fx "$required" "$fragment" >/dev/null
done
for forbidden in \
  CONFIG_INET=y CONFIG_IPV6=y CONFIG_VIRTIO_NET=y CONFIG_USB_SUPPORT=y \
  CONFIG_SOUND=y CONFIG_SCSI=y CONFIG_ATA=y CONFIG_VIRTIO_BLK=y CONFIG_VIRTIO_FS=y; do
  if grep -Fx "$forbidden" "$fragment" >/dev/null; then
    echo "fragment contains a forbidden enabled setting: $forbidden" >&2
    exit 1
  fi
done
for forbidden in AF_INET execve '/bin/sh' mount ptrace; do
  if grep -F "$forbidden" "$guest_source" >/dev/null; then
    echo "guest PID 1 contains forbidden surface: $forbidden" >&2
    exit 1
  fi
done
for required in GPT_AF_VSOCK GPT_GUEST_BOOTSTRAP_PORT GPT_SYS_REBOOT GPT_SYS_SOCKET; do
  grep -F "$required" "$guest_source" >/dev/null
done
printf 'isolated guest source, protocol, and closed kernel fragment: pass\n'
