//! herdr-claude-memories — surface and curate Claude Code auto-memory in herdr.
//!
//! One binary, no library crate. See `docs/DESIGN.md` for why each subcommand
//! behaves the way it does.
//!
//! * `reconcile` — install this plugin's hook into `~/.claude/settings.json`.
//!   Runs from `[[startup]]` on every herdr server start, so it must be
//!   idempotent and must never fail the server.
//! * `notify` — the `PostToolUse` hook body. Reads the hook payload on stdin
//!   and fires a herdr toast when a memory topic file is written.
//! * `panel` — the read-only machine-wide corpus overlay.
//! * `panel-open` — ask herdr to open that overlay. An action, not a pane,
//!   because a keybinding can only reach a pane through one.
//! * `resolve` — print the store/repository index. Undocumented and absent
//!   from the manifest: it exists to exercise `resolution` against a real
//!   `~/.claude` and to debug the panel and the dream.
//!
//! A hook that fails is a hook that interrupts the agent's turn, so every path
//! here exits `SUCCESS` unless the user asked for something that does not
//! exist.

use std::io::Read;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod panel;
mod resolution;

/// Tool names whose writes can land in a memory store.
///
/// Claude Code has no memory tool: memories are saved with the ordinary file
/// tools, so this is the whole surface to watch.
const HOOK_MATCHER: &str = "Write|Edit";

/// The index file every memory save rewrites alongside the topic file.
///
/// One logical memory is two writes. Skipping this one is what turns two
/// notifications back into one.
const INDEX_FILE: &str = "MEMORY.md";

/// Presence of this file under `HERDR_PLUGIN_STATE_DIR` silences the hook, so
/// adopting a reviewed proposal does not produce a burst of toasts.
const SUPPRESS_FILE: &str = "suppress-toasts";

fn main() -> ExitCode {
    let Some(command) = std::env::args().nth(1) else {
        usage();
        return ExitCode::FAILURE;
    };

    match command.as_str() {
        "reconcile" => run_reconcile(),
        "notify" => run_notify(),
        "resolve" => run_resolve(&std::env::args().skip(2).collect::<Vec<_>>()),
        "panel" => panel::run(&config_dir()),
        "panel-open" => panel::run_open(),
        other => {
            eprintln!("herdr-claude-memories: unknown command '{other}'");
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: herdr-claude-memories <reconcile|notify|panel|panel-open|resolve>");
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

/// Print which repository every memory store belongs to, and what evidence
/// says so.
///
/// `--json` for machines, `--no-cache` to prove the cache is only ever a
/// memo — with and without it the answer must be identical.
fn run_resolve(args: &[String]) -> ExitCode {
    let cache = match args.iter().any(|arg| arg == "--no-cache") {
        true => None,
        false => resolution::cache_path(),
    };
    let index = resolution::index(&config_dir(), cache.as_deref());
    if args.iter().any(|arg| arg == "--json") {
        println!("{}", index.to_json());
    } else {
        print!("{}", index.render());
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// notify
// ---------------------------------------------------------------------------

/// Hook body: toast when a memory topic file is written.
///
/// Every path returns `SUCCESS`. This runs inside the agent's turn, where a
/// non-zero exit interrupts the user — a missed toast is always the better
/// failure.
fn run_notify() -> ExitCode {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return ExitCode::SUCCESS;
    }

    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return ExitCode::SUCCESS;
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ExitCode::SUCCESS;
    };

    let tool = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !HOOK_MATCHER.split('|').any(|name| name == tool) {
        return ExitCode::SUCCESS;
    }

    let Some(file_path) = payload
        .get("tool_input")
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str())
    else {
        return ExitCode::SUCCESS;
    };

    // The hook fires after the write, so the file exists and canonicalising it
    // resolves any symlinks in the written path. That alone is not enough — see
    // `is_memory_topic_file`, which canonicalises the roots it is compared
    // against too.
    let path = PathBuf::from(file_path);
    let path = std::fs::canonicalize(&path).unwrap_or(path);

    // The payload's `cwd` is the session's project root, which is where a
    // repository-scoped `autoMemoryDirectory` is configured. Without it, a
    // session whose store has been moved would write memories this hook never
    // recognises.
    let cwd = payload
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(PathBuf::from);

    let config_dir = config_dir();
    let mut extra_roots = resolution::configured_memory_roots(&config_dir);
    extra_roots.extend(cwd.as_deref().and_then(resolution::relocated_store));
    if !is_memory_topic_file(&path, &config_dir, &extra_roots) {
        return ExitCode::SUCCESS;
    }

    if suppressed() {
        return ExitCode::SUCCESS;
    }

    // The payload carries `cwd`, so labelling the toast needs no slug
    // resolution. Resolving a store to its repository is a separate concern.
    let project = cwd
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".to_string());

    report(&project, &summarise(&path));

    ExitCode::SUCCESS
}

/// What a memory says about itself, for the toast body.
#[derive(Debug, PartialEq, Eq)]
struct Summary {
    name: String,
    description: String,
}

/// Read `name` and `description` out of a memory's YAML frontmatter.
///
/// Deliberately a line scan rather than a YAML dependency: two scalar fields at
/// the top level of a file Claude Code writes to a known shape. A malformed or
/// frontmatter-less memory still deserves a toast, so this always yields a
/// name, falling back to the file stem.
fn summarise(path: &Path) -> Summary {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "memory".to_string());
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Summary {
            name: stem,
            description: String::new(),
        };
    };
    let front = frontmatter(&contents);
    Summary {
        name: front
            .and_then(|front| scalar(front, "name"))
            .unwrap_or(stem),
        description: front
            .and_then(|front| scalar(front, "description"))
            .unwrap_or_default(),
    }
}

