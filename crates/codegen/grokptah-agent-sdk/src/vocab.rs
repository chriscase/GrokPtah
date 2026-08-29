//! Wire vocabularies that survive a host that knows more words than this build.
//!
//! # The problem this solves
//!
//! Every enum on this seam is a *vocabulary*: a set of tokens the host may
//! send. The contract promises that a minor version bump is additive — a host
//! may add a run state, a stop cause, a tool kind, an operation class — and an
//! older consumer keeps working. A plain `#[derive(Deserialize)]` enum cannot
//! keep that promise. It rejects the token it does not know, and because these
//! vocabularies sit *inside* larger structures, one unknown token fails the
//! whole [`RunView`], or the whole event page, not just the field that carries
//! it. A single added token would then break every deployed consumer at once,
//! which is the definition of a breaking change.
//!
//! The `open_vocabulary!` macro closes that gap. Each vocabulary gains an
//! `Unknown(Label)` arm that decoding falls back to, and that re-serializes as
//! the host's original token, so a consumer that reads and forwards a record
//! does not quietly rewrite it.
//!
//! # Decode open, act closed
//!
//! Tolerating the token is not the same as trusting it. Decoding never fails;
//! *interpretation* always fails closed. Every predicate written on top of
//! these vocabularies takes the conservative branch for `Unknown` — an
//! unrecognized receipt status is uncertain, an unrecognized lifecycle is not
//! terminal, an unrecognized digest algorithm cannot verify anything. A
//! consumer that needs to distinguish "this build does not know this word" from
//! "this build understood it" asks the generated `is_known`.
//!
//! [`RunView`]: crate::dto::RunView

use crate::ids::Label;

/// The token a host sent for a word this build does not have.
///
/// Sanitized through [`Label`], so a hostile or buggy host cannot use an
/// unrecognized vocabulary token to push control characters, terminal escapes,
/// or an unbounded string into a consumer's log or UI. A token that is empty
/// once sanitized becomes the fixed sentinel rather than an error: decoding a
/// vocabulary must not be the thing that fails a page.
pub(crate) fn unknown_label(raw: &str) -> Label {
    Label::new(raw).unwrap_or_else(|_| {
        Label::new("unrecognized").expect("the sentinel is a valid non-empty label")
    })
}

/// Define a wire vocabulary that decodes an unknown token instead of failing.
///
/// Generates the enum with a trailing `Unknown(Label)`, the `as_wire` /
/// `from_wire` pair, `is_known`, `Display`, and `Serialize`/`Deserialize` that
/// round-trip the host's token verbatim.
///
/// These types are deliberately **not** `Copy`: the unknown arm owns its token.
macro_rules! open_vocabulary {
    (
        $(#[$meta:meta])*
        $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $(
                $(#[$vmeta])*
                $variant,
            )+
            /// A token this build does not know, carried verbatim.
            ///
            /// Present so a newer host can add a word without breaking this
            /// consumer. Every predicate on this type treats it as the
            /// conservative case; see the [module docs](crate::vocab).
            Unknown($crate::ids::Label),
        }

        impl $name {
            /// Canonical wire token.
            pub fn as_wire(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(raw) => raw.as_str(),
                }
            }

            /// Decode a wire token, falling back to [`Self::Unknown`].
            pub fn from_wire(raw: &str) -> Self {
                match raw {
                    $($wire => Self::$variant,)+
                    other => Self::Unknown($crate::vocab::unknown_label(other)),
                }
            }

            /// `false` when the host used a word this build does not have.
            ///
            /// A consumer that must not silently degrade — a migration, an
            /// audit export, a compatibility gate — checks this and escalates
            /// rather than acting on a value it cannot interpret.
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Unknown(_))
            }

            /// Every token this build knows, in declaration order.
            ///
            /// The compatibility matrix uses this to diff one build's
            /// vocabulary against another's without reflection.
            pub fn known_tokens() -> &'static [&'static str] {
                &[$($wire,)+]
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_wire())
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_wire())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <String as serde::Deserialize>::deserialize(d)?;
                Ok(Self::from_wire(&raw))
            }
        }
    };
}

pub(crate) use open_vocabulary;
