//! `mogwai man [TOPIC]` - the bundled reference docs.
//!
//! The durable `reference/*.md` docs are compiled into the binary with
//! `include_str!` and rendered to the terminal through the markdown->ANSI
//! renderer in `render`. With no topic, list what is available; with a topic,
//! render it (colour auto-disabled when stdout is not a TTY or `NO_COLOR` is
//! set). Detected as the first CLI argument so the server stays clap-free,
//! alongside the hand-rolled `--config` / `--version` handling in `main`.

mod render;

use std::io::{IsTerminal, Write};

/// The reference topics, in listing order: the name as typed on the command
/// line plus the one-line summary shown by a bare `mogwai man`. Each name must
/// have an arm in `content`. The user-facing reference docs only - the process
/// docs (`orchestrate`, `technical-implementation-spec`) are deliberately out.
const TOPICS: &[(&str, &str)] = &[
    ("cli", "the mogwai command line"),
    ("config", "the mogwai.toml run knobs"),
    (
        "architecture",
        "how the system works, subsystem by subsystem",
    ),
    (
        "havoc",
        "the havoc model: every divergence and the four surfaces",
    ),
];

/// The bundled markdown for a topic, compiled into the binary, or `None` for an
/// unknown topic name. Each topic maps to one `reference/*.md`.
fn content(topic: &str) -> Option<&'static str> {
    match topic {
        "cli" => Some(include_str!("../../../reference/cli.md")),
        "config" => Some(include_str!("../../../reference/config.md")),
        "architecture" => Some(include_str!("../../../reference/architecture.md")),
        "havoc" => Some(include_str!("../../../reference/havoc.md")),
        _ => None,
    }
}

/// Colour is on only when stdout is a real terminal and `NO_COLOR` is unset.
fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal()
}

/// If the first CLI argument is `man`, render the requested topic (or list the
/// topics when none is given) and exit; otherwise return so normal server
/// startup proceeds. An unknown topic prints the available set to stderr and
/// exits non-zero.
pub(crate) fn run_if_requested() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("man") {
        return;
    }
    let code = match args.next() {
        None => {
            write_stdout(&list_topics());
            0
        }
        Some(topic) => match content(&topic) {
            Some(markdown) => {
                write_stdout(&render::render(markdown, no_color()));
                0
            }
            None => {
                eprintln!("mogwai man: unknown topic {topic:?}\n");
                eprint!("{}", list_topics());
                2
            }
        },
    };
    std::process::exit(code);
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
    let width = TOPICS.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut out = String::from("Bundled reference docs. Run `mogwai man <topic>` to read one.\n\n");
    for (name, summary) in TOPICS {
        out.push_str(&format!("  {name:width$}  {summary}\n"));
    }
    out
}
