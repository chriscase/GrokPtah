#!/bin/sh
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: verify-helper-source.sh" >&2
  exit 64
fi

script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
helper_source="$script_dir/main.m"
shared_protocol="$script_dir/../isolated-visual-guest/protocol.h"
native_shim="$script_dir/../../../../crates/codegen/grokptah-agent-bridge/src/computer_use/macos_native_shim.m"
configuration="$script_dir/grokptah-isolated-config-v1.json"
work=$(mktemp -d /private/tmp/grokptah-helper-source-proof.XXXXXX)
cleanup() {
  rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

sh -n "$script_dir/build-helper.sh"
sh -n "$script_dir/package-signed-app.sh"
xcrun clang -fobjc-arc -fblocks -fsyntax-only -mmacosx-version-min=11.0 \
  -Wall -Wextra -Werror "$native_shim"
native_object="$work/macos_native_shim.o"
xcrun clang -fobjc-arc -fblocks -c -mmacosx-version-min=11.0 \
  -Wall -Wextra -Werror "$native_shim" -o "$native_object"
nm -gU "$native_object" | grep -F '_gpt_macos_isolated_runtime_spawn' >/dev/null
nm -gU "$native_object" | grep -F '_gpt_macos_isolated_runtime_spawn_result_free' >/dev/null
plutil -lint \
  "$script_dir/isolated-visual-helper.entitlements.plist" \
  "$script_dir/grokptah-main.entitlements.plist"
jq -e '
  .schemaVersion == 1 and
  .guestProtocolVersion == 1 and
  .kernelCommandLine == "panic=-1 reboot=t init=/init grokptah.isolated_visual=1" and
  .securityProfile == {
    "networkDevices": 0,
    "hostClipboard": false,
    "sharedDirectories": false,
    "credentialForwarding": false,
    "hostInputForwarding": false,
    "usbPassthrough": false,
    "camera": false,
    "microphone": false
  } and
  .limits == {
    "virtualCpus": 2,
    "memoryMib": 4096,
    "overlayBytes": 8589934592,
    "displayWidth": 1280,
    "displayHeight": 800,
    "framesPerSecond": 10,
    "encodedFrameBytes": 16777216,
    "durationSeconds": 600,
    "inputEvents": 256,
    "textEventBytes": 4096
  }
' "$configuration" >/dev/null
for required in \
  'machine.networkDevices = @[];' \
  'machine.directorySharingDevices = @[];' \
  'machine.audioDevices = @[];' \
  'machine.storageDevices = @[];' \
  'machine.keyboards = @[];' \
  'machine.pointingDevices = @[];' \
  'setSocketListener:guestSocketListener' \
  'GPTGuestWaitForReady' \
  'GPTGuestRequestShutdown' \
  'GPTMonotonicMilliseconds' \
  'GPTDeadlineAfter' \
  'durationMilliseconds' \
  'deadline - now' \
  'now == 0' \
  'GPTIsolatedHelperFailureGuestProtocol' \
  'GPT_INPUT_FD' \
  'GPT_FRAME_FD' \
  'GPT_CHALLENGE_FD' \
  'GPTRelayHostInputToGuest' \
  'GPTRelayGuestFrameToHost'; do
  grep -F "$required" "$helper_source" >/dev/null
done
for required in \
  'gpt_macos_isolated_runtime_spawn' \
  'int32_t helper_fd' \
  'int32_t guest_image_fd' \
  'int32_t configuration_fd' \
  'fstat(guest_image_fd' \
  'GPTIsCloseOnExec' \
  'GPTMacIsolatedRuntimeSpawnResult' \
  'POSIX_SPAWN_CLOEXEC_DEFAULT' \
  'posix_spawn_file_actions_adddup2' \
  'GPT_CHALLENGE_FD'; do
  grep -F "$required" "$native_shim" >/dev/null
done
for required in \
  'GPT_ISOLATED_HELPER_EVENT_MAGIC' \
  'GPT_ISOLATED_HELPER_EVENT_BYTES' \
  'GPT_ISOLATED_HELPER_EVENT_BOUND' \
  'GPT_ISOLATED_HELPER_CONTROL_START' \
  'GPT_ISOLATED_HELPER_CONTROL_STOP' \
  'GPT_ISOLATED_HELPER_CONTROL_BIND' \
  'gpt_isolated_helper_event' \
  'GPT_ISOLATED_VISUAL_BINDING_MAGIC' \
  'GPT_ISOLATED_VISUAL_BINDING_HEADER_BYTES' \
  'gpt_isolated_visual_binding_header' \
  'GPT_ISOLATED_VISUAL_FRAME_MAX_PACKET_BYTES'; do
  grep -F "$required" "$shared_protocol" >/dev/null
done
grep -F 'GPTGuestAcceptBindingControl' "$helper_source" >/dev/null
grep -F 'GPTIsolatedHelperEventBound' "$helper_source" >/dev/null
for forbidden in \
  VZNATNetworkDeviceAttachment \
  VZVirtioFileSystemDeviceConfiguration \
  NSPasteboard \
  CGEventPost \
  NSURLSession; do
  if grep -F "$forbidden" "$helper_source" >/dev/null; then
    echo "helper source contains forbidden capability: $forbidden" >&2
    exit 1
  fi
done

helper="$work/grokptah-isolated-visual-helper"
"$script_dir/build-helper.sh" "$helper"
test "$(stat -f '%Lp' "$helper")" = "555"
file "$helper" | grep -F 'Mach-O 64-bit executable'
otool -L "$helper" | grep -F '/Virtualization.framework/'

control_fifo="$work/control"
event_fifo="$work/events"
event_capture="$work/events.bin"
input_fifo="$work/input"
frame_fifo="$work/frames"
challenge_fifo="$work/challenge"
guest_fixture="$work/guest.img"
printf '\001' >"$guest_fixture"
chmod 0444 "$guest_fixture"
mkfifo -m 0600 "$control_fifo" "$event_fifo" "$input_fifo" "$frame_fifo" "$challenge_fifo"
dd if="$event_fifo" of="$event_capture" status=none &
event_reader=$!
dd if="$frame_fifo" of=/dev/null status=none &
frame_reader=$!
cat < /dev/null >"$input_fifo" &
input_writer=$!
dd if="$challenge_fifo" of=/dev/null status=none &
challenge_reader=$!
printf '\003' >"$control_fifo" &
control_writer=$!
set +e
"$helper" \
  3<"$guest_fixture" \
  4<"$configuration" \
  5<"$control_fifo" \
  6>"$event_fifo" \
  7<"$input_fifo" \
  8>"$frame_fifo" \
  9>"$challenge_fifo"
helper_exit=$?
set -e
wait "$control_writer"
wait "$event_reader"
wait "$frame_reader"
wait "$input_writer"
wait "$challenge_reader"
if [ "$helper_exit" -ne 4 ]; then
  echo "invalid start command was not rejected before VM launch" >&2
  exit 1
fi
event_hex=$(od -An -tx1 -v "$event_capture" | tr -d ' \n')
expected_hex=4750544900010001000000000000000047505449000100040000000400000000
if [ "$event_hex" != "$expected_hex" ]; then
  echo "helper emitted an unexpected bootstrap event sequence" >&2
  exit 1
fi

trap - EXIT HUP INT TERM
rm -rf -- "$work"
