# Bug hunt: mogwai-data

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: findings 1 and 2 are extended from the server side in
`bugs-server-tape.md` findings 6 and 2. The `BoundedSeek` finding was reported
independently by both hunters.

## 4. `symbol: String` is cloned per materialized tick

MOSTLY CLOSED by `bugs-server-tape.md` rounds 1 and 2 (findings 3 and 4a): the
checkpoint stride is 8,192 again (not 4,194,304), so the per-request residual is
bounded by thousands of ticks rather than millions, and `GeneratedSource` now
overrides `seek_to` to skip whole parents through `advance_parent` without
materializing protocol objects, with a continuation golden pinning identical
draws. `CheckpointIndex::extend_toward` uses the same compact step.

What remains open is the allocation on the ticks that ARE materialized:
`next_child` does `symbol: self.scalars.symbol.clone()` per tick, and an
`Arc<str>` (or interned symbol) would remove the crate's most frequent heap
allocation on the feed and history paths.

## 8. `MergeSource` documents a heap it does not have

`lib.rs` - "a heap of per-source heads yields a global ordering". `next_tick` is a
linear `min_by_key` scan over `heads`. Fine at k=1 (the only production shape), and
the tie-break is deterministic by index, which matters; but the doc is wrong. Given
production runs k=1 and the type exists solely as a one-tick pushback buffer (per the
essay in `source.rs`), delete `MergeSource` and name the thing what it is: a
`Peekable`-style one-tick pushback over a `TickSource`. That also removes a
`Vec<Box<dyn TickSource>>` and a `Vec<Option<TickEvent>>` from every history request.

## 9. `TAPE_PROTOCOL_VERSION`

No bump is owed. The working tree contains zero Rust changes and does not touch
`analysis/fingerprint.json`; the diffs are `.review.toml`, `mogwai.toml`
(`fanout_depth` 65536 -> 1048576, a broadcast-ring capacity that never reaches the
generator), two markdown files, and untracked `analysis/` Python.
`notes/sampling-frame-preregistration.md` already states this explicitly.
`reference/architecture.md` says version 9, matching `lib.rs`.

## Smaller items

- `SessionCalendar::settlement_instants` iterates every minute in `[from_ns, to_ns]`
  calling `is_open` per minute. Over a span of days that is fine; over a from-origin
  span it is millions of iterations. It should step day-by-day from the first
  local-midnight crossing rather than scanning minutes.
- `PublishedBook` carries `TopOfBookSizes`, which carries a `CalibrationProvenance`
  that can hold a `String`. `place_book` clones it on every parent event and
  `begin_event` clones `last_book` again on a compatible repeat. Today all presets
  are `Uncalibrated` so no allocation happens - the moment anyone fits `top_sizes`,
  this becomes two heap allocations per parent event on the hot path. Provenance is
  calibration metadata; it does not belong in a per-event runtime value.
- `SweepShape::new` is re-solved per event (five floats plus two `ln` calls). The
  comment justifies it by `FlowSurge` possibly scaling `children_mean`. It could be
  cached and invalidated on surge arm/clear; minor, but the `ln` pair is not free at
  50 events/s times multi-million-tick drains.
- `TickRuleAggressor` keys a `HashMap<String, _>` and clones the symbol on every
  trade. Unbounded growth is bounded in practice by symbol count, but it is another
  per-tick allocation.
- `checkpoint.rs` retains up to 4096 full `GeneratedSource` clones, each duplicating
  the immutable config (`GeneratorScalars` with its `String`s, `SessionCalendar` with
  its `Vec`, three distribution objects, three 24/7-element arrays). The mutable walk
  state is a few hundred bytes; everything else should sit behind one `Arc` shared by
  the lead and all snapshots.
