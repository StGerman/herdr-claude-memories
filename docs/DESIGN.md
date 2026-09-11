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
  project name has to live in the title text.
- **Active-tab suppression does not apply to this path.** `handle_notification_show`
  never calls `active_tab_suppresses_notifications`; only the agent-state-change
  paths do. A CLI notification has no pane association for the check to test, so
  the toast fires even when you are watching the session that wrote the memory.
  An earlier version of this document claimed the opposite and it was
  load-bearing in the design conversation.
- **Notifications are rate limited to one per second and the excess is dropped,**
  not queued: a second call inside the window returns
  `reason: "rate_limited", shown: false`. A third memory saved in the same turn
  is silently missed. There is a second dropped case, `reason: "busy"`, when a
  toast is already on screen.
- **No memory carries a `modified` date.** Frontmatter is `name`, `description`,
  `type` and `originSessionId`; newer Claude Code nests it as `metadata.type`.
  Anything wanting a memory's age has no source for one.
- **97.5% of a transcript is not conversation.** Measured across 26.5 MB: tool
  calls, tool results and `attachment` records dominate, and `thinking` blocks
  are 98.9% cryptographic `signature`. Prose, thinking text and compaction
  summaries together are 1.5% of the bytes — the whole machine's history
  reduces to roughly 101K tokens.

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

**Writes are coalesced, because herdr drops the excess.** `notify` appends the
memory's name to a pending file and spawns a detached flusher; the flusher
sleeps past the rate-limit window, claims the batch by renaming the file — a
losing racer finds nothing and exits — and fires one toast naming every memory
in it. This is the only shape that fixes the drop without awaiting the child,
and it turns the 1/sec limit from the bug into the batching interval: one
logical save produces one toast even when it is three memories.

### The panel is the machine-wide corpus view

An overlay pane listing every memory store on the machine, which is the one
capability `/memory` structurally cannot offer. One row per repository: root,
store, memory count, `MEMORY.md` size against the startup cap, and whether a
dream proposal is waiting. It is also where a dream starts (`d`) and where a
pending proposal is opened for review (`enter`).

**It was specified as a doctor that opens on what is wrong, and the corpus
refused to cooperate.** Audited across 8 stores: no broken index lines, no
orphan topic files, no unresolvable stores, and the largest `MEMORY.md` at 1.8%
of the 25 KB cap. Every check reported nothing. The checks are still worth
having — they are insurance against a failure that is silent when it happens —
but they are not what makes the pane worth opening today, so they became their
own story.

Deterministic checks only, no model involved, whenever they land:

- **Index integrity** — `MEMORY.md` lines pointing at missing files, and topic
  files nothing points at.
- **Load-cap pressure** — lines and bytes of each `MEMORY.md` against the
  200-line / 25 KB startup limit, with a warning band below it.
- **Dead directories** — stores with content whose transcripts have all been
  swept, so the project can no longer be resolved.

**Staleness was dropped, not deferred.** It was specified as age from a
`modified` frontmatter field that does not exist. File mtime answers "when did
bytes change", not "is this belief old", so it is not a substitute.

**An empty store is never a finding.** Five of eight stores are bare `memory/`
directories. That is Claude Code creating a directory, not a defect, and the
dead-store check must require content or every swept empty directory becomes a
false alarm about nothing.

Full scan when the overlay opens, `r` to rescan. The corpus is small; a scan is
milliseconds and a watcher would be more machinery than the problem deserves.

**The doctor is deterministic; the dream is semantic.** Detecting that a memory
is misfiled *in substance* requires judgement and belongs to the dream. The
checks only report what can be computed.

### Resolution

Read `cwd` from each project directory's transcripts, resolve it with
`git rev-parse --show-toplevel`, and group project directories by repository
root. This is the only method that survives both the lossy slug and the
worktree split. Stores whose transcripts are all swept resolve to nothing and
are reported as unresolvable. That is an outcome of resolution, not yet a
judgement: an unresolvable store *with memories in it* is a finding, and an
unresolvable empty directory is listed and left alone.

`--show-toplevel` inside a linked worktree answers with the *worktree* root, so
grouping needs one more step: `--git-common-dir` names `<main>/.git`, whose
parent is the repository the store belongs to. A submodule's common directory
is `<super>/.git/modules/<name>`, which does not match, so submodules keep
their own root.

**A `cwd` that no longer exists resolves to itself.** No ancestor is probed for
a repository willing to adopt it. A deleted worktree therefore forms its own
group rather than joining its parent — the price of never producing a
wrong-but-plausible answer, and the same rule that forbids slug reversal.

**The resolution cache is a memo that checks its own work.** An entry in
`$HERDR_PLUGIN_STATE_DIR/resolution.json` records every fact its answer rested
on — the transcript that supplied the `cwd` (not merely the newest one, since a
transcript can carry no `cwd` at all), the newest transcript, whether that `cwd`
existed, and whether git or the fallback produced the root — and is used only
while all of them still hold. Deleting the file changes nothing but speed, and a
store whose evidence has been swept is never resurrected from it.

Re-running git on every hit would cost as much as a cold scan — 120ms against
this corpus, essentially all of it subprocess spawns — so the cache re-checks
the *state* git answered from rather than re-asking it. The one drift it cannot
notice is a repository appearing or moving under a `cwd` that itself never
changed; `--no-cache` settles that, and so does deleting the file.

