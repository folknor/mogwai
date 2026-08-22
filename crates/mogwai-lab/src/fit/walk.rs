// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The protocol-11 walk layer: one `gen --type summary` evaluation, cached.
//!
//! `mnq_fit.py` shelled out to `gen --type summary` once per walk
//! and cached the result under `analysis/out/mnq-fit-scratch/cache`, keyed by
//! `sha256(json.dumps({overrides, seed, start_ns, length, warmup, commit},
//! sort_keys=True))`. This port runs the walk IN-PROCESS through
//! `crate::summary::summarize` and honours BOTH cache layouts:
//!
//! - the phase-1 storage policy, keyed by the binary's own provenance token,
//!   for anything this port writes;
//! - the PYTHON-ERA layout, read-only, so the parity gate can replay the
//!   protocol-11 run's 10,192 cached walks instead of spending hours
//!   re-walking them. Re-deriving the Python's key requires reproducing
//!   `json.dumps(..., sort_keys=True)` byte-for-byte, floats included, which
//!   is why `PyJson` below renders through `kernel::py_float_repr`.
//!
//! The hashed object's `warmup` member is therefore INHERITED AND FROZEN: it
//! is a field of a historical serialization, and spelling it `burn_in` would
//! miss all 10,192 entries. The Rust identifiers around it carry the burn-in
//! vocabulary; the hashed bytes do not.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{LabError, LabResult};
use crate::kernel::py_float_repr;

/// One override entry of the scratch config: the Python wrote a TOML
/// `[instrument.override]` table whose values are strings, numbers or float
/// arrays, and hashed the SAME values through `json.dumps`.
#[derive(Clone, Debug, PartialEq)]
pub enum OverrideValue {
    Str(String),
    Float(f64),
    Floats(Vec<f64>),
}

/// An override set, sorted by path exactly as the Python's
/// `sorted(overrides.items())` and `sort_keys=True` both do.
pub type Overrides = BTreeMap<String, OverrideValue>;

fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn dump_override(v: &OverrideValue, out: &mut String) {
    match v {
        OverrideValue::Str(s) => escape(s, out),
        OverrideValue::Float(x) => out.push_str(&py_float_repr(*x)),
        OverrideValue::Floats(xs) => {
            out.push('[');
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&py_float_repr(*x));
            }
            out.push(']');
        }
    }
}

/// The walk cache key: `sha256` over
/// `json.dumps({"commit", "length", "overrides", "seed", "start_ns",
/// "warmup"}, sort_keys=True)`. The key order below IS the sorted order.
///
/// The `warmup` member is the Python-era spelling and is frozen, because the
/// same key resolves both cache layouts and the read-only one holds 10,192
/// entries hashed under it. The parameter is the burn-in prefix; only the
/// serialized field keeps the inherited name.
#[must_use]
pub fn walk_cache_key(
    overrides: &Overrides,
    seed: i64,
    start_ns: i64,
    length: &str,
    burn_in: &str,
    commit: &str,
) -> String {
    let mut s = String::from("{");
    s.push_str("\"commit\": ");
    escape(commit, &mut s);
    s.push_str(", \"length\": ");
    escape(length, &mut s);
    s.push_str(", \"overrides\": {");
    for (i, (k, v)) in overrides.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        escape(k, &mut s);
        s.push_str(": ");
        dump_override(v, &mut s);
    }
    s.push_str("}, \"seed\": ");
    s.push_str(&seed.to_string());
    s.push_str(", \"start_ns\": ");
    s.push_str(&start_ns.to_string());
    // Inherited spelling, frozen: see this function's doc.
    s.push_str(", \"warmup\": ");
    escape(burn_in, &mut s);
    s.push('}');
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    crate::delivery::hex_digest(&h.finalize())
}

/// `scratch_config_text`: the config the Python handed `gen --config`, and
/// the config this port hands `Config::load`. Floats render through Python's
/// `repr`, which is what the original wrote.
#[must_use]
pub fn scratch_config_text(overrides: &Overrides) -> String {
    let mut lines = vec!["[instrument]".to_string(), "preset = \"MNQ\"".to_string()];
    if !overrides.is_empty() {
        lines.push("[instrument.override]".to_string());
        for (path, value) in overrides {
            match value {
                OverrideValue::Str(s) => lines.push(format!("\"{path}\" = \"{s}\"")),
                OverrideValue::Floats(xs) => {
                    let body: Vec<String> = xs.iter().map(|v| py_float_repr(*v)).collect();
                    lines.push(format!("\"{path}\" = [{}]", body.join(", ")));
                }
                OverrideValue::Float(x) => {
                    lines.push(format!("\"{path}\" = {}", py_float_repr(*x)));
                }
            }
        }
    }
    // An EMPTY balances table: the offline walk builds a profile and never
    // books a trade, so it has nothing to fund. Without it the boot funding
    // sweep would refuse this MNQ scratch config against the default USDT-only
    // balances, over a currency no order here could ever quote.
    lines.push("[balances]".to_string());
    lines.join("\n") + "\n"
}

