//! Gate on the spellings retired by the glossary reconciliation arc.
//!
//! This is deliberately a spelling gate, not an English parser. Each entry in
//! `RETIRED_SPELLINGS` is unambiguous wherever it appears: it is either an old
//! identifier, an old operator spelling, or a phrase whose former meaning has
//! no legitimate live use. Sense-scoped words such as `session`, `ledger`,
//! `tape`, `warmup` and `reservation` are therefore not banned bare.
//!
//! The allowlist is part of the contract, and it is cut in two shapes because
//! the two are not interchangeable.
//!
//! `ALLOWED_SITES` is the shape used for a live production file: an exemption
//! is a pair of path and spelling, applied at match time. `config.rs` is exempt
//! for `server_heartbeat_ms` and for nothing else, `ws.rs` for the retired
//! websocket query key and for nothing else, `count_curve.rs` for the frozen
//! `first_divergence` artifact key and for nothing else. A whole-file exemption
//! on a mixed file is the vacuous-gate shape: it reads as gated while any
//! retired spelling introduced anywhere in that file passes silently.
//!
//! `ALLOWED_FILES` and `ALLOWED_PREFIXES` skip a whole file, and they are
//! correct only where nothing in the file is live repository vocabulary:
//! frozen preregistrations and specs, the historical arc records, generated or
//! vendored content, and this gate's own reviewed data table.
//!
//! Every `ALLOWED_SITES` entry must still match something, or the gate carries
//! an exemption nobody needs and the next reader cannot tell a live carve-out
//! from a dead one. Adding a spelling or an exemption is one reviewed data
//! edit; weakening the walk or teaching the matcher ad hoc exceptions is not.

use std::fs;
use std::path::{Path, PathBuf};

struct RetiredSpelling {
    spelling: &'static str,
    reason: &'static str,
    ascii_case_insensitive: bool,
}

/// Spellings whose presence outside the allowlist can only be vocabulary
/// drift. Phrase entries are narrow enough not to capture the surviving sense
/// of any individual word.
const RETIRED_SPELLINGS: &[RetiredSpelling] = &[
    RetiredSpelling {
        spelling: "mogwai-server",
        reason: "the crate is mogwai-venue",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "mogwai_server",
        reason: "the Rust crate path is mogwai_venue",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "ServerMessage",
        reason: "venue-originated frames use VenueMessage",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "ServerClock",
        reason: "the clock envelope is VenueClock",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "server_now_ns",
        reason: "the wire field is venue_now_ns",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "server_heartbeat_ms",
        reason: "the config key is venue_heartbeat_ms",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "ClientMessage",
        reason: "inbound frames carry Command",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "SocketSession",
        reason: "one connected trader is a Passenger",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "/ws?session=",
        reason: "the live websocket query key is callsign",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "LedgerTemplate",
        reason: "account engines open from AccountOpeningTerms",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "template_engine",
        reason: "the constructor is engine_from_account_opening_terms",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "ensure_on_tape",
        reason: "history is bounded by a river",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "off-tape",
        reason: "a history window outside the river is off-river",
        ascii_case_insensitive: true,
    },
    RetiredSpelling {
        spelling: "on-tape",
        reason: "a history window inside the river is on-river",
        ascii_case_insensitive: true,
    },
    RetiredSpelling {
        spelling: "segments tape",
        reason: "the typed subcommand is segments compose",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "first_divergence",
        reason: "the Rust identifier is first_mismatch",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "gen --warmup",
        reason: "the estimator flag is gen --burn-in",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "--ledger-key",
        reason: "the operator flag is --delivery-key",
        ascii_case_insensitive: false,
    },
    // A prefix of the entry above, so a `--ledger-key` site reports twice. Both
    // lines are true and the one edit closes both; that is cheaper than a
    // matcher rule which would have to know which flag a site meant.
    RetiredSpelling {
        spelling: "--ledger",
        reason: "the operator flag is --jobs-manifest",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "Run::admit",
        reason: "an attached connection is recorded by Run::attach",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "admit_history",
        reason: "history concurrency is acquired as a slot",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "order_reservation",
        reason: "funds tied to an order are an order hold",
        ascii_case_insensitive: false,
    },
    RetiredSpelling {
        spelling: "locked_balances",
        reason: "the internal funds collection is held_balances",
        ascii_case_insensitive: false,
    },
];