/// The text between the opening and closing `---` fences, if the file opens
/// with one.
fn frontmatter(contents: &str) -> Option<&str> {
    let rest = contents.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// A top-level `key: value` from frontmatter.
///
/// Nested keys are indented, so requiring the key at column zero is what keeps
/// `metadata.name` from answering a lookup for `name`.
fn scalar(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        let Some(value) = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
        else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Is this a memory topic file, as opposed to the index or an unrelated file?
///
/// Matches the default `<config>/projects/<slug>/memory/` shape structurally
/// rather than by globbing, plus any store relocated with
/// `autoMemoryDirectory`. Never a substring test on `/memory/`: plenty of
/// repositories have a directory by that name.
///
/// Both sides of the comparison are canonicalised. The caller hands us a
/// canonicalised path, and `$HOME` is a symlink on plenty of machines, so
/// leaving the roots as written is what would make the prefix never match and
/// no toast ever fire.
fn is_memory_topic_file(path: &Path, config_dir: &Path, extra_roots: &[PathBuf]) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return false;
    }
    if path.file_name().is_some_and(|name| name == INDEX_FILE) {
        return false;
    }
    if extra_roots
        .iter()
        .any(|root| path.starts_with(resolution::normalise(root)))
    {
        return true;
    }
    let projects = resolution::normalise(&config_dir.join("projects"));
    let Ok(rest) = path.strip_prefix(projects) else {
        return false;
    };
    let mut parts = rest.components();
    parts.next(); // the project slug, which is lossy and not resolved here
    parts
        .next()
        .is_some_and(|part| part.as_os_str() == "memory")
}

fn suppressed() -> bool {
    std::env::var("HERDR_PLUGIN_STATE_DIR")
        .map(|dir| Path::new(&dir).join(SUPPRESS_FILE).exists())
        .unwrap_or(false)
}

/// Fire the toast, detached.
///
/// The agent's turn is waiting on this process, so the child is spawned and
/// abandoned rather than awaited.
fn report(project: &str, summary: &Summary) {
    let body = if summary.description.is_empty() {
        summary.name.clone()
    } else {
        format!("{} — {}", summary.name, summary.description)
    };
    let bin = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());
    let _ = std::process::Command::new(bin)
        .args([
            "notification",
            "show",
            &format!("{project} · memory saved"),
            "--body",
            &body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---------------------------------------------------------------------------
// reconcile
// ---------------------------------------------------------------------------

/// Install this plugin's `PostToolUse` hook, idempotently.
///
/// Runs from `[[startup]]` on every server start and again on live handoff, so
/// this is a convergence step rather than an installer. It never fails the
/// server.
fn run_reconcile() -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("herdr-claude-memories: cannot determine own path, skipping reconcile");
        return ExitCode::SUCCESS;
    };
    let command = format!("{} notify", exe.display());
    let settings = config_dir().join("settings.json");

    let mut root = match std::fs::read_to_string(&settings) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(value) => value,
            Err(err) => {
                // A file we cannot parse is a file we cannot safely round-trip,
                // and settings.json is shared. Leave it exactly as it is.
                eprintln!(
                    "herdr-claude-memories: {} is not valid JSON ({err}), leaving it untouched",
                    settings.display()
                );
                return ExitCode::SUCCESS;
            }
        },
        Err(_) => serde_json::json!({}),
    };

    if !ensure_hook(&mut root, HOOK_MATCHER, &command) {
        return ExitCode::SUCCESS;
    }

    match write_atomically(&settings, &root) {
        Ok(()) => eprintln!("herdr-claude-memories: installed PostToolUse hook"),
        Err(err) => eprintln!("herdr-claude-memories: could not write settings.json: {err}"),
    }
    ExitCode::SUCCESS
}

