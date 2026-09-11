//! The machine-wide corpus pane.
//!
//! `/memory` only ever shows the project you are standing in, so nothing else
//! on the machine can show the corpus as a whole. This is that view: one row
//! per repository, plus the stores that belong to no repository and the ones
//! whose project can no longer be named.
//!
//! Two halves, and the useful one has no terminal in it. [`rows`] turns a
//! [`resolution::Index`] into what the pane displays and is pure with respect
//! to the corpus — it reads, and that is all it does. The rest is a `ratatui`
//! shell around it.
//!
//! Store discovery belongs to `resolution` and stays there. This module never
//! walks `~/.claude/projects` and never resolves anything itself.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row as TableRow, Table};
use ratatui::{DefaultTerminal, Frame};

use crate::resolution;

/// `MEMORY.md` is truncated to this many lines at session start, and anything
/// past it is dropped in silence. The pane reports the distance; judging it is
/// #13's job.
const LINE_CAP: usize = 200;

/// The byte half of the same cap.
const BYTE_CAP: u64 = 25 * 1024;

/// The index file that sits alongside the topic files in every store.
const INDEX_FILE: &str = "MEMORY.md";

/// Where a dream leaves proposals, under `HERDR_PLUGIN_STATE_DIR`.
const DREAMS_DIR: &str = "dreams";

// ---------------------------------------------------------------------------
// the part worth testing
// ---------------------------------------------------------------------------

/// What kind of thing a row describes.
///
/// A store with no repository is not a lesser store; it is a different fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Backed by a git repository, or by a `cwd` that stands in for one.
    Repository,
    /// `autoMemoryDirectory` at user scope, belonging to no single project.
    UserScope,
    /// Every transcript swept, so nothing can name the project any more.
    Unresolved,
}

/// Lines and bytes of a store's `MEMORY.md`, against the startup cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexSize {
    pub lines: usize,
    pub bytes: u64,
}

/// One line of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: Kind,
    /// The repository root, or the store directory when nothing can name it.
    pub label: PathBuf,
    pub store: Option<PathBuf>,
    /// Topic files in the store. `MEMORY.md` is the index, not a memory.
    pub memories: usize,
    /// `None` when the store has no `MEMORY.md` at all.
    pub index: Option<IndexSize>,
    /// A dream proposal is waiting for review.
    pub proposal: bool,
}

impl Row {
    /// Whether this row describes a store with nothing in it.
    ///
    /// Five of the eight stores on the author's machine are bare `memory/`
    /// directories. That is Claude Code creating a directory, not a defect, so
    /// this exists to render them plainly — never to flag them.
    pub fn is_empty_store(&self) -> bool {
        self.memories == 0 && self.index.is_none()
    }
}

/// Every store in the index, as rows.
///
/// `state_dir` is `HERDR_PLUGIN_STATE_DIR`; `None` simply means no proposals
/// can be pending, which is the state until #5 lands.
pub fn rows(index: &resolution::Index, state_dir: Option<&Path>) -> Vec<Row> {
    let mut out = Vec::new();

    for repo in &index.repos {
        let proposal = has_proposal(state_dir, &repo.root);
        // A repository with no store still belongs in the list: it is a project
        // that has never saved a memory, which is a fact about the corpus.
        if repo.stores.is_empty() {
            out.push(Row {
                kind: Kind::Repository,
                label: repo.root.clone(),
                store: None,
                memories: 0,
                index: None,
                proposal,
            });
            continue;
        }
        for store in &repo.stores {
            let (memories, index_size) = store_facts(store);
            out.push(Row {
                kind: Kind::Repository,
                label: repo.root.clone(),
                store: Some(store.clone()),
                memories,
                index: index_size,
                proposal,
            });
        }
    }

    for store in &index.global_stores {
        let (memories, index_size) = store_facts(store);
        out.push(Row {
            kind: Kind::UserScope,
            label: store.clone(),
            store: Some(store.clone()),
            memories,
            index: index_size,
            proposal: false,
        });
    }

    for store in &index.unresolved {
        let (memories, index_size) = store_facts(&store.store_dir);
        out.push(Row {
            // Labelled by the directory it actually is. Naming a repository for
            // it would be the wrong-but-plausible answer `resolution` exists to
            // avoid.
            kind: Kind::Unresolved,
            label: store.store_dir.clone(),
            store: Some(store.store_dir.clone()),
            memories,
            index: index_size,
            proposal: false,
        });
    }

    out
}

