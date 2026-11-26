-- stage-a-batch: the frozen Stage A throughput panels through `mogwai-bench
-- stage-a-batch`, the optimization round's stopwatch.
--
--   quick, full  run the committed manifest's panel, walk cache bypassed;
--                the work size (`cells`, `seed_walks`, `parents`, `prints`)
--                and the panel hash are required
--   pilot        regenerate the pilot readings
--   manifest     rebuild the manifest from a pilot
--
-- pilot and manifest write into the iteration: without --output, manifest
-- mode would write the committed analysis/stage-a-batch-pilot.json. The
-- inputs (--measure, --manifest, --pilot) are the binary's defaults unless
-- --args names others, and are not bound.
--
-- Profiling. A timing run is never instrumented: brokkr fails the arm if the
-- hotpath features resolve, which is what registering the instrument bare
-- once guaranteed. Under --hotpath, though, full is hours of an event-per-call
-- profiler over the arrival draw, which backlogs at seconds scale; prefer
-- quick there.
--
--   brokkr bench stage-a-batch --mode quick --jobs 16

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--output", "--jobs" }

local REQUIRED = {
    quick = { "panel_sha256", "cells", "seed_walks", "parents", "prints" },
    full = { "panel_sha256", "cells", "seed_walks", "parents", "prints" },
    pilot = { "pilot_cells", "elapsed_s" },
    manifest = { "panel_sha256", "quick_cells", "full_cells" },
}

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local argv = { "stage-a-batch", params.mode }
    local outputs = {}
    if params.mode == "pilot" or params.mode == "manifest" then
        local out = output(params.mode .. ".json")
        argv[#argv + 1] = "--output"
        argv[#argv + 1] = out
        outputs[1] = out
    end
    if params.jobs then
        argv[#argv + 1] = "--jobs"
        argv[#argv + 1] = params.jobs
    end
    passthrough.append(argv, params, OWNED)
    measure.run {
        argv = argv,
        hotpath = "panel",
        required = REQUIRED[params.mode],
        outputs = outputs,
    }
end