/// Append our hook if it is not already there. Returns whether anything changed.
///
/// Matches by exact command string and only ever appends. It must never
/// rewrite, reorder or remove an entry it did not create: `settings.json` is
/// shared with other tools and with the sibling `stgerman.claude-tasks` plugin,
/// whose hooks carry different command strings and are therefore invisible to
/// this comparison.
fn ensure_hook(root: &mut serde_json::Value, matcher: &str, command: &str) -> bool {
    let Some(object) = root.as_object_mut() else {
        return false;
    };
    let entries = object
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .and_then(|hooks| {
            hooks
                .entry("PostToolUse")
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
        });
    let Some(entries) = entries else {
        // `hooks` or `hooks.PostToolUse` exists with an unexpected type.
        // Someone else owns that shape; it is not ours to reinterpret.
        return false;
    };

    let already_present = entries.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|hooks| hooks.as_array())
            .is_some_and(|hooks| {
                hooks
                    .iter()
                    .any(|hook| hook.get("command").and_then(|v| v.as_str()) == Some(command))
            })
    });
    if already_present {
        return false;
    }

    entries.push(serde_json::json!({
        "matcher": matcher,
        "hooks": [{ "type": "command", "command": command }],
    }));
    true
}

/// Write via temp + rename in the same directory, so a crash mid-write cannot
/// truncate a file several tools depend on.
pub fn write_atomically(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", file_name_of(path)));
    let mut body = serde_json::to_string_pretty(value)?;
    body.push('\n');
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_string())
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PathBuf {
        PathBuf::from("/home/x/.claude")
    }

    #[test]
    fn topic_file_in_the_default_store_is_a_memory() {
        let path = config().join("projects/-home-x-Code-app/memory/user_role.md");
        assert!(is_memory_topic_file(&path, &config(), &[]));
    }

    /// One logical memory save is two writes; reacting to both would double
    /// every notification.
    #[test]
    fn the_index_is_not_a_memory() {
        let path = config().join("projects/-home-x-Code-app/memory/MEMORY.md");
        assert!(!is_memory_topic_file(&path, &config(), &[]));
    }

    /// Plenty of repositories contain a directory called `memory`. Matching on
    /// the substring rather than the store's structure would toast for them.
    #[test]
    fn an_unrelated_memory_directory_is_not_a_store() {
        let path = PathBuf::from("/home/x/Code/app/src/memory/notes.md");
        assert!(!is_memory_topic_file(&path, &config(), &[]));
    }

    #[test]
    fn a_project_directory_without_a_memory_component_is_not_a_store() {
        let path = config().join("projects/-home-x-Code-app/notes.md");
        assert!(!is_memory_topic_file(&path, &config(), &[]));
    }

    #[test]
    fn non_markdown_files_are_ignored() {
        let path = config().join("projects/-home-x-Code-app/memory/scratch.txt");
        assert!(!is_memory_topic_file(&path, &config(), &[]));
    }

    #[test]
    fn a_relocated_store_is_a_memory() {
        let root = PathBuf::from("/home/x/my-memories");
        let path = root.join("feedback_testing.md");
        assert!(is_memory_topic_file(&path, &config(), &[root]));
    }

    /// `$HOME` is a symlink on plenty of machines, and the hook canonicalises
    /// the written path before this comparison — so the store's own root has
    /// to be canonicalised too, or nothing ever matches and the toast is
    /// silently dead.
    #[test]
    fn a_symlinked_config_directory_still_matches() {
        let temp =
            std::env::temp_dir().join(format!("herdr-memories-notify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let store = temp.join("real/.claude/projects/-Code-app/memory");
        std::fs::create_dir_all(&store).expect("create store");
        let memory = store.join("user_role.md");
        std::fs::write(&memory, "---\nname: x\n---\n").expect("write memory");
        std::os::unix::fs::symlink(temp.join("real"), temp.join("link")).expect("symlink");

        // What `run_notify` has in hand: the written path, canonicalised.
        let written = std::fs::canonicalize(&memory).expect("canonicalize");
        let through_the_link = temp.join("link/.claude");
        let matched = is_memory_topic_file(&written, &through_the_link, &[]);
        let relocated = is_memory_topic_file(&written, &temp.join("nowhere"), &[store.clone()]);

        let _ = std::fs::remove_dir_all(&temp);
        assert!(matched, "a symlinked config directory must still match");
        assert!(relocated, "so must a symlinked relocated store");
    }

    #[test]
    fn frontmatter_yields_name_and_description() {
        let contents = "---\nname: feedback-questions-ui\ndescription: Ask via the tool\nmetadata:\n  type: feedback\n---\n\nbody\n";
        let front = frontmatter(contents).expect("frontmatter");
        assert_eq!(
            scalar(front, "name").as_deref(),
            Some("feedback-questions-ui")
        );
        assert_eq!(
            scalar(front, "description").as_deref(),
            Some("Ask via the tool")
        );
    }

    /// `metadata:` nests its keys, so a top-level lookup must not reach into it.
    #[test]
    fn nested_keys_do_not_satisfy_a_top_level_lookup() {
        let contents = "---\ndescription: real\nmetadata:\n  name: nested\n---\n";
        let front = frontmatter(contents).expect("frontmatter");
        assert_eq!(scalar(front, "name"), None);
    }

    #[test]
    fn a_file_without_frontmatter_has_none() {
        assert_eq!(frontmatter("# just a heading\n"), None);
    }

    #[test]
    fn ensure_hook_appends_when_absent() {
        let mut root = serde_json::json!({});
        assert!(ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify"));
        let groups = root["hooks"]["PostToolUse"].as_array().expect("array");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["matcher"], HOOK_MATCHER);
        assert_eq!(groups[0]["hooks"][0]["command"], "/p/bin/m notify");
    }

    /// `reconcile` runs on every server start, so the second run must change
    /// nothing at all.
    #[test]
    fn ensure_hook_is_idempotent() {
        let mut root = serde_json::json!({});
        assert!(ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify"));
        let after_first = root.clone();
        assert!(!ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify"));
        assert_eq!(root, after_first);
    }

    /// settings.json is shared with other tools and with the sibling
    /// claude-tasks plugin. Their entries must survive untouched.
    #[test]
    fn ensure_hook_leaves_foreign_entries_alone() {
        let mut root = serde_json::json!({
            "env": { "SOMETHING": "1" },
            "hooks": {
                "PostToolUse": [
                    { "matcher": "TaskCreate|TaskUpdate",
                      "hooks": [{ "type": "command", "command": "/other/tasks sync" }] }
                ],
                "Stop": [
                    { "matcher": "*",
                      "hooks": [{ "type": "command", "command": "/other/tasks sync" }] }
                ]
            }
        });
        let before = root.clone();
        assert!(ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify"));

        assert_eq!(root["env"], before["env"]);
        assert_eq!(root["hooks"]["Stop"], before["hooks"]["Stop"]);
        let groups = root["hooks"]["PostToolUse"].as_array().expect("array");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], before["hooks"]["PostToolUse"][0]);
    }

    /// A `hooks` value someone else owns with an unexpected shape is not ours
    /// to reinterpret.
    #[test]
    fn ensure_hook_refuses_an_unexpected_shape() {
        let mut root = serde_json::json!({ "hooks": "surprise" });
        let before = root.clone();
        assert!(!ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify"));
        assert_eq!(root, before);
    }

    /// This plugin observes auto-memory; it does not configure it.
    #[test]
    fn ensure_hook_writes_no_env_keys() {
        let mut root = serde_json::json!({});
        ensure_hook(&mut root, HOOK_MATCHER, "/p/bin/m notify");
        assert!(root.get("env").is_none());
    }
}
