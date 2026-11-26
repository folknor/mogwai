-- tag-decode: the per-frame cost of the wire decoder over one representative
-- `Trade`, four arms, through `mogwai-bench tag-decode`. The harness counts
-- allocator calls through its own allocator and prints its table on stdout,
-- which this bench keeps as record.report; stderr counters are recorded as
-- for every bench, and none is required. It refuses an instrumented build,
-- so the bench carries no profiling features.
--
--   brokkr bench tag-decode --iterations 5000000

local measure = require("measure")

function bench(params)
    local argv = { "tag-decode" }
    if params.iterations then argv[#argv + 1] = params.iterations end
    local r = measure.run { argv = argv }
    record.report = r.stdout
end
