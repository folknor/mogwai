// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The pieces of the section-10 artifact that are pure functions of phase
//! 2b's inference: the observed monthly aggregates, each seed's `blocks` and
//! `count_substitution` record, the 8-seed `central` block, the `bootstrap`
//! and `ladder` sections and the `diagnostics.worsening_23` object.
//!
//! Phase 2c owns the rest of the assembly - `binding`, `constants`, `cost`,
//! `diagnostics.refused_cells`, `diagnostics.empty_bins` and the
//! schema/semantic validators. Nothing here re-derives a statistic 2c would
//! have to re-derive: [`Measurement`] hands 2c the refusal records and the
//! per-seed records it needs already computed.

use serde_json::Value;

use super::context::ObsContext;
use super::countsub::{count_substitution, obs_shares_under, support_refusals_of};
use super::family::stat_cond_sqrtn;
use super::ladder::{LadderOutcome, evaluate_ladder};
use super::monthly::{
    aggregate_permutations, blocks_from_sessions, central_blocks_from_seeds, pool_session_hists,
};
use super::{RefusalRec, ja, jnum};
use crate::error::LabResult;

/// One generated seed's cached walk record, as it sits on disk.
pub struct SeedRecord {
    pub seed: i64,
    pub per_session: Vec<Value>,
    pub forensic: Value,
}

/// The evaluated measurement: everything phase 2b computes from the cached
/// per-session records.
pub struct Measurement {
    pub observed_monthly: Value,
    pub observed_permutations_monthly: Value,
    /// Per seed, in the input order: `(seed, blocks, count_substitution)`.
    pub per_seed: Vec<(i64, Value, Value)>,
    pub central: Value,
    pub ladder: LadderOutcome,
    pub replicates: usize,
}

impl Measurement {
    /// The `observed.monthly` / `observed.permutations_monthly` pair.
    #[must_use]
    pub fn observed_json(&self) -> Value {
        serde_json::json!({
            "monthly": self.observed_monthly,
            "permutations_monthly": self.observed_permutations_monthly,
        })
    }

    /// The `generated.per_seed` array minus the `forensic` and `cost` fields
    /// (2c pastes those straight from the cached records).
    #[must_use]
    pub fn per_seed_json(&self) -> Value {
        Value::Array(
            self.per_seed
                .iter()
                .map(|(seed, blocks, csub)| {
                    serde_json::json!({
                        "seed": seed,
                        "blocks": blocks,
                        "count_substitution": csub,
                    })
                })
                .collect(),
        )
    }

    /// The `generated.central` object.
    #[must_use]
    pub fn central_json(&self) -> Value {
        let mut union: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
        for (_, _, csub) in &self.per_seed {
            for h in ja(csub, "refused_hours") {
                union.insert(h.as_i64().expect("an hour"));
            }
        }
        serde_json::json!({
            "blocks": self.central,
            "count_substitution": {
                "closure_p999_median": jnum(self.ladder.count_substitution.closure_median),
                "refused_hour_union": union.into_iter().collect::<Vec<_>>(),
            },
            "pooled_diagnostic_hist": Value::Null,
        })
    }

    /// `diagnostics.worsening_23` - null when the reversion rung did not
    /// fire, or fired and could not measure it (Amendment E).
    #[must_use]
    pub fn worsening_23_json(&self) -> Value {
        self.ladder
            .worsening_23
            .as_ref()
            .map_or(Value::Null, super::ladder::Worsening23::to_json)
    }

    /// Every refusal record 2c must collect, in the frozen order: the family
    /// envelopes, then the rungs, then each seed's count-substitution
    /// support refusals. The per-session and forensic mirrors are 2c's, and
    /// deduplication is 2c's too.
    #[must_use]
    pub fn refusals(&self) -> Vec<RefusalRec> {
        let mut out = Vec::new();
        for (_, env) in &self.ladder.envelopes {
            out.extend(env.refusals.iter().cloned());
        }
        for rung in &self.ladder.rungs {
            out.extend(rung.refusals.iter().cloned());
        }
        for (_, _, csub) in &self.per_seed {
            out.extend(support_refusals_of(csub));
        }
        out
    }
}