/// Count the topic files in a store and measure its index.
fn store_facts(store: &Path) -> (usize, Option<IndexSize>) {
    let Ok(entries) = std::fs::read_dir(store) else {
        return (0, None);
    };

    let mut memories = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == INDEX_FILE || !name.ends_with(".md") {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            memories += 1;
        }
    }

    let index = std::fs::read_to_string(store.join(INDEX_FILE))
        .ok()
        .map(|body| IndexSize {
            lines: body.lines().count(),
            bytes: body.len() as u64,
        });

    (memories, index)
}

/// The directory name a dream's proposals live under for a repository.
///
/// Borrowed from Claude Code's own project slug: an absolute path flattened
/// into one component. It is lossy in the same way and for the same reason —
/// nothing ever reverses it, and both sides of the contract compute it here.
pub fn dream_key(root: &Path) -> String {
    root.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

/// Whether a dream has left a proposal for this repository.
fn has_proposal(state_dir: Option<&Path>, root: &Path) -> bool {
    let Some(state_dir) = state_dir else {
        return false;
    };
    let repo_dreams = state_dir.join(DREAMS_DIR).join(dream_key(root));
    std::fs::read_dir(repo_dreams).is_ok_and(|mut entries| {
        entries.any(|entry| entry.is_ok_and(|e| e.file_type().is_ok_and(|k| k.is_dir())))
    })
}

// ---------------------------------------------------------------------------
// presentation
// ---------------------------------------------------------------------------

/// The line under the table.
///
/// `d` and `enter` are named but unbound: #5 and #6 deliver them, and a key
/// whose only job is to apologise is worse than a key that is visibly not here
/// yet.
const FOOTER: &str = "r rescan · q close · d dream (#5) · enter review (#6)";

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Repository => "repo",
            Kind::UserScope => "user",
            Kind::Unresolved => "dead",
        }
    }
}

/// The cells of one row, in column order.
///
/// Separate from rendering so the wording is checkable without a terminal.
fn cells(row: &Row) -> [String; 5] {
    let memories = match row.store {
        None => "no store".to_string(),
        Some(_) if row.is_empty_store() => "empty".to_string(),
        Some(_) => format!("{} memories", row.memories),
    };
    let index = match row.index {
        None => String::new(),
        Some(size) => format!(
            "{}/{} lines · {}/{} KiB",
            size.lines,
            LINE_CAP,
            size.bytes / 1024,
            BYTE_CAP / 1024
        ),
    };
    [
        row.kind.tag().to_string(),
        row.label.display().to_string(),
        memories,
        index,
        match row.proposal {
            true => "proposal".to_string(),
            false => String::new(),
        },
    ]
}

fn draw(frame: &mut Frame, rows: &[Row]) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    let header = TableRow::new(["", "repository / store", "memories", "index", ""])
        .style(Style::new().add_modifier(Modifier::BOLD));

    let table_rows: Vec<TableRow> = rows
        .iter()
        .map(|row| TableRow::new(cells(row).map(Cell::from)))
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Min(20),
        Constraint::Length(13),
        Constraint::Length(26),
        Constraint::Length(8),
    ];

    let title = format!(" Claude memories — {} ", summary(rows));
    frame.render_widget(
        Table::new(table_rows, widths)
            .header(header)
            .block(Block::bordered().title(title)),
        body,
    );
    frame.render_widget(
        Paragraph::new(Line::from(FOOTER)).style(Style::new().add_modifier(Modifier::DIM)),
        footer,
    );
}

