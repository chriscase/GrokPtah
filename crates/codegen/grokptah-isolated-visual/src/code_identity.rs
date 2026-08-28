//! OS-verifiable code-signing identity for packaged Computer Use admission.
//!
//! # Why this module exists
//!
//! Signing class, Team ID, and the designated requirement of a macOS bundle
//! are properties the operating system computes by verifying signatures
//! against the code directory. A text file sitting inside the bundle stating
//! those properties is not evidence of them: anyone who can place the bundle
//! can place the file. Admission therefore reads them only from a
//! [`CodeIdentityProbe`], whose production implementation invokes the pinned
//! `codesign(1)` and `spctl(8)` binaries and records their authoritative
//! output verbatim.
//!
//! On any platform without those binaries the probe reports
//! [`CodeIdentityProbe::available`] = false and every inspection fails closed.
//! There is no fallback path that reads an attestation from the artifact.
//!
//! ## Parsing discipline
//!
//! `codesign -d --verbose=2` emits `Key=Value` lines. Classification reads only
//! values parsed from a recognized key at the *start of a line*, so prose that
//! merely contains the words "Developer ID Application" or "notarized" cannot
//! promote a bundle, and a negated sentence ("code object is not signed")
//! cannot be inverted into a positive verdict by appearing alongside one. A
//! value is additionally rejected outright if it carries a negation token, so a
//! hypothetical `Authority=not Developer ID Application` classifies as
//! unsigned rather than as Developer ID.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};

/// Pinned absolute paths. A `codesign` found on `PATH` is not authoritative.
pub const CODESIGN_BIN: &str = "/usr/bin/codesign";
pub const SPCTL_BIN: &str = "/usr/sbin/spctl";
const MAX_CAPTURED_BYTES: usize = 64 * 1024;

/// Words that, appearing anywhere in a parsed value, mean the value cannot be
/// read as a positive assertion about that key.
const NEGATION_TOKENS: &[&str] = &["not ", "no ", "never", "invalid", "failed", "rejected"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningClass {
    /// No authoritative probe ran. Never admits.
    #[default]
    Uninspected,
    Unsigned,
    AdHoc,
    AppleDevelopment,
    DeveloperId,
    NotarizedDeveloperId,
}

impl SigningClass {
    /// Only a notarized Developer ID signature is packaged-release identity.
    pub fn counts_as_packaged_release(self) -> bool {
        self == Self::NotarizedDeveloperId
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uninspected => "uninspected",
            Self::Unsigned => "unsigned",
            Self::AdHoc => "ad_hoc",
            Self::AppleDevelopment => "apple_development",
            Self::DeveloperId => "developer_id",
            Self::NotarizedDeveloperId => "notarized_developer_id",
        }
    }
}

/// Authoritative output captured from the OS, retained so a reviewer can see
/// exactly what was verified rather than a boolean someone computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedCodesignOutput {
    pub target: String,
    /// Verbatim `codesign -d --verbose=2` output (stdout+stderr).
    pub display: String,
    /// Verbatim `codesign -d -r-` output, which prints the designated
    /// requirement the OS derives from the signature.
    pub requirement: String,
    /// Verbatim `spctl --assess --type execute -vv` output.
    pub gatekeeper: String,
    /// Whether `codesign --verify --deep --strict` exited zero.
    pub verify_ok: bool,
    /// Whether `spctl --assess` exited zero.
    pub gatekeeper_ok: bool,
}

/// Structured, OS-derived code identity for one bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedCodeIdentity {
    pub identifier: Option<String>,
    pub team_id: Option<String>,
    /// The designated requirement as the OS printed it. `None` when the OS
    /// could not derive one; admission then fails closed.
    pub designated_requirement: Option<String>,
    pub signing_class: SigningClass,
    pub stapled: bool,
    pub gatekeeper_accepted: bool,
    pub captured: CapturedCodesignOutput,
}

/// Source of OS-verifiable code identity.
pub trait CodeIdentityProbe: std::fmt::Debug + Send + Sync {
    /// A short label recorded in evidence so a reader can tell which probe ran.
    fn probe_id(&self) -> &'static str;

