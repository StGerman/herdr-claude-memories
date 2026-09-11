# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

A herdr plugin that surfaces and curates Claude Code's auto-memory. Read
`docs/DESIGN.md` first — it records what was decided and, more usefully, the
constraints that forced each decision. Do not re-litigate a decision without
reading the constraint underneath it.

It is a plugin rather than a herdr feature on purpose: the author is an
external contributor on `herdrdev/herdr` and cannot land code in core. Every
capability must be reachable through herdr's public extension points —
`herdr-plugin.toml` (`[[build]]`, `[[startup]]`, `[[actions]]`, `[[events]]`,
`[[panes]]`, `[[link_handlers]]`) and the herdr CLI. Do not plan changes that
require patching herdr.

## Commands

```bash
cargo test
cargo build --release
cargo fmt
herdr plugin link .
./bin/herdr-claude-memories reconcile   # apply without restarting a live server
herdr plugin log list --plugin stgerman.claude-memories
```

## Architecture

Four subcommands in a single binary (`src/main.rs`), no library crate.
`src/resolution.rs` is the shared index the panel and the dream both read.

```
Write/Edit ──▶ PostToolUse hook ──▶ `notify` ──▶ herdr notification show
                                       │reads frontmatter of the written file
                                       │
herdr server start ──▶ [[startup]] ──▶ `reconcile` ──▶ settings.json
                                       │
keybinding / action ──▶ [[panes]] ───▶ `panel` ──▶ scans every memory store
                                                     │launches
                                              `claude` split pane (review)

verification / debugging ─────────────▶ `resolve` ──▶ prints the same index
                                       (not in the manifest)
```

**Hooks are triggers, never carriers.** A hook says *when* to look; every fact
comes from disk. That keeps `notify` stateless with respect to memory content,
so a missed, duplicated or out-of-order hook cannot desynchronise anything.

## Constraints that are easy to break

- **Never write to a memory store.** Not from `notify`, not from `panel`, not
  from `reconcile`. Stores are repo-keyed and shared by every concurrent
  session and worktree with no locking. Where memories must change, the
  launched Claude session writes them after confirming each change with the
  user. This is the load-bearing decision in the whole design.
- **Skip `MEMORY.md` in `notify`.** One logical memory save is two writes — the
  topic file and the index. Reacting to both doubles every notification.
- **Never reverse a project slug with string surgery.** Dashes in directory
  names are indistinguishable from path separators, so
  `-Users-x-Code-herdr-claude-tasks` has several readings and only one is real.
  Resolve through a transcript's `cwd` and `git rev-parse --show-toplevel`.
- **A `cwd` that no longer exists resolves to itself.** No ancestor is probed
  for a repository to adopt it, so a deleted worktree forms its own group.
  Producing a wrong-but-plausible parent is the failure this whole module
  exists to avoid.
- **The resolution cache is a memo, never a record.** An entry is valid only
  while the transcript that produced it survives unchanged; once transcripts
  are swept, the store is unresolvable and the cache must say so too.
- **A store's transcripts are not one directory's `*.jsonl`.** Worktrees get
  their own project directory for transcripts while memories stay in the parent
  repo's store. Group project directories by resolved repository root.
- **`reconcile` runs on every server start.** It must stay idempotent; verify
  with two consecutive runs producing a byte-identical `settings.json`. It
  matches hooks by exact command string and only appends — it must never
  rewrite, reorder or remove an entry it did not create, because that file is
  shared with other tools and with the sibling `stgerman.claude-tasks` plugin.
- **`notify` must be an invisible no-op outside herdr,** and must never fail.
  It runs inside the agent's turn; a non-zero exit interrupts the user.
- **Toasts are spawned detached and never awaited.** Blocking adds latency to
  the agent's turn.
- **Honour the suppression lock.** Adoption writes confirmed memories to the
  real store, which is exactly what `notify` watches. Without the lock every
  review ends in a burst of notifications.

## Testing changes that touch settings.json

`reconcile` edits the real `~/.claude/settings.json`. Never exercise that path
against the live file. Point `CLAUDE_CONFIG_DIR` at a copy, run `reconcile`,
then diff hook command strings before and after to prove pre-existing entries
are untouched.

## Release

Tagging `v*` runs `.github/workflows/release.yml`, which builds macOS
aarch64/x86_64 and Linux x86_64. Asset names must stay in sync with the
`${os}-${arch}` string `scripts/fetch-binary.sh` constructs, or installs
silently fall back to building from source. `min_herdr_version` in
`herdr-plugin.toml` gates against older herdr servers.

## Commit style

Lowercase conventional commits. Reference the issue a change implements with a
`refs #<n>` line in the body.
