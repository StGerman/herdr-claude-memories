# Design

Settled decisions. Each issue in this repository implements one slice of what
follows and links back to the section that governs it. Where a decision has a
"why", the why is the part worth preserving — the choice can be revisited, the
constraint that forced it usually cannot.

## The problem

Claude Code writes auto-memory silently. Across three to five concurrent
sessions there is no moment at which you learn that a memory was saved, and
`/memory` shows only the current project — so nothing on the machine can show
you the corpus as a whole.

Two jobs follow from that:

1. **Writes are invisible.** Tell me, ambiently, when a session saves a memory.
2. **The corpus is fragmented.** Show me every project's memories at once, and
   what is structurally wrong with them.

Everything else in this document is in service of those two.

## Constraints discovered before designing

These are facts about Claude Code and herdr, verified on disk, not assumptions.

- **Memories are written with the ordinary `Write` tool.** There is no memory
  tool to hook. A `PostToolUse` hook matching `Write|Edit` sees every memory
  write in `tool_input.file_path`.
- **One logical memory is two writes.** Saving a memory writes the topic file
  *and* rewrites `MEMORY.md`. Anything that reacts per write fires twice.
- **`MEMORY.md` is capped at 200 lines / 25 KB at session start.** Content past
  the cap is silently dropped. This is the only silent data-loss path in the
  system and nothing surfaces it.
- **The project directory slug is lossy.** `-Users-sgerman-Code-herdr-claude-tasks`
  reverses naively to `/Users/sgerman/Code/herdr/claude/tasks`, which does not
  exist. Dashes in directory names are indistinguishable from separators, so no
  string transform resolves a memory directory to a project.
- **Transcripts carry the truth.** `cwd` appears on `type: user` records. That
  is the only authoritative resolution.
- **Transcripts are swept, memories are not.** `cleanupPeriodDays` deletes
  transcripts; the memory directory is explicitly excluded. Old memory
  directories therefore become unresolvable by design.
- **Worktrees split transcripts away from memories.** A worktree gets its own
  project directory for transcripts, while memories stay in the parent repo's
  store. "This project's transcripts" is every project directory whose `cwd`
  resolves into the same git repository — not one directory's `*.jsonl`.
- **Every memory records `originSessionId`** in its frontmatter.
- **`autoMemoryDirectory` can relocate a store** and is readable from any
  settings scope, so globbing `~/.claude/projects/*/memory/` is an incomplete
  scan by design.
- **herdr's sidebar is closed to plugins.** `SidebarConfig` has exactly two
  sections, `agents` and `spaces`, both keyed to herdr's own entities, and the
  plugin docs state that native non-terminal plugin UI is not part of plugin v1.
  A memory list in the sidebar is not buildable. Panes, actions, keybindings,
  link handlers and per-agent metadata tokens are.
- **`herdr notification show` has no `--pane`.** A toast is global, so the
  project name has to live in the title text. herdr suppresses popups for the
  active tab, which is desirable here: you are only told about writes in
  sessions you are not watching.

## Decisions

### Scope

Machine-wide. The plugin indexes every memory store it can find, not just the
focused pane's project. This is the one capability `/memory` structurally
cannot offer, and it is where fragmentation becomes visible.

### The plugin never writes to a memory store

It reads, indexes, diffs and renders. Where a memory must change, a real Claude
Code session does the writing, having just confirmed each change interactively.

**Why:** the store is repo-keyed and shared by every concurrent session and
worktree, with no locking. Keeping the plugin out of the write path means it
never has to solve that, and the audit trail for any change is a conversation
you took part in.

### Write visibility is a toast, not a token

A `PostToolUse` hook on `Write|Edit` fires `herdr notification show` when a
**topic file** under a memory directory is written.

- `MEMORY.md` writes are skipped — index churn is bookkeeping, not news, and
  skipping it is what turns two writes back into one notification.
- Updates to existing memories notify too. A silent rewrite of something Claude
  already believes is more worth knowing about than a new note.
- A suppression lock under `HERDR_PLUGIN_STATE_DIR` silences the hook while a
  review is adopting changes, then one summary toast fires when it clears.

### The panel is a read-only doctor

An overlay pane that opens on what is *wrong* across every project rather than
on a file tree. Deterministic checks only, no model involved:

- **Index integrity** — `MEMORY.md` lines pointing at missing files, and topic
  files nothing points at.
- **Load-cap pressure** — lines and bytes of each `MEMORY.md` against the
  200-line / 25 KB startup limit, with a warning band below it.
