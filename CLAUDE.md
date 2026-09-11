# CLAUDE.md

## What this is

A herdr plugin that surfaces and curates Claude Code's auto-memory.

Read `docs/DESIGN.md` before planning anything. It records what was decided
and, more usefully, the constraint that forced each decision — reach for it when
choosing where a capability belongs, when a decision looks wrong, or when an
issue and the code disagree. Read the constraint underneath a decision before
revisiting the decision.

It is a plugin rather than a herdr feature on purpose: the author is an
external contributor on `herdrdev/herdr` and cannot land code in core. Every
capability must be reachable through herdr's public extension points —
`herdr-plugin.toml` (`[[build]]`, `[[startup]]`, `[[actions]]`, `[[events]]`,
`[[panes]]`, `[[link_handlers]]`) and the herdr CLI. Every plan must land
entirely inside the plugin.

## Commands

```bash
herdr plugin link .
herdr plugin log list --plugin stgerman.claude-memories
./bin/herdr-claude-memories reconcile   # apply to a live server without restarting it
./bin/herdr-claude-memories resolve     # print the store/repository index
```

## Architecture

One binary (`src/main.rs`), no library crate. `src/resolution.rs` is the shared
index every other surface reads.

```
Write/Edit ──▶ PostToolUse hook ──▶ `notify` ──▶ coalesced herdr toast
                                       │reads frontmatter of the written file
                                       │
herdr server start ──▶ [[startup]] ──▶ `reconcile` ──▶ settings.json
                                       │
keybinding / action ──▶ [[panes]] ───▶ `panel` ──▶ every store on the machine
                                                     │launches
                                       `claude` split pane (dream, then review)

verification / debugging ─────────────▶ `resolve` ──▶ the same index, as text
                                       (not in the manifest)
```

**Hooks are triggers, never carriers.** A hook says *when* to look; every fact
comes from disk. That keeps `notify` stateless with respect to memory content,
so a missed, duplicated or out-of-order hook cannot desynchronise anything.

**Claims about herdr and Claude Code are checked, not remembered.** Two
load-bearing claims in `docs/DESIGN.md` went unchallenged for months and were
both false. Both fell in minutes to reading herdr's source and running the
binary. Confirm behaviour against the tool and against the live corpus before
designing on top of it.

## Constraints that are easy to break

### Stores

- **Never write to a memory store.** Not from `notify`, not from `panel`, not
  from `reconcile`. Stores are repo-keyed and shared by every concurrent
  session and worktree with no locking. Where memories must change, the
  launched Claude session writes them after confirming each change with the
  user. This is the load-bearing decision in the whole design.
- **An empty `memory/` is a directory Claude Code created, not a defect.** Five
  of the eight stores on this machine are empty. Count them in any inventory;
  report them as a problem nowhere, including as dead stores.
- **No memory carries a date.** Frontmatter is `name`, `description`, `type`,
  `originSessionId`, nested as `metadata.type` in newer Claude Code. File mtime
  answers "when did bytes change", not "is this belief old".

### Resolution

- **Never reverse a project slug with string surgery.** Dashes in directory
  names are indistinguishable from path separators, so
  `-Users-x-Code-herdr-claude-tasks` has several readings and only one is real.
  Resolve through a transcript's `cwd` and `git rev-parse --show-toplevel`.
- **A `cwd` that no longer exists resolves to itself.** No ancestor is probed
  for a repository to adopt it, so a deleted worktree forms its own group.
  Producing a wrong-but-plausible parent is the failure this whole module
  exists to avoid.
- **The resolution cache is a memo, never a record.** An entry is valid only
  while every fact behind it holds: the transcript that supplied the `cwd`
  (which is not always the newest one), the newest transcript, whether the
  `cwd` still exists, and the repository marker git answered from. A cached
  answer that disagrees with `resolve --no-cache` is a bug, not a trade-off.
- **A store's transcripts are not one directory's `*.jsonl`.** Worktrees get
  their own project directory for transcripts while memories stay in the parent
  repo's store. Group project directories by resolved repository root.

### Hooks and toasts

- **`reconcile` runs on every server start.** It must stay idempotent; verify
  with two consecutive runs producing a byte-identical `settings.json`. It
  matches hooks by exact command string and only appends — it must never
  rewrite, reorder or remove an entry it did not create, because that file is
  shared with other tools and with the sibling `stgerman.claude-tasks` plugin.
- **`notify` must be an invisible no-op outside herdr,** and must never fail.
  It runs inside the agent's turn; a non-zero exit interrupts the user.
- **Skip `MEMORY.md` in `notify`.** One logical memory save is two writes — the
  topic file and the index. Reacting to both doubles every notification.
- **Toasts are spawned detached and never awaited.** Blocking adds latency to
  the agent's turn.
- **Coalesce toasts.** herdr rate limits `notification show` to one per second
  and drops the excess rather than queuing it, so a turn that saves three
  memories raises one toast and loses two. Batch through the pending file and a
  detached flusher, which keeps the toast unawaited and makes one logical save
  one notification. herdr does **not** suppress toasts for the active tab on
  this path, whatever earlier drafts of the design claimed.
- **Honour the suppression lock.** Adoption writes confirmed memories to the
  real store, which is exactly what `notify` watches. Without the lock every
  review ends in a burst of notifications.

### Transcripts and dreams

- **Extract before a model reads a transcript.** 97.5% of transcript bytes are
  tool calls, tool results and attachments; `thinking` blocks are 98.9%
  cryptographic signature. Prose, thinking text and compaction summaries reduce
  26.5 MB to roughly 101K tokens, which is what makes a dream affordable and
  machine-wide.
- **Transcripts carry credentials.** Anything ever pasted or printed into a
  session is in one. The extractor redacts, and when investigating auth, confirm
  a secret exists without printing it — `security find-generic-password -g`
  writes the secret straight into the transcript you are standing in.
- **Redirect a dream's auto-memory** with
  `--settings '{"autoMemoryDirectory": ...}'`, verified to move the store and
  leave auth alone. A sandboxed `CLAUDE_CONFIG_DIR` isolates just as well but
  breaks subscription auth while exiting 0, so the dream produces nothing and
  reports success.

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
