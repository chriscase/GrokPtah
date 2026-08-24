#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

if [ "$#" -ne 5 ]; then
  echo "usage: package-signed-app.sh INPUT_APP OUTPUT_APP GUEST_IMAGE EXPECTED_GUEST_SHA256 SIGNING_IDENTITY" >&2
  exit 64
fi

input_app=$1
output_app=$2
guest_image=$3
expected_guest_sha=$4
signing_identity=$5
script_dir=$(unset CDPATH; cd -- "$(dirname -- "$0")" && pwd -P)
configuration="$script_dir/grokptah-isolated-config-v1.json"

for candidate in "$input_app" "$guest_image"; do
  case "$candidate" in
    /*) ;;
    *) echo "all input paths must be absolute" >&2; exit 64 ;;
  esac
  if [ -L "$candidate" ] || [ ! -e "$candidate" ]; then
    echo "input must exist and must not be a symlink: $candidate" >&2
    exit 66
  fi
  if [ "$(realpath "$candidate")" != "$candidate" ]; then
    echo "input path must already be canonical: $candidate" >&2
    exit 66
  fi
done
case "$output_app" in
  /*) ;;
  *) echo "output app path must be absolute" >&2; exit 64 ;;
esac
if [ -e "$output_app" ] || [ -L "$output_app" ]; then
  echo "output app must not already exist" >&2
  exit 73
fi
output_parent=$(dirname "$output_app")
if [ ! -d "$output_parent" ] || [ -L "$output_parent" ] ||
   [ "$(unset CDPATH; cd -- "$output_parent" && pwd -P)" != "$output_parent" ]; then
  echo "output parent must be an existing canonical non-symlink directory" >&2
  exit 73
fi
if [ -z "$signing_identity" ] || [ "$signing_identity" = "-" ]; then
  echo "a non-ad-hoc signing identity is required" >&2
  exit 64
fi
if [ ! -d "$input_app" ] || [ ! -f "$guest_image" ] ||
   [ ! -f "$configuration" ] || [ -L "$configuration" ]; then
  echo "expected an app directory, regular guest image, and reviewed configuration" >&2
  exit 66
fi
case "$expected_guest_sha" in
  *[!0-9a-f]*|'') echo "expected guest digest must be lowercase hexadecimal" >&2; exit 64 ;;
esac
if [ "${#expected_guest_sha}" -ne 64 ]; then
  echo "expected guest digest must contain exactly 64 characters" >&2
  exit 64
fi
guest_bytes=$(stat -f '%z' "$guest_image")
if [ "$guest_bytes" -le 0 ] || [ "$guest_bytes" -gt 34359738368 ]; then
  echo "guest image violates the 32 GiB package ceiling" >&2
  exit 65
fi
observed_guest_sha=$(shasum -a 256 "$guest_image" | awk '{print $1}')
if [ "$observed_guest_sha" != "$expected_guest_sha" ]; then
  echo "guest image does not match the reviewed digest" >&2
  exit 65
fi

work=$(mktemp -d /private/tmp/grokptah-isolated-package.XXXXXX)
cleanup() {
  rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

helper="$work/grokptah-isolated-visual-helper"
staged_guest="$work/grokptah-isolated-guest-v1.img"
staged_configuration="$work/grokptah-isolated-config-v1.json"
install -m 0444 "$guest_image" "$staged_guest"
install -m 0444 "$configuration" "$staged_configuration"
if [ "$(shasum -a 256 "$staged_guest" | awk '{print $1}')" != "$expected_guest_sha" ] ||
   ! cmp -s "$staged_configuration" "$configuration"; then
  echo "an isolated artifact changed during staging" >&2
  exit 65
fi
"$script_dir/build-helper.sh" "$helper"
/usr/bin/codesign \
  --force \
  --options runtime \
  --timestamp \
  --identifier com.chriscase.grokptah.isolated-visual-helper \
  --entitlements "$script_dir/isolated-visual-helper.entitlements.plist" \
  --sign "$signing_identity" \
  "$helper"
/usr/bin/codesign --verify --strict --all-architectures "$helper"

/usr/bin/ditto "$input_app" "$output_app"
macos_dir="$output_app/Contents/MacOS"
resource_dir="$output_app/Contents/Resources/isolated-visual"
if [ ! -d "$macos_dir" ] || [ -L "$macos_dir" ]; then
  echo "input app has no safe Contents/MacOS directory" >&2
  exit 65
fi
mkdir -p "$resource_dir"
chmod 0755 "$resource_dir"
if [ -L "$resource_dir" ] || [ "$(realpath "$resource_dir")" != "$resource_dir" ]; then
  echo "isolated resource directory must not be a symlink" >&2
  exit 65
fi
for target in \
  "$macos_dir/grokptah-isolated-visual-helper" \
  "$resource_dir/grokptah-isolated-guest-v1.img" \
  "$resource_dir/grokptah-isolated-config-v1.json" \
  "$resource_dir/grokptah-isolated-manifest-v1.json"; do
  if [ -e "$target" ] || [ -L "$target" ]; then
    echo "input app already contains an isolated artifact target" >&2
    exit 65
  fi
done
install -m 0555 "$helper" "$macos_dir/grokptah-isolated-visual-helper"
install -m 0444 "$staged_guest" "$resource_dir/grokptah-isolated-guest-v1.img"
install -m 0444 "$staged_configuration" "$resource_dir/grokptah-isolated-config-v1.json"

packaged_helper="$macos_dir/grokptah-isolated-visual-helper"
packaged_guest="$resource_dir/grokptah-isolated-guest-v1.img"
packaged_configuration="$resource_dir/grokptah-isolated-config-v1.json"
helper_sha=$(shasum -a 256 "$packaged_helper" | awk '{print $1}')
guest_sha=$(shasum -a 256 "$packaged_guest" | awk '{print $1}')
configuration_sha=$(shasum -a 256 "$packaged_configuration" | awk '{print $1}')
requirement_line=$(/usr/bin/codesign -d -r- "$packaged_helper" 2>&1 | sed -n 's/^designated => //p')
if [ -z "$requirement_line" ]; then
  echo "helper has no designated requirement" >&2
  exit 65
fi
requirement_binary="$work/helper-designated-requirement.bin"
/usr/bin/csreq -r "$requirement_line" -b "$requirement_binary"
requirement_sha=$(shasum -a 256 "$requirement_binary" | awk '{print $1}')
if [ "$guest_sha" != "$expected_guest_sha" ]; then
  echo "guest image changed during package assembly" >&2
  exit 65
fi

manifest="$resource_dir/grokptah-isolated-manifest-v1.json"
{
  printf '{\n'
  printf '  "schemaVersion": 1,\n'
  printf '  "backendId": "macos_isolated_visual_candidate_v1",\n'
  printf '  "guestProtocolVersion": 1,\n'
  printf '  "helperContentSha256": "%s",\n' "$helper_sha"
  printf '  "helperSigningRequirementSha256": "%s",\n' "$requirement_sha"
  printf '  "guestImageSha256": "%s",\n' "$guest_sha"
  printf '  "configurationSha256": "%s",\n' "$configuration_sha"
  printf '  "securityProfile": {"networkDevices":0,"hostClipboard":false,"sharedDirectories":false,"credentialForwarding":false,"hostInputForwarding":false,"usbPassthrough":false,"camera":false,"microphone":false},\n'
  printf '  "limits": {"virtualCpus":2,"memoryMib":4096,"overlayBytes":8589934592,"displayWidth":1280,"displayHeight":800,"framesPerSecond":10,"encodedFrameBytes":16777216,"durationSeconds":600,"inputEvents":256,"textEventBytes":4096}\n'
  printf '}\n'
} >"$manifest"
chmod 0444 "$manifest"

/usr/bin/codesign \
  --force \
  --options runtime \
  --timestamp \
  --entitlements "$script_dir/grokptah-main.entitlements.plist" \
  --sign "$signing_identity" \
  "$output_app"
/usr/bin/codesign --verify --deep --strict --all-architectures "$output_app"

assert_entitlement_present() {
  artifact=$1
  entitlement=$2
  if ! /usr/bin/codesign -d --entitlements :- "$artifact" 2>&1 |
    grep -F "$entitlement" >/dev/null; then
    echo "signed artifact is missing required entitlement: $entitlement" >&2
    exit 65
  fi
}

assert_entitlement_absent() {
  artifact=$1
  entitlement=$2
  if /usr/bin/codesign -d --entitlements :- "$artifact" 2>&1 |
    grep -F "$entitlement" >/dev/null; then
    echo "signed artifact carries forbidden entitlement: $entitlement" >&2
    exit 65
  fi
}

assert_entitlement_present "$packaged_helper" "com.apple.security.app-sandbox"
assert_entitlement_present "$packaged_helper" "com.apple.security.virtualization"
assert_entitlement_absent "$packaged_helper" "com.apple.vm.networking"
assert_entitlement_absent "$packaged_helper" "com.apple.security.get-task-allow"
assert_entitlement_absent "$output_app" "com.apple.security.virtualization"
assert_entitlement_absent "$output_app" "com.apple.vm.networking"
assert_entitlement_absent "$output_app" "com.apple.security.get-task-allow"

printf 'packaged_app=%s\n' "$output_app"
printf 'helper_content_sha256=%s\n' "$helper_sha"
printf 'helper_signing_requirement_sha256=%s\n' "$requirement_sha"
printf 'guest_image_sha256=%s\n' "$guest_sha"
printf 'configuration_sha256=%s\n' "$configuration_sha"
trap - EXIT HUP INT TERM
rm -rf -- "$work"
