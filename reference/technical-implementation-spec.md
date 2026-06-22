# Technical implementation specification

The single document from which an open TODO item is built to completion without
re-deriving its design. Two implementers working from it independently produce
the same artifact.

## What it is

1. **Every brick.** It lays each step on the road from the current code to the
   finished item. No step is left to discover during implementation.
2. **Obstacles resolved inline.** Anything blocking the road is solved in the
   document, as part of it. An unresolved obstacle is a missing brick.
3. **No deferral.** Nothing in the originating TODO is pushed to "later" -
   deferred work is a hole in the road. (Work that belongs to a genuinely
   separate TODO is named and excluded; that is not deferral.)
4. **No shoehorning.** We do not fit the work into existing abstractions,
   structures, or conventions because they already exist. The structure that
   best serves the end goal is the one we build; whatever stands in its way is
   ripped out and rebuilt. Pre-1.0, breaking any internal API is legal.

## What it must also pin (or it is aspiration, not a spec)

5. **Verification per brick.** Every change names its gate, matched to what
   the change can break:
   - **Execution and engine semantics** - order handling, fills, divergence
     injection, account state: the `mogwai-engine` unit tests plus the
     end-to-end smoke test that drives the live WS and control-plane path
     (`scripts/smoke.py`). The spec names which tests gate the change and,
     where it changes expected output, the re-bless expectation.
   - **Wire protocol** - the types in `mogwai-protocol`: serde round-trip
     tests proving both ends serialize identical bytes.
   - **Data-loader throughput or memory** - the streaming Kraken reader and
     k-way merge, which must stay O(1) in memory over multi-GB files: a
     measurement on the real dump, not an assertion. The accepted cost of a
     change that knowingly trades throughput for capability is stated as an
     explicit bound the keep/revert verdict is read against.
   - **Behavior neither the engine tests nor the smoke test reach**: named
     tests that pin it.

   A brick whose load is unproven is not laid. Per gate, the spec contains
   the EXACT command to run - copy-pasteable, flags and all, not "run the
   relevant tests". Concretely that is `brokkr check` (gremlins, clippy and
   all tests), `brokkr test -p mogwai-engine <NAME>` for a focused engine
   test, or launching `mogwai-server` and running `python3 scripts/smoke.py`
   for the live end-to-end path. If no command exists that can verify a gate
   (no test pins the behavior, no harness measures the path), building that
   instrument is itself a brick of the spec - specified to the same standard
   and laid before the brick it gates. A spec justified by an estimated
   volume or throughput win leads with the instrument that prices it: the
   measurement is the first landing, and the spec states an explicit
   proceed/close threshold the reading is judged against - below it, the item
   closes as mispriced and the rewrite is never laid. The estimate motivates
   the spec; only the measurement justifies the landing.
6. **A keep/revert path.** The implementation unit is one coherent, fully
   intrusive change that lands and is then kept or reverted on its gate
   results - never a tiny gated probe or an env-var experiment switch. The
   sequence of such landings is ordered so the test suite stays green at every
   boundary between them. Complete-but-unorderable is a failed spec.
7. **The target as concrete artifacts.** "The ideal structure" is pinned to
   exact types, signatures, ownership, and data flow - buildable, not merely
   directional.
8. **A survey of the ground.** The current structure and everything depending on
   it is inventoried before the teardown, so the rip is precise and drops no
   load-bearing work. A survey that prices a hot path traces the premise
   through the actual caller ordering at the priced call site - what the
   structure admits is not what the callers do. Specs
   authored as a batch reconcile their surveys against siblings covering
   the same ground before any is implemented; a sibling's survey may
   already state the fact that refutes this spec's premise.
9. **A stopping rule.** The rebuild has a bounded blast radius. Where the
   teardown stops, and what is out of scope, is stated explicitly.
10. **The standing references.** Every spec MUST cite, by path: this document
    (`reference/technical-implementation-spec.md`) as the contract it is
    written against, AND the document the spec was spawned from (the TODO
    source naming the item - e.g. the owning `docs/*.md` section or
    `docs/todo.md` entry), if it exists.

## Stance

- **Structural over micro.** The spec pursues the structural change that
  materially moves the goal - real throughput for performance work, real
  capability for feature work - not local tweaks. Full rewrites are labeled
  as such, distinct from local changes.
- **Cleanliness is a deliverable.** No env-var scaffolding, benchmark knobs, or
  temporary routing switches left as the way forward.
- **Unlimited resources, aggressive internal rewrites assumed.** Old
  abstractions earn no protection from age; shared writer abstractions and
  generic reuse are not goals. Correctness and maintainability of the *result*
  still hold.