/// The rows as plain text, for when there is no terminal to draw on.
///
/// Built from the same [`cells`] as the table so a pipe shows what the pane
/// shows, rather than something merely adjacent to it.
pub fn render_text(rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows {
        let [kind, label, memories, index, proposal] = cells(row);
        out.push_str(&format!("{kind:<5} {label}\n"));
        out.push_str(&format!("      {memories}"));
        if !index.is_empty() {
            out.push_str(&format!("  ·  {index}"));
        }
        if !proposal.is_empty() {
            out.push_str(&format!("  ·  {proposal}"));
        }
        out.push('\n');
    }
    out.push_str(&format!("\n{}\n", summary(rows)));
    out
}

/// How many stores, across how many repositories.
///
/// A row without a store is a project that has never saved a memory — worth
/// listing, and not a store. Counting rows instead would disagree with
/// `resolve`, which is the same index seen from the other side.
fn summary(rows: &[Row]) -> String {
    let stores = rows.iter().filter(|row| row.store.is_some()).count();
    let repositories: std::collections::BTreeSet<&PathBuf> = rows
        .iter()
        .filter(|row| row.kind == Kind::Repository)
        .map(|row| &row.label)
        .collect();
    format!(
        "{} across {}",
        plural(stores, "store", "stores"),
        plural(repositories.len(), "repository", "repositories")
    )
}

fn plural(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("{count} {one}"),
        _ => format!("{count} {many}"),
    }
}

// ---------------------------------------------------------------------------
// entry points
// ---------------------------------------------------------------------------

/// Build the index and the rows from the live environment.
fn scan(config_dir: &Path) -> Vec<Row> {
    let index = resolution::index(config_dir, resolution::cache_path().as_deref());
    let state_dir = std::env::var("HERDR_PLUGIN_STATE_DIR")
        .ok()
        .map(PathBuf::from);
    rows(&index, state_dir.as_deref())
}

