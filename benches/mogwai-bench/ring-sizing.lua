-- ring-sizing: the eager resident and allocator cost of one shipped
-- per-boat broadcast ring at the config's default fan-out depth, through
-- `mogwai-bench ring-sizing`. `ring_depth` and `ring_resident_bytes` are
-- required; under --alloc the allocation itself is the annotated region.
--
--   brokkr bench --alloc ring-sizing

local measure = require("measure")

function bench()
    measure.run {
        argv = { "ring-sizing" },
        hotpath = "ring",
        required = { "ring_depth", "ring_resident_bytes" },
    }
end
