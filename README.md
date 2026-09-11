# herdr-claude-memories

A [herdr](https://github.com/herdrdev/herdr) plugin that makes Claude Code's
auto-memory visible and auditable while you run several sessions at once.

Claude Code saves memories silently, and `/memory` only ever shows the project
you are standing in. So two things are hard: knowing that a memory was just
written in one of the other four panes, and seeing the corpus as a whole.

This plugin does both:

- **A toast when a memory is saved.** herdr suppresses notifications for the
  tab you are looking at, so you are only told about writes in sessions you are
  *not* watching — which is exactly the ones you would otherwise miss.
- **A read-only doctor panel**, machine-wide, that opens on what is structurally
  wrong across every project's memory store: broken `MEMORY.md` index lines,
  stores approaching the 200-line / 25 KB cap past which content is silently
  dropped at session start, stale entries, and stores whose project can no
  longer be resolved.
- **Dreams**, modelled on the [Anthropic Dreams
  API](https://platform.claude.com/docs/en/managed-agents/dreams) contract but
  run locally: a Claude session reads your store and new transcripts and writes
  a *shadow* store, leaving the original untouched. You review the proposal
  change by change, and a real Claude session applies the ones you confirm.

**The plugin never writes to a memory store.** It reads, indexes, diffs and
renders; where memories must change, a Claude session you are talking to does
the writing. Memory stores are keyed to the git repository and shared by every
concurrent session and worktree with no locking, so staying out of the write
path is the design, not an omission.

## Status

Early. `docs/DESIGN.md` is settled; the code is landing behind it. Each open
issue is one user story with its implementation RFC.

| | |
|---|---|
| Toast on memory write | landed |
| Hook installation (`reconcile`) | landed |
| Store resolution | landed |
| Corpus pane | landed |
| Structural checks | [#13](https://github.com/StGerman/herdr-claude-memories/issues/13) |
| Dreams | [#5](https://github.com/StGerman/herdr-claude-memories/issues/5) |
| Review and adoption | [#6](https://github.com/StGerman/herdr-claude-memories/issues/6) |

## Install

```bash
herdr plugin install StGerman/herdr-claude-memories
```

Or, for local development:

```bash
git clone https://github.com/StGerman/herdr-claude-memories
herdr plugin link ./herdr-claude-memories
```

First server start runs `reconcile`, which appends a `PostToolUse` hook to
`~/.claude/settings.json`. It matches existing entries by exact command string
and only ever appends, refuses to touch a file it could not parse, and writes
via temp + rename — `settings.json` is shared with other tools.

## The corpus pane

Open it from the herdr action list, or bind a key by adding this to your
`config.toml`:

```toml
[[keys.command]]
key = "prefix+m"
type = "plugin_action"
command = "stgerman.claude-memories.doctor"
description = "claude memories"
```

It opens as an overlay, so closing it restores whatever you were looking at.
One row per repository — where its store is, how many memories it holds, and
how close its `MEMORY.md` is to the 200-line / 25 KB cap past which content is
silently dropped at session start. Stores belonging to no repository, and
stores whose project can no longer be resolved, are listed too and marked as
such.

`r` rescans, `q` or `Esc` closes. `d` and `enter` are named in the footer for
dreams and review; they arrive with
[#5](https://github.com/StGerman/herdr-claude-memories/issues/5) and
[#6](https://github.com/StGerman/herdr-claude-memories/issues/6).

## Commands

```bash
cargo test
cargo build --release
sh scripts/fetch-binary.sh              # what [[build]] runs
herdr plugin link .
herdr plugin unlink stgerman.claude-memories
./bin/herdr-claude-memories reconcile   # apply to a running server without restarting it
./bin/herdr-claude-memories resolve     # which repository each memory store belongs to
./bin/herdr-claude-memories panel       # the pane; prints the index when not a terminal
```

## Requirements

herdr 0.9.0 or newer, macOS or Linux.

## License

MIT