/// Open the pane, or print its rows when there is no terminal to draw on.
///
/// The non-terminal path keeps the subcommand safe to run in a pipe, and is the
/// only way to inspect what the pane is showing without opening it — so it
/// prints the pane's own rows rather than the raw index.
pub fn run(config_dir: &Path) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        print!("{}", render_text(&scan(config_dir)));
        return ExitCode::SUCCESS;
    }

    // `init` installs a panic hook that restores the terminal first. That still
    // runs under `panic = "abort"`, where no `Drop` would.
    let result = ratatui::run(|terminal| event_loop(terminal, config_dir));
    if let Err(err) = result {
        eprintln!("herdr-claude-memories: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn event_loop(terminal: &mut DefaultTerminal, config_dir: &Path) -> std::io::Result<()> {
    let mut rows = scan(config_dir);
    loop {
        terminal.draw(|frame| draw(frame, &rows))?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if !key.is_press() {
            continue;
        }
        match key.code {
            // The corpus is kilobytes, so a rescan is a full rescan. A watcher
            // would be more machinery than the problem deserves.
            KeyCode::Char('r') => rows = scan(config_dir),
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            _ => {}
        }
    }
}

/// Ask herdr to open the pane.
///
/// herdr actions run a process and cannot open a pane declaratively, and a
/// `[[keys.command]]` binding must name a `plugin_action` — so this is how a
/// keybinding reaches the pane at all.
pub fn run_open() -> ExitCode {
    let bin = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string());
    let status = std::process::Command::new(bin)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            "stgerman.claude-memories",
            "--entrypoint",
            "doctor",
        ])
        .status();
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("herdr-claude-memories: herdr exited with {status}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("herdr-claude-memories: could not run herdr: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A `<config>` tree with stores in it, plus a state dir for proposals.
    struct Fixture {
        temp: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture {
                temp: tempfile::tempdir().expect("tempdir"),
            }
        }

        fn root(&self) -> PathBuf {
            // /tmp is a symlink to /private/tmp on macOS, and `resolution`
            // canonicalises, so assertions comparing paths must too.
            std::fs::canonicalize(self.temp.path()).expect("canonicalize")
        }

        fn config(&self) -> PathBuf {
            self.root().join(".claude")
        }

        /// A project directory whose transcript records `cwd`, as `resolution`
        /// expects to find it.
        fn project(&self, slug: &str, cwd: &Path) -> PathBuf {
            let dir = self.config().join("projects").join(slug);
            std::fs::create_dir_all(&dir).expect("project dir");
            let body = format!(
                "{}\n{}\n",
                r#"{"type":"queue-operation","operation":"enqueue"}"#,
                serde_json::json!({ "type": "user", "cwd": cwd.to_string_lossy() })
            );
            std::fs::write(dir.join("session.jsonl"), body).expect("transcript");
            dir
        }

        fn store(&self, project_dir: &Path) -> PathBuf {
            let store = project_dir.join("memory");
            std::fs::create_dir_all(&store).expect("store");
            store
        }

        fn memory(&self, store: &Path, name: &str, body: &str) {
            std::fs::write(store.join(name), body).expect("memory");
        }

        fn index(&self) -> resolution::Index {
            resolution::index(&self.config(), None)
        }
    }

    /// Path -> contents for every file under a directory, so a test can prove
    /// nothing moved.
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => stack.push(path),
                    Ok(kind) if kind.is_file() => {
                        out.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
                    }
                    _ => {}
                }
            }
        }
        out
    }

    fn row_for<'a>(rows: &'a [Row], label: &Path) -> &'a Row {
        rows.iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("no row for {}", label.display()))
    }

    #[test]
    fn every_resolvable_store_becomes_a_row() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        fixture.store(&project);

        let rows = rows(&fixture.index(), None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Repository);
        assert_eq!(rows[0].label, cwd);
    }

    /// Five of the eight stores on the author's machine are bare directories.
    /// Claude Code made them; nothing is wrong with them.
    #[test]
    fn an_empty_store_is_a_row_with_no_findings() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        fixture.store(&project);

        let rows = rows(&fixture.index(), None);
        let row = row_for(&rows, &cwd);

        assert!(row.is_empty_store());
        assert_eq!(row.memories, 0);
        assert_eq!(row.index, None);
        assert_eq!(cells(row)[2], "empty");
        // No marker of any kind: the proposal column is the only other signal
        // a row can carry, and an empty store never sets it.
        assert!(!row.proposal);
    }

    #[test]
    fn memories_are_counted_without_the_index() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        let store = fixture.store(&project);
        fixture.memory(&store, "user_role.md", "---\nname: role\n---\nbody\n");
        fixture.memory(&store, "project_app.md", "---\nname: app\n---\nbody\n");
        fixture.memory(&store, "MEMORY.md", "- [role](user_role.md)\n");
        fixture.memory(&store, "scratch.txt", "not a memory");

        let rows = rows(&fixture.index(), None);

        assert_eq!(row_for(&rows, &cwd).memories, 2);
    }

    #[test]
    fn index_size_is_lines_and_bytes() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        let store = fixture.store(&project);
        let body = "- [a](a.md)\n- [b](b.md)\n- [c](c.md)\n";
        fixture.memory(&store, "MEMORY.md", body);

        let rows = rows(&fixture.index(), None);
        let size = row_for(&rows, &cwd).index.expect("index size");

        assert_eq!(size.lines, 3);
        assert_eq!(size.bytes, body.len() as u64);
        assert_eq!(cells(row_for(&rows, &cwd))[3], "3/200 lines · 0/25 KiB");
    }

    /// Naming a repository for a store nothing can resolve would be exactly the
    /// wrong-but-plausible answer `resolution` exists to refuse.
    #[test]
    fn an_unresolved_store_is_labelled_by_its_directory() {
        let fixture = Fixture::new();
        let orphan = fixture.config().join("projects/-tmp-Code-gone");
        let store = fixture.store(&orphan);
        fixture.memory(&store, "user_role.md", "---\nname: role\n---\n");

        let rows = rows(&fixture.index(), None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, Kind::Unresolved);
        assert_eq!(rows[0].label, store);
        assert_eq!(rows[0].memories, 1);
    }

    #[test]
    fn a_pending_proposal_is_detected() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        fixture.store(&project);
        let index = fixture.index();

        // No state dir at all is the state until #5 lands.
        assert!(!row_for(&rows(&index, None), &cwd).proposal);

        let state = fixture.root().join("state");
        std::fs::create_dir_all(&state).expect("state");
        assert!(!row_for(&rows(&index, Some(&state)), &cwd).proposal);

        let dreams = state
            .join(DREAMS_DIR)
            .join(dream_key(&cwd))
            .join("20260911");
        std::fs::create_dir_all(&dreams).expect("dreams");
        assert!(row_for(&rows(&index, Some(&state)), &cwd).proposal);
    }

    /// A project with transcripts but no `memory/` is worth listing and is not
    /// a store. Counting rows instead made the pane claim ten stores where
    /// `resolve` said eight.
    #[test]
    fn a_repository_without_a_store_is_not_counted_as_one() {
        let fixture = Fixture::new();
        let with_store = fixture.root().join("Code/app");
        let without = fixture.root().join("Code/other");
        std::fs::create_dir_all(&with_store).expect("cwd");
        std::fs::create_dir_all(&without).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &with_store);
        fixture.store(&project);
        fixture.project("-tmp-Code-other", &without);

        let rows = rows(&fixture.index(), None);

        assert_eq!(rows.len(), 2);
        assert_eq!(row_for(&rows, &without).store, None);
        assert_eq!(cells(row_for(&rows, &without))[2], "no store");
        assert_eq!(summary(&rows), "1 store across 2 repositories");
    }

    /// The load-bearing decision in the whole design, asserted rather than
    /// inspected: building the view must leave the corpus byte-identical.
    #[test]
    fn the_pane_writes_nothing() {
        let fixture = Fixture::new();
        let cwd = fixture.root().join("Code/app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let project = fixture.project("-tmp-Code-app", &cwd);
        let store = fixture.store(&project);
        fixture.memory(&store, "user_role.md", "---\nname: role\n---\nbody\n");
        fixture.memory(&store, "MEMORY.md", "- [role](user_role.md)\n");
        let orphan = fixture.store(&fixture.config().join("projects/-tmp-Code-gone"));
        fixture.memory(&orphan, "old.md", "---\nname: old\n---\n");

        let before = snapshot(&fixture.config());
        assert!(before.len() >= 4, "fixture should have files to protect");

        let index = fixture.index();
        let rows = rows(&index, None);
        let _ = rows.iter().map(cells).collect::<Vec<_>>();

        assert_eq!(snapshot(&fixture.config()), before);
    }

    #[test]
    fn rows_render_into_a_table() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let rows = vec![Row {
            kind: Kind::Repository,
            label: PathBuf::from("/home/x/Code/app"),
            store: Some(PathBuf::from(
                "/home/x/.claude/projects/-home-x-Code-app/memory",
            )),
            memories: 2,
            index: Some(IndexSize {
                lines: 3,
                bytes: 446,
            }),
            proposal: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(100, 8)).expect("terminal");
        terminal.draw(|frame| draw(frame, &rows)).expect("draw");

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(rendered.contains("/home/x/Code/app"), "{rendered}");
        assert!(rendered.contains("2 memories"), "{rendered}");
        assert!(
            rendered.contains("1 store across 1 repository"),
            "{rendered}"
        );
        assert!(rendered.contains("r rescan"), "{rendered}");
        assert!(rendered.contains("d dream (#5)"), "{rendered}");

        // The pipe must show the pane, not something adjacent to it.
        let text = render_text(&rows);
        assert!(text.contains("/home/x/Code/app"), "{text}");
        assert!(text.contains("2 memories"), "{text}");
        assert!(text.contains("3/200 lines"), "{text}");
        assert!(text.contains("1 store across 1 repository"), "{text}");
    }
}