    /// False on any host where the authoritative tools are absent. Callers
    /// must treat false as "cannot admit", never as "admit unchecked".
    fn available(&self) -> bool;

    fn inspect(&self, bundle: &Path) -> IsolatedResult<ObservedCodeIdentity>;
}

/// Production probe: pinned `codesign` / `spctl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemCodeIdentityProbe;

impl SystemCodeIdentityProbe {
    fn tools_present() -> bool {
        Path::new(CODESIGN_BIN).is_file() && Path::new(SPCTL_BIN).is_file()
    }

    fn capture(args: &[&str], bin: &str) -> (String, bool) {
        match Command::new(bin).args(args).output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push('\n');
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                if text.len() > MAX_CAPTURED_BYTES {
                    text.truncate(MAX_CAPTURED_BYTES);
                }
                (text, output.status.success())
            }
            Err(error) => (format!("<{bin} failed: {error}>"), false),
        }
    }
}

impl CodeIdentityProbe for SystemCodeIdentityProbe {
    fn probe_id(&self) -> &'static str {
        "system_codesign_spctl_v1"
    }

    fn available(&self) -> bool {
        cfg!(target_os = "macos") && Self::tools_present()
    }

    fn inspect(&self, bundle: &Path) -> IsolatedResult<ObservedCodeIdentity> {
        if !self.available() {
            return Err(IsolatedError::unsupported(
                "OS code-signing verification is unavailable on this host; \
                 packaged identity cannot be established",
            ));
        }
        let target = bundle.to_string_lossy().into_owned();
        let (display, _) = Self::capture(&["-d", "--verbose=2", &target], CODESIGN_BIN);
        let (requirement, requirement_ok) = Self::capture(&["-d", "-r-", &target], CODESIGN_BIN);
        let (_, verify_ok) =
            Self::capture(&["--verify", "--deep", "--strict", &target], CODESIGN_BIN);
        let (gatekeeper, gatekeeper_ok) = Self::capture(
            &["--assess", "--type", "execute", "-vv", &target],
            SPCTL_BIN,
        );
        let captured = CapturedCodesignOutput {
            target,
            display,
            requirement: requirement.clone(),
            gatekeeper: gatekeeper.clone(),
            verify_ok,
            gatekeeper_ok,
        };
        Ok(parse_observed_identity(
            captured,
            requirement_ok,
            verify_ok,
            gatekeeper_ok,
        ))
    }
}

/// Build structured identity from captured authoritative output.
///
/// Kept separate from process invocation so the parser can be exercised
/// adversarially against real and hostile transcripts without running tools.
pub fn parse_observed_identity(
    captured: CapturedCodesignOutput,
    requirement_ok: bool,
    verify_ok: bool,
    gatekeeper_ok: bool,
) -> ObservedCodeIdentity {
    let identifier = keyed_value(&captured.display, "Identifier");
    let team_id = keyed_value(&captured.display, "TeamIdentifier");
    let authorities = keyed_values(&captured.display, "Authority");
    let designated_requirement = if requirement_ok {
        parse_requirement(&captured.requirement)
    } else {
        None
    };
    let signing_class = classify(&captured, &authorities, verify_ok, gatekeeper_ok);
    // Stapling is only meaningful when Gatekeeper actually accepted; the
    // presence of the word "stapled" in prose proves nothing on its own.
    let stapled = gatekeeper_ok && keyed_value(&captured.gatekeeper, "source").is_some();
    ObservedCodeIdentity {
        identifier,
        team_id,
        designated_requirement,
        signing_class,
        stapled,
        gatekeeper_accepted: gatekeeper_ok,
        captured,
    }
}

