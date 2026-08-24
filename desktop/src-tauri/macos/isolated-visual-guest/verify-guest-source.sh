#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-guest-source.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
guest_source="$script_dir/guest-init.c"
protocol_header="$script_dir/protocol.h"
fragment="$script_dir/kernel.config.fragment"
lock="$script_dir/guest-source.lock.json"
work=$(mktemp -d /private/tmp/grokptah-guest-source-proof.XXXXXX)
cleanup() {
  rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

sh -n "$script_dir/fetch-kernel-source.sh"
sh -n "$script_dir/build-guest-image.sh"
# shellcheck disable=SC2016 # literal source fragments are intentional
for required in '--proto-redir' '--connect-timeout 15' '--max-time 900' \
  '--max-filesize 2147483648' \
  'output appeared during source fetch' 'mv "$temporary" "$output"'; do
  grep -F -- "$required" "$script_dir/fetch-kernel-source.sh" >/dev/null
done
# shellcheck disable=SC2016 # literal source fragments are intentional
for required in staged_output_image staged_output_manifest \
  published_image published_manifest \
  'trap - EXIT HUP INT TERM' \
  "trap 'exit 129' HUP" "trap 'exit 130' INT" "trap 'exit 143' TERM" \
  'mv "$staged_output_image" "$output_image"' \
  'mv "$staged_output_manifest" "$output_manifest"' \
  'guest image or manifest output appeared during staged build'; do
  grep -F "$required" "$script_dir/build-guest-image.sh" >/dev/null
done
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
  'CONFIG_DRM_FBDEV_EMULATION=y' \
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
for required in GPT_AF_VSOCK GPT_GUEST_BOOTSTRAP_PORT GPT_GUEST_BOOTSTRAP_BIND \
  GPT_GUEST_BOOTSTRAP_INPUT GPT_GUEST_BOOTSTRAP_EVENT_BINDING_ACK GPT_SYS_REBOOT GPT_SYS_SOCKET \
  GPT_SYS_OPENAT GPT_SYS_LSEEK GPT_SYS_POLL GPT_SYS_GETRANDOM GPT_GUEST_FRAME_BYTES \
  GPT_POLLOUT GPT_POLLNVAL GPT_GUEST_IO_ATTEMPTS GPT_GUEST_IO_WAIT_MILLISECONDS \
  gpt_wait_for_io 'attempts < GPT_GUEST_IO_ATTEMPTS' GPT_O_RDWR gpt_open_framebuffer \
  gpt_sleep_retry GPT_SYS_NANOSLEEP \
  'ready < 0' \
  gpt_render_fixture gpt_capture_frame gpt_send_frame \
  gpt_apply_fixture_input; do
  grep -F "$required" "$guest_source" >/dev/null
done
grep -F '"/dev/fb0"' "$guest_source" >/dev/null
for required in \
  GPT_ISOLATED_VISUAL_FRAME_MAGIC GPT_ISOLATED_VISUAL_FRAME_HEADER_BYTES \
  GPT_ISOLATED_VISUAL_FRAME_MAX_PACKET_BYTES \
  gpt_isolated_visual_frame_header GPT_ISOLATED_VISUAL_INPUT_MAGIC \
  GPT_ISOLATED_VISUAL_INPUT_HEADER_BYTES GPT_ISOLATED_VISUAL_INPUT_MAX_PACKET_BYTES \
  gpt_isolated_visual_input_header gpt_isolated_visual_input_valid \
  gpt_isolated_visual_frame_seal \
  GPT_ISOLATED_VISUAL_BINDING_MAGIC GPT_ISOLATED_VISUAL_BINDING_HEADER_BYTES \
  gpt_isolated_visual_binding_header gpt_isolated_visual_binding_digest \
  gpt_isolated_visual_channel_secret gpt_isolated_visual_binding_valid; do
  grep -F "$required" "$protocol_header" >/dev/null
done
if grep -F 'while (gpt_syscall3(GPT_SYS_NANOSLEEP' "$guest_source" >/dev/null; then
  echo "guest reconnect backoff must not retry nanosleep inside an unbounded loop" >&2
  exit 1
fi
printf 'isolated guest source, protocol, and closed kernel fragment: pass\n'
