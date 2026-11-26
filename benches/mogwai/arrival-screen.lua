-- arrival-screen: the arrival model's necessary-condition screen through the
-- shipped `mogwai arrival-screen`, in one of its three modes.
--
--   screen         the official grid run, artifact to --out
--   cost-probe     --cost-probe: prices the walk itself, cache bypassed
--   demand-census  --demand-census: counts lazy envelope demand, artifact
--                  to that path instead of --out, which it conflicts with
--
-- Every artifact path points into the iteration, never at analysis/. The
-- grid run may read and fill an application cache when --args passes
-- --cache; the binary's default is none, and the cost probe bypasses it.
-- The measurement input is the binary's default unless --args passes
-- --measure, and it is not bound.
--
--   brokkr bench arrival-screen --mode cost-probe --jobs 16

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--cost-probe", "--demand-census", "--jobs" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local argv = { "arrival-screen" }
    local outputs = {}
    if params.mode == "demand-census" then
        local census = output("demand-census.json")
        argv[#argv + 1] = "--demand-census"
        argv[#argv + 1] = census
        outputs[1] = census
    else
        local out = output("screen.json")
        argv[#argv + 1] = "--out"
        argv[#argv + 1] = out
        if params.mode == "cost-probe" then
            argv[#argv + 1] = "--cost-probe"
        else
            outputs[1] = out
        end
    end
    if params.jobs then
        argv[#argv + 1] = "--jobs"
        argv[#argv + 1] = params.jobs
    end
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, outputs = outputs }
end