### Dreams

Modelled on the Anthropic Dreams API contract, run locally.

The real [Dreams API](https://platform.claude.com/docs/en/managed-agents/dreams)
consumes managed `memstore_*` stores and `sesn_*` sessions — server-side
resources that Claude Code's local memory directories and `*.jsonl` transcripts
are not, with no documented import path. It is also gated behind the Managed
Agents research preview plus a second beta header, authenticated with an API key
rather than Claude Code's own auth, and billed separately.

What is worth stealing is the contract, and it is stolen exactly:

> The input store is never modified. The dream produces a separate output
> store. Review it, then adopt or discard.

So: `claude -p` reads an extract of the transcripts plus every live store and
writes **shadow memory directories** under
`HERDR_PLUGIN_STATE_DIR/dreams/<repo>/<timestamp>/`. The live stores are never
touched. `diff -r` is the entire review UI.

- **Manual trigger only.** A dream spends real tokens and rewrites what shapes
  every future session. Idle, threshold and scheduled triggers are all
  addable later without redesign; none of them should be first.
- **Cross-project moves and user-scope promotion are in scope.** A dream may
  propose relocating a memory to another repository's store, or promoting a
  genuinely global preference up to `~/.claude/CLAUDE.md`. This is what makes
  fragmentation a fixable problem rather than a report.
- **A dream may propose anything** — rewrite, merge, split, move, delete —
  because the most common real defect is a memory that is 70% right and 30%
  stale, which a move-or-delete-only dream cannot touch. Every entry carries the
  original text, which is what keeps that reviewable.

**Extraction, not a time window.** The first design bounded input to a recent
window. That bounds nothing: every transcript on this machine is inside it, and
`cleanupPeriodDays` retention is only 30 days. What actually bounds the input is
throwing away the 97.5% of transcript bytes that are tool traffic and
attachments. The plugin does that itself, deterministically, before the model
sees anything — keeping prose, thinking text and compaction summaries, dropping
synthetic `<tag>` blocks and `isMeta` records, since 60% of what looks like user
text is the harness talking.

**Which is why the dream is machine-wide and has no watermark.** At ~101K tokens
for every transcript ever written, per-repository scoping buys nothing and costs
the thing that matters: a dream scoped to repo A can propose moving a memory to
B but can never notice that B already holds the duplicate. A watermark is a
cache, and #3 already established that a stale memo is worse than a cheap
recompute.

**The extractor redacts credentials.** Transcripts contain whatever was ever
pasted or printed into them — demonstrated live while designing this, when a
shell command wrote a working OAuth token pair into one. Extraction is a
deterministic pass over every byte and the only point where the plugin controls
what leaves the machine. The pattern list grows and is never complete.

**The dream's own auto-memory is redirected, not disabled.** A `claude -p`
session would otherwise write into the stores it must not touch. Passing
`--settings '{"autoMemoryDirectory": ...}'` sends it somewhere disposable, which
is verified behaviour: with the override, the default store directory is not
created and the override is. Two alternatives were tested and rejected —
`--bare` disables auto-memory but forces API-key auth, which is the exact trade
this design refused when it declined the real Dreams API; a sandboxed
`CLAUDE_CONFIG_DIR` isolates perfectly but breaks subscription auth while
**exiting 0**, so a dream would silently produce nothing and look successful.

**The live stores are guarded by hashing, not by the prompt.** Every file is
hashed before the dream launches and re-checked on exit; drift aborts the review
and reports what changed. It is the check that catches the isolation mechanism
failing.

**The model is configuration, not design.** `config.json` in
`HERDR_PLUGIN_CONFIG_DIR` holds one key, defaulting to `opus`, passed through to
`claude --model` unchecked — it accepts aliases and full ids alike, and an
allowlist shipped today would be wrong by the next model release.

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

The corpus pane opens as a zoomed **overlay** — browsing is something you open,
read and close, and an overlay restores your previous focus and zoom on exit,
which is herdr's behaviour rather than this plugin's. The dream and the review
both open as a **split**, because each is a real Claude session you may want to
move, zoom, or leave running.

A popup was rejected: it has no pane id and is invisible to the pane, layout
and persistence APIs, so the pane could never be scripted or restored.

herdr actions run a process; they cannot open a pane declaratively, and a
`[[keys.command]]` binding must name a `plugin_action`. A keybinding therefore
reaches the pane through a small `panel-open` subcommand that calls back into
`herdr plugin pane open`.

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
- **A complete record of memory changes.** The `PostToolUse` hook matches
  `Write|Edit`, which is the path auto-memory actually uses — but a memory you
  edit yourself through `/memory` and `$EDITOR`, or one a session writes with
  `Bash`, is invisible to it. The toast is a notification, not an audit log.
  Filesystem watching would be complete by construction, but herdr `[[startup]]`
  hooks are one-shot initialisation commands with nowhere to supervise a daemon.
- **Memory staleness.** No memory carries a date, and file mtime does not mean
  what the check would need it to mean.
- **Clickable `[[wikilinks]]`.** herdr link handlers match clicked *URLs* only,
  not arbitrary terminal text.
