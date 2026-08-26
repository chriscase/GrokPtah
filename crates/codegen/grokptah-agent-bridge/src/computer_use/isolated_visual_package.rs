//! Invariants the shipped guest and helper package must satisfy.
//!
//! These are test-only: the crate never reads the package at runtime, and the
//! packaged supervisor measures a signed bundle rather than this source tree.
//! What they pin is agreement — the profile and limits the Rust contract
//! enforces are exactly the ones the shipped configuration declares, and the
//! guest kernel is configured so the bridges that profile forbids cannot exist
//! even if the configuration were wrong.

use std::fs;
use std::path::{Path, PathBuf};

use super::isolated_visual::{IsolatedVisualResourceLimits, IsolatedVisualSecurityProfile};

fn package_root() -> PathBuf {
    // `dunce::canonicalize` per the repository-wide ban on raw canonicalize.
    dunce::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/src-tauri/macos"),
    )
    .expect("packaged isolated visual source must be present in the repository")
}

fn guest(file: &str) -> PathBuf {
    package_root().join("isolated-visual-guest").join(file)
}

fn helper(file: &str) -> PathBuf {
    package_root().join("isolated-visual-helper").join(file)
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Every `CONFIG_` line in the guest kernel fragment, as (name, value).
fn kernel_config() -> Vec<(String, String)> {
    read(guest("kernel.config.fragment"))
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn kernel_value(name: &str) -> Option<String> {
    kernel_config()
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

#[test]
fn shipped_configuration_matches_the_locked_down_contract() {
    let shipped: serde_json::Value =
        serde_json::from_str(&read(helper("grokptah-isolated-config-v1.json"))).unwrap();

    assert_eq!(shipped["schemaVersion"], 1);
    assert_eq!(shipped["guestProtocolVersion"], 1);

    // The shipped profile must deserialize into exactly the closed profile the
    // Rust contract enforces, so the two cannot drift apart.
    let profile: IsolatedVisualSecurityProfile =
        serde_json::from_value(shipped["securityProfile"].clone()).unwrap();
    assert_eq!(profile, IsolatedVisualSecurityProfile::locked_down());
    profile.validate().unwrap();

    let limits: IsolatedVisualResourceLimits =
        serde_json::from_value(shipped["limits"].clone()).unwrap();
    assert_eq!(limits, IsolatedVisualResourceLimits::proof_defaults());
    limits.validate().unwrap();
}

#[test]
fn shipped_kernel_command_line_grants_no_console_or_network() {
    let shipped: serde_json::Value =
        serde_json::from_str(&read(helper("grokptah-isolated-config-v1.json"))).unwrap();
    let command_line = shipped["kernelCommandLine"].as_str().unwrap();
    for forbidden in [
        "ip=",
        "nfsroot",
        "root=/dev/",
        "console=hvc",
        "rdinit=/bin/sh",
        "init=/bin/sh",
        "single",
        "rw ",
    ] {
        assert!(
            !command_line.contains(forbidden),
            "guest command line grants {forbidden}: {command_line}"
        );
    }
    assert!(command_line.contains("init=/init"));
}

#[test]
fn guest_kernel_cannot_reach_network_storage_usb_or_audio() {
    // Anything that could become a host bridge is compiled out, not merely
    // left unconfigured, so a wrong configuration file cannot open one.
    for name in [
        "CONFIG_INET",
        "CONFIG_IPV6",
        "CONFIG_VIRTIO_NET",
        "CONFIG_NETFILTER",
        "CONFIG_WIRELESS",
        "CONFIG_WLAN",
        "CONFIG_BLUETOOTH",
        "CONFIG_USB_SUPPORT",
        "CONFIG_SOUND",
        "CONFIG_SCSI",
        "CONFIG_ATA",
        "CONFIG_VIRTIO_BLK",
        "CONFIG_VIRTIO_FS",
        "CONFIG_EXT4_FS",
        "CONFIG_FUSE_FS",
        "CONFIG_NFS_FS",
        "CONFIG_CIFS",
        "CONFIG_MODULES",
        "CONFIG_KEXEC",
        "CONFIG_HIBERNATION",
        "CONFIG_CRASH_DUMP",
    ] {
        assert_eq!(
            kernel_value(name).as_deref(),
            Some("n"),
            "{name} must be disabled in the guest kernel"
        );
    }

    // The guest keeps exactly the display and private-channel transport it
    // needs, and nothing else.
    for name in [
        "CONFIG_VSOCKETS",
        "CONFIG_VIRTIO_VSOCKETS",
        "CONFIG_DRM_VIRTIO_GPU",
        "CONFIG_BLK_DEV_INITRD",
        "CONFIG_DEVTMPFS",
        "CONFIG_TMPFS",
    ] {
        assert_eq!(
            kernel_value(name).as_deref(),
            Some("y"),
            "{name} must be enabled in the guest kernel"
        );
    }

    // The root filesystem is the built-in initramfs, so there is no host
    // filesystem to mount in the first place.
    assert_eq!(
        kernel_value("CONFIG_INITRAMFS_SOURCE").as_deref(),
        Some("\"grokptah-initramfs.cpio\"")
    );
}

#[test]
fn helper_holds_only_sandbox_and_virtualization_entitlements() {
    let entitlements = read(helper("isolated-visual-helper.entitlements.plist"));
    for required in [
        "com.apple.security.app-sandbox",
        "com.apple.security.virtualization",
    ] {
        assert!(
            entitlements.contains(required),
            "helper must declare {required}"
        );
    }
    // Anything that would reopen a bridge the profile closes must be absent.
    for forbidden in [
        "com.apple.security.network.client",
        "com.apple.security.network.server",
        "com.apple.security.device.camera",
        "com.apple.security.device.microphone",
        "com.apple.security.device.audio-input",
        "com.apple.security.device.usb",
        "com.apple.security.files.user-selected",
        "com.apple.security.files.downloads",
        "com.apple.security.temporary-exception",
        "com.apple.security.cs.disable-library-validation",
        "com.apple.security.cs.allow-unsigned-executable-memory",
        "com.apple.security.cs.debugger",
        "com.apple.vm.networking",
    ] {
        assert!(
            !entitlements.contains(forbidden),
            "helper must not declare {forbidden}"
        );
    }

    // Virtualization authority lives only in the helper, never in the app that
    // hosts the agent.
    let main = read(helper("grokptah-main.entitlements.plist"));
    assert!(
        !main.contains("com.apple.security.virtualization"),
        "the main application must not hold virtualization authority"
    );
    assert!(!main.contains("com.apple.vm.networking"));
}

#[test]
fn guest_source_is_pinned_to_an_exact_verifiable_release() {
    let lock: serde_json::Value =
        serde_json::from_str(&read(guest("guest-source.lock.json"))).unwrap();
    assert_eq!(lock["schemaVersion"], 1);
    assert_eq!(lock["architecture"], "arm64");

    let version = lock["kernelVersion"].as_str().unwrap();
    let url = lock["sourceUrl"].as_str().unwrap();
    assert!(
        url.starts_with("https://"),
        "guest source must be fetched over TLS"
    );
    assert!(
        url.contains(&format!("linux-{version}.tar.xz")),
        "pinned url {url} does not name kernel {version}"
    );

    let digest = lock["sourceSha256"].as_str().unwrap();
    assert_eq!(digest.len(), 64, "guest source digest must be SHA-256");
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "guest source digest must be lowercase hex"
    );
}

#[test]
fn reproducibility_verifiers_ship_and_are_executable() {
    for path in [
        guest("verify-guest-source.sh"),
        guest("build-guest-image.sh"),
        guest("fetch-kernel-source.sh"),
        helper("verify-helper-source.sh"),
        helper("package-signed-app.sh"),
        helper("build-helper.sh"),
    ] {
        let metadata = fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(metadata.len() > 0, "{} is empty", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                metadata.permissions().mode() & 0o111,
                0,
                "{} must be executable",
                path.display()
            );
        }
    }
}
