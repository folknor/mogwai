// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The source identity an executable was built from, parsed once at its entry
//! point and handed down to everything that renders or keys on it.
//!
//! The identity line is `<full HEAD sha> <clean|dirty> <commit time>`, which
//! the workspace's build runner computes with `scripts/source_identity.py` and
//! exports for every cargo invocation (`[cargo_env]`). Only an executable's
//! own `main.rs` reads that variable, with `option_env!` under
//! `cfg(not(test))`. That placement is the point of this type: a value read at
//! compile time by a library recompiles the library, every test target of its
//! package and everything that depends on it whenever the value changes, which
//! is every commit and the first edit after one. Read at the entry point, an
//! identity change rebuilds the executables alone.
//!
//! Two projections, kept separate because they always were and the cache key
//! depends on the second staying byte-identical:
//!
//! - [`SourceStamp::long_version`] wants a nine-character hash, a recognized
//!   clean or dirty flag and a commit time, and renders `unknown unknown`
//!   unless all three parse.
//! - [`SourceStamp::provenance_sha`] is the twelve-character `HEAD` prefix
//!   whenever the line starts with one followed by a space, and empty
//!   otherwise. It carries no dirty flag, so a dirty tree shares its `HEAD`'s
//!   provenance.

/// A parsed source identity, or the absence of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStamp {
    /// `<hash>[-dirty]` and the commit time, when the line parsed completely.
    version: Option<(String, String)>,
    /// The twelve-character sha prefix, or empty.
    provenance_sha: String,
}

impl SourceStamp {
    /// Parse the identity line, `None` when the variable was not set. The
    /// script's own `unknown` reads as unknown in both projections.
    #[must_use]
    pub fn parse(identity: Option<&str>) -> Self {
        let Some(identity) = identity else {
            return Self::unknown();
        };
        let provenance_sha = identity
            .split_once(' ')
            .and_then(|(sha, _)| sha.get(..12))
            .map_or_else(String::new, str::to_owned);
        let version = (|| {
            let (sha, rest) = identity.split_once(' ')?;
            let (flag, commit_time) = rest.split_once(' ')?;
            let short = sha.get(..9)?;
            let hash = match flag {
                "clean" => short.to_owned(),
                "dirty" => format!("{short}-dirty"),
                _ => return None,
            };
            Some((hash, commit_time.to_owned()))
        })();
        Self {
            version,
            provenance_sha,
        }
    }

    /// No identity at all: what a build outside the workspace's runner, and
    /// every test build, carries.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            version: None,
            provenance_sha: String::new(),
        }
    }

    /// The `--version` string and the readiness record's `version_string`:
    /// semver, hash and commit time, composed with the tape protocol version.
    /// One function renders both, so one run cannot announce a different
    /// version than the one it prints.
    #[must_use]
    pub fn long_version(&self) -> String {
        let (hash, commit_time) = self
            .version
            .as_ref()
            .map_or(("unknown", "unknown"), |(h, t)| (h.as_str(), t.as_str()));
        format!(
            "{} ({hash} {commit_time}) tape {}",
            env!("CARGO_PKG_VERSION"),
            mogwai_data::TAPE_PROTOCOL_VERSION
        )
    }

    /// The `HEAD` prefix a provenance token folds in, empty when unknown.
    #[must_use]
    pub fn provenance_sha(&self) -> &str {
        &self.provenance_sha
    }
}

#[cfg(test)]
mod tests {
    use super::SourceStamp;

    const SHA: &str = "00c1401ba596cedd838d660a73f500f8499cd543";

    #[test]
    fn a_clean_line_renders_the_short_hash_and_the_commit_time() {
        let stamp = SourceStamp::parse(Some(&format!("{SHA} clean 2026-11-26 06:45:03 UTC")));
        assert_eq!(
            stamp.long_version(),
            format!(
                "{} (00c1401ba 2026-11-26 06:45:03 UTC) tape {}",
                env!("CARGO_PKG_VERSION"),
                mogwai_data::TAPE_PROTOCOL_VERSION
            )
        );
        assert_eq!(stamp.provenance_sha(), "00c1401ba596");
    }

    #[test]
    fn a_dirty_line_marks_the_hash_and_leaves_the_provenance_sha_alone() {
        let stamp = SourceStamp::parse(Some(&format!("{SHA} dirty 2026-11-26 06:45:03 UTC")));
        assert!(stamp.long_version().contains("(00c1401ba-dirty 2026-11-26"));
        assert_eq!(stamp.provenance_sha(), "00c1401ba596");
    }

    #[test]
    fn an_absent_or_unknown_identity_is_unknown_in_both_projections() {
        for stamp in [
            SourceStamp::parse(None),
            SourceStamp::parse(Some("unknown")),
        ] {
            assert!(stamp.long_version().contains("(unknown unknown) tape "));
            assert_eq!(stamp.provenance_sha(), "");
        }
    }

    #[test]
    fn a_bad_flag_loses_the_version_but_keeps_the_provenance_sha() {
        // The two projections were two build-script parsers and accepted
        // different inputs; the cache key must not move with this type.
        let stamp = SourceStamp::parse(Some(&format!("{SHA} weird 2026-11-26 06:45:03 UTC")));
        assert!(stamp.long_version().contains("(unknown unknown)"));
        assert_eq!(stamp.provenance_sha(), "00c1401ba596");
    }
}
