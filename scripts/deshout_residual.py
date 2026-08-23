"""Close the residual comment shouts the crate agents missed.

Throwaway cleanup tooling for the 2026-08-23 no-shouting arc, same shape as
deshout_toml.py: exact per-file replacements, each asserted to occur exactly
once, so a drifted file fails loudly instead of half-applying. Delete after
the arc lands.
"""

import sys

EDITS = {
    "crates/mogwai-cli/src/arrival_control.rs": [
        ("Pins the COMMITTED artifact,", "Pins the committed artifact,"),
    ],
    "crates/mogwai-cli/src/arrival_screen.rs": [
        ("// NOT WRAPPED. `ScreenContext::open`", "// Not wrapped. `ScreenContext::open`"),
    ],
    "crates/mogwai-cli/src/cache.rs": [
        ("under the WORKSPACE target directory", "under the workspace target directory"),
    ],
    "crates/mogwai-cli/src/characterize.rs": [
        ("as a LIBRARY with no way", "as a library with no way"),
    ],
    "crates/mogwai-cli/src/gen.rs": [
        ("the UNIFIED block engine", "the unified block engine"),
        ("generated WITHOUT its calendar", "generated without its calendar"),
    ],
    "crates/mogwai-cli/src/man/render.rs": [
        ("so a LOOSE item's", "so a loose item's"),
        ("be the FIRST child of a list item", "be the first child of a list item"),
    ],
    "crates/mogwai-cli/src/measure.rs": [
        ("WRITES the artifact atomically", "Writes the artifact atomically"),
    ],
    "crates/mogwai-cli/src/ordered_counts.rs": [
        ("a RUNTIME artifact", "a runtime artifact"),
    ],
    "crates/mogwai-cli/src/segments.rs": [
        ("answer BEFORE running", "answer before running"),
        ("assumed. BOTH DIRECTIONS", "assumed. Both directions"),
        ("/// MATTER and they fail", "/// matter, and they fail"),
    ],
    "crates/mogwai-cli/src/slow_geometry.rs": [
        ("Amendment 1: COMPLETE-CASE TRAINING. A training",
         "Amendment 1: complete-case training. A training"),
    ],
    "crates/mogwai-cli/src/synth.rs": [
        ("The committed fingerprint, READ as", "The committed fingerprint, read as"),
        ("/// WRITES so a bare invocation", "/// writes, so a bare invocation"),
        ("/// WORKING DIRECTORY, so a synth run", "/// working directory, so a synth run"),
    ],
    "crates/mogwai-cli/tests/parity3b.rs": [
        ("and REPLAYING the protocol-11", "and replaying the protocol-11"),
        ("//! LIVE-FIELD EXCLUSIONS, enumerated", "//! Live-field exclusions, enumerated"),
        ("the PYTHON ran from", "the Python ran from"),
    ],
    "crates/mogwai-cli/tests/serving.rs": [
        ("without SHARED-1 and", "without `SHARED-1` and"),
        ("contain PRIVATE-1, and", "contain `PRIVATE-1`, and"),
    ],
    "crates/mogwai-data/src/generated/tests.rs": [
        ('say "COUNTERFACTUAL" while', 'say `"COUNTERFACTUAL"` while'),
        ('said "AS SHIPPED", exactly', 'said `"AS SHIPPED"`, exactly'),
    ],
    "crates/mogwai-data/tests/tape_version_prose.rs": [
        ('"= 15 (AMENDED ...)"', '`= 15 (AMENDED ...)`'),
    ],
    "crates/mogwai-lab/src/fit/curves.rs": [
        ("holiday-free SEARCH week", "holiday-free search week"),
    ],
    "crates/mogwai-lab/src/fit/mod.rs": [
        ("the shared SLACK convention", "the shared slack convention"),
    ],
    "crates/mogwai-lab/tests/parity3a.rs": [
        ('"parent/child verdict: PROCEED" and proceeds',
         '`parent/child verdict: PROCEED` and proceeds'),
    ],
    "crates/mogwai-protocol/src/sizing.rs": [
        ("Every optional field PRESENT and", "Every optional field present and"),
    ],
    "crates/mogwai-venue/src/admission.rs": [
        ("the cap BINDS here:", "the cap binds here:"),
    ],
    "crates/mogwai-venue/src/boatyard.rs": [
        ('because "SECOND" is configured', 'because `"SECOND"` is configured'),
    ],
    "crates/mogwai-venue/src/config.rs": [
        ("whose QUOTE currency", "whose quote currency"),
        ("fails STARTUP rather than", "fails at startup rather than"),
        ("job GLBX-20260805-HAPEWPABKG.", "job `GLBX-20260805-HAPEWPABKG`."),
    ],
    "crates/mogwai-venue/src/http.rs": [
        ("the resolver REFUSES - illegal", "the resolver refuses - illegal"),
        ("so the SELECTION is testable", "so the selection is testable"),
        ("// TERMINAL, so it is handled", "// terminal, so it is handled"),
        ("from local SUNDAY 00:00", "from local Sunday 00:00"),
        ("answers EVERY member", "answers every member"),
    ],
    "crates/mogwai-venue/src/run.rs": [
        ("anchored LATER than", "anchored later than"),
        ("connection RECEIVES rather", "connection receives rather"),
        ("are STORE-not-merge", "are store-not-merge"),
        ("capped and shed from the OLDEST", "capped and shed from the oldest"),
        ("is shed from the OLDEST end", "is shed from the oldest end"),
        ("onto EVERY ledger", "onto every ledger"),
        ("posted for OTHER names", "posted for other names"),
        ("/// TAKING RATHER THAN READING is what bounds",
         "/// Taking rather than reading is what bounds"),
        ("is the OPENING balance", "is the opening balance"),
        ("handed a CLEAN ledger", "handed a clean ledger"),
        ("/// The VENUE clock, and not", "/// The venue clock, and not"),
        ("so the CONTROL PLANE can", "so the control plane can"),
        ("venue-ORIGINATED output", "venue-originated output"),
        ("This was a BROADCAST target", "This was a broadcast target"),
        ("Delivery is now ATTRIBUTED through", "Delivery is now attributed through"),
        ("is the ACCOUNT ID the order", "is the account id the order"),
        ("belongs to. EVERY live order", "belongs to. Every live order"),
        ("/// BY ACCOUNT, NOT BY CONNECTION, and the difference",
         "/// By account, not by connection, and the difference"),
        ("the SAME TRADER and must", "the same trader and must"),
        ("order's TERMINAL state", "order's terminal state"),
        ("belongs to an ACCOUNT, and no", "belongs to an account, and no"),
        ("starts EMPTY of instruments", "starts empty of instruments"),
        ("nobody has opened WOULD open with", "nobody has opened would open with"),
        ("/// HISTORY is the operator's", "/// History is the operator's"),
        ("by ACCOUNT - two sockets", "by account - two sockets"),
        ("one engine pass PER ACCOUNT, so an", "one engine pass per account, so an"),
        ("about the VENUE - a completion", "about the venue - a completion"),
        ("The COMPLETENESS half", "The completeness half"),
    ],
    "crates/mogwai-venue/src/serve.rs": [
        ("must fail LOUDLY rather", "must fail loudly rather"),
        ("serve the UNARMED river", "serve the unarmed river"),
    ],
    "crates/mogwai-venue/src/sweeper.rs": [
        ("account is BOUND to", "account is bound to"),
        ("belongs to the CALENDAR, so", "belongs to the calendar, so"),
        ("// Marked FIRST, then funded", "// Marked first, then funded"),
    ],
    "crates/mogwai-venue/src/ws.rs": [
        ("Refused BEFORE the account is claimed", "Refused before the account is claimed"),
        ("bound it ACROSS connections", "bound it across connections"),
    ],
}


def main():
    total = 0
    for path, pairs in EDITS.items():
        s = open(path).read()
        for old, new in pairs:
            n = s.count(old)
            if n == 0 and new in s:
                continue  # already applied by an earlier partial run
            if n != 1:
                print(f"{path}: count {n}: {old[:60]!r}", file=sys.stderr)
                sys.exit(1)
            s = s.replace(old, new)
            total += 1
        open(path, "w").write(s)
    print("ok", total, "replacements in", len(EDITS), "files")


if __name__ == "__main__":
    main()
