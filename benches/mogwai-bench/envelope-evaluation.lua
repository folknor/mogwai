-- envelope-evaluation: predictive-envelope replicate months for one family's
-- probe cell, through `mogwai-bench envelope-evaluation`. The month is the
-- unit the envelope budgets are derived from (an evaluation is 500 * (1 + K)
-- months and nothing else), so `per_month_s` multiplies out to every K tier.
-- Reads the committed analysis/mnq-measure-12a.json.
--
-- `family`, `months`, `grid_cells`, `elapsed_s` and `per_month_s` are
-- required. The family is always emitted, since the harness takes the month
-- count only after it.
--
--   brokkr bench envelope-evaluation --family wall_mmpp --months 8

local measure = require("measure")

function bench(params)
    local argv = { "envelope-evaluation", params.family }
    if params.months then argv[#argv + 1] = params.months end
    measure.run {
        argv = argv,
        required = { "family", "months", "grid_cells", "elapsed_s", "per_month_s" },
    }
end
