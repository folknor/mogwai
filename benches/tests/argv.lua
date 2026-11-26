-- The argv every bench builds: words in order, outputs pointed at handles the
-- iteration owns, the corpus as a bound input, the passthrough appended last
-- and refused where it would change which workload runs.

local CORPUS = "research/market-data/databento/mnqv/2026-07.full.tbbo"
local HOST = { datasets = { ["mnq-tbbo-july"] = { path = CORPUS } } }

-- Compare a call's argv against want, where an OUTPUT word matches any word
-- whose origin is an output and an INPUT word any bound input.
local OUTPUT = {}
local SCRATCH = {}
local INPUT = {}
local function argv_is(call, want)
    assert(#call.argv == #want, "argv length " .. #call.argv .. ": " .. table.concat(call.argv, " "))
    for i, word in ipairs(want) do
        if word == OUTPUT then
            assert(call.origins[i] == "output", "word " .. i .. " is " .. call.origins[i])
        elseif word == SCRATCH then
            assert(call.origins[i] == "scratch", "word " .. i .. " is " .. call.origins[i])
        elseif word == INPUT then
            assert(call.origins[i] == "input", "word " .. i .. " is " .. call.origins[i])
        else
            assert(call.argv[i] == word, "word " .. i .. " is " .. call.argv[i] .. ", want " .. word)
        end
    end
end

local function one(spec, want)
    local r = run(spec)
    assert(r.outcome == "completed", tostring(r.reason))
    assert(#r.calls == 1)
    assert(r.calls[1].measured, "the workload call is the measured one")
    argv_is(r.calls[1], want)
    return r
end

-- -- mogwai

test("gen leaves untyped flags to the binary and writes into the iteration", function()
    one({ bench = "gen", calls = { { outputs = { ["gen.out"] = "x" } } } },
        { "gen", "--type", "trades", "--out", OUTPUT })
end)

test("gen emits every typed flag and appends the passthrough", function()
    one({ bench = "gen",
        params = { "--type", "summary", "--length", "30d", "--symbol", "MNQ", "--seed", "7",
            "--args", '["--start-price","20000"]' },
        calls = { { stderr = "parents=10\nrows=20\n", outputs = { ["gen.out"] = "{}" } } } },
        { "gen", "--type", "summary", "--out", OUTPUT, "--length", "30d", "--symbol", "MNQ",
            "--seed", "7", "--start-price", "20000" })
end)

test("a summary walk requires its work size", function()
    local r = run { bench = "gen", params = { "--type", "summary" },
        calls = { { outputs = { ["gen.out"] = "{}" } } } }
    assert(r.outcome == "failed" and string.find(r.reason, "parents", 1, true), tostring(r.reason))
end)

test("an empty output after exit zero fails", function()
    local r = run { bench = "gen", calls = { { outputs = { ["gen.out"] = "" } } } }
    assert(r.outcome == "failed" and string.find(r.reason, "empty", 1, true), tostring(r.reason))
end)

test("the passthrough refuses a flag the bench owns, in either spelling", function()
    local r = run { bench = "gen", params = { "--args", '["--out","x"]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
    r = run { bench = "gen", params = { "--args", '["--out=x"]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
    r = run { bench = "fit", host = HOST, params = { "--args", '["--help"]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
end)

test("the passthrough must be a JSON array of strings", function()
    local r = run { bench = "gen", params = { "--args", '{"a":"b"}' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
    r = run { bench = "gen", params = { "--args", '["--seed",7]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
end)

test("the passthrough keeps repeated words and commas", function()
    one({ bench = "tick-composition", params = { "--args", '["-x","a,b","-x","a,b"]' },
        calls = { { outputs = { ["composition.json"] = "x" } } } },
        { "tick-composition", "--out", OUTPUT, "-x", "a,b", "-x", "a,b" })
end)

test("tick-composition passes its typed work size", function()
    one({ bench = "tick-composition", params = { "--parents", "1000", "--jobs", "4" },
        calls = { { outputs = { ["composition.json"] = "x" } } } },
        { "tick-composition", "--out", OUTPUT, "--parents", "1000", "--jobs", "4" })
end)

test("preflight, measure and fit take the bound corpus", function()
    local r = one({ bench = "preflight", host = HOST,
        calls = { { outputs = { ["preflight.json"] = "x" } } } },
        { "preflight", "--corpus", INPUT, "--out", OUTPUT })
    assert(r.calls[1].argv[3] == project_root .. "/" .. CORPUS, r.calls[1].argv[3])
    one({ bench = "measure", host = HOST,
        calls = { { stderr = "elapsed_ms=1.5\nseeds=8\nsessions=20\n",
            outputs = { ["measure.json"] = "x" } } } },
        { "measure", "--corpus", INPUT, "--out", OUTPUT })
    one({ bench = "fit", host = HOST, calls = { { outputs = { ["fit.json"] = "x" } } } },
        { "fit", "--corpus", INPUT, "--out", OUTPUT })
end)

test("measure requires its work size", function()
    local r = run { bench = "measure", host = HOST,
        calls = { { stderr = "elapsed_ms=1.5\n", outputs = { ["measure.json"] = "x" } } } }
    assert(r.outcome == "failed" and string.find(r.reason, "seeds", 1, true), tostring(r.reason))
end)

test("a host without the corpus refuses at bind", function()
    local r = run { bench = "measure" }
    assert(r.stage == "bind" and r.outcome == "refused", r.outcome)
end)

test("cache stats and cache clean", function()
    one({ bench = "cache", params = { "--entries", "yes" }, calls = { {} } },
        { "cache", "stats", "--entries" })
    one({ bench = "cache", params = { "--action", "clean" }, calls = { {} } },
        { "cache", "clean", "--cache-dir", SCRATCH })
    local r = run { bench = "cache", params = { "--action", "clean", "--args", '["--cache-dir","x"]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
end)

test("synth runs the chosen subcommand", function()
    one({ bench = "synth", params = { "--command", "cadence" },
        calls = { { outputs = { ["cadence.json"] = "x" } } } },
        { "synth", "cadence", "--out", OUTPUT })
end)

test("arrival-screen modes", function()
    one({ bench = "arrival-screen", params = { "--jobs", "8" },
        calls = { { outputs = { ["screen.json"] = "x" } } } },
        { "arrival-screen", "--out", OUTPUT, "--jobs", "8" })
    one({ bench = "arrival-screen", params = { "--mode", "cost-probe" }, calls = { {} } },
        { "arrival-screen", "--out", OUTPUT, "--cost-probe" })
    one({ bench = "arrival-screen", params = { "--mode", "demand-census" },
        calls = { { outputs = { ["demand-census.json"] = "x" } } } },
        { "arrival-screen", "--demand-census", OUTPUT })
end)

-- -- mogwai-bench

test("the argument-free instruments are marked for hotpath", function()
    local r = one({ bench = "arrival-walk",
        calls = { { stderr = "elapsed_ms=1.0\nparents=6\nchildren=9\n" } } }, { "arrival-walk" })
    assert(r.calls[1].hotpath == "walk")
    r = one({ bench = "screen-projection",
        calls = { { stderr = "elapsed_ms=1.0\nparents=6\nprints=9\n" } } }, { "screen-projection" })
    assert(r.calls[1].hotpath == "projection")
    r = one({ bench = "ring-sizing",
        calls = { { stderr = "ring_depth=4096\nring_resident_bytes=10\n" } } }, { "ring-sizing" })
    assert(r.calls[1].hotpath == "ring")
end)

local TAPE = "frames=10\nending=sample_complete\nsample_ms=10000\n"
    .. "p50_lateness_ns=1\np99_lateness_ns=2\nmax_lateness_ns=3\n"

test("tape-lateness argv and venue", function()
    one({ bench = "tape-lateness", calls = { { stderr = TAPE } } }, { "tape-lateness" })
    local r = one({ bench = "tape-lateness",
        params = { "--sample-seconds", "30", "--venue", "target/release/mogwai" },
        calls = { { stderr = TAPE } } }, { "tape-lateness", "", "30" })
    assert(r.calls[1].env.MOGWAI_VENUE_BINARY == "target/release/mogwai")
    one({ bench = "tape-lateness", params = { "--config", "c.toml" }, calls = { { stderr = TAPE } } },
        { "tape-lateness", "c.toml" })
end)

test("tape-lateness fails a sample that did not complete", function()
    local stderr = string.gsub(TAPE, "sample_complete", "stream_ended")
    local r = run { bench = "tape-lateness", calls = { { stderr = stderr } } }
    assert(r.outcome == "failed" and string.find(r.reason, "stream_ended", 1, true), tostring(r.reason))
    stderr = string.gsub(TAPE, "frames=10", "frames=0")
    r = run { bench = "tape-lateness", calls = { { stderr = stderr } } }
    assert(r.outcome == "failed", r.outcome)
end)

test("stage-a-batch points pilot and manifest output into the iteration", function()
    one({ bench = "stage-a-batch", params = { "--mode", "pilot", "--jobs", "4" },
        calls = { { stderr = "pilot_cells=72\nelapsed_s=1.0\n", outputs = { ["pilot.json"] = "x" } } } },
        { "stage-a-batch", "pilot", "--output", OUTPUT, "--jobs", "4" })
    local r = one({ bench = "stage-a-batch", params = { "--mode", "quick" },
        calls = { { stderr = "panel_sha256=ab\ncells=8\nseed_walks=8\nparents=1\nprints=1\n" } } },
        { "stage-a-batch", "quick" })
    assert(r.calls[1].hotpath == "panel")
    r = run { bench = "stage-a-batch", params = { "--mode", "full", "--args", '["--output","x"]' } }
    assert(r.stage == "validate" and r.outcome == "refused", r.outcome)
end)

test("envelope-evaluation always names the family", function()
    local stderr = "family=wall_mmpp\nmonths=8\ngrid_cells=3\nelapsed_s=1.0\nper_month_s=0.1\n"
    one({ bench = "envelope-evaluation", params = { "--months", "8" }, calls = { { stderr = stderr } } },
        { "envelope-evaluation", "shot_noise", "8" })
    one({ bench = "envelope-evaluation", params = { "--family", "wall_mmpp" },
        calls = { { stderr = stderr } } }, { "envelope-evaluation", "wall_mmpp" })
    local r = run { bench = "envelope-evaluation", params = { "--months", "0" } }
    assert(r.stage == "bind" and r.outcome == "refused", r.outcome)
end)

test("tag-decode keeps its stdout table", function()
    local r = one({ bench = "tag-decode", params = { "--iterations", "10" },
        calls = { { stdout = "internally tagged: 1 ns\n" } } }, { "tag-decode", "10" })
    assert(r.record.report == "internally tagged: 1 ns\n")
    assert(r.calls[1].hotpath == nil)
end)

test("order-path-refold always names the resting count", function()
    local stderr = "refolds_performed=8\norders_resting=50\ncommands=6\n"
    one({ bench = "order-path-refold", params = { "--rounds", "2" }, calls = { { stderr = stderr } } },
        { "order-path-refold", "50", "2" })
    local r = run { bench = "order-path-refold", params = { "--resting", "1" } }
    assert(r.stage == "bind" and r.outcome == "refused", r.outcome)
end)
