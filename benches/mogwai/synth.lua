-- synth: offline synthesis through the shipped `mogwai synth`, fingerprint
-- or cadence, writing the artifact into the iteration rather than over the
-- committed analysis/ files. The inputs are the binary's defaults
-- (analysis/ reports, the cadence measurement, the Binance archives) unless
-- --args names others; none of them is bound, so a changed input does not
-- show in the pairing.
--
--   brokkr bench synth --command fingerprint

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output(params.command .. ".json")
    local argv = { "synth", params.command, "--out", out }
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, outputs = { out } }
end
