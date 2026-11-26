-- tape-lateness: how late a paced tape arrives at a consumer that keeps up,
-- under acceleration, through `mogwai-bench tape-lateness`. The harness
-- launches the shipped venue through the shipped launcher and reads the boot
-- river for the sample length.
--
-- The result is the lateness distribution on stderr, not the wall: the
-- ranked child wall is mostly the requested sample plus venue startup and
-- teardown, so read record.counters (p50, p99 and max lateness beside the
-- frame count) and never the fastest wall as a latency comparison. A run
-- whose loop ended any way but `sample_complete`, or saw no frame, measured a
-- prefix of some other length and fails.
--
-- The venue binary. brokkr builds and snapshots this bench's target alone,
-- so no `mogwai` sits beside the snapshot. --venue names the binary to
-- launch, handed over as MOGWAI_VENUE_BINARY; it is a path, not a bound
-- input, so brokkr neither builds nor digests it, and under --commit it is
-- whatever that path holds, not that commit's venue.
--
-- Argv is `[config] [sample_seconds]`, with an empty config meaning the
-- accelerated fixture, so a run that sets only --sample-seconds passes "".
--
--   brokkr bench tape-lateness --venue target/release/mogwai --sample-seconds 30

local measure = require("measure")

local REQUIRED = {
    "frames", "ending", "sample_ms", "p50_lateness_ns", "p99_lateness_ns", "max_lateness_ns",
}

function bench(params)
    local argv = { "tape-lateness" }
    if params.config or params["sample-seconds"] then
        argv[#argv + 1] = params.config or ""
    end
    if params["sample-seconds"] then argv[#argv + 1] = params["sample-seconds"] end
    local env = nil
    if params.venue then env = { MOGWAI_VENUE_BINARY = params.venue } end
    local _, counters = measure.run { argv = argv, required = REQUIRED, env = env }
    if counters.ending ~= "sample_complete" then
        fail("the sample ended " .. tostring(counters.ending) .. ", not sample_complete")
    end
    if type(counters.frames) ~= "number" or counters.frames <= 0 then
        fail("the sample saw no frame")
    end
end
