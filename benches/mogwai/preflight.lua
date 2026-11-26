-- preflight: the fail-closed TBBO corpus contract check through the shipped
-- `mogwai preflight`, over a bound corpus, so brokkr reserves the corpus
-- directory and records its digest with the run. The artifact goes into the
-- iteration, never over the committed one. Hashing the corpus dominates, so
-- the wall moves with the corpus's page-cache state, which this bench does
-- not prepare.
--
-- The jobs manifest stays the binary's default unless --args names one; it
-- is not bound, so a changed manifest does not show in the pairing.
--
--   brokkr bench preflight --corpus mnq-tbbo-july

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--corpus" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output("preflight.json")
    local argv = { "preflight", "--corpus", params.corpus.path, "--out", out }
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, outputs = { out } }
end