/// Evaluate the whole of phase 2b from the cached records.
pub fn measure(
    observed_per_session: &[Value],
    seeds: &[SeedRecord],
    mults: &[Vec<i64>],
) -> LabResult<Measurement> {
    let obs_ctx = ObsContext::new(observed_per_session.to_vec());
    let gen_ctxs: Vec<ObsContext> = seeds
        .iter()
        .map(|s| ObsContext::new(s.per_session.clone()))
        .collect();
    let gen_hists = seeds
        .iter()
        .map(|s| pool_session_hists(&s.per_session))
        .collect::<LabResult<Vec<_>>>()?;
    let forensics: Vec<&Value> = seeds.iter().map(|s| &s.forensic).collect();
    let ladder = evaluate_ladder(&obs_ctx, &gen_ctxs, &gen_hists, &forensics, mults);

    let observed_monthly = blocks_from_sessions(observed_per_session)?;
    let perms: Vec<&[Value]> = observed_per_session
        .iter()
        .map(|r| ja(r, "permutations"))
        .collect();
    let observed_permutations_monthly = aggregate_permutations(&perms);

    let obs_shares = obs_shares_under(&obs_ctx, &obs_ctx.ones());
    let mut per_seed = Vec::with_capacity(seeds.len());
    for (i, seed) in seeds.iter().enumerate() {
        let mut csub = count_substitution(&gen_hists[i], &obs_shares);
        let obj = csub.as_object_mut().expect("a count-substitution record");
        obj.insert(
            "closure_p999".into(),
            jnum(ladder.count_substitution.per_seed_closure[i]),
        );
        obj.insert(
            "closure_lcb".into(),
            jnum(ladder.count_substitution.closure_lcb),
        );
        obj.insert(
            "conditional_adequacy".into(),
            conditional_adequacy_records(&obs_ctx, &gen_ctxs[i], &ladder),
        );
        obj.insert(
            "diagnostic_closure_to_bound".into(),
            jnum(ladder.count_substitution.diagnostic_closure_to_bound[i]),
        );
        let blocks = blocks_from_sessions(&seed.per_session)?;
        per_seed.push((seed.seed, blocks, csub));
    }
    let block_refs: Vec<&Value> = per_seed.iter().map(|(_, b, _)| b).collect();
    let central = central_blocks_from_seeds(&block_refs)?;

    Ok(Measurement {
        observed_monthly,
        observed_permutations_monthly,
        per_seed,
        central,
        ladder,
        replicates: mults.len(),
    })
}

/// The rung-2c conditional-adequacy evidence, per seed. A bin that is not
/// both required and supported records its two flags and nothing else - the
/// envelope fields would be a fabricated measurement.
fn conditional_adequacy_records(
    obs: &ObsContext,
    seed: &ObsContext,
    ladder: &LadderOutcome,
) -> Value {
    let ones_obs = obs.ones();
    let arrival = ladder.envelope("arrival");
    Value::Array(
        ladder
            .cond_bins
            .iter()
            .map(|cb| {
                let mut rec = serde_json::json!({
                    "hour": cb.hour,
                    "bin_name": cb.bin_name,
                    "observed_p99": Value::Null,
                    "generated_p99": Value::Null,
                    "ratio": Value::Null,
                    "interval_low": Value::Null,
                    "interval_high": Value::Null,
                    "interval_inside_band": Value::Null,
                    "seed_inside_count": Value::Null,
                    "required": cb.required,
                    "supported": cb.supported,
                });
                if cb.required && cb.supported {
                    let stat = stat_cond_sqrtn(cb.hour, cb.bin_name.clone());
                    let obs_p99 = stat(obs, &ones_obs);
                    let gen_p99 = stat(seed, &seed.ones());
                    let name = format!("cond_sqrtn_p99_h{}_{}", cb.hour, cb.bin_name);
                    let m = arrival.metrics.iter().find(|m| m.name == name);
                    let obj = rec.as_object_mut().expect("a record");
                    obj.insert("observed_p99".into(), jnum(obs_p99));
                    obj.insert("generated_p99".into(), jnum(gen_p99));
                    // The Python `if obs_p99 and gen_p99` is a truthiness
                    // test: a zero on either side refuses the ratio.
                    let ratio = match (obs_p99, gen_p99) {
                        (Some(o), Some(g)) if o != 0.0 && g != 0.0 => Some(g / o),
                        _ => None,
                    };
                    obj.insert("ratio".into(), jnum(ratio));
                    obj.insert("interval_low".into(), jnum(m.and_then(|m| m.interval_low)));
                    obj.insert(
                        "interval_high".into(),
                        jnum(m.and_then(|m| m.interval_high)),
                    );
                    obj.insert(
                        "interval_inside_band".into(),
                        super::jbool(m.and_then(|m| m.interval_inside_band)),
                    );
                    obj.insert(
                        "seed_inside_count".into(),
                        super::jint(m.and_then(|m| m.seed_inside_count)),
                    );
                }
                rec
            })
            .collect(),
    )
}
