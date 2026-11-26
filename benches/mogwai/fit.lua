-- fit: the session calibration fit through the shipped `mogwai fit` over a
-- bound corpus, writing the fit artifact into the iteration rather than over
-- the committed one. brokkr reserves the corpus directory and records its
-- digest with the run.
--
-- Whether walks come from a cache is the binary's own decision unless
-- --args passes --cache-dir and --cache-commit, and nothing here records how
-- many hit, so two rows may mix cached and fresh walks differently.
--
--   brokkr bench fit --corpus mnq-tbbo-july

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--corpus" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output("fit.json")
    local argv = { "fit", "--corpus", params.corpus.path, "--out", out }
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, outputs = { out } }
end
