-- The stderr `key=value` counters every mogwai surface prints at the end of
-- a run (`mogwai_lab::sidecar::kv` and `report`, and the hand-written `kv` in
-- the harnesses below the lab). The retired `brokkr mogwai` scraped them into
-- the results row; script benches do not, so this module does, into
-- record.counters.
--
-- What a line must look like to count: `KEY=VALUE` on a line of its own, KEY
-- a letter or `_` then letters, digits and `_`, split at the first `=`, a
-- terminal carriage return dropped. Anything else on stderr is a diagnostic
-- and is ignored. A value is a number only when it is a plain decimal that
-- converts to a finite number without losing digits: an integer of at most
-- 15 digits, or a decimal fraction of at most 32 characters. Everything else,
-- `inf` and `NaN` among it, stays a string, since record refuses a non-finite
-- number. A value longer than 256 characters is clipped, and one holding
-- anything but printable ASCII is dropped and named in counters_dropped, so
-- no raw diagnostic bytes reach the record.
--
-- Where the lines come from. A child's stderr preview is its first MiB. When
-- the preview is whole, that is the stream. When it is truncated and the
-- spool is at most 16 MiB, the most a string holds, the whole spool is read.
-- Past that only the first and last MiB are seen: the head is kept through
-- its last newline, the tail from after its first newline, since where the
-- tail starts is unknown, and record.counters_incomplete is set because a
-- line in the unseen middle, or one the tail cut, is lost.
--
-- Duplicates. A key printed more than once keeps its last value, and
-- record.counters_repeated counts how many times it was seen. Each bench
-- names the counters it requires: a required counter that is missing or
-- repeated fails the iteration, since a row without its work size cannot be
-- told from a row that did less, and a stream too large to read whole fails
-- it too when anything is required, since uniqueness cannot then be shown.

local M = {}

local SPOOL_LIMIT = 16 * 1024 * 1024
local VALUE_LIMIT = 256

local function finite(n)
    return n ~= nil and n == n and n ~= math.huge and n ~= -math.huge
end

-- The typed value of one counter, or nil when it must be dropped.
local function convert(text)
    if #text > VALUE_LIMIT then text = string.sub(text, 1, VALUE_LIMIT) end
    if not string.match(text, "^[%w%p ]*$") then return nil end
    local digits = string.match(text, "^%-?(%d+)$")
    if digits then
        if #digits <= 15 then return tonumber(text) end
        return text
    end
    if #text <= 32 and string.match(text, "^%-?%d+%.%d+$") then
        local n = tonumber(text)
        if finite(n) then return n end
    end
    return text
end

local function scan(text, state)
    for line in string.gmatch(text, "[^\n]+") do
        if string.sub(line, -1) == "\r" then line = string.sub(line, 1, -2) end
        local key, value = string.match(line, "^([%a_][%w_]*)=(.*)$")
        if key then
            local typed = convert(value)
            if typed == nil then
                state.dropped[#state.dropped + 1] = key
            else
                state.values[key] = typed
                state.seen[key] = (state.seen[key] or 0) + 1
            end
        end
    end
end

-- The head preview through its last newline, and the tail preview from
-- after its first one.
local function complete_head(text)
    local last = nil
    local from = 1
    while true do
        local at = string.find(text, "\n", from, true)
        if not at then break end
        last = at
        from = at + 1
    end
    if last == nil then return "" end
    return string.sub(text, 1, last)
end

local function complete_tail(text)
    local first = string.find(text, "\n", 1, true)
    if not first then return "" end
    return string.sub(text, first + 1)
end

-- Parse a binary() result's stderr. Returns the counters, the repeat counts,
-- the dropped keys and whether the scrape is incomplete.
function M.parse(r)
    local state = { values = {}, seen = {}, dropped = {} }
    local incomplete = false
    if not r.stderr_truncated then
        scan(r.stderr, state)
    elseif filesize(r.stderr_file) <= SPOOL_LIMIT then
        scan(read(r.stderr_file), state)
    else
        scan(complete_head(r.stderr), state)
        scan(complete_tail(r.stderr_tail), state)
        incomplete = true
    end
    return state.values, state.seen, state.dropped, incomplete
end

-- Parse r's stderr into record.counters, holding the required keys to
-- exactly once. Returns the counters.
function M.record(r, required)
    local values, seen, dropped, incomplete = M.parse(r)
    required = required or {}
    if incomplete and #required > 0 then
        fail("stderr is past 16 MiB, so the required counters cannot be shown to occur once")
    end
    for _, key in ipairs(required) do
        if seen[key] == nil then fail("required counter missing from stderr: " .. key) end
        if seen[key] > 1 then fail("required counter repeated on stderr: " .. key) end
    end
    record.counters = values
    local repeated = nil
    for key, count in pairs(seen) do
        if count > 1 then
            repeated = repeated or {}
            repeated[key] = count
        end
    end
    if repeated then record.counters_repeated = repeated end
    if #dropped > 0 then record.counters_dropped = dropped end
    if incomplete then record.counters_incomplete = true end
    return values
end

return M
