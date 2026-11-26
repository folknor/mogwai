-- screen-projection: one Stage A screen cell per family at one seed,
-- projection attached and cache bypassed, through `mogwai-bench
-- screen-projection`. Reads the committed analysis/mnq-measure-12a.json, so
-- it needs no market data. Read against arrival-walk by subtraction.
--
-- A deliberately ms-scale workload: the profiler queues one event per
-- instrumented call and drains at roughly 1.3M a second, so a seconds-scale
-- run under --hotpath backlogs gigabytes. `elapsed_ms`, `parents` and
-- `prints` are required.
--
--   brokkr bench --hotpath screen-projection

local measure = require("measure")

function bench()
    measure.run {
        argv = { "screen-projection" },
        hotpath = "projection",
        required = { "elapsed_ms", "parents", "prints" },
    }
end
