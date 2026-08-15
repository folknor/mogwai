# Bug hunt: mogwai-protocol

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

THE DOCUMENT IS EXHAUSTED: no open findings remain. What each one became is
recorded below, and the machinery is carried forward in
`notes/bug-loop-carry-forward.md`. The disposition lines exist so a later reader
can tell a CLOSED finding from a DELETED one.

## Disposition

- 1, `AdmissionSubject` echoes an unbounded client id. CLOSED by the
  `bugs-server-transport.md` round-1 landing: `AdmissionSubject` has a
  hand-written `Serialize` that truncates every id to `MAX_CLIENT_ID_LEN` on a
  char boundary, and the websocket carrier caps inbound frames and reassembled
  messages at `MAX_CLIENT_MESSAGE_BYTES`. Disclosed residual, unchanged: the
  variants stay constructible from a raw `String`, so the invariant holds at
  serialization rather than at construction.
- 2, `LaunchSpec.duration = Some(Duration::ZERO)` silently means "run forever".
  CLOSED by the `bugs-server-transport.md` round-2 landing. Server-side
  duration resolution is three-state and an explicit zero overrides a finite
  config to indefinite; `LaunchSpec::duration` DOCUMENTS that meaning at the
  protocol layer. Refusing zero was considered and rejected: `--duration 0s` is
  a documented spelling of "run until the launcher ends it".
- 3, `validate_divergence` is not the authoritative guard it claims to be.
  CLOSED. Both single-shot client-order-id targets now pass
  `validate_client_order_id`, and `RejectNextSubmit.reason` is refused above
  `MAX_REASON_LEN` in the protocol crate. The HTTP handler's post-validation
  truncation is deleted, so there is no second, quieter spelling of the rule.
  The engine additionally truncates the reason at the ECHO, because
  `Engine::arm` is a public in-process API that reaches no validator.
- 4, `read_ready` has no line-length bound. CLOSED. The readiness read is
  capped at one byte past a 4,096-byte ceiling by taking the CHILD stream, and
  the refusal retains only a valid UTF-8 prefix.
- 5, stale derivations in `sizing.rs`. CLOSED. `ORDER_EVENT_MAX_BYTES` is now
  the maximum of two derivations - a maximal `OrderFilled` and a maximal
  reason-bearing rejection - each matching its struct field for field, and both
  are pinned by a constructed worst-case domination test rather than argued.
  `Position` lists all six fields; the admission derivation no longer charges a
  symbol `AdmissionRejected` does not have.
- 6, `AccountId`'s invariant is bypassable. CLOSED BY CONSTRUCTION. The field is
  private, `parse` is the only constructor, there is no `From` impl, and the
  derived `Deserialize` is replaced by one that routes through `parse`.
- 7, every wire frame pays serde's internally-tagged buffering. CLOSED AS AN
  ALLOCATION FIX, NOT A THROUGHPUT ONE. The measured numbers and the plain
  statement that the landed decoder is marginally SLOWER are in
  `reference/performance.md`. Its two adjacent smells: the per-tick `Symbol`
  allocation was closed separately by `bugs-data.md` round 2 (`Arc<str>`), and
  outbound batching remains unaddressed and unmeasured - not carried as an open
  finding because no measurement names a decision it would change.
- 8, seed derivation lives outside the crate its versioning rule names. CLOSED
  as documentation. The derivation stays in `mogwai-protocol` to preserve the
  workspace dependency direction and now carries the pointer to the downstream
  `TAPE_PROTOCOL_VERSION` obligation.
- 9, reproducibility gap in the shipped launcher. DEAD, verified against the
  source rather than the claim. `docs/cli.md` states the decided contract:
  there is deliberately no `--seed` flag because a reproduced path is a written
  act, so the seed is overridden through the config file's `seed` key alone.
  Commit `a6f57760` is what decided it. The finding proposed adding a second
  spelling of something already ruled on.

## Checked and found sound

`clock.rs` (the f64-precision caveat is documented and the anchors correctly stay
`u64`; `validate_sim_clock`'s note about non-finite `f64` failing to round-trip
through JSON is correct); `decimal.rs`; `RunSeeds` domain separation (splitmix64 is
a bijection, so `tape != fill` is structural, not luck); `trades_through` /
`touches_trigger` strictness and the argument for keeping them separate functions;
`ServerMessage::category` covering all variants exhaustively with `is_market_data`
correctly narrower; `ConnHavoc`'s container-level `#[serde(default)]` and the
two-sided reconnect-spin gate; `worst_case_output_bytes` matching all five
`ClientMessage` variants; the `own_venue` thread architecture (the dedicated-thread
`PR_SET_PDEATHSIG` reasoning, the readiness deadline living with the `Child` owner,
and the decision not to join the drain thread are all correctly argued and
correctly implemented).
