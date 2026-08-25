#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

if [ "$#" -ne 3 ]; then
  echo "usage: build-guest-image.sh SOURCE_TARBALL ABSOLUTE_OUTPUT_IMAGE ABSOLUTE_OUTPUT_MANIFEST" >&2
  exit 64
fi

source_tarball=$1
output_image=$2
output_manifest=$3
case "$source_tarball" in
  /*) ;;
  *) echo "source tarball must be absolute" >&2; exit 64 ;;
esac
for output in "$output_image" "$output_manifest"; do
  case "$output" in
    /*) ;;
    *) echo "outputs must be absolute" >&2; exit 64 ;;
  esac
  if [ -e "$output" ] || [ -L "$output" ]; then
    echo "output must not already exist: $output" >&2
    exit 73
  fi
done
if [ ! -f "$source_tarball" ] || [ -L "$source_tarball" ] ||
   [ "$(realpath "$source_tarball")" != "$source_tarball" ]; then
  echo "source tarball must be a canonical regular file" >&2
  exit 66
fi

image_parent=$(dirname "$output_image")
manifest_parent=$(dirname "$output_manifest")
for parent in "$image_parent" "$manifest_parent"; do
  if [ ! -d "$parent" ] || [ -L "$parent" ] ||
     [ "$(cd -- "$parent" && pwd -P)" != "$parent" ]; then
    echo "output parent must be an existing canonical directory" >&2
    exit 73
  fi
done
if [ "$(uname -s)" != "Linux" ]; then
  echo "guest image builds must run in the pinned Linux qualification environment" >&2
  exit 69
fi

for tool in clang ld.lld make cpio sha256sum tar xz file python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "required guest build tool is missing: $tool" >&2
    exit 69
  }
done

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
kernel_version=$(jq -er 'if .schemaVersion == 1 and .architecture == "arm64" then .kernelVersion else empty end' \
  "$script_dir/guest-source.lock.json")
expected_source_sha=$(jq -er '.sourceSha256' "$script_dir/guest-source.lock.json")
observed_source_sha=$(sha256sum "$source_tarball" | awk '{print $1}')
if [ "$observed_source_sha" != "$expected_source_sha" ]; then
  echo "kernel source digest does not match guest-source.lock.json" >&2
  exit 65
fi

work=$(mktemp -d /tmp/grokptah-isolated-guest-build.XXXXXX)
published_image=0
published_manifest=0
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  rm -rf -- "$work"
  if [ "$status" -ne 0 ]; then
    if [ "$published_image" -eq 1 ]; then
      rm -f -- "$output_image"
    fi
    if [ "$published_manifest" -eq 1 ]; then
      rm -f -- "$output_manifest"
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

staged_output_image="$work/grokptah-isolated-guest-v1.Image"
staged_output_manifest="$work/grokptah-isolated-guest-v1.manifest.json"

top_level=$(tar -tf "$source_tarball" | sed -n '1s#/.*##p')
if [ -z "$top_level" ] ||
   tar -tf "$source_tarball" | awk -v root="$top_level" '
     $0 ~ /^\// || $0 ~ /(^|\/)\.\.\// || ($0 != root && index($0, root "/") != 1) { bad=1 }
     END { exit bad ? 0 : 1 }
   '; then
  echo "kernel source archive has unsafe or unexpected paths" >&2
  exit 65
fi
tar -xf "$source_tarball" -C "$work" --no-same-owner --no-same-permissions
kernel="$work/$top_level"

initramfs="$kernel/grokptah-initramfs.cpio"
init_binary="$work/grokptah-guest-init"
clang --target=aarch64-linux-gnu \
  -fuse-ld=lld \
  -std=c11 \
  -ffreestanding \
  -nostdlib \
  -static \
  -fno-builtin \
  -fno-stack-protector \
  -fno-pie \
  -O2 \
  -Wall \
  -Wextra \
  -Werror \
  -Wl,-e,_start \
  -Wl,--build-id=none \
  -Wl,-z,noexecstack \
  "$script_dir/guest-init.c" \
  -o "$init_binary"
file "$init_binary" | grep -F 'ELF 64-bit LSB pie executable' >/dev/null ||
  file "$init_binary" | grep -F 'ELF 64-bit LSB executable' >/dev/null
install -d -m 0700 "$work/initramfs"
install -m 0555 "$init_binary" "$work/initramfs/init"
touch -d '@0' "$work/initramfs/init"
(cd "$work/initramfs" && printf './init\n' | cpio --quiet --create --format=newc --owner=0:0 --reproducible > "$initramfs")

make -C "$kernel" ARCH=arm64 LLVM=1 KCONFIG_NOTIMESTAMP=1 defconfig
# merge_config.sh writes KCONFIG_CONFIG relative to CWD unless -O is set.
# Hosted CI CWD is the repository root, so a CWD merge left the kernel
# tree's .config as stock defconfig and olddefconfig reported no change
# while CONFIG_INITRAMFS_SOURCE never reached require_config.
"$kernel/scripts/kconfig/merge_config.sh" -m -r -O "$kernel" "$kernel/.config" \
  "$script_dir/kernel.config.fragment"
make -C "$kernel" ARCH=arm64 LLVM=1 KCONFIG_NOTIMESTAMP=1 olddefconfig

require_config() {
  wanted=$1
  if grep -Fx "$wanted" "$kernel/.config" >/dev/null; then
    return 0
  fi
  # After olddefconfig, kconfig writes disabled bools as
  # `# CONFIG_FOO is not set` rather than `CONFIG_FOO=n`. The fragment still
  # uses `=n` so merge_config.sh can override defconfig's `=y`.
  case "$wanted" in
    CONFIG_*=n)
      name=${wanted%=n}
      if grep -Fx "# ${name} is not set" "$kernel/.config" >/dev/null; then
        return 0
      fi
      ;;
  esac
  echo "kernel configuration lost required setting: $wanted" >&2
  echo "observed INITRAMFS/INITRD/MODULES lines:" >&2
  grep -E '^(CONFIG_(BLK_DEV_INITRD|INITRAMFS_|MODULES)=|# CONFIG_MODULES is not set)' \
    "$kernel/.config" >&2 || true
  exit 65
}
require_config CONFIG_BLK_DEV_INITRD=y
require_config 'CONFIG_INITRAMFS_SOURCE="grokptah-initramfs.cpio"'
require_config CONFIG_VSOCKETS=y
require_config CONFIG_VIRTIO_VSOCKETS=y
require_config CONFIG_DRM_VIRTIO_GPU=y
require_config CONFIG_PCI_HOST_GENERIC=y
require_config CONFIG_MODULES=n
for forbidden in CONFIG_INET=y CONFIG_IPV6=y CONFIG_VIRTIO_NET=y CONFIG_USB_SUPPORT=y \
  CONFIG_SOUND=y CONFIG_SCSI=y CONFIG_ATA=y CONFIG_VIRTIO_BLK=y CONFIG_VIRTIO_FS=y; do
  if grep -Fx "$forbidden" "$kernel/.config" >/dev/null; then
    echo "kernel configuration enables forbidden device/network surface: $forbidden" >&2
    exit 65
  fi
done

export SOURCE_DATE_EPOCH=0
export KBUILD_BUILD_TIMESTAMP='1970-01-01 00:00:00 UTC'
export KBUILD_BUILD_USER=grokptah
export KBUILD_BUILD_HOST=isolated-visual
export KBUILD_BUILD_VERSION=1
make -C "$kernel" ARCH=arm64 LLVM=1 KCONFIG_NOTIMESTAMP=1 LOCALVERSION=-grokptah-isolated-v1 \
  -j"${JOBS:-2}" Image
image="$kernel/arch/arm64/boot/Image"
test -s "$image"
install -m 0444 "$image" "$staged_output_image"

source_sha=$(sha256sum "$source_tarball" | awk '{print $1}')
initramfs_sha=$(sha256sum "$initramfs" | awk '{print $1}')
config_sha=$(sha256sum "$kernel/.config" | awk '{print $1}')
image_sha=$(sha256sum "$staged_output_image" | awk '{print $1}')
clang_sha=$(clang --version | sha256sum | awk '{print $1}')
lld_sha=$(ld.lld --version | sha256sum | awk '{print $1}')
make_sha=$(make --version | sha256sum | awk '{print $1}')
export GPT_KERNEL_VERSION="$kernel_version"
export GPT_SOURCE_SHA="$source_sha"
export GPT_INITRAMFS_SHA="$initramfs_sha"
export GPT_CONFIG_SHA="$config_sha"
export GPT_IMAGE_SHA="$image_sha"
export GPT_CLANG_SHA="$clang_sha"
export GPT_LLD_SHA="$lld_sha"
export GPT_MAKE_SHA="$make_sha"
python3 - "$staged_output_manifest" <<'PY'
import json
import os
import sys

output = sys.argv[1]
data = {
    "schemaVersion": 1,
    "artifact": "grokptah-isolated-guest-v1",
    "architecture": "arm64",
    "kernelVersion": os.environ["GPT_KERNEL_VERSION"],
    "kernelSourceSha256": os.environ["GPT_SOURCE_SHA"],
    "initramfsSha256": os.environ["GPT_INITRAMFS_SHA"],
    "kernelConfigSha256": os.environ["GPT_CONFIG_SHA"],
    "imageSha256": os.environ["GPT_IMAGE_SHA"],
    "clangVersionSha256": os.environ["GPT_CLANG_SHA"],
    "lldVersionSha256": os.environ["GPT_LLD_SHA"],
    "makeVersionSha256": os.environ["GPT_MAKE_SHA"],
    "init": "/init",
    "network": False,
    "storage": False,
    "sharedDirectories": False,
    "hostCredentials": False,
    "bootstrapProtocol": 1,
}
with open(output, "w", encoding="utf-8") as handle:
    json.dump(data, handle, sort_keys=True, separators=(",", ":"))
    handle.write("\n")
PY
chmod 0444 "$staged_output_manifest"
if [ -e "$output_image" ] || [ -L "$output_image" ] ||
   [ -e "$output_manifest" ] || [ -L "$output_manifest" ]; then
  echo "guest image or manifest output appeared during staged build" >&2
  exit 73
fi
mv "$staged_output_image" "$output_image"
published_image=1
mv "$staged_output_manifest" "$output_manifest"
published_manifest=1
printf 'guest_image=%s\nimage_sha256=%s\nmanifest=%s\n' "$output_image" "$image_sha" "$output_manifest"
