# The Databento downloader: implementation contract

Written against `reference/technical-implementation-spec.md`. Spawned from
`DATA-PURCHASE-REPORT.md` section 14.1 ("The downloader does not exist") and
scoped to the staged purchase of section 9.6. This document is the contract a
single implementer builds from without re-deriving any decision; the decisions
below are settled, not open.

Building the tool is authorized by this spec once accepted. RUNNING it against
`batch.submit_job` is not: every purchase remains a separate, explicit,
human-authorized act, and the tool's own gating (below) enforces that shape.

## Survey of the ground

- `analysis/databento_price.py` is the pricing tool and the single source of
  truth for `SCOPES`, `WINDOWS`, `PLANS`, `session_bounds_utc` (explicit UTC
  bounds from the Central session boundary, DST included) and the cached,
  free `request()` machinery with its error-never-cached rule. Its hard
  invariant - no reachable path to `timeseries.get_range` or
  `batch.submit_job`, verified by two independent reviews - must survive this
  work UNCHANGED: the downloader is a separate file and the pricing script
  gains no new capability.
- The API key lives at `research/databento.key`, read the same way the
  pricing script reads it.
- Reference implementations, read-only, in `research/`:
  - `databento-ingest` - the proven download lifecycle: `.downloading` temp,
    streaming SHA-256, size check, atomic promote, Range-header resume with a
    206 guard, bounded retries with exponential backoff, stale-temp cleanup,
    fail-closed partials, a provenance manifest. Its submit wrapper is
    minimal and its jobs were submitted externally, which is exactly the gap
    this contract fills.
  - `databento-python` - the authoritative parameter names for
    `batch.submit_job`, `batch.list_jobs`, `batch.list_files`,
    `metadata.get_cost`. The implementer reads the exact field names and
    response shapes from here rather than assuming them; the futures-versus-
    spot schema trap is this project's recorded failure mode.
  - `dbn` and `databento-stream-downloader` - format and streaming reference.
    Neither becomes a dependency.
- Existing archives land in `research/market-data/` (gitignored), verified
  before first read. The downloader follows the same convention.

## Settled decisions

1. **One new file, `analysis/databento_download.py`, stdlib-only.** Python
   3.14 stdlib covers everything needed: `urllib.request` for HTTPS,
   `hashlib` for SHA-256, `compression.zstd` for delivery decompression
   checks. No pip, no SDK import, no `dbn` binary.
2. **Batch, never streaming.** `batch.submit_job` with
   `encoding=csv`, `compression=zstd`, `split_duration=month`,
   `stype_in=continuous`, symbols and bounds taken from the imported
   `databento_price` tables. CSV-zstd because `compression.zstd` is stdlib
   from 3.14, the whole analysis pipeline already parses CSV bytes, and the
   dollar cost is identical - only transfer size grows, immaterial at ~4 GB
   DBN scale. DBN is rejected as an analysis input: it would force a
   non-stdlib reader into `analysis/`.
3. **Dry-run is the default and the confirm path is doubly gated.** A plain
   invocation prices, prints, and submits nothing. Submission requires BOTH
   `--confirm` AND an explicit `--max-dollars N`. There is exactly ONE call
   site of `batch.submit_job` in the file, and it is reachable only through
   the function that enforces both flags plus the re-price check. This is
   the same audit invariant the pricing script carries, extended: the
   reviewer must be able to prove the property by reading one function.
4. **Re-price immediately before submitting.** `metadata.get_cost` is called
   fresh (cache bypassed) at submit time; if the live quote exceeds
   `--max-dollars`, or exceeds the plan's recorded quote by more than 10
   percent, the tool refuses and submits nothing. Vendor rates are not
   eternal; a cached quote is only as good as its timestamp.
5. **A committed ledger makes re-runs free and re-buys impossible.**
   `analysis/databento-jobs.json`, keyed by `(scope, window, schema)`,
   records job id, submitted-at, the live quote at submission, job state,
   and per-file SHA-256 on completion. A keyed entry with a job id is NEVER
   resubmitted - re-running the tool polls and downloads instead. The ledger
   is committed; it is the spend record.
6. **Download lifecycle copies the proven shape.** Per file:
   `.downloading` temp, streaming SHA-256 against the hash
   `batch.list_files` publishes, size check, atomic `os.replace` promote,
   Range resume with a 206-or-restart guard, bounded retries with
   exponential backoff. A hash or size mismatch deletes the temp and fails
   that file hard. Any failed file fails the run with a nonzero exit and a
   named list - fail closed on partial success, no silent partials.
7. **Landing layout**: `research/market-data/databento/<scope>/<window>.<schema>/`
   holding the delivered files plus a `manifest.json` recording job id,
   request parameters, quotes (plan-time and submit-time), file inventory
   with verified hashes, and tool version. Gitignored like its siblings; the
   committed record is the ledger.
8. **Plans are imported, not duplicated.** The tool imports `SCOPES`,
   `WINDOWS`, `PLANS` and `session_bounds_utc` from `databento_price` so a
   window edit cannot desynchronize pricing from purchasing. It never
   imports anything that would let `databento_price` reach a paid endpoint.

## Bricks, in landing order

Each brick leaves the tree green; gates are exact commands.

1. **Selftest harness and ledger.** The file with `selftest` and `status`
   modes, the ledger read/write (atomic, refuses unparseable), and fixture
   coverage for: ledger idempotency (a keyed job is never resubmitted), the
   double gate (submission code path unreachable without both flags - proven
   by invoking the internal decision function against fixture argument
   sets), and the 10-percent re-price refusal. No network anywhere in
   selftest.
   Gate: `python3 -u analysis/databento_download.py selftest`
2. **Pricing and dry-run.** `plan <scope> <plan>` mode: reuse the imported
   tables, fetch live quotes through a fresh (non-cached) `metadata.get_cost`
   path, print the would-submit table with quotes, drift versus the cached
   plan quote, and the refusal verdicts. Free endpoints only.
   Gate: `python3 -u analysis/databento_download.py plan pairv paircurrent`
   (prints, submits nothing; run after selftest passes)
3. **Submission, polling, listing.** The doubly-gated submit function, job
   polling via `batch.list_jobs`, file listing via `batch.list_files`,
   ledger updates at each transition. Verified in selftest with recorded
   response fixtures taken from `research/databento-python`'s documented
   shapes; NO live submission in any gate.
   Gate: `python3 -u analysis/databento_download.py selftest`
4. **Download engine.** The per-file lifecycle of decision 6, sequential or
   `--parallel 2`, plus the landing manifest. Fixture-driven selftest for
   resume math, 206 fallback, hash mismatch, and partial-failure exit codes;
   live verification happens only during the separately authorized purchase.
   Gate: `python3 -u analysis/databento_download.py selftest`
5. **Record.** Update `DATA-PURCHASE-REPORT.md` 14.1 to point here and state
   what is and is not authorized. Bundle with the code commit.
   Gate: the diff itself; no command.

## Stopping rule

Out of scope, deliberately: reading or converting the delivered CSVs (a
consumer concern), any DBN decoding, any change to `databento_price.py`
beyond zero, any automatic purchase of the 9.6 basket, retry-forever
daemonics, and FTP. The tool ends at verified bytes on disk plus the ledger
entry; analysis of purchased data is its own future work item.

## Authorization boundary, restated

Accepting this spec authorizes BUILDING the tool and running its free modes
(`selftest`, `status`, `plan`). It does not authorize `--confirm` on
anything. The 9.6 purchase sequence - pair first and alone, read, then the
four contiguous months - is executed only on explicit instruction, one stage
at a time, inside the 125-dollar credit.