/// Where a walk result came from. The parity gate reports these.
#[derive(Default, Debug, Clone, Copy)]
pub struct CacheStats {
    pub python_hits: u64,
    pub native_hits: u64,
    pub misses: u64,
}

/// The walk store: a read-only Python-era directory plus the phase-1
/// provenance-keyed store this port writes into.
pub struct WalkCache {
    python_dir: Option<PathBuf>,
    python_commit: String,
    native: Option<crate::storage::CacheStore>,
    pub stats: CacheStats,
}

impl WalkCache {
    /// `python_dir` is the Python's `mnq-fit-scratch` directory (its `cache`
    /// subdirectory is where the entries live); `python_commit` is the
    /// harness commit those entries were keyed under.
    #[must_use]
    pub fn new(
        python_dir: Option<PathBuf>,
        python_commit: String,
        native: Option<crate::storage::CacheStore>,
    ) -> Self {
        Self {
            python_dir,
            python_commit,
            native,
            stats: CacheStats::default(),
        }
    }

    fn python_path(&self, key: &str) -> Option<PathBuf> {
        self.python_dir
            .as_ref()
            .map(|d| d.join("cache").join(format!("{key}.json")))
    }

    /// Look a walk up in both layouts, Python first (it is the one the
    /// parity gate must replay from).
    pub fn get(
        &mut self,
        overrides: &Overrides,
        seed: i64,
        start_ns: i64,
        length: &str,
        burn_in: &str,
    ) -> Option<Value> {
        let key = walk_cache_key(
            overrides,
            seed,
            start_ns,
            length,
            burn_in,
            &self.python_commit,
        );
        if let Some(path) = self.python_path(&key)
            && let Ok(bytes) = std::fs::read(&path)
            && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
        {
            self.stats.python_hits += 1;
            return Some(v);
        }
        if let Some(store) = &self.native
            && let Ok(Some(bytes)) = store.read(&format!("walk-{key}"))
            && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
        {
            self.stats.native_hits += 1;
            return Some(v);
        }
        self.stats.misses += 1;
        None
    }

    /// Record a freshly walked summary in the native store only: the
    /// Python-era directory is a committed-run artifact and is never
    /// written.
    pub fn put(
        &self,
        overrides: &Overrides,
        seed: i64,
        start_ns: i64,
        length: &str,
        burn_in: &str,
        summary: &Value,
    ) -> LabResult<()> {
        let Some(store) = &self.native else {
            return Ok(());
        };
        let key = walk_cache_key(
            overrides,
            seed,
            start_ns,
            length,
            burn_in,
            &self.python_commit,
        );
        store
            .write(&format!("walk-{key}"), &serde_json::to_vec(summary)?)
            .map_err(LabError::from)
    }
}

/// `--length`/`--burn-in` grammar: `<n><unit>`, unit one of s m h d w mo y.
/// A faithful reading of `mogwai gen`'s own parser, which the Python drove
/// through the CLI.
pub fn parse_duration(s: &str) -> LabResult<i64> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    let unit = &s[digits.len()..];
    let n: i64 = digits
        .parse()
        .map_err(|_| LabError::refusal(format!("malformed duration {s}")))?;
    let mult: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        "mo" => 30 * 86_400,
        "y" => 365 * 86_400,
        _ => return Err(LabError::refusal(format!("unknown duration unit {unit:?}"))),
    };
    Ok(n * mult * 1_000_000_000)
}

/// One in-process `gen --type summary` walk: resolve the scratch profile
/// through the venue's own `Config::load` (the SAME path the Python's
/// `--config` walks took), build the generator at `start - burn-in`, and fold
/// the tick stream through `summary::summarize`.
pub fn run_summary_walk(
    scratch_dir: &Path,
    overrides: &Overrides,
    seed: i64,
    start_ns: i64,
    length: &str,
    burn_in: &str,
) -> LabResult<Value> {
    let len_ns = parse_duration(length)?;
    let burn_in_ns = parse_duration(burn_in)?;
    let end = start_ns + len_ns;
    let walk_start = start_ns - burn_in_ns;
    if walk_start < 0 {
        return Err(LabError::refusal(
            "the burn-in underflows the start; the walk must begin at exactly start - burn-in",
        ));
    }
    std::fs::create_dir_all(scratch_dir)?;
    let config_path = scratch_dir.join(format!(
        "candidate-{}.toml",
        &walk_cache_key(overrides, seed, start_ns, length, burn_in, "in-process")[..16]
    ));
    std::fs::write(&config_path, scratch_config_text(overrides))?;
    let profile = profile_from_config(&config_path)?;
    drop(std::fs::remove_file(&config_path));

    let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
        profile.scalars.clone(),
        seed as u64,
        walk_start as u64,
        mogwai_venue::source::fingerprint(),
        &profile.session,
        None,
        mogwai_data::SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
    .map_err(|e| LabError::refusal(format!("building the generator: {e:?}")))?;
    let acc = crate::summary::summarize(
        &mut source,
        &profile,
        seed as u64,
        start_ns as u64,
        end as u64,
    );
    Ok(serde_json::to_value(&acc)?)
}

