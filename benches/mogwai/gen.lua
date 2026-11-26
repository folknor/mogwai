-- gen: one offline generator walk through the shipped `mogwai gen`, end to
-- end - startup, argument parsing, the walk and the formatting of what it
-- emits. The output goes to a file the iteration owns, so the wall includes
-- writing it, and a CSV mode's wall moves with the drive it lands on.
--
-- A summary walk reports `parents` and `rows`, both pure functions of
-- preset, window, burn-in and seed, and they are required there: a row whose
-- counts moved walked a different tape. The other modes print no work size
-- on stderr, so their rows carry none.
--
-- Flags the registry does not type go through --args, a JSON array.
--
--   brokkr bench gen --type summary --length 30d --seed 7
--   brokkr bench gen --symbol MNQ --args '["--start-price","20000"]'

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--type", "--length", "--symbol", "--seed", "--interval" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output("gen.out")
    local argv = { "gen", "--type", params.type, "--out", out }
    if params.length then
        argv[#argv + 1] = "--length"
        argv[#argv + 1] = params.length
    end
    if params.interval then
        argv[#argv + 1] = "--interval"
        argv[#argv + 1] = params.interval
    end
    if params.symbol then
        argv[#argv + 1] = "--symbol"
        argv[#argv + 1] = params.symbol
    end
    if params.seed then
        argv[#argv + 1] = "--seed"
        argv[#argv + 1] = params.seed
    end
    passthrough.append(argv, params, OWNED)
    local required = {}
    if params.type == "summary" then required = { "parents", "rows" } end
    measure.run { argv = argv, required = required, outputs = { out } }
end
