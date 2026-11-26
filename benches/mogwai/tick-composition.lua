-- tick-composition: the BBO protocol composition measurement through the
-- shipped `mogwai tick-composition`, writing its paired fixture into the
-- iteration rather than the tree. --parents is the work size per preset,
-- seed and configuration; --jobs the traversal workers, omitted for the
-- machine's parallelism.
--
--   brokkr bench tick-composition --parents 200000 --jobs 8

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--parents", "--jobs" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output("composition.json")
    local argv = { "tick-composition", "--out", out }
    if params.parents then
        argv[#argv + 1] = "--parents"
        argv[#argv + 1] = params.parents
    end
    if params.jobs then
        argv[#argv + 1] = "--jobs"
        argv[#argv + 1] = params.jobs
    end
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, outputs = { out } }
end
