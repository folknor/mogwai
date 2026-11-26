-- measure: the corpus measurement gate through the shipped `mogwai measure`
-- - the live observed pass over a bound corpus plus the eight in-process
-- attestation walks - writing the measurement artifact into the iteration.
-- brokkr reserves the corpus directory and records its digest with the run.
--
-- `elapsed_ms`, `seeds` and `sessions` are required on stderr: they are the
-- work size a moved wall is read against.
--
-- The cache root is the binary's default unless --args passes --cache-dir.
-- It is an input as well as a store: the Brick G walk references in it are
-- read-only, but measure rewrites the observed cross-check file there, so a
-- run touches the operator's cache, and nothing here binds or digests it.
--
--   brokkr bench measure --corpus mnq-tbbo-july

local measure = require("measure")
local passthrough = require("passthrough")

local OWNED = { "--out", "--corpus" }

function validate(params)
    passthrough.check(params, OWNED)
end

function bench(params)
    local out = output("measure.json")
    local argv = { "measure", "--corpus", params.corpus.path, "--out", out }
    passthrough.append(argv, params, OWNED)
    measure.run { argv = argv, required = { "elapsed_ms", "seeds", "sessions" }, outputs = { out } }
end
