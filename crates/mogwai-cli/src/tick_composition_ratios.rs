// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai tick-composition-ratios`: the BBO budget sizing policy.
//!
//! ITS OWN SUBCOMMAND, not a `--report` mode on `tick-composition`. That
//! command MEASURES a tape; this one turns a measurement into proposed
//! constants and refuses landings that fail its acceptance gates. Fusing them
//! would let one invocation measure a fixture and bless it in the same breath,
//! which is how a sizing policy stops being evidence about anything.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::tick_composition_ratios as tcr;
use serde_json::Value;

#[derive(Args)]
pub(crate) struct RatiosArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare a version pair and propose the four sized constants.
    Compare {
        /// Which comparison contract to run.
        #[arg(long, default_value = "projection")]
        mode: String,
        /// Override the mode's committed before-fixture.
        #[arg(long, value_name = "FILE")]
        before: Option<PathBuf>,
        /// Override the mode's committed after-fixture.
        #[arg(long, value_name = "FILE")]
        after: Option<PathBuf>,
    },
    /// Assert the remeasured protocol-9 fixture equals protocol 8 outside the
    /// three separately validated identity fields.
    Verify89Identity,
    /// List the modes, their version pairs and their frozen baselines.
    Modes,
}

fn read(path: &PathBuf) -> anyhow::Result<Value> {
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub(crate) fn run(args: &RatiosArgs) -> anyhow::Result<()> {
    match &args.command {
        Command::Modes => {
            for mode in &tcr::MODES {
                let (lo, hi) = mode.versions;
                println!("{:<20} protocol {lo} to {hi}", mode.name);
                println!("  before   {}", mode.before);
                println!("  after    {}", mode.after);
                println!(
                    "  pairing  {}",
                    if mode.same_pairing {
                        "SAME (one traversal)"
                    } else {
                        "DIFFERENT (two traversals)"
                    }
                );
                println!(
                    "  baseline checkpoint_k        {}",
                    mode.baseline.checkpoint_k
                );
                println!(
                    "           sweep_drain_budget  {}",
                    mode.baseline.sweep_drain_budget
                );
                println!(
                    "           max_extend_ticks    {}",
                    mode.baseline.max_extend_ticks
                );
                println!(
                    "           warmup_baseline     {}",
                    mode.baseline.warmup_baseline
                );
                println!(
                    "           fanout_depth        {}",
                    mode.baseline.fanout_depth
                );
            }
            println!();
            println!("rejected proposals, carried forward as data:");
            for (mode, field, value) in tcr::REJECTED_PROPOSALS {
                println!("  {mode}: {field} {value} was proposed and REFUSED");
            }
        }
        Command::Verify89Identity => {
            let root = PathBuf::from(".");
            tcr::verify_8_9_identity(
                &root.join("analysis/tick-composition-protocol-8.json"),
                &root.join("analysis/tick-composition-protocol-9.json"),
            )?;
            println!(
                "8/9 identity: PASS - every field equal outside {} (each separately validated)",
                tcr::IDENTITY_SEPARATELY_VALIDATED.join(", ")
            );
        }
        Command::Compare {
            mode,
            before,
            after,
        } => {
            let mode = tcr::mode(mode)?;
            let before_path = before.clone().unwrap_or_else(|| PathBuf::from(mode.before));
            let after_path = after.clone().unwrap_or_else(|| PathBuf::from(mode.after));
            let before = read(&before_path)?;
            let after = read(&after_path)?;

            // The calendar classification is DERIVED from the presets the
            // fixture actually carries, not from a hardcoded tuple: a new
            // instrument appears here and gets classified, where a literal list
            // would have left it unchecked while still passing.
            let names = tcr::PresetCalendars::presets_in(&after)?;
            let presets = tcr::PresetCalendars::derive(&names)?;

            let result = tcr::compare(mode, &before, &after, &presets)?;
            // The PROPOSALS render as integers, matching the Python, because
            // they are integers by construction - `power_of_two` and `million`
            // both produce whole numbers, and `max_extend_ticks` is carried
            // through unchanged. Printing `67108864.0` for a value somebody
            // pastes into a source constant is a small lie about what it is.
            // The ratios and horizons stay fractional, because they are.
            let integral = |values: &std::collections::BTreeMap<String, f64>| -> Value {
                let mut out = serde_json::Map::new();
                for (key, value) in values {
                    let rendered = if value.fract() == 0.0 && value.abs() < 9e15 {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "checked integral and in range immediately above"
                        )]
                        Value::from(*value as i64)
                    } else {
                        Value::from(*value)
                    };
                    out.insert(key.clone(), rendered);
                }
                Value::Object(out)
            };
            let json = serde_json::json!({
                "ratios": result.ratios,
                "observed": result.observed,
                "required_reach": result.required_reach,
                "proposed": integral(&result.proposed),
                "horizons": result.horizons,
            });
            println!("{}", serde_json::to_string_pretty(&json)?);

            for (rejected_mode, field, value) in tcr::REJECTED_PROPOSALS {
                if rejected_mode == mode.name
                    && result.proposed.get(field).is_some_and(|v| *v == value)
                {
                    eprintln!(
                        "NOTE: {field} {value} for mode {rejected_mode} was proposed before and \
                         REFUSED. Re-proposing it does not reopen that ruling."
                    );
                }
            }
        }
    }
    Ok(())
}