fn classify(
    captured: &CapturedCodesignOutput,
    authorities: &[String],
    verify_ok: bool,
    gatekeeper_ok: bool,
) -> SigningClass {
    // An unverifiable signature is never promoted, whatever the text says.
    if !verify_ok {
        if authorities.is_empty() && mentions_unsigned(&captured.display) {
            return SigningClass::Unsigned;
        }
        return SigningClass::Uninspected;
    }
    if authorities
        .iter()
        .any(|value| positive(value) && value.starts_with("Developer ID Application"))
    {
        // Notarization is a Gatekeeper property, read from `spctl`, not from
        // the word "notarized" appearing in codesign output.
        let source = keyed_value(&captured.gatekeeper, "source").unwrap_or_default();
        if gatekeeper_ok
            && positive(&source)
            && source.eq_ignore_ascii_case("Notarized Developer ID")
        {
            return SigningClass::NotarizedDeveloperId;
        }
        return SigningClass::DeveloperId;
    }
    if authorities
        .iter()
        .any(|value| positive(value) && value.starts_with("Apple Development"))
    {
        return SigningClass::AppleDevelopment;
    }
    if is_adhoc(&captured.display) {
        return SigningClass::AdHoc;
    }
    if mentions_unsigned(&captured.display) {
        return SigningClass::Unsigned;
    }
    SigningClass::Uninspected
}

/// `Signature=adhoc`, or `flags=…(adhoc)` from the CodeDirectory flags line.
fn is_adhoc(display: &str) -> bool {
    if keyed_value(display, "Signature").is_some_and(|value| value.eq_ignore_ascii_case("adhoc")) {
        return true;
    }
    keyed_value(display, "CodeDirectory")
        .or_else(|| keyed_value(display, "flags"))
        .is_some_and(|value| value.contains("(adhoc)"))
}

/// The "not signed" diagnostics `codesign` emits are whole-line messages, not
/// key/value pairs, so they are matched as such rather than by substring.
fn mentions_unsigned(display: &str) -> bool {
    display.lines().any(|line| {
        let line = line.trim();
        line.ends_with("code object is not signed at all")
            || line.ends_with("is not signed at all")
            || line.ends_with("code has no signature")
    })
}

/// A value carrying a negation token cannot be read as a positive assertion.
fn positive(value: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let lower = format!(" {} ", value.to_ascii_lowercase());
    !NEGATION_TOKENS.iter().any(|token| lower.contains(token))
}

/// First value for `key`, matched only as `key=` anchored at line start.
fn keyed_value(text: &str, key: &str) -> Option<String> {
    keyed_values(text, key).into_iter().next()
}

fn keyed_values(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}=");
    text.lines()
        .filter_map(|line| {
            // Anchored: leading whitespace is tolerated, arbitrary prose is not.
            let line = line.trim_start();
            line.strip_prefix(prefix.as_str())
                .map(|value| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
        .collect()
}

/// `codesign -d -r-` prints `designated => <requirement>`.
fn parse_requirement(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("designated =>")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty() && value.is_ascii())
    })
}

