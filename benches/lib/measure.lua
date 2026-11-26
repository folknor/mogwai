-- The one measured call every mogwai bench makes, and what it records.
--
-- Each bench builds an argv and hands it here. The call is marked measured,
-- so the result is that child's wall from launch to reap: process start,
-- argument parsing and the end-of-run writes included, the bench's own Lua
-- excluded. A bench that can be profiled passes a hotpath stage, which marks
-- the same call. The child must exit zero; its stderr counters then go into
-- record.counters (counters.lua), holding the bench's required ones to
-- exactly once, and every output the bench names must exist and be non-empty,
-- so an exit zero that wrote nothing is never recorded as the workload.

local counters = require("counters")

local M = {}

-- spec: argv (list), hotpath (stage name or nil), required (counter keys),
-- outputs (output handles the child must write), env (extra environment).
-- Returns the result and the counters.
function M.run(spec)
    local options = { measured = true }
    if spec.hotpath then options.hotpath = spec.hotpath end
    if spec.env then options.env = spec.env end
    local r = binary_with(options, table.unpack(spec.argv)):ok()
    local values = counters.record(r, spec.required)
    for _, h in ipairs(spec.outputs or {}) do
        if filesize(h) == 0 then fail("the child exited zero and left " .. h.path .. " empty") end
    end
    return r, values
end

return M