struct AllowedSite {
    path: &'static str,
    spelling: &'static str,
    reason: &'static str,
}

/// Scoped exemptions: one path, one spelling, applied at match time. Every
/// other retired spelling in these files is still refused, which is the whole
/// difference between this table and a skipped file.
const ALLOWED_SITES: &[AllowedSite] = &[
    AllowedSite {
        path: "crates/mogwai-venue/src/config.rs",
        spelling: "server_heartbeat_ms",
        reason: "the field doc and the refusal regression test must spell the key they refuse",
    },
    AllowedSite {
        path: "crates/mogwai-venue/src/ws.rs",
        spelling: "/ws?session=",
        reason: "the query doc must spell the retired identity key it records as refused",
    },
    AllowedSite {
        path: "crates/mogwai-cli/src/count_curve.rs",
        spelling: "first_divergence",
        reason: "the frozen serialized artifact key, whose spelling the preregistration fixed",
    },
];

struct AllowedFile {
    path: &'static str,
    reason: &'static str,
}

/// Whole files outside the live vocabulary. Reasons are intentionally repeated
/// per path so no exemption inherits an argument that does not fit it, and each
/// one must be a file where NO retired spelling could be drift.
const ALLOWED_FILES: &[AllowedFile] = &[
    AllowedFile {
        path: "crates/mogwai-data/tests/glossary_vocabulary_prose.rs",
        reason: "the gate's reviewed data table must spell every spelling it refuses",
    },
    AllowedFile {
        path: "notes/count-curve-preregistration.md",
        reason: "frozen preregistration; later vocabulary changes may not rewrite it",
    },
    AllowedFile {
        path: "notes/pair-test-preregistration.md",
        reason: "frozen preregistration; later vocabulary changes may not rewrite it",
    },
    AllowedFile {
        path: "notes/protocol-12b-arrival-composition-spec.md",
        reason: "frozen specification whose body is amended, never rewritten",
    },
    AllowedFile {
        path: "notes/glossary-ledger.md",
        reason: "historical inventory feeding the reconciliation arc",
    },
    AllowedFile {
        path: "notes/glossary-reconciliation.md",
        reason: "historical arc record which names every spelling it retired",
    },
    AllowedFile {
        path: "analysis/asia_jump_probe.py",
        reason: "owner's untracked work in progress, explicitly outside repository sweeps",
    },
];

struct AllowedPrefix {
    prefix: &'static str,
    reason: &'static str,
}

const ALLOWED_PREFIXES: &[AllowedPrefix] = &[
    AllowedPrefix {
        prefix: "notes/glossary-scope-",
        reason: "historical scope records from the reconciliation arc",
    },
    AllowedPrefix {
        prefix: "analysis/out/",
        reason: "generated analysis output, not repository content",
    },
    AllowedPrefix {
        prefix: "analysis/bars/",
        reason: "gitignored derived per-month bars, not repository content",
    },
    AllowedPrefix {
        prefix: "analysis/targets/",
        reason: "gitignored derived targets, not repository content",
    },
];

struct SkippedDir {
    name: &'static str,
    reason: &'static str,
}

/// Directories the walk never descends into, matched by name at any depth.
///
/// Every dot-directory is skipped in addition to these, and that is not
/// tidiness: `.git` holds git's own object store, and `.gitignore` puts the
/// tooling and agent scratch trees at the repository root outside the
/// repository while leaving them squarely inside a filesystem walk. Scratch
/// state is exactly where a retired spelling lives legitimately and where
/// failing the workspace gate on one would be absurd.
const SKIPPED_DIRS: &[SkippedDir] = &[
    SkippedDir {
        name: "target",
        reason: "generated build output, not repository content",
    },
    SkippedDir {
        name: "research",
        reason: "read-only vendored upstream source, not repository vocabulary",
    },
    SkippedDir {
        name: "__pycache__",
        reason: "generated python bytecode caches, not repository content",
    },
];

/// Text-bearing repository files covered by the vocabulary contract. Binary
/// assets and lockfiles cannot carry live prose or project-owned identifiers.
const SCANNED_EXTENSIONS: &[&str] = &["md", "rs", "toml", "py", "sh"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the repository root")
        .to_path_buf()
}

fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

