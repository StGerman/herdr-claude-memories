//! herdr-claude-memories — surface and curate Claude Code auto-memory in herdr.
//!
//! Three subcommands, one binary, no library crate. See `docs/DESIGN.md` for
//! why each of them behaves the way it does.
//!
//! * `reconcile` — install this plugin's hook into `~/.claude/settings.json`.
//!   Runs from `[[startup]]` on every herdr server start, so it must be
//!   idempotent and must never fail the server.
//! * `notify` — the `PostToolUse` hook body. Reads the hook payload on stdin
//!   and fires a herdr toast when a memory topic file is written.
//! * `panel` — the read-only doctor overlay.
//!
//! A hook that fails is a hook that interrupts the agent's turn, so every path
//! here exits `SUCCESS` unless the user asked for something that does not
//! exist.

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(command) = std::env::args().nth(1) else {
        usage();
        return ExitCode::FAILURE;
    };

    match command.as_str() {
        "reconcile" => unimplemented_yet("reconcile"),
        "notify" => unimplemented_yet("notify"),
        "panel" => unimplemented_yet("panel"),
        other => {
            eprintln!("herdr-claude-memories: unknown command '{other}'");
            usage();
            ExitCode::FAILURE
        }
    }
}

/// Exit quietly and successfully for a command that is scaffolded but not built.
///
/// `reconcile` runs from `[[startup]]` and `notify` runs inside the agent's
/// turn; neither may fail while the feature is still landing.
fn unimplemented_yet(command: &str) -> ExitCode {
    eprintln!("herdr-claude-memories: {command} is not implemented yet");
    ExitCode::SUCCESS
}

fn usage() {
    eprintln!("usage: herdr-claude-memories <reconcile|notify|panel>");
}
