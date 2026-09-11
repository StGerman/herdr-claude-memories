//! Resolve memory stores to repositories, and repositories to their transcripts.
//!
//! Two facts the filesystem does not state directly, and that both the doctor
//! panel and the dream need: which repository a store belongs to, and which
//! transcripts belong with it.
//!
//! Neither is derivable from the project directory name. The slug is lossy —
//! `-Users-sgerman-Code-herdr-claude-tasks` has several readings and only one
//! is real — so the only authoritative source is the `cwd` recorded inside a
//! transcript, resolved through git. Nothing here reverses a slug, and nothing
//! probes the filesystem for a path it was not told about.
//!
//! Transcripts are swept after `cleanupPeriodDays` while memory files are
//! explicitly excluded from that sweep, so a store outliving its evidence is a
//! normal steady state. [`Unresolved`] is a result, not an error.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The store directory inside a project directory, when it has not been moved.
const MEMORY_DIR: &str = "memory";

/// How far into a transcript to look for a `cwd` before giving up on it.
///
/// The header records sit at the top and transcripts reach tens of megabytes,
/// so this is what keeps resolution to roughly one page of IO per project.
const SCAN_LINES: usize = 200;
const SCAN_BYTES: u64 = 256 * 1024;

/// Bumped when the cache entry shape changes; an older file is simply ignored.
///
/// 2: entries record the transcript that supplied the `cwd` and the state git
/// answered from, not just the newest transcript.
const CACHE_VERSION: u64 = 2;

/// Everything resolvable about the memory corpus on this machine.
#[derive(Debug, Default)]
pub struct Index {
    /// Resolved repositories, ordered by root.
    pub repos: Vec<Repo>,
    /// Stores relocated by a user-scope `autoMemoryDirectory`. Not attached to
    /// a repository because that setting is not repository-specific.
    pub global_stores: Vec<PathBuf>,
    /// Stores whose project can no longer be resolved.
    pub unresolved: Vec<Unresolved>,
}

/// One repository, with every store and project directory that belongs to it.
#[derive(Debug)]
pub struct Repo {
    /// The git toplevel, or the recorded `cwd` when it is not inside a repository.
    pub root: PathBuf,
    /// Usually one. A `Vec` because a repository can have several project
    /// directories, each able to carry a store.
    pub stores: Vec<PathBuf>,
    /// Every project directory grouped here, worktrees included.
    pub project_dirs: Vec<ProjectDir>,
}

/// One `<config>/projects/<slug>` directory and what it holds.
#[derive(Debug)]
pub struct ProjectDir {
    pub path: PathBuf,
    /// The `cwd` as recorded in the transcript that resolved this directory.
    pub cwd: PathBuf,
    /// Newest first.
    pub transcripts: Vec<Transcript>,
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub len: u64,
}

/// One project directory's answer, together with what it rests on.
///
/// The cache stores exactly this, which is what lets it check its own work:
/// every field here is a fact that can change independently of the others.
#[derive(Debug, Clone)]
struct Resolution {
    cwd: PathBuf,
    /// Whether that `cwd` was on disk when git was asked about it. A deleted
    /// worktree is the case this catches.
    cwd_existed: bool,
    root: Root,
    /// The transcript that actually supplied the `cwd` — not necessarily the
    /// newest one, since a transcript can carry no `cwd` at all.
    evidence: Stamp,
}

/// A repository root, and whether git or the fallback produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Root {
    path: PathBuf,
    /// `false` when the `cwd` was outside any repository and resolved to
    /// itself. Knowing which it was is what makes the answer re-checkable.
    from_git: bool,
}

/// A file and the mtime it had when it was read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Stamp {
    path: PathBuf,
    mtime_ns: u64,
}

impl Stamp {
    fn of(transcript: &Transcript) -> Stamp {
        Stamp {
            path: transcript.path.clone(),
            mtime_ns: mtime_ns(transcript),
        }
    }
}

/// A store with no surviving evidence of which project wrote it.
///
/// Reported rather than guessed: a confidently wrong project name is worse
/// than an honest "unresolvable".
#[derive(Debug)]
pub struct Unresolved {
    pub store_dir: PathBuf,
    pub slug: String,
}

