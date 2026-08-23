// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai man [TOPIC]` - the bundled reference docs.
//!
//! The durable `docs/*.md` and `reference/*.md` documents are compiled into
//! the binary with
//! `include_str!` and rendered to the terminal through the markdown->ANSI
//! renderer in `render`. With no topic, list what is available; with a topic,
//! render it (colour auto-disabled when stdout is not a TTY or `NO_COLOR` is
//! set). The topic is a clap `ValueEnum`, so an unknown topic is rejected by the
//! argument parser before `run` is reached.
//!
//! This command was deleted once, in `fd3baa1`, on the argument that mogwai's
//! operator is an agent and an agent reads `reference/*.md` off disk. That
//! argument does not survive `cargo install`: an installed `mogwai` has no
//! source tree beside it, so without the bundled copies its documentation is
//! simply absent wherever the binary actually runs. Restored 2026-08-03. Weigh
//! that before removing it a second time.

mod render;

use std::io::{IsTerminal, Write};

use clap::ValueEnum;

/// The bundled topics: everything in `docs/`, which is by definition how the
/// venue is used, plus the two `reference/` documents an operator rather than a
/// contributor needs. Deliberately not bundled: `technical-implementation-spec`
/// and `performance`, which serve someone changing this repo, and `glossary`,
/// which is a working aid for the same audience. Each variant must have an arm
/// in `content`.
#[derive(Clone, Copy, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ManTopic {
    /// The mogwai command line (docs/cli.md).
    Cli,
    /// The mogwai.toml run knobs (docs/config.md).
    Config,
    /// How the system works, subsystem by subsystem (reference/architecture.md).
    Architecture,
    /// The havoc model: divergences and the four surfaces (docs/havoc.md).
    Havoc,
    /// The simulated clock for accelerated forward testing (reference/clock.md).
    Clock,
    /// Choosing a built-in instrument preset (docs/presets.md).
    Presets,
    /// Netting versus hedging, and what each does on the wire (docs/oms-types.md).
    OmsTypes,
    /// Contingent order lists and how the venue executes them (docs/order-lists.md).
    OrderLists,
    /// The order a nautilus host must drive the shipped clients in (docs/adapter-lifecycle.md).
    AdapterLifecycle,
}

/// The kebab-case topic name as typed on the command line.
fn name(topic: ManTopic) -> &'static str {
    match topic {
        ManTopic::Cli => "cli",
        ManTopic::Config => "config",
        ManTopic::Architecture => "architecture",
        ManTopic::Havoc => "havoc",
        ManTopic::Clock => "clock",
        ManTopic::Presets => "presets",
        ManTopic::OmsTypes => "oms-types",
        ManTopic::OrderLists => "order-lists",
        ManTopic::AdapterLifecycle => "adapter-lifecycle",
    }
}

/// One-line summary shown in the topic listing.
fn summary(topic: ManTopic) -> &'static str {
    match topic {
        ManTopic::Cli => "the mogwai command line",
        ManTopic::Config => "the mogwai.toml run knobs",
        ManTopic::Architecture => "how the system works, subsystem by subsystem",
        ManTopic::Havoc => "the havoc model: every divergence and the four surfaces",
        ManTopic::Clock => "the simulated clock for accelerated forward testing",
        ManTopic::Presets => "choosing a built-in instrument preset, and its provenance",
        ManTopic::OmsTypes => "netting versus hedging, and what each does on the wire",
        ManTopic::OrderLists => "order lists: OCO, OTO and OUO linkage on this venue",
        ManTopic::AdapterLifecycle => "the order a nautilus host must call the shipped clients in",
    }
}

/// The bundled markdown for a topic, compiled into the binary. Each topic maps
/// to exactly one durable document: five under `docs/` and two under
/// `reference/`, per the split the `ManTopic` doc above states. It is NOT
/// `reference/*.md` throughout, which this comment claimed until 2026-08-20 -
/// most of what an operator needs is `docs/`, which is the folder that
/// documents how the venue is USED.
fn content(topic: ManTopic) -> &'static str {
    match topic {
        ManTopic::Cli => include_str!("../../../docs/cli.md"),
        ManTopic::Config => include_str!("../../../docs/config.md"),
        ManTopic::Architecture => include_str!("../../../reference/architecture.md"),
        ManTopic::Havoc => include_str!("../../../docs/havoc.md"),
        ManTopic::Clock => include_str!("../../../reference/clock.md"),
        ManTopic::Presets => include_str!("../../../docs/presets.md"),
        ManTopic::OmsTypes => include_str!("../../../docs/oms-types.md"),
        ManTopic::OrderLists => include_str!("../../../docs/order-lists.md"),
        ManTopic::AdapterLifecycle => include_str!("../../../docs/adapter-lifecycle.md"),
    }
}

/// Colour is on only when stdout is a real terminal and `NO_COLOR` is unset.
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal()
}

/// Render `topic` to the terminal, or list the available topics when `None`.
pub(crate) fn run(topic: Option<ManTopic>) {
    let out = match topic {
        Some(topic) => render::render(content(topic), no_color()),
        None => list_topics(),
    };
    write_stdout(&out);
}

/// Write to stdout, treating a closed downstream pipe (EPIPE, e.g. `| less`
/// quit early) as a clean exit; any other write error is reported but left
/// non-fatal for a one-shot doc print.
fn write_stdout(text: &str) {
    if let Err(err) = std::io::stdout().write_all(text.as_bytes())
        && err.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("mogwai man: write error: {err}");
    }
}

/// The bare-`mogwai man` listing: each topic name padded to a common width
/// followed by its one-line summary.
fn list_topics() -> String {
    let topics = ManTopic::value_variants();
    let width = topics
        .iter()
        .map(|topic| name(*topic).len())
        .max()
        .unwrap_or(0);
    let mut out = String::from("Bundled documentation. Run `mogwai man <topic>` to read one.\n\n");
    for topic in topics {
        out.push_str(&format!(
            "  {name:width$}  {summary}\n",
            name = name(*topic),
            summary = summary(*topic)
        ));
    }
    out
}