/// Whole-file exemption, keyed on the repository-relative path.
fn allowed_file(path: &Path) -> bool {
    let shown = path.to_string_lossy();
    ALLOWED_FILES.iter().any(|entry| shown == entry.path)
        || ALLOWED_PREFIXES
            .iter()
            .any(|entry| shown.starts_with(entry.prefix))
}

/// Index of the scoped exemption covering this path and spelling, if any.
fn allowed_site(path: &str, spelling: &str) -> Option<usize> {
    ALLOWED_SITES
        .iter()
        .position(|entry| entry.path == path && entry.spelling == spelling)
}

fn skipped_dir(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRS.iter().any(|entry| entry.name == name)
}

fn scanned_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SCANNED_EXTENSIONS.contains(&extension))
}

/// Walk without following symlinks, and refuse unreadable repository content
/// rather than silently certifying a partial scan.
fn source_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry");
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("file type of {}: {error}", entry.path().display()));
        let path = entry.path();
        let rel = relative(root, &path);
        if file_type.is_dir() {
            let name = entry.file_name();
            if !skipped_dir(&name.to_string_lossy()) && !allowed_file(rel) {
                source_files(root, &path, out);
            }
        } else if file_type.is_file() && scanned_file(&path) && !allowed_file(rel) {
            out.push(path);
        }
    }
}

fn occurrences(line: &str, retired: &RetiredSpelling) -> usize {
    if retired.ascii_case_insensitive {
        let folded = line.to_ascii_lowercase();
        folded
            .match_indices(retired.spelling)
            .filter(|(at, _)| {
                folded[..*at]
                    .chars()
                    .next_back()
                    .is_none_or(|before| !before.is_ascii_alphanumeric())
            })
            .count()
    } else {
        line.matches(retired.spelling).count()
    }
}

#[test]
fn repository_uses_the_reconciled_glossary_spellings() {
    // Every table entry states its own grounds. An entry with no reason is an
    // unreviewed exemption, and reading the field here is also what keeps the
    // grounds from being quietly deleted as an unused field.
    for entry in ALLOWED_SITES {
        assert!(!entry.reason.is_empty(), "{} states no grounds", entry.path);
    }
    for entry in ALLOWED_FILES {
        assert!(!entry.reason.is_empty(), "{} states no grounds", entry.path);
    }
    for entry in ALLOWED_PREFIXES {
        assert!(
            !entry.reason.is_empty(),
            "{} states no grounds",
            entry.prefix
        );
    }
    for entry in SKIPPED_DIRS {
        assert!(!entry.reason.is_empty(), "{} states no grounds", entry.name);
    }

    let root = repo_root();
    let mut files = Vec::new();
    source_files(&root, &root, &mut files);
    files.sort();
    assert!(
        files.len() > 20,
        "the vocabulary walk found only {} source files under {}; it is not reading the repository it thinks it is",
        files.len(),
        root.display()
    );

    let mut drift = Vec::new();
    let mut exercised = vec![0usize; ALLOWED_SITES.len()];
    for path in files {
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let rel = relative(&root, &path).to_string_lossy().into_owned();
        for (line_index, line) in text.lines().enumerate() {
            for retired in RETIRED_SPELLINGS {
                let found = occurrences(line, retired);
                if found == 0 {
                    continue;
                }
                if let Some(site) = allowed_site(&rel, retired.spelling) {
                    exercised[site] += found;
                    continue;
                }
                for _ in 0..found {
                    drift.push(format!(
                        "{rel}:{}: retired spelling {:?} ({})",
                        line_index + 1,
                        retired.spelling,
                        retired.reason
                    ));
                }
            }
        }
    }

    assert!(
        drift.is_empty(),
        "glossary vocabulary drifted:\n{}",
        drift.join("\n")
    );

    // An exemption nobody needs is worse than none: the next reader cannot tell
    // a live carve-out from a dead one, and a path typo silently exempts
    // nothing while reading as though it exempts something.
    let unexercised: Vec<String> = ALLOWED_SITES
        .iter()
        .zip(&exercised)
        .filter(|(_, hits)| **hits == 0)
        .map(|(entry, _)| {
            format!(
                "{} is exempt for {:?} ({}) but carries no such spelling",
                entry.path, entry.spelling, entry.reason
            )
        })
        .collect();
    assert!(
        unexercised.is_empty(),
        "scoped exemptions that matched nothing - delete them or fix the path:\n{}",
        unexercised.join("\n")
    );
}
