-- benches/lib/counters.lua, driven through real benches: what a stderr line
-- must look like to count, how values are typed, and when a required counter
-- fails the iteration.

-- dellingr has no string.rep; this builds the same by doubling.
local function rep(text, n)
    local out, piece = "", text
    while n > 0 do
        if n % 2 == 1 then out = out .. piece end
        piece = piece .. piece
        n = math.floor(n / 2)
    end
    return out
end

local REFOLD ="refolds_performed=8000\norders_resting=50\ncommands=6000\n"

local function refold(stderr, extra)
    local call = { stderr = stderr, stdout = "elapsed_ns=123456\n" }
    for k, v in pairs(extra or {}) do call[k] = v end
    return run { bench = "order-path-refold", calls = { call } }
end

test("counter lines are parsed and typed; diagnostics are ignored", function()
    local r = refold("warming up\n" .. REFOLD
        .. "label=sample_complete\r\nratio=0.250\nbig=12345678901234567890\n"
        .. "weird=inf\nnot a counter=1\n9bad=1\n")
    assert(r.outcome == "completed", r.reason)
    local c = r.record.counters
    assert(c.refolds_performed == 8000)
    assert(c.orders_resting == 50 and c.commands == 6000)
    assert(c.label == "sample_complete", tostring(c.label))
    assert(c.ratio == 0.25)
    assert(c.big == "12345678901234567890", "an integer past 15 digits stays text")
    assert(c.weird == "inf", "a non-finite spelling stays text")
    assert(c["9bad"] == nil)
    assert(r.record.counters_incomplete == nil)
    assert(r.record.elapsed_ns == "123456")
end)

test("a value splits at the first equals sign", function()
    local r = refold(REFOLD .. "pair=a=b\n")
    assert(r.outcome == "completed", r.reason)
    assert(r.record.counters.pair == "a=b")
end)

test("a repeated optional counter keeps its last value and is counted", function()
    local r = refold(REFOLD .. "step=1\nstep=2\nstep=3\n")
    assert(r.outcome == "completed", r.reason)
    assert(r.record.counters.step == 3)
    assert(r.record.counters_repeated.step == 3)
end)

test("a missing required counter fails the iteration", function()
    local r = refold("refolds_performed=8000\ncommands=6000\n")
    assert(r.outcome == "failed", r.outcome)
    assert(string.find(r.reason, "orders_resting", 1, true), r.reason)
end)

test("a repeated required counter fails the iteration", function()
    local r = refold(REFOLD .. "commands=6001\n")
    assert(r.outcome == "failed", r.outcome)
    assert(string.find(r.reason, "repeated", 1, true), r.reason)
end)

test("a value that is not printable ASCII is dropped and named", function()
    local r = refold(REFOLD .. "blob=a\tb\n")
    assert(r.outcome == "completed", r.reason)
    assert(r.record.counters.blob == nil)
    assert(r.record.counters_dropped[1] == "blob")
end)

test("a long value is clipped", function()
    local r = refold(REFOLD .. "long=" .. rep("x", 300) .. "\n")
    assert(r.outcome == "completed", r.reason)
    assert(#r.record.counters.long == 256)
end)

test("a stderr past the preview is read whole from the spool", function()
    local filler = rep("diagnostic line that is not a counter\n", 40000)
    local r = refold(filler .. REFOLD)
    assert(r.outcome == "completed", r.reason)
    assert(r.record.counters.commands == 6000)
    assert(r.record.counters_incomplete == nil)
end)

test("a nonzero exit fails before any counter is read", function()
    local r = refold(REFOLD, { exit = 1 })
    assert(r.outcome == "failed", r.outcome)
end)
