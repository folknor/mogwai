-- cache: the on-disk cache controls through the shipped `mogwai cache`.
--
-- stats walks a cache root and counts it: the binary's default root, or the
-- one --args names with --cache-dir. clean always runs against an empty
-- directory the iteration owns, injected as --cache-dir: against a real root
-- it would delete the operator's cache, and every iteration after the first
-- would time an empty tree anyway. So a clean row prices the command's fixed
-- cost and nothing more; there is no populated fixture to seed it with.
-- Neither prints its counts on stderr, so the rows carry no work size.
--
--   brokkr bench cache --entries yes

local measure = require("measure")
local passthrough = require("passthrough")

local function owned(params)
    if params.action == "clean" then return { "--cache-dir", "--stale", "--keep" } end
    return { "--entries" }
end

function validate(params)
    if params.action == "clean" and params.entries == "yes" then
        fail("--entries is a stats flag")
    end
    passthrough.check(params, owned(params))
end

function bench(params)
    local argv = { "cache", params.action }
    if params.action == "clean" then
        argv[#argv + 1] = "--cache-dir"
        argv[#argv + 1] = scratch("cache")
    elseif params.entries == "yes" then
        argv[#argv + 1] = "--entries"
    end
    passthrough.append(argv, params, owned(params))
    measure.run { argv = argv }
end
