-- order-path-refold: submit, amend and cancel against the aggregate hold
-- fold for a margin-equity account, through `mogwai-bench
-- order-path-refold`. The canonical decision cell is 50 resting orders.
-- `refolds_performed`, `orders_resting` and `commands` are required; the
-- harness's own `elapsed_ns` goes to stdout and is kept as record.elapsed_ns.
-- Resting orders are always emitted, since the harness takes the round count
-- only after them.
--
--   brokkr bench order-path-refold --resting 200 --rounds 5000

local measure = require("measure")

function bench(params)
    local argv = { "order-path-refold", params.resting }
    if params.rounds then argv[#argv + 1] = params.rounds end
    local r = measure.run {
        argv = argv,
        required = { "refolds_performed", "orders_resting", "commands" },
    }
    local elapsed = string.match(r.stdout, "elapsed_ns=(%d+)")
    if elapsed then record.elapsed_ns = elapsed end
end