/// Where the resolution cache lives, if this process is running under herdr.
///
/// herdr creates the state directory; outside it there is no cache and
/// everything else behaves identically.
pub fn cache_path() -> Option<PathBuf> {
    std::env::var("HERDR_PLUGIN_STATE_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("resolution.json"))
}

/// Scan every project directory, resolve each to a repository, and group.
///
/// `cache` is a memo, and an entry is used only while every fact the answer
/// rested on is unchanged: the transcript that supplied the `cwd`, the newest
/// transcript, whether that `cwd` still exists, and the repository marker git
/// answered from. Deleting the file changes nothing but speed.
pub fn index(config_dir: &Path, cache: Option<&Path>) -> Index {
    let loaded = cache.map(Cache::load).unwrap_or_default();
    let mut fresh = Cache::default();
    let mut roots: HashMap<PathBuf, Root> = HashMap::new();
    let mut groups: BTreeMap<PathBuf, Repo> = BTreeMap::new();
    let mut unresolved = Vec::new();

    for dir in project_dirs(config_dir) {
        let transcripts = transcripts(&dir);
        let store = store_dir(&dir);

        let resolved = match loaded.hit(&dir, &transcripts) {
            Some(entry) => Some(entry.resolution()),
            // Only live resolutions seed the memo. A cache hit answers for its
            // own project directory and no other: letting it seed this would
            // hand a remembered root to a directory whose own evidence is
            // fresh enough to ask git about.
            None => resolve(&transcripts, &mut roots),
        };

        let Some(resolution) = resolved else {
            // Evidence gone, or none of it carries a `cwd`. Another project
            // directory resolving to the same repository does not rescue this
            // one: nothing links an orphan slug to a repository.
            if let Some(store_dir) = store {
                unresolved.push(Unresolved {
                    slug: file_name(&dir),
                    store_dir,
                });
            }
            continue;
        };

        if let Some(newest) = transcripts.first() {
            fresh.record(&dir, newest, &resolution);
        }

        let root = resolution.root.path.clone();
        let repo = groups.entry(root.clone()).or_insert_with(|| Repo {
            root,
            stores: Vec::new(),
            project_dirs: Vec::new(),
        });
        if let Some(store) = store {
            repo.stores.push(store);
        }
        repo.project_dirs.push(ProjectDir {
            path: dir,
            cwd: resolution.cwd,
            transcripts,
        });
    }

    let mut repos: Vec<Repo> = groups.into_values().collect();
    for repo in &mut repos {
        relocate_store(repo);
        repo.stores.sort();
        repo.stores.dedup();
    }
    unresolved.sort_by(|a, b| a.store_dir.cmp(&b.store_dir));

    if let Some(path) = cache {
        fresh.save(path, &loaded);
    }

    Index {
        repos,
        global_stores: configured_memory_roots(config_dir),
        unresolved,
    }
}

/// Stores relocated with a user-scope `autoMemoryDirectory`.
///
/// Globbing the default location alone is an incomplete scan by design: the
/// setting moves a store wholesale. Checked-in `.claude/settings.json` is
/// deliberately not consulted — Claude Code ignores the key there for
/// security, and so must this.
pub fn configured_memory_roots(config_dir: &Path) -> Vec<PathBuf> {
    auto_memory_directory(&config_dir.join("settings.json"))
        .into_iter()
        .collect()
}

/// A repository's own `autoMemoryDirectory`, from its local (never checked-in)
/// settings.
///
/// When it is set, Claude Code writes there *instead of* the default location,
/// so the relocated directory replaces the store rather than joining it.
fn relocate_store(repo: &mut Repo) {
    let Some(dir) = relocated_store(&repo.root) else {
        return;
    };
    if dir.is_dir() {
        repo.stores = vec![dir];
    }
}

/// Where a session running in `project_root` writes its memories, when that
/// repository has moved its own store.
///
/// `notify` needs this as much as the index does: a session writing to a
/// repository-scoped store writes nowhere near `<config>/projects`, so without
/// this the toast for it never fires. Settings live beside the session's
/// working directory, so this costs one read and never a subprocess — the hook
/// runs inside the agent's turn.
pub fn relocated_store(project_root: &Path) -> Option<PathBuf> {
    auto_memory_directory(&project_root.join(".claude/settings.local.json"))
}

fn auto_memory_directory(settings: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(settings).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    value
        .get("autoMemoryDirectory")
        .and_then(|dir| dir.as_str())
        .map(expand_home)
}

pub fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(raw),
    }
}