- **Staleness and dead directories** — age from the `modified` frontmatter
  field, and stores whose transcripts have all been swept so the project can no
  longer be resolved.

Staleness is a prompt to look, not a verdict. An old memory can still be true.

Full scan when the overlay opens, `r` to rescan. The corpus is small; a scan is
milliseconds and a watcher would be more machinery than the problem deserves.

**The doctor is deterministic; the dream is semantic.** Detecting that a memory
is misfiled *in substance* requires judgement and belongs to the dream. The
doctor only reports what can be computed.

### Resolution

Read `cwd` from each project directory's transcripts, resolve it with
`git rev-parse --show-toplevel`, and group project directories by repository
root. This is the only method that survives both the lossy slug and the
worktree split. Stores whose transcripts are all swept are reported as
unresolvable, which is itself a finding.

### Dreams

Modelled on the Anthropic Dreams API contract, run locally.

The real [Dreams API](https://platform.claude.com/docs/en/managed-agents/dreams)
consumes managed `memstore_*` stores and `sesn_*` sessions — server-side
resources that Claude Code's local memory directories and `*.jsonl` transcripts
are not, with no documented import path. It is also gated behind the Managed
Agents research preview plus a second beta header and billed separately.

What is worth stealing is the contract, and it is stolen exactly:

> The input store is never modified. The dream produces a separate output
> store. Review it, then adopt or discard.

So: a real interactive Claude session, launched in a herdr pane, reads the
live store plus new transcripts and writes a **shadow memory directory** to
`HERDR_PLUGIN_STATE_DIR/dreams/<repo>/<timestamp>/`. The live store is never
touched. `diff -r` is the entire review UI.

- **Manual trigger only.** A dream spends real tokens and rewrites what shapes
  every future session. Idle, threshold and scheduled triggers are all
  addable later without redesign; none of them should be first.
- **Incremental.** A watermark records how far the last dream read; each pass
  mines only newer transcripts. The first pass reads a bounded recent window so
  the most expensive run is also the most predictable.
- **Cross-project moves and user-scope promotion are in scope.** A dream may
  propose relocating a memory to another repository's store, or promoting a
  genuinely global preference up to `~/.claude/CLAUDE.md`. This is what makes
  fragmentation a fixable problem rather than a report. Reading every store to
  do it is free — the whole corpus is kilobytes; only transcripts cost money.

### Review and adoption

The panel launches `claude` in a split pane with a short prompt pointing at a
versioned `PROCEDURE.md` in the plugin root and at the shadow directory. The
review session walks the proposal with `AskUserQuestion`, one confirmation per
change.

Auto-memory stays **enabled** for the review session — what you say while
reviewing is durable feedback worth keeping. The loop that opens up is closed
with `originSessionId`: the plugin records the review session's id and excludes
memories carrying it from the next dream's input. The learnings still load into
every session normally; a dream simply never re-consumes its own reasoning.

Adoption is **per memory, then rebuild the index**. Claude writes each confirmed
memory individually and regenerates `MEMORY.md` from what is actually on disk
afterwards.

**Why not a wholesale directory swap:** it is atomic and trivially reversible,
and it silently destroys anything the other four sessions wrote since the
snapshot. Merging is the only behaviour that is correct under the concurrency
this plugin exists to serve.

### Panes

Panel opens as a zoomed **overlay** — browsing is something you open, read and
close, and an overlay restores your previous focus and zoom on exit. The review
opens as a **split**, because it is a real Claude session you may want to move,
zoom, or leave running.

A popup was rejected: it has no pane id and is invisible to the pane, layout
and persistence APIs, so the panel could never be scripted or restored.

### Installation

`[[startup]]` runs `reconcile`, which appends a `PostToolUse` hook to
`~/.claude/settings.json` by exact-command match, refuses to touch JSON it
could not parse, and writes via temp + rename.

`~/.claude/settings.json` is shared — with other tools, and with the sibling
`stgerman.claude-tasks` plugin, which already writes two hooks into it. A
distinct command string is what lets the two coexist without either knowing
about the other. Verify idempotence the same way: two consecutive runs must
produce a byte-identical file.

## Deliberately not built

- **A sidebar list of memories.** Not reachable from a plugin. See constraints.
- **Editing memories in the panel.** `/memory` already opens them in `$EDITOR`,
  and the plugin does not write to stores.
- **Automatic dreaming.** Trigger stays manual until the proposals have earned
  trust.
- **Clickable `[[wikilinks]]`.** herdr link handlers match clicked *URLs* only,
  not arbitrary terminal text.
