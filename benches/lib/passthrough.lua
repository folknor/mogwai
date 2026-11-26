-- The passthrough param every bench takes for the long tail of a surface's
-- flags: `--args '["--length","7d","--regime","{...}"]'`, one JSON array of
-- strings, appended to the child's argv after everything the script builds,
-- in order. JSON rather than a `many` param because a raw argv repeats words
-- and holds words with commas, spaces or `..`, all of which a brokkr
-- collection refuses or reinterprets.
--
-- Words that would change which workload runs are refused: the flags the
-- script owns for this bench (its outputs and bound inputs, which it points
-- at handles), and help or version, which exit zero without doing the work.
-- A refused flag is matched bare and in its `--flag=value` spelling. A word
-- holding a NUL byte is left to brokkr, which refuses such an argument at
-- the call; dellingr has no string.char to name the byte here.
-- Declare the param as `{ validated = true, split = false }` and call
-- check() from validate().

local M = {}

local ALWAYS = { "--help", "-h", "--version", "-V" }

local function owned_by(word, flags)
    for _, flag in ipairs(flags) do
        if word == flag or string.sub(word, 1, #flag + 1) == flag .. "=" then
            return flag
        end
    end
    return nil
end

-- The decoded words, or a fail() naming why they cannot run.
function M.words(text, owned)
    if text == nil then return {} end
    local words = json.decode(text)
    if type(words) ~= "table" or getmetatable(words) == nil then
        fail("--args must be a JSON array of strings, got " .. text)
    end
    local count = 0
    for _ in pairs(words) do count = count + 1 end
    if count ~= #words then fail("--args must be a JSON array of strings, got " .. text) end
    for i, word in ipairs(words) do
        if type(word) ~= "string" then
            fail("--args word " .. i .. " is not a string")
        end
        local flag = owned_by(word, ALWAYS) or owned_by(word, owned or {})
        if flag then
            fail("--args may not carry " .. flag .. ": the bench sets it or it skips the workload")
        end
    end
    return words
end

-- Check params.args from validate().
function M.check(params, owned)
    M.words(params.args, owned)
end

-- Append params.args to argv, a list of child arguments.
function M.append(argv, params, owned)
    for _, word in ipairs(M.words(params.args, owned)) do
        argv[#argv + 1] = word
    end
    return argv
end

return M
