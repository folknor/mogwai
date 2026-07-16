// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai man [TOPIC]` - the bundled reference docs.
//!
//! The durable `reference/*.md` docs are compiled into the binary with
//! `include_str!` and rendered to the terminal through the markdown->ANSI
//! renderer in `render`. With no topic, list what is available; with a topic,
//! render it (colour auto-disabled when stdout is not a TTY or `NO_COLOR` is
//! set). The topic is a clap `ValueEnum`, so an unknown topic is rejected by the
//! argument parser before `run` is reached.

mod render;

use std::io::{IsTerminal, Write};

use clap::ValueEnum;

/// The bundled reference topics. The user-facing reference docs only - the
/// process docs (`orchestrate`, `technical-implementation-spec`) are
/// deliberately not bundled. Each variant must have an arm in `content`.
#[derive(Clone, Copy, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum ManTopic {
    /// The mogwai command line (reference/cli.md).
    Cli,
    /// The mogwai.toml run knobs (reference/config.md).
    Config,
    /// How the system works, subsystem by subsystem (reference/architecture.md).
    Architecture,
    /// The havoc model: divergences and the four surfaces (reference/havoc.md).
    Havoc,
    /// The simulated clock for accelerated forward testing (reference/clock.md).
    Clock,
}

/// The kebab-case topic name as typed on the command line.
fn name(topic: ManTopic) -> &'static str {
    match topic {
        ManTopic::Cli => "cli",
        ManTopic::Config => "config",
        ManTopic::Architecture => "architecture",
        ManTopic::Havoc => "havoc",
        ManTopic::Clock => "clock",
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
    }
}

/// The bundled markdown for a topic, compiled into the binary. Each topic maps
/// to one `reference/*.md`.
fn content(topic: ManTopic) -> &'static str {
    match topic {
        ManTopic::Cli => include_str!("../../../reference/cli.md"),
        ManTopic::Config => include_str!("../../../reference/config.md"),
        ManTopic::Architecture => include_str!("../../../reference/architecture.md"),
        ManTopic::Havoc => include_str!("../../../reference/havoc.md"),
        ManTopic::Clock => include_str!("../../../reference/clock.md"),
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
    let mut out = String::from("Bundled reference docs. Run `mogwai man <topic>` to read one.\n\n");
    for topic in topics {
        out.push_str(&format!(
            "  {name:width$}  {summary}\n",
            name = name(*topic),
            summary = summary(*topic)
        ));
    }
    out
}