// ---------------------------------------------------------------------------
// the filesystem side
// ---------------------------------------------------------------------------

fn project_dirs(config_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(config_dir.join("projects")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn store_dir(project_dir: &Path) -> Option<PathBuf> {
    let store = project_dir.join(MEMORY_DIR);
    store.is_dir().then_some(store)
}

/// A project directory's transcripts, newest first.
///
/// Depth one only: `<slug>/<session-id>/` holds tool results, not transcripts.
fn transcripts(project_dir: &Path) -> Vec<Transcript> {
    let Ok(entries) = std::fs::read_dir(project_dir) else {
        return Vec::new();
    };
    let mut found: Vec<Transcript> = entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(Transcript {
                path: entry.path(),
                mtime: meta.modified().unwrap_or(UNIX_EPOCH),
                len: meta.len(),
            })
        })
        .collect();
    // Newest first, so a long-lived project resolves from recent evidence.
    // Path breaks ties, so two files written in the same instant still order
    // deterministically.
    found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    found
}

/// The first `cwd` recorded in a transcript.
///
/// Any record carrying one will do — the first line is often a
/// `queue-operation`, which has none, and the record type is not the point.
fn recorded_cwd(path: &Path) -> Option<PathBuf> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(SCAN_BYTES));
    let mut line = String::new();
    for _ in 0..SCAN_LINES {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let cwd = record.get("cwd").and_then(|cwd| cwd.as_str()).unwrap_or("");
        if !cwd.is_empty() {
            return Some(PathBuf::from(cwd));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// the git side
// ---------------------------------------------------------------------------

/// The repository a recorded `cwd` belongs to.
///
/// A `cwd` outside any repository — or one that no longer exists — resolves to
/// itself. Claude Code uses the working directory as the project root in that
/// case too, and an honest self-reference beats a guessed parent.
fn repo_root(cwd: &Path) -> Root {
    let Some(toplevel) = git(cwd, &["rev-parse", "--show-toplevel"]) else {
        return Root {
            path: normalise(cwd),
            from_git: false,
        };
    };
    let toplevel = PathBuf::from(toplevel);
    Root {
        path: main_worktree(&toplevel).unwrap_or_else(|| normalise(&toplevel)),
        from_git: true,
    }
}

/// Read the `cwd` out of the newest transcript that carries one, and resolve it.
///
/// The memo spares a second `git rev-parse` for two project directories that
/// record the same `cwd`.
fn resolve(transcripts: &[Transcript], roots: &mut HashMap<PathBuf, Root>) -> Option<Resolution> {
    let (evidence, cwd) = transcripts
        .iter()
        .find_map(|transcript| Some((transcript, recorded_cwd(&transcript.path)?)))?;
    let root = roots
        .entry(cwd.clone())
        .or_insert_with_key(|cwd| repo_root(cwd))
        .clone();
    Some(Resolution {
        cwd_existed: cwd.exists(),
        cwd,
        root,
        evidence: Stamp::of(evidence),
    })
}

/// The main repository behind a linked worktree.
///
/// `--show-toplevel` inside a worktree answers with the *worktree* root, so
/// grouping a worktree's transcripts with the parent's store needs the common
/// git directory: `<main>/.git` for a worktree, `<super>/.git/modules/<name>`
/// for a submodule — which is why a submodule keeps its own root here.
fn main_worktree(toplevel: &Path) -> Option<PathBuf> {
    let common = git(toplevel, &["rev-parse", "--git-common-dir"])?;
    let common = match PathBuf::from(common) {
        path if path.is_absolute() => path,
        relative => toplevel.join(relative),
    };
    if common.file_name()? != ".git" {
        return None;
    }
    let parent = normalise(common.parent()?);
    if parent == normalise(toplevel) {
        return None;
    }
    let confirmed = git(&parent, &["rev-parse", "--show-toplevel"])?;
    (normalise(Path::new(&confirmed)) == parent).then_some(parent)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

/// Resolve symlinks so `/tmp` and `/private/tmp` cannot split one repository
/// into two groups. A path that no longer exists is kept exactly as recorded.
pub fn normalise(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// cache
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    version: u64,
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    /// Invalidates when a newer transcript lands, so fresh evidence is read.
    newest: Stamp,
    /// Invalidates when the transcript that supplied the `cwd` is swept or
    /// rewritten — which is not always the newest one.
    evidence: Stamp,
    cwd: PathBuf,
    cwd_existed: bool,
    root: Root,
}

impl Cache {
    fn load(path: &Path) -> Cache {
        let cache = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Cache>(&raw).ok())
            .unwrap_or_default();
        if cache.version == CACHE_VERSION {
            cache
        } else {
            Cache::default()
        }
    }

    /// The cached resolution for a project directory, if every fact it rests
    /// on still holds.
    ///
    /// Re-running git on each hit would cost about as much as resolving from
    /// scratch — the subprocess spawns are the expensive part — so instead
    /// this re-checks the two pieces of state git's answer depended on: the
    /// `cwd` it was asked about, and the repository marker it found. What it
    /// cannot notice is a repository appearing or moving *under* an unchanged
    /// `cwd`; `--no-cache`, or deleting the file, settles that.
    fn hit(&self, project_dir: &Path, transcripts: &[Transcript]) -> Option<&CacheEntry> {
        let entry = self.entries.get(&key(project_dir))?;
        let newest = transcripts.first()?;
        if entry.newest != Stamp::of(newest) {
            return None;
        }
        if !transcripts
            .iter()
            .any(|transcript| Stamp::of(transcript) == entry.evidence)
        {
            return None;
        }
        if entry.cwd.exists() != entry.cwd_existed {
            return None;
        }
        if entry.root.from_git && !entry.root.path.join(".git").exists() {
            return None;
        }
        Some(entry)
    }

    fn record(&mut self, project_dir: &Path, newest: &Transcript, resolution: &Resolution) {
        self.entries.insert(
            key(project_dir),
            CacheEntry {
                newest: Stamp::of(newest),
                evidence: resolution.evidence.clone(),
                cwd: resolution.cwd.clone(),
                cwd_existed: resolution.cwd_existed,
                root: resolution.root.clone(),
            },
        );
    }

    /// Write the freshly built cache, which by construction holds no entry for
    /// a project directory that has gone away.
    fn save(&mut self, path: &Path, previous: &Cache) {
        self.version = CACHE_VERSION;
        if self.entries == previous.entries && previous.version == CACHE_VERSION {
            return;
        }
        let Ok(value) = serde_json::to_value(self) else {
            return;
        };
        // Concurrent panels are last-writer-wins on a complete file; a torn
        // cache would be read back as no cache at all, which is still correct.
        let _ = crate::write_atomically(path, &value);
    }
}

fn key(project_dir: &Path) -> String {
    project_dir.to_string_lossy().into_owned()
}

fn mtime_ns(transcript: &Transcript) -> u64 {
    transcript
        .mtime
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64)
        .unwrap_or_default()
}

impl CacheEntry {
    /// What this entry answers, once `hit` has established it still holds.
    fn resolution(&self) -> Resolution {
        Resolution {
            cwd: self.cwd.clone(),
            cwd_existed: self.cwd_existed,
            root: self.root.clone(),
            evidence: self.evidence.clone(),
        }
    }
}

impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.from_git == other.from_git
    }
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