#[cfg(test)]
pub(crate) fn captured_fixture(
    display: &str,
    requirement: &str,
    gatekeeper: &str,
) -> CapturedCodesignOutput {
    CapturedCodesignOutput {
        target: "/fixture/Helper.app".into(),
        display: display.into(),
        requirement: requirement.into(),
        gatekeeper: gatekeeper.into(),
        verify_ok: true,
        gatekeeper_ok: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELPER_ID: &str = "com.chriscase.grokptah.computer-use-helper";

    fn notarized_display(team: &str) -> String {
        format!(
            "Executable=/fixture/Helper.app/Contents/MacOS/helper\n\
             Identifier={HELPER_ID}\n\
             CodeDirectory v=20500 size=1234 flags=0x10000(runtime)\n\
             Signature size=9000\n\
             TeamIdentifier={team}\n\
             Authority=Developer ID Application: Example Corp ({team})\n\
             Authority=Developer ID Certification Authority\n\
             Authority=Apple Root CA\n"
        )
    }

    fn requirement_output(team: &str) -> String {
        format!(
            "designated => identifier \"{HELPER_ID}\" and anchor apple generic and \
             certificate leaf[subject.OU] = {team}\n"
        )
    }

    fn gatekeeper_output() -> String {
        "/fixture/Helper.app: accepted\nsource=Notarized Developer ID\norigin=Developer ID Application: Example Corp\n".to_string()
    }

    #[test]
    fn notarized_developer_id_is_recognized_from_os_output() {
        let identity = parse_observed_identity(
            captured_fixture(
                &notarized_display("TEAMID1234"),
                &requirement_output("TEAMID1234"),
                &gatekeeper_output(),
            ),
            true,
            true,
            true,
        );
        assert_eq!(identity.signing_class, SigningClass::NotarizedDeveloperId);
        assert_eq!(identity.team_id.as_deref(), Some("TEAMID1234"));
        assert_eq!(identity.identifier.as_deref(), Some(HELPER_ID));
        assert!(identity
            .designated_requirement
            .as_deref()
            .unwrap()
            .contains(HELPER_ID));
        assert!(identity.stapled);
    }

    #[test]
    fn gatekeeper_rejection_demotes_to_developer_id() {
        let identity = parse_observed_identity(
            captured_fixture(
                &notarized_display("TEAMID1234"),
                &requirement_output("TEAMID1234"),
                "/fixture/Helper.app: rejected\n",
            ),
            true,
            true,
            false,
        );
        assert_eq!(identity.signing_class, SigningClass::DeveloperId);
        assert!(!identity.gatekeeper_accepted);
        assert!(!identity.stapled);
    }

    #[test]
    fn failed_verification_is_never_promoted() {
        let identity = parse_observed_identity(
            captured_fixture(
                &notarized_display("TEAMID1234"),
                &requirement_output("TEAMID1234"),
                &gatekeeper_output(),
            ),
            true,
            false,
            true,
        );
        assert_eq!(identity.signing_class, SigningClass::Uninspected);
        assert!(!identity.signing_class.counts_as_packaged_release());
    }

    #[test]
    fn prose_containing_authority_text_does_not_classify() {
        // The tokens appear, but never as a value of an anchored key line.
        let hostile = "note: this bundle is not signed by Authority=Developer ID Application\n\
                       comment: source=Notarized Developer ID would be nice\n\
                       Identifier=com.evil.helper\n";
        let identity = parse_observed_identity(
            captured_fixture(hostile, "", "some prose source=Notarized Developer ID"),
            false,
            true,
            true,
        );
        assert_eq!(identity.signing_class, SigningClass::Uninspected);
        assert!(identity.designated_requirement.is_none());
    }

    #[test]
    fn negated_values_cannot_invert_into_a_positive_class() {
        let negated = format!(
            "Identifier={HELPER_ID}\n\
             TeamIdentifier=TEAMID1234\n\
             Authority=not Developer ID Application: Example Corp\n"
        );
        let identity = parse_observed_identity(
            captured_fixture(
                &negated,
                &requirement_output("TEAMID1234"),
                &gatekeeper_output(),
            ),
            true,
            true,
            true,
        );
        assert_ne!(identity.signing_class, SigningClass::NotarizedDeveloperId);
        assert_ne!(identity.signing_class, SigningClass::DeveloperId);
    }

    #[test]
    fn unsigned_diagnostics_are_matched_as_whole_lines() {
        let unsigned = "/fixture/Helper.app: code object is not signed at all\n";
        let identity =
            parse_observed_identity(captured_fixture(unsigned, "", ""), false, false, false);
        assert_eq!(identity.signing_class, SigningClass::Unsigned);
    }

    #[test]
    fn adhoc_is_read_from_the_signature_key_not_prose() {
        let adhoc = format!(
            "Identifier={HELPER_ID}\n\
             CodeDirectory v=20400 size=100 flags=0x2(adhoc)\n\
             Signature=adhoc\n"
        );
        let identity =
            parse_observed_identity(captured_fixture(&adhoc, "", ""), false, true, false);
        assert_eq!(identity.signing_class, SigningClass::AdHoc);
        assert!(!identity.signing_class.counts_as_packaged_release());
    }

    #[test]
    fn probe_is_unavailable_off_macos_and_refuses_to_guess() {
        let probe = SystemCodeIdentityProbe;
        if !probe.available() {
            let error = probe.inspect(Path::new("/fixture/Helper.app")).unwrap_err();
            assert_eq!(
                error.code,
                crate::error::IsolatedErrorCode::UnsupportedPlatform
            );
        }
    }
}