/// One instrument profile from a scratch config, through the SAME
/// `Config::load` and profile construction a served run boots with.
pub fn profile_from_config(path: &Path) -> LabResult<mogwai_venue::source::InstrumentProfile> {
    let cfg = mogwai_venue::config::Config::load(Some(path.to_path_buf()))
        .map_err(|e| LabError::refusal(format!("loading scratch config: {e}")))?;
    if cfg.boot_symbol_carries_no_knobs() {
        return Err(LabError::refusal(
            "the scratch config carries no [instrument] or matching [symbols.*] knobs for its \
             boot symbol; it would ignore every scratch scalar",
        ));
    }
    let profiles = mogwai_venue::config::build_instrument_profiles(&cfg)
        .map_err(|e| LabError::refusal(format!("building instrument profiles: {e}")))?;
    let def = profiles
        .boot_symbol_def(cfg.boot_symbol())
        .map_err(|e| LabError::refusal(format!("resolving boot shape: {e}")))?;
    Ok((*profiles
        .configured(&def.symbol)
        .expect("just listed this symbol"))
    .clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch config that reaches a second shape used to bail with "expected
    /// exactly one"; profiles are plural now and the boot shape resolves by
    /// name. The absent top-level `symbol` is the point: `scratch_config_text`
    /// writes none, so the boot shape is the one `[instrument] preset` names.
    #[test]
    fn a_scratch_config_with_a_second_symbol_table_still_resolves_its_boot_shape() {
        let dir = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/walk-scratch-configs"
        ));
        std::fs::create_dir_all(&dir).expect("creating the scratch dir");
        let path = dir.join("two-symbol-scratch.toml");
        let mut text = scratch_config_text(&Overrides::new());
        text.push_str("\n[symbols.ES]\npreset = \"MNQ\"\n");
        std::fs::write(&path, text).expect("writing the scratch config");
        let profile = profile_from_config(&path).expect("boot shape resolves by name");
        assert_eq!(profile.def.symbol.as_ref(), "MNQ");
    }

    /// The key derivation, pinned on a small hand-checkable set, inherited
    /// `warmup` member included. The live proof is the parity gate, which
    /// resolves every one of the protocol-11 run's cached walks by this key.
    #[test]
    fn the_walk_cache_key_renders_sorted_keys_and_repr_floats() {
        let mut ov: Overrides = Overrides::new();
        ov.insert("generator.vol_scalar".into(), OverrideValue::Float(1e-8));
        ov.insert(
            "session.vol_hour".into(),
            OverrideValue::Floats(vec![1.0, 0.5]),
        );
        // Reproduce the exact JSON the hash is taken over, so a future edit
        // to the renderer breaks HERE rather than silently in the gate.
        let mut s = String::from("{");
        s.push_str("\"commit\": \"abc\", \"length\": \"7d\", \"overrides\": ");
        s.push_str("{\"generator.vol_scalar\": 1e-08, \"session.vol_hour\": [1.0, 0.5]}");
        s.push_str(", \"seed\": 1, \"start_ns\": 5, \"warmup\": \"3d\"}");
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        assert_eq!(
            walk_cache_key(&ov, 1, 5, "7d", "3d", "abc"),
            crate::delivery::hex_digest(&h.finalize())
        );
    }

    #[test]
    fn the_scratch_config_declares_the_preset_and_sorted_overrides() {
        let mut ov: Overrides = Overrides::new();
        ov.insert("z.b".into(), OverrideValue::Float(2.0));
        ov.insert("a.b".into(), OverrideValue::Floats(vec![1.0]));
        let text = scratch_config_text(&ov);
        assert!(text.starts_with("[instrument]\npreset = \"MNQ\"\n[instrument.override]\n"));
        assert!(text.contains("\"a.b\" = [1.0]\n\"z.b\" = 2.0"));
    }

    #[test]
    fn a_scratch_config_whose_only_knobs_are_for_another_symbol_is_refused() {
        let path = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/walk-wrong-symbol.toml"
        ));
        std::fs::write(&path, "[symbols.MNQ]\nprice_increment = \"0.25\"\n").unwrap();
        let error = profile_from_config(&path).unwrap_err().to_string();
        assert!(error.contains("boot symbol"), "{error}");
    }

    #[test]
    fn duration_units_match_the_gen_grammar() {
        assert_eq!(parse_duration("7d").unwrap(), 7 * 86_400 * 1_000_000_000);
        assert_eq!(
            parse_duration("2674800s").unwrap(),
            2_674_800 * 1_000_000_000
        );
        assert!(parse_duration("7q").is_err());
    }
}