impl Index {
    /// The whole index as one JSON document, for `resolve --json` and for
    /// anything downstream that would rather not re-derive it.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "repos": self.repos.iter().map(|repo| serde_json::json!({
                "root": repo.root,
                "stores": repo.stores,
                "project_dirs": repo.project_dirs.iter().map(|dir| serde_json::json!({
                    "path": dir.path,
                    "cwd": dir.cwd,
                    "transcripts": dir.transcripts.iter().map(|transcript| serde_json::json!({
                        "path": transcript.path,
                        "mtime_ns": mtime_ns(transcript),
                        "len": transcript.len,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "global_stores": self.global_stores,
            "unresolved": self.unresolved.iter().map(|store| serde_json::json!({
                "store_dir": store.store_dir,
                "slug": store.slug,
            })).collect::<Vec<_>>(),
        })
    }

    /// A plain-text report: repositories with their stores and evidence, then
    /// the stores nothing can name.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for repo in &self.repos {
            // The dream's input is the repository's whole history, worktrees
            // included, so that total is the number worth leading with.
            let history = repo_transcripts(repo);
            let bytes: u64 = history.iter().map(|transcript| transcript.len).sum();
            out.push_str(&format!(
                "{}  ({}, {} KiB)\n",
                repo.root.display(),
                plural(history.len(), "transcript"),
                bytes / 1024
            ));
            if repo.stores.is_empty() {
                out.push_str("  store        (none)\n");
            }
            for store in &repo.stores {
                out.push_str(&format!("  store        {}\n", store.display()));
            }
            for dir in &repo.project_dirs {
                out.push_str(&format!(
                    "  transcripts  {:>3}  {}\n",
                    dir.transcripts.len(),
                    dir.path.display()
                ));
                if normalise(&dir.cwd) != repo.root {
                    out.push_str(&format!("               cwd  {}\n", dir.cwd.display()));
                }
            }
            out.push('\n');
        }
        for store in &self.global_stores {
            out.push_str(&format!(
                "user-scope store (not repository-specific)\n  store        {}\n\n",
                store.display()
            ));
        }
        if self.unresolved.is_empty() {
            out.push_str("no unresolvable stores\n");
        } else {
            out.push_str("unresolvable — transcripts swept, project unknowable\n");
            for store in &self.unresolved {
                out.push_str(&format!("  {}\n", store.store_dir.display()));
            }
        }
        out.push_str(&format!(
            "\n{} across {}\n",
            plural(all_stores(self).len(), "store"),
            plural(self.repos.len(), "repository")
        ));
        out
    }
}

fn plural(count: usize, noun: &str) -> String {
    match (count, noun) {
        (1, _) => format!("{count} {noun}"),
        (_, "repository") => format!("{count} repositories"),
        _ => format!("{count} {noun}s"),
    }
}

/// Every transcript belonging to a repository, newest first.
///
/// The dream reads history per repository, and a worktree's transcripts live
/// in their own project directory while its memories stay in the parent store.
pub fn repo_transcripts(repo: &Repo) -> Vec<Transcript> {
    let mut all: Vec<Transcript> = repo
        .project_dirs
        .iter()
        .flat_map(|dir| dir.transcripts.iter().cloned())
        .collect();
    all.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    all
}

/// Distinct memory stores in this index, resolved and unresolved alike.
///
/// The doctor checks every store it can find; whether its project has a name
/// is a separate finding.
pub fn all_stores(index: &Index) -> Vec<PathBuf> {
    let stores: BTreeSet<PathBuf> = index
        .repos
        .iter()
        .flat_map(|repo| repo.stores.iter().cloned())
        .chain(index.global_stores.iter().cloned())
        .chain(index.unresolved.iter().map(|store| store.store_dir.clone()))
        .collect();
    stores.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// A `<config>` tree plus the working directories transcripts point at.
    ///
    /// Hand-rolled rather than pulled from `tempfile`: the crate's current
    /// release needs a newer toolchain than this plugin builds with, and a
    /// unique directory that deletes itself is twenty lines.
    struct Fixture {
        temp: PathBuf,
    }

    impl Fixture {
        fn new() -> Fixture {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "herdr-claude-memories-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let temp = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&temp);
            std::fs::create_dir_all(&temp).expect("create fixture");
            Fixture { temp }
        }

        fn root(&self) -> PathBuf {
            // Canonicalised so assertions comparing against resolved paths are
            // not defeated by /tmp being a symlink to /private/tmp on macOS.
            std::fs::canonicalize(&self.temp).expect("canonicalize")
        }

        fn config(&self) -> PathBuf {
            self.root().join(".claude")
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.root().join(relative);
            std::fs::create_dir_all(&path).expect("create dir");
            path
        }

        /// A project directory holding one transcript that records `cwd`.
        fn project(&self, slug: &str, cwd: &Path) -> PathBuf {
            let dir = self.dir(&format!(".claude/projects/{slug}"));
            self.transcript(&dir, "session.jsonl", cwd);
            dir
        }

        fn transcript(&self, project_dir: &Path, name: &str, cwd: &Path) -> PathBuf {
            let path = project_dir.join(name);
            let body = format!(
                "{}\n{}\n",
                r#"{"type":"queue-operation","operation":"enqueue"}"#,
                serde_json::json!({ "type": "user", "cwd": cwd.to_string_lossy() })
            );
            std::fs::write(&path, body).expect("write transcript");
            path
        }

        fn store(&self, project_dir: &Path) -> PathBuf {
            let store = project_dir.join(MEMORY_DIR);
            std::fs::create_dir_all(&store).expect("create store");
            store
        }

        fn index(&self) -> Index {
            index(&self.config(), None)
        }

        fn cache(&self) -> PathBuf {
            let path = self.root().join("state/resolution.json");
            std::fs::create_dir_all(path.parent().expect("parent")).expect("state dir");
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // A worktree fixture leaves a `.git` file pointing into the repo;
            // nothing here is read-only, so a plain recursive remove is enough.
            let _ = std::fs::remove_dir_all(&self.temp);
        }
    }

    fn git_at(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    /// A repository with one commit, which is what `git worktree add` needs.
    fn git_repo(dir: &Path) {
        git_at(dir, &["init", "-q"]);
        git_at(
            dir,
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "root",
            ],
        );
    }

    fn touch(path: &Path, at: SystemTime) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open transcript");
        file.set_modified(at).expect("set mtime");
    }

