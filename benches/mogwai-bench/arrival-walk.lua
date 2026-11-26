-- arrival-walk: the parent draw with no projection attached, through
-- `mogwai-bench arrival-walk`. Three kernel families, two million parents
-- each, on fingerprint medians with a flat session profile: the floor no
-- measurement-side optimization can go below. Read against screen-projection
-- by subtraction, on cost per parent.
--
-- The harness reports its own `elapsed_ms`, which excludes the hotpath
-- report write the ranked child wall includes; `parents` and `children` are
-- the work size, and all three are required. Under --hotpath and --alloc the
-- one annotated region is each family's whole walk.
--
--   brokkr bench arrival-walk
--   brokkr bench --hotpath arrival-walk

local measure = require("measure")

function bench()
    measure.run {
        argv = { "arrival-walk" },
        hotpath = "walk",
        required = { "elapsed_ms", "parents", "children" },
    }
end
