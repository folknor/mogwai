"""One-shot de-shouting of brokkr.toml's comments.

Throwaway cleanup tooling for the 2026-08-23 no-shouting arc: every
replacement is exact and asserted to occur exactly once, so a drifted
file fails loudly instead of half-applying. Delete after the arc lands.
"""

import sys

PATH = "brokkr.toml"

PAIRS = [
    ("That is the SHIPPED shape", "That is the shipped shape"),
    ("compiles\n# RELEASE.", "compiles\n# release."),
    ("# IT USED TO NAME TWO.", "# It used to name two."),
    ("is a statement about the HOST, not about this code",
     "is a statement about the host, not about this code"),
    ("it became a\n# MEASURABLE TARGET", "it became a\n# measurable target"),
    ("# NOTE ON `only`:", "# A note on `only`:"),
    ("excluded from the debug gate BECAUSE the",
     "excluded from the debug gate exactly because the"),
    ("# WHAT ENFORCES THE AGREEMENT, and what does not.",
     "# What enforces the agreement, and what does not."),
    ("which the gate does NOT skip is an ORPHANED PAIR",
     "which the gate does not skip is an orphaned pair"),
    ("matching no test at all is a DEAD FILTER.",
     "matching no test at all is a dead filter."),
    ("that the two lists were WRITTEN from one decision",
     "that the two lists were written from one decision"),
    ("# `brokkr test` builds DEV.", "# `brokkr test` builds dev."),
    ("belongs in that sweep too, NOT in a",
     "belongs in that sweep too, never in a"),
    ("both shipped RED and stayed red", "both shipped red and stayed red"),
    ("`#[ignore]`d for an\n# ENVIRONMENT reason",
     "`#[ignore]`d for an\n# environment reason"),
    ("they are skipped is NOT one sentence",
     "they are skipped is not one sentence"),
    ("is an\n#     ENVIRONMENT exclusion:", "is an\n#     environment exclusion:"),
    ("# A SKIP ENTRY STATES A COST AND NOTHING CHECKS IT.",
     "# A skip entry states a cost and nothing checks it."),
    ("# THE INVARIANT every pattern below must satisfy: it may match ONLY tests that",
     "# The invariant every pattern below must satisfy: it may only ever match tests that"),
    ("# IT IS ENFORCED BY THE TOOL ITSELF,", "# It is enforced by the tool itself,"),
    ("leaves an ORPHANED PAIR; a pattern matching\n# nothing at all is a DEAD FILTER,",
     "leaves an orphaned pair; a pattern matching\n# nothing at all is a dead filter,"),
    ("the wall was the SUM\n# of", "the wall was the sum\n# of"),
    ("most of them are WAITING, on a declared",
     "most of them are waiting, on a declared"),
    ("# THE DECLARED-DEADLINE FAMILY IS GONE FROM THIS LIST,",
     "# The declared-deadline family is gone from this list,"),
    ("a WRONG ANSWER rather than a timeout",
     "a wrong answer rather than a timeout"),
    ("establishes the PREMISE", "establishes the premise"),
    ("drains every socket and DISCARDS the whole",
     "drains every socket and discards the whole"),
    ("# A SECOND CAUSE SURFACED ONCE THE FAILURES WERE TRUTHFUL,",
     "# A second cause surfaced once the failures were truthful,"),
    ("# A RELEASE-profile wall-clock contract, excluded from this DEBUG lane",
     "# A release-profile wall-clock contract, excluded from this debug lane"),
    ("`dwell_is_bounded_across_run_seeds` USED TO SIT HERE",
     "`dwell_is_bounded_across_run_seeds` used to sit here"),
    ("excluded under the COST heading above",
     "excluded under the cost heading above"),
    ("the repository's ONLY multi-seed\n    # dwell gate",
     "the repository's only multi-seed\n    # dwell gate"),
    ("# TWO MORE ENTRIES LEFT ON 2026-08-19,",
     "# Two more entries left on 2026-08-19,"),
    ("repository's ONLY measurement behind",
     "repository's only measurement behind"),
    ("`#[ignore]`d at the source for a\n    # COST reason:",
     "`#[ignore]`d at the source for a\n    # cost reason:"),
    ("# THE FIDELITY GATE HAS NO ONE-COMMAND FORM,",
     "# The fidelity gate has no one-command form,"),
    ("the filter matches\n    # exactly ONE test,",
     "the filter matches\n    # exactly one test,"),
    ("excluded for an ENVIRONMENT reason rather than a cost",
     "excluded for an environment reason rather than a cost"),
    ("it needs a CLEAN tree to get past", "it needs a clean tree to get past"),
    ("written to defend the TESTS against it",
     "written to defend the tests against it"),
    ("# NO EXCLUSIONS, as of 2026-08-20,", "# No exclusions remain, as of 2026-08-20,"),
    ("two invariants about the lists ABOVE",
     "two invariants about the lists above"),
    ("by reading THIS FILE.", "by reading this very file."),
    ("(it read the\n# config as DATA and spawned nothing,",
     "(it read the\n# config as data and spawned nothing,"),
    ("catching a live test is an ORPHANED PAIR, and a filter\n# matching nothing is a DEAD FILTER",
     "catching a live test is an orphaned pair, and a filter\n# matching nothing is a dead filter"),
    ("# THE BENCHMARK REGISTRY: targets", "# The benchmark registry: targets"),
    ("# WHAT IS NOT HERE, deliberately: workloads.",
     "# What is not here, deliberately: workloads."),
    ("is the PRODUCT of surfaces", "is the product of surfaces"),
    ("composed at the CALL SITE\n# and captured",
     "composed at the call site\n# and captured"),
    ("# TWO KINDS OF SURFACE.", "# Two kinds of surface."),
    ("# ARGV-SHAPED, through the shipped bin",
     "# Argv-shaped, through the shipped bin"),
    ("# HARNESS-SHAPED, through an example target",
     "# Harness-shaped, through an example target"),
    ("# These are the MAJORITY of the eventual surface",
     "# These are the majority of the eventual surface"),
    ("# Deliberately a SEPARATE ms-scale target",
     "# Deliberately a separate ms-scale target"),
    ("# A GATE THAT BECAME A MEASUREMENT.", "# A gate that became a measurement."),
    ("# Carries NO registered features: its canonical output is a set of scraped",
     "# Carries no registered features: its canonical output is a set of scraped"),
    ("# Carries NO registered features - its canonical output is a wall and a unit",
     "# Carries no registered features - its canonical output is a wall and a unit"),
    ("Carries NO features\n# and the profiler MODES do not apply:",
     "Carries no features,\n# and the profiler modes do not apply:"),
    ("# NOT REGISTERED: `fill_walk_bench`,", "# Not registered: `fill_walk_bench`,"),
    ("# NOT REGISTERED: `symbol_decode_probe`,",
     "# Not registered: `symbol_decode_probe`,"),
    ("This records WHICH delivery a row was measured",
     "This records exactly which delivery a row was measured"),
    ("It does NOT\n# replace the run's own verification:",
     "It does not\n# replace the run's own verification:"),
    ("that check is about the CONTENTS being the ones",
     "that check is about the contents being the ones"),
    ("This digest is about DRIFT - the file", "This digest is about drift - the file"),
    ("# The digest is over the DIRECTORY - two",
     "# The digest is over the whole directory - two"),
]


def main():
    s = open(PATH).read()
    for old, new in PAIRS:
        n = s.count(old)
        if n != 1:
            print(f"count {n}: {old[:60]!r}", file=sys.stderr)
            sys.exit(1)
        s = s.replace(old, new)
    open(PATH, "w").write(s)
    print("ok", len(PAIRS), "replacements")


if __name__ == "__main__":
    main()
