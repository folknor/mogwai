# Bug hunt: mogwai-adapter

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-adapter`: the factories and their configs, the DataClient and
ExecutionClient pair, the socket upgrade and reconnection path, order and
order-group submission, history pagination, reconciliation, and the four
socket-backed test binaries.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read the full crate plus the nautilus emitter source in `research/`,
and skimmed the four socket test binaries. Nothing was built or run.

Round 1 closed findings 1, 2 and 3: all three were real defects, all three are
fixed, each with a bite-checked regression test. They are removed rather than
annotated, per the arc's convention. Numbering is unchanged. Two things the
round learned that the removed text would otherwise have carried:

- The hunter's claim that finding 1's ordering was "untested by construction"
  because every socket binary starts before connecting was HALF right. The
  ordering is untested, but `havoc.rs`'s `ships_server_havoc` installed no
  execution sink at all, so its client's `start()` found nothing on the
  thread-local and it connected DEAF - the new guard failed it on the first
  run. The test now installs a sink. That is the defect the finding describes,
  standing live in the crate's own suite.
- The fix is the REFUSAL, not the shared emitter. `try_get_exec_event_sender`
  reads a `thread_local!` set on the runner's thread, so resolving the sender
  lazily from `exec_context()` - the hunter's preferred shape - is not reliably
  possible from a spawned task. `connect()` retries the lookup (free when it
  really is on that thread) and then refuses. The nautilus-side fix would be an
  emitter that shares its sender; `research/` is read-only, so this is the
  local fail-loud mitigation. Filed in `notes/todo.md`; the HOST-FACING
  contract it creates is durable and now lives in `docs/adapter-lifecycle.md`,
  because a refusal a shipped consumer can hit is documentation, not a note.
- THE COLD REVIEW'S HEADLINE FINDING WAS EMPIRICALLY REFUTED, AND THE MECHANISM
  IS WORTH KEEPING. The reviewer argued that
  `connect_refuses_a_client_with_no_execution_event_sink` cannot pass under
  `brokkr test`, on the model that libtest at `--test-threads=1` runs tests
  INLINE on the main thread, which would share one `EXEC_EVENT_SENDER` across a
  whole binary and let an alphabetically-earlier test install a sender this one
  would then find. THE MODEL IS WRONG. libtest spawns a fresh named thread per
  test unconditionally on any threaded target - the thread name is how a panic
  is attributed to a test - and `--test-threads` caps how many run AT ONCE, not
  whether a thread is created. Measured, not argued: an `eprintln!` probe under
  `brokkr test -p mogwai-adapter "" --debug` reported distinct `ThreadId`s named
  for each test and an EMPTY sender slot on entry to both new tests. So the
  test's doc comment is literally true, and `tests/common/mod.rs`'s isolation
  claim - which every negative sink window in these binaries rests on - is
  sound. The claim now says so with the mechanism spelled out, and it is PINNED
  rather than asserted: `common::owns_a_fresh_exec_sink_on_every_lane` runs in
  all three binaries and in all lanes, and `assert_owns_a_fresh_exec_sink` lets
  an individual test restate the premise it depends on so a libtest change
  fails on the premise rather than on whatever the test was really asserting.
  Both new tests call it. The close pass bite-checked the refusal in the
  `brokkr test` lane specifically: with the `ensure!` removed as a text edit,
  the test goes red on the ERROR-TEXT assertion, `connect()` having failed with
  the transport error `fetch instruments` instead of the named refusal - which
  is the one assertion that distinguishes "refused for the right reason" from
  "failed for any reason", and no other site emits that string.

Round 2 closed findings 4, 5, 6 and 7 and verified the relayed broadarrow
section. All were real; none was refused. What the round learned that the
removed text would otherwise have carried:

- FINDING 6 IS SETTLED BY READING THE SERVER, not left half-confident. The
  venue closes WS 1000 for THREE things - `admission::CLOSE_EVICTED` is
  literally `1000`, and `ws.rs` sends 1000 on run completion and on a
  passenger's duration elapsing - so the adapter's "any 1000 is completion"
  was wrong on two of the three and on every proxy close besides. The fix is
  `mogwai_protocol::close`, a shared module the venue writes its reason
  strings from and `classify` reads: recognized reasons are terminal (and say
  WHICH in the log), everything else redials. The eviction reason is now
  prefixed so it is matchable rather than sniffed.
- THE FIRST SHAPE OF THAT REGRESSION TEST WAS VACUOUS AGAINST ITS OWN DEFECT,
  caught by its bite-check. The stub closed with `ws.close(None)`, which sends
  `Message::Close(None)` - and the OLD code required `Close(Some(frame))` too,
  so the test passed against the defect. A close-frame test must send a
  REASONED close or it is testing the `None` arm nobody changed.
- FINDING 5'S FIX IS A RECEIPT BOOK, and its retire is the frontier rule
  applied to a write: a receipt is filed before the frame is queued and retired
  only by the expression that saw `writer.send` return `Ok`. What survives the
  writer's abort is exactly what the venue never saw. The loop's own RETURN is
  an undelivery too, and covers the residue in the command receiver that no
  receipt can see. Commands sent after the return need nothing: the receiver is
  dropped, so `dispatch_order` already synthesizes.
- AND THE FIX WAS DELIBERATELY NOT A REPLAY. The report preferred reporting
  over re-queueing and the round agrees: replaying orders across a reconnect is
  the host's policy call. The host-facing half is `docs/adapter-lifecycle.md`.
- FINDING 4 DELETED RATHER THAN PRESERVED. `SubState.start_ts` is gone with
  `advance_sub_start_ts`, `start_ts_param` and the plumbing, and the two false
  prose claims with them; the site now says there is no resume cursor and that
  reintroducing one means reintroducing a reader in the same change. The
  frontier tail went with it.
- FINDING 7, all six items. The account watermark now advances only over a
  snapshot every row of which survived conversion (extracted as
  `admit_account_snapshot` so it could be bitten); group attribution reads ANY
  leg's link via `group_id_of`; `peek_group` no longer consumes, so a duplicated
  refusal is absorbed by the terminal-state guard instead of an ERROR that reads
  like a real attribution failure; `stop()` clears the group ring, which is
  transport state. THE PRUNE ITEM WAS CLOSED AS DOCUMENTATION AND A WARN, not a
  bound: open orders are live reconciliation truth and an adapter that forgot
  one is strictly worse than one that uses memory, so the AE6 claim was
  corrected rather than made true. The paginator drift was closed by stating at
  the quote site WHY that one line must not be made identical to the trade
  path's; the unification and the two busy-wait shims are filed in
  `notes/todo.md`.
- THE RELAYED BROADARROW ITEMS ARE GENUINELY FIXED, verified against the code
  rather than assumed: `docs/havoc.md` splits the raw-protocol client from the
  nautilus one and names the ERROR log as the nautilus channel, which is what
  `data.rs`'s `FeedLagged` arm does; `process_session_id`'s comment now derives
  the id from the `OnceLock`'s first-call instant and says so explicitly.
- No tape-generation path is touched, so no `TAPE_PROTOCOL_VERSION` bump is
  owed.

The close pass found three defects in the round's own work, all of the arc's
signature shape - a thing that reads as gated and is not - and closed them:

- The `peek_group` doc block had never been separated from the one below it, so
  the round's central rationale documented `admit_account_snapshot` and
  `peek_group` had no doc at all. Split and reattached.
- `CLOSE_EVICTED`'s comment claimed `CloseSpec::evicted` prefixes
  `close::EVICTED_PREFIX`; it did not, the prefix was composed by hand at the
  single `run.rs` call site. The constructor now takes the DETAIL and prepends
  the prefix itself, before truncation, so the wire contract is structural and
  a second call site cannot forget it - a forgotten prefix reads as
  non-terminal to the adapter and redials straight into the eviction loop.
- Nothing gated that end to end: `close.rs`'s test hand-built the prefixed
  string and `serving.rs` asserted only `contains("claimed account")`, which
  passes either way. The eviction test now runs the venue's own close frame
  through `close::classify` and asserts `Terminal::Evicted`. Bite-checked by
  deleting the prefix - the old assertion passed, the new one fired.

Three smaller observations closed with it: the reattach loop now reports the
untried REMAINDER of `on_connect()` rather than only the command that failed
(extracted as `send_reattach_commands`, bite-checked); the unavoidable
spurious-reject window inside `writer.send` and the no-`await`-before-the-retire
constraint are stated at the site; and the advertised three-way terminal log
distinction was retracted, because the venue sends a `RunComplete` frame ahead
of the close on the duration path too, so the adapter stops on the frame and
`DurationComplete` is reachable only as the close-frame fallback. That last is
a protocol imprecision on the venue's side and is filed in `notes/todo.md`.

## What the hunter checked and found sound

The venue-truth report generators (the argument for querying the venue rather
than the mirror is correct and consistently applied), the terminal-state guards
and forward-only `ts_last` discipline across every `handle_exec_message` arm,
`final_ts_group_start`'s timestamp-cursor rule (the `AGENTS.md` cursor invariant
genuinely holds on both paginators), the `retire_connected_flag` Arc-swap, the
drop-and-warn discipline in `convert.rs` (every panicking nautilus constructor
is routed through a `*_checked` twin - checked for `Price`, `Quantity`, `Money`,
`TradeId`, `ClientOrderId`, `VenueOrderId`, `Symbol`, `AccountBalance`,
`MarginBalance`, `TradeTick`, `QuoteTick`), and the identity-check three-outcome
classification.