    /// The whole reason this module exists: dashes in a directory name are
    /// indistinguishable from separators, so only the recorded `cwd` is true.
    #[test]
    fn a_dashed_repository_name_resolves_from_cwd_not_the_slug() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/herdr-claude-tasks");
        git_repo(&repo);
        let project = fixture.project("-Code-herdr-claude-tasks", &repo);
        fixture.store(&project);

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1);
        assert_eq!(index.repos[0].root, repo);
        assert_ne!(
            index.repos[0].root,
            fixture.root().join("Code/herdr/claude/tasks")
        );
        assert_eq!(index.repos[0].stores, vec![project.join(MEMORY_DIR)]);
        assert!(index.unresolved.is_empty());
    }

    /// A worktree gets its own project directory for transcripts while its
    /// memories stay in the parent's store, so both must land in one group.
    #[test]
    fn a_worktree_groups_under_the_parent_repository() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let worktree = repo.join(".claude/worktrees/wt");
        git_at(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                worktree.to_str().expect("utf8"),
            ],
        );

        let main = fixture.project("-Code-app", &repo);
        let store = fixture.store(&main);
        let linked = fixture.project("-Code-app--claude-worktrees-wt", &worktree);

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1, "{:#?}", index.repos);
        let group = &index.repos[0];
        assert_eq!(group.root, repo);
        assert_eq!(group.stores, vec![store]);
        let dirs: BTreeSet<&PathBuf> = group.project_dirs.iter().map(|dir| &dir.path).collect();
        assert_eq!(dirs, BTreeSet::from([&main, &linked]));
        assert_eq!(repo_transcripts(group).len(), 2);
    }

    /// Stores outlive the transcripts that could name them. Unresolvable is a
    /// steady state, and guessing here would be worse than saying so.
    #[test]
    fn a_store_with_no_transcripts_is_unresolved() {
        let fixture = Fixture::new();
        let project = fixture.dir(".claude/projects/-Code-swept");
        let store = fixture.store(&project);

        let index = fixture.index();
        assert!(index.repos.is_empty());
        assert_eq!(index.unresolved.len(), 1);
        assert_eq!(index.unresolved[0].store_dir, store);
        assert_eq!(index.unresolved[0].slug, "-Code-swept");
    }

    /// A project directory with neither evidence nor a store is nothing at all.
    #[test]
    fn an_empty_project_directory_without_a_store_is_dropped() {
        let fixture = Fixture::new();
        fixture.dir(".claude/projects/-Code-empty");

        let index = fixture.index();
        assert!(index.repos.is_empty());
        assert!(index.unresolved.is_empty());
    }

    #[test]
    fn a_cwd_outside_any_repository_resolves_to_itself() {
        let fixture = Fixture::new();
        let plain = fixture.dir("Code");
        let project = fixture.project("-Code", &plain);
        fixture.store(&project);

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1);
        assert_eq!(index.repos[0].root, plain);
    }

    /// A deleted worktree still has transcripts. They resolve to the path as
    /// recorded — no ancestor is probed for a repository to adopt them.
    #[test]
    fn a_vanished_cwd_resolves_to_itself() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let gone = repo.join(".claude/worktrees/deleted");
        fixture.project("-Code-app--claude-worktrees-deleted", &gone);

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1);
        assert_eq!(index.repos[0].root, gone);
    }

    /// A long-lived project should resolve from recent evidence.
    #[test]
    fn the_newest_transcript_wins() {
        let fixture = Fixture::new();
        let old = fixture.dir("Code/old");
        let new = fixture.dir("Code/new");
        let project = fixture.dir(".claude/projects/-Code-somewhere");
        let stale = fixture.transcript(&project, "stale.jsonl", &old);
        let recent = fixture.transcript(&project, "recent.jsonl", &new);
        touch(&stale, UNIX_EPOCH + Duration::from_secs(1_000));
        touch(&recent, UNIX_EPOCH + Duration::from_secs(2_000));

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1);
        assert_eq!(index.repos[0].root, new);
    }

    /// Not every record carries a `cwd`, and a transcript can hold none at all.
    #[test]
    fn a_transcript_without_a_cwd_falls_through_to_the_next() {
        let fixture = Fixture::new();
        let target = fixture.dir("Code/app");
        let project = fixture.dir(".claude/projects/-Code-app");
        let headerless = project.join("headerless.jsonl");
        let filler = "{\"type\":\"queue-operation\"}\n".repeat(SCAN_LINES + 50);
        std::fs::write(&headerless, filler).expect("write");
        let usable = fixture.transcript(&project, "usable.jsonl", &target);
        touch(&headerless, UNIX_EPOCH + Duration::from_secs(2_000));
        touch(&usable, UNIX_EPOCH + Duration::from_secs(1_000));

        let index = fixture.index();
        assert_eq!(index.repos.len(), 1);
        assert_eq!(index.repos[0].root, target);
    }

    /// `autoMemoryDirectory` moves a store wholesale, so globbing the default
    /// location alone is an incomplete scan by design.
    #[test]
    fn a_user_scope_relocated_store_is_discovered() {
        let fixture = Fixture::new();
        let relocated = fixture.dir("my-memories");
        std::fs::create_dir_all(fixture.config()).expect("config");
        std::fs::write(
            fixture.config().join("settings.json"),
            serde_json::json!({ "autoMemoryDirectory": relocated }).to_string(),
        )
        .expect("write settings");

        let index = fixture.index();
        assert_eq!(index.global_stores, vec![relocated.clone()]);
        assert!(all_stores(&index).contains(&relocated));
    }

    /// A repository's own local settings say where *its* store is, and Claude
    /// Code writes there instead of the default location.
    #[test]
    fn a_repository_scope_relocated_store_replaces_the_default() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let relocated = fixture.dir("Code/app-memories");
        std::fs::create_dir_all(repo.join(".claude")).expect("claude dir");
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            serde_json::json!({ "autoMemoryDirectory": relocated }).to_string(),
        )
        .expect("write settings");
        let project = fixture.project("-Code-app", &repo);
        fixture.store(&project);

        let index = fixture.index();
        assert_eq!(index.repos[0].stores, vec![relocated]);
    }

    /// The cache is a memo over the transcript that produced each answer:
    /// unchanged mtime is a hit, a newer one re-reads.
    #[test]
    fn the_cache_is_reused_and_invalidated_by_mtime() {
        let fixture = Fixture::new();
        let first = fixture.dir("Code/first");
        let second = fixture.dir("Code/second");
        let project = fixture.dir(".claude/projects/-Code-somewhere");
        let transcript = fixture.transcript(&project, "session.jsonl", &first);
        let pinned = UNIX_EPOCH + Duration::from_secs(1_000);
        touch(&transcript, pinned);

        let cache = fixture.cache();
        assert_eq!(index(&fixture.config(), Some(&cache)).repos[0].root, first);
        assert!(cache.exists());

        // Rewrite the evidence but keep the mtime: the cache still answers.
        fixture.transcript(&project, "session.jsonl", &second);
        touch(&transcript, pinned);
        assert_eq!(index(&fixture.config(), Some(&cache)).repos[0].root, first);

        // A newer mtime is what makes it look again.
        touch(&transcript, pinned + Duration::from_secs(1));
        assert_eq!(index(&fixture.config(), Some(&cache)).repos[0].root, second);
    }

    /// The transcript that supplies the `cwd` is not always the newest one, so
    /// keying the entry on the newest alone would let a cache hit outlive the
    /// evidence underneath it and keep naming a repository nothing can prove.
    #[test]
    fn sweeping_the_evidence_invalidates_even_when_the_newest_is_unchanged() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let project = fixture.dir(".claude/projects/-Code-app");
        let store = fixture.store(&project);

        // The newest transcript carries no `cwd`; an older one does.
        let headerless = project.join("headerless.jsonl");
        std::fs::write(&headerless, "{\"type\":\"queue-operation\"}\n").expect("write");
        let evidence = fixture.transcript(&project, "evidence.jsonl", &repo);
        touch(&headerless, UNIX_EPOCH + Duration::from_secs(2_000));
        touch(&evidence, UNIX_EPOCH + Duration::from_secs(1_000));

        let cache = fixture.cache();
        assert_eq!(index(&fixture.config(), Some(&cache)).repos[0].root, repo);

        // Sweep only the evidence. The newest transcript is untouched, so an
        // entry keyed on it alone would still look valid.
        std::fs::remove_file(&evidence).expect("sweep evidence");
        let index = index(&fixture.config(), Some(&cache));
        assert!(index.repos.is_empty(), "{:#?}", index.repos);
        assert_eq!(index.unresolved.len(), 1);
        assert_eq!(index.unresolved[0].store_dir, store);
    }

    /// Git topology changes without any transcript changing — a worktree gets
    /// removed and its `cwd` stops existing. A cached answer that ignored that
    /// would disagree with an uncached one, and the uncached one is right.
    #[test]
    fn a_removed_worktree_invalidates_its_cached_root() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let worktree = repo.join(".claude/worktrees/wt");
        git_at(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                worktree.to_str().expect("utf8"),
            ],
        );
        fixture.project("-Code-app--claude-worktrees-wt", &worktree);

        let cache = fixture.cache();
        assert_eq!(
            index(&fixture.config(), Some(&cache)).repos[0].root,
            repo,
            "a live worktree groups under its parent"
        );

        git_at(&repo, &["worktree", "remove", "--force", "wt"]);
        let cached = index(&fixture.config(), Some(&cache));
        let uncached = index(&fixture.config(), None);
        assert_eq!(
            cached.repos[0].root, worktree,
            "resolves to itself once gone"
        );
        assert_eq!(
            cached.repos[0].root, uncached.repos[0].root,
            "the cache must never disagree with a cold scan"
        );
    }

    /// The hook has to know about a repository-scoped relocated store too, or
    /// a session whose store has moved writes memories nothing toasts for.
    #[test]
    fn a_repository_scope_relocated_store_is_visible_to_the_hook() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        let relocated = fixture.dir("Code/app-memories");
        std::fs::create_dir_all(repo.join(".claude")).expect("claude dir");
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            serde_json::json!({ "autoMemoryDirectory": relocated }).to_string(),
        )
        .expect("write settings");

        assert_eq!(relocated_store(&repo), Some(relocated));
        assert_eq!(relocated_store(&fixture.dir("Code/plain")), None);
    }

    /// The cache must never resurrect a store whose evidence has been swept.
    #[test]
    fn a_swept_transcript_drops_its_cache_entry() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let project = fixture.project("-Code-app", &repo);
        let store = fixture.store(&project);

        let cache = fixture.cache();
        assert_eq!(index(&fixture.config(), Some(&cache)).repos[0].root, repo);

        std::fs::remove_file(project.join("session.jsonl")).expect("sweep");
        let index = index(&fixture.config(), Some(&cache));
        assert!(index.repos.is_empty());
        assert_eq!(index.unresolved.len(), 1);
        assert_eq!(index.unresolved[0].store_dir, store);

        // And the pruned entry is gone from the file, not merely ignored.
        let raw = std::fs::read_to_string(&cache).expect("read cache");
        assert!(!raw.contains("-Code-app"), "{raw}");
    }

    /// Resolution reads; it never writes to a store. The cache is the only
    /// file this module creates.
    #[test]
    fn resolving_does_not_touch_a_store() {
        let fixture = Fixture::new();
        let repo = fixture.dir("Code/app");
        git_repo(&repo);
        let project = fixture.project("-Code-app", &repo);
        let store = fixture.store(&project);
        let memory = store.join("user_role.md");
        std::fs::write(&memory, "---\nname: x\n---\n").expect("write memory");
        let before = std::fs::metadata(&memory).expect("meta").modified().ok();

        let cache = fixture.cache();
        index(&fixture.config(), Some(&cache));

        assert_eq!(
            std::fs::read_to_string(&memory).expect("read"),
            "---\nname: x\n---\n"
        );
        assert_eq!(
            std::fs::metadata(&memory).expect("meta").modified().ok(),
            before
        );
        assert_eq!(
            std::fs::read_dir(&store).expect("read store").count(),
            1,
            "nothing was added to the store"
        );
    }
}
