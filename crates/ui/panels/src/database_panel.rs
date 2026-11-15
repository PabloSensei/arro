//! The Database tool window: the sources of the project, their schema, the SQL console
//! of each source and the grid of its last result.
//!
//! The window itself knows no database product and no driver: it asks [`database::Support`]
//! for what the project has and starts a helper through it. The helper and its protocol
//! live in `services/database`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use database::{
    BATCH_ROWS, Column, Command, Driver, DriverKind, Event, MAX_RESULT_BYTES, MAX_RESULT_ROWS,
    ResultSet, Secret, Session, Source, SourceId, SourceStore, Suggestion, Support, Table, Value,
    VendorTemplate,
};
use editor_view::{EditorView, RunFiles};
use lang_api::{
    MigrationHistory, MigrationHistoryRows, MigrationKind, MigrationModel, MigrationState,
    MigrationSupport, MigrationTool,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DATABASE_TOOL, Workspace};

/// The panel of the window, for scripts and for tests.
pub struct ActiveDatabase(pub Entity<DatabasePanel>);

impl Global for ActiveDatabase {}

/// The state folder of the project, for the editor that asks whether a `.sql` file is a
/// console and may run it.
#[derive(Clone)]
pub struct DatabaseConsoleRoot(pub PathBuf);

impl Global for DatabaseConsoleRoot {}

/// The support the application gave before the window was built.
static SUPPORT: Mutex<Option<Arc<dyn Support>>> = Mutex::new(None);

/// Puts the language side of the window in place. The application calls it before it opens
/// the project, so no file of the project or of the language is read here.
pub fn set_database_support(support: Arc<dyn Support>) {
    if let Ok(mut slot) = SUPPORT.lock() {
        *slot = Some(support);
    }
}

fn support() -> Option<Arc<dyn Support>> {
    SUPPORT.lock().ok().and_then(|slot| slot.clone())
}

/// The language side of the migrations, put in place before the window opens. It lives in
/// a global so that a window test gives its own project without touching another one.
pub struct ActiveMigrations(pub Option<Arc<dyn MigrationSupport>>);

impl Global for ActiveMigrations {}

/// The migrations support the application gave before the window was built.
static MIGRATIONS: Mutex<Option<Arc<dyn MigrationSupport>>> = Mutex::new(None);

/// Puts the language side of the migrations in place. The window copies it into
/// [`ActiveMigrations`] when it is built.
pub fn set_migration_support(support: Arc<dyn MigrationSupport>) {
    if let Ok(mut slot) = MIGRATIONS.lock() {
        *slot = Some(support);
    }
}

/// The migrations support the application gave, for the global of the window.
pub fn migration_support() -> Option<Arc<dyn MigrationSupport>> {
    MIGRATIONS.lock().ok().and_then(|slot| slot.clone())
}

/// How often the window reads the events of its helpers.
const POLL: Duration = Duration::from_millis(50);
/// The width of a cell of the result grid.
const CELL_WIDTH: f32 = 160.;
/// The width of the tree.
const TREE_WIDTH: f32 = 240.;
/// The height of a field of the form.
const FIELD_HEIGHT: f32 = 26.;
/// How long a result of a double-clicked table may be.
const TABLE_SAMPLE: usize = BATCH_ROWS;

/// One field of the new source form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Url,
    User,
    Password,
    Driver,
}

impl Field {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Url,
            Self::Url => Self::User,
            Self::User => Self::Password,
            Self::Password => Self::Driver,
            Self::Driver => Self::Name,
        }
    }
}

/// The form behind `+`, kept in the memory of the window: the password is never written.
struct Dialog {
    kind: DriverKind,
    module: Option<String>,
    name: TextField,
    url: TextField,
    user: TextField,
    password: TextField,
    driver: TextField,
    field: Field,
}

impl Dialog {
    fn new(kind: DriverKind, vendors: &[VendorTemplate]) -> Self {
        let mut dialog = Self {
            kind,
            module: None,
            name: TextField::default(),
            url: TextField::default(),
            user: TextField::default(),
            password: TextField::default(),
            driver: TextField::default(),
            field: Field::Url,
        };
        dialog.template(vendors);
        dialog
    }

    fn template(&mut self, vendors: &[VendorTemplate]) {
        let Some(vendor) = vendors.iter().find(|vendor| vendor.kind == self.kind) else {
            return;
        };
        let url = vendor
            .url_template
            .replace("{host}", &vendor.host)
            .replace(
                "{port}",
                &vendor.port.map(|port| port.to_string()).unwrap_or_default(),
            )
            .replace("{database}", "database")
            .replace("{file}", "~/test");
        self.url = TextField::from_text(url);
    }

    fn field(&mut self, field: Field) -> &mut TextField {
        match field {
            Field::Name => &mut self.name,
            Field::Url => &mut self.url,
            Field::User => &mut self.user,
            Field::Password => &mut self.password,
            Field::Driver => &mut self.driver,
        }
    }
}

/// A statement the window sends, with the row limit that goes with it.
type SentQuery = (String, Option<usize>);

/// A connection of the window tests: no process, the answers are known in advance.
struct Scripted {
    source: SourceId,
    rows: ResultSet,
    error: Option<String>,
    closed: Arc<AtomicBool>,
    /// The statements the window would send.
    queries: Arc<Mutex<Vec<SentQuery>>>,
}

pub struct DatabasePanel {
    workspace: WeakEntity<Workspace>,
    state_dir: PathBuf,
    focus: FocusHandle,
    sources: Vec<Source>,
    suggestions: Vec<Suggestion>,
    drivers: Vec<Driver>,
    selected: Option<SourceId>,
    tables: HashMap<SourceId, Vec<Table>>,
    expanded: HashSet<(SourceId, String, String)>,
    errors: HashMap<SourceId, String>,
    schema_loading: HashSet<SourceId>,
    result: Option<ResultSet>,
    result_source: Option<SourceId>,
    result_bytes: usize,
    sort: Option<(usize, bool)>,
    selected_cell: Option<(usize, usize)>,
    auto_commit: bool,
    /// The quote of each connected driver, from its `connected` answer.
    quotes: HashMap<SourceId, String>,
    sessions: HashMap<SourceId, Session>,
    query_ids: HashMap<SourceId, u64>,
    queue: VecDeque<(SourceId, String, Option<usize>)>,
    next_id: u64,
    passwords: HashMap<SourceId, Secret>,
    password: TextField,
    password_field: bool,
    status: Option<String>,
    dialog: Option<Dialog>,
    /// The migration files of the project, the checks over them and the actions of their
    /// build plugin.
    migrations: MigrationModel,
    /// The migration node of the tree is open.
    migrations_open: bool,
    /// The history of the connected source was read, so the rows carry a state.
    history_read: bool,
    /// The statements that read the history tables and the rows they gave, one tool at a
    /// time.
    history_queue: VecDeque<MigrationHistory>,
    history_columns: Vec<String>,
    history_rows: Vec<Vec<String>>,
    history_collected: Vec<MigrationHistoryRows>,
    history_pending: Option<(SourceId, u64, MigrationTool)>,
    /// The Clean action waiting for the answer of the user.
    clean_pending: Option<lang_api::MigrationAction>,
    scripted: Option<Scripted>,
    /// Set while the task that reads the answers of the helpers runs: it runs only while
    /// at least one connection is alive.
    polling: bool,
    /// How many times the task read the answers, for the window tests.
    #[cfg(test)]
    poll_ticks: usize,
    /// Set when the window goes away, so the helpers do not outlive it.
    window_closed: Arc<AtomicBool>,
    /// The flag of a scripted connection, raised when the window goes away.
    scripted_closed: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    tree_scroll: ScrollHandle,
    result_scroll: ScrollHandle,
}

impl DatabasePanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        state_dir: PathBuf,
        cx: &mut App,
    ) -> Entity<Self> {
        let panel = cx.new(|cx| {
            let mut panel = Self {
                workspace,
                state_dir: state_dir.clone(),
                focus: cx.focus_handle(),
                sources: Vec::new(),
                suggestions: Vec::new(),
                drivers: Vec::new(),
                selected: None,
                tables: HashMap::new(),
                expanded: HashSet::new(),
                errors: HashMap::new(),
                schema_loading: HashSet::new(),
                result: None,
                result_source: None,
                result_bytes: 0,
                sort: None,
                selected_cell: None,
                auto_commit: true,
                quotes: HashMap::new(),
                sessions: HashMap::new(),
                query_ids: HashMap::new(),
                queue: VecDeque::new(),
                next_id: 1,
                passwords: HashMap::new(),
                password: TextField::default(),
                password_field: false,
                status: None,
                dialog: None,
                migrations: MigrationModel::default(),
                migrations_open: true,
                history_read: false,
                history_queue: VecDeque::new(),
                history_columns: Vec::new(),
                history_rows: Vec::new(),
                history_collected: Vec::new(),
                history_pending: None,
                clean_pending: None,
                scripted: None,
                polling: false,
                #[cfg(test)]
                poll_ticks: 0,
                window_closed: Arc::new(AtomicBool::new(false)),
                scripted_closed: Arc::new(Mutex::new(None)),
                tree_scroll: ScrollHandle::new(),
                result_scroll: ScrollHandle::new(),
            };
            panel.load_sources(cx);
            panel.load_migrations(cx);
            panel.refresh_catalog();
            panel
        });
        panel.update(cx, |panel, cx| {
            let workspace = panel.workspace.clone();
            let closed = panel.window_closed.clone();
            let scripted = panel.scripted_closed.clone();
            // The panel is built while the window is being updated; the hook is added
            // after that update is over.
            cx.defer(move |cx| {
                if let Some(workspace) = workspace.upgrade() {
                    workspace.update(cx, |workspace, _| {
                        workspace.on_close(move || {
                            closed.store(true, Ordering::Relaxed);
                            if let Some(flag) =
                                scripted.lock().ok().and_then(|flag| flag.as_ref().cloned())
                            {
                                flag.store(true, Ordering::Relaxed);
                            }
                        })
                    });
                }
            });
        });
        panel
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The sources of the project, as the tree shows them.
    pub fn sources(&self) -> Vec<Source> {
        self.sources.clone()
    }

    /// The sources that the files of the project suggest.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        self.suggestions.clone()
    }

    /// The drivers the project already has.
    pub fn drivers(&self) -> Vec<Driver> {
        self.drivers.clone()
    }

    /// The tables and views of one source, as the tree shows them.
    pub fn tables(&self, source: &SourceId) -> Vec<Table> {
        self.tables.get(source).cloned().unwrap_or_default()
    }

    /// The last result of the console or of a double-clicked table.
    pub fn result(&self) -> Option<ResultSet> {
        self.result.clone()
    }

    /// The error of the last statement, as the driver wrote it.
    pub fn last_error(&self) -> Option<String> {
        let source = self.selected.as_ref()?;
        self.errors.get(source).cloned()
    }

    /// The `.sql` file of the console of a source.
    pub fn console_path(&self, source: &SourceId) -> Option<PathBuf> {
        self.sources
            .iter()
            .any(|known| &known.id == source)
            .then(|| database::console_path(&self.state_dir, source))
    }

    /// The migrations of the project, as the tree shows them.
    pub fn migrations(&self) -> &MigrationModel {
        &self.migrations
    }

    /// Whether the migration node of the tree is open.
    pub fn migrations_open(&self) -> bool {
        self.migrations_open
    }

    /// The rows of a history table, as a scripted connection would answer, for the window
    /// tests.
    #[cfg(test)]
    pub(crate) fn migration_history_for_test(
        &mut self,
        rows: Vec<MigrationHistoryRows>,
        cx: &mut Context<Self>,
    ) {
        self.apply_history(rows, cx);
    }

    /// Reads the migration files in the background, so the window shows them as soon as
    /// the language knows the project.
    fn load_migrations(&mut self, cx: &mut Context<Self>) {
        let Some(support) = cx
            .try_global::<ActiveMigrations>()
            .and_then(|active| active.0.clone())
        else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { support.model() });
        cx.spawn(async move |this, cx| {
            let model = job.await;
            this.update(cx, |this, cx| {
                this.migrations = model;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sends the statements that read the history tables of the migrations, one tool at a
    /// time, once a connection is up.
    fn request_history(&mut self, source: SourceId, cx: &mut Context<Self>) {
        if self.history_pending.is_some() || !self.history_queue.is_empty() {
            return;
        }
        if !self.sessions.contains_key(&source) || self.migrations.histories.is_empty() {
            return;
        }
        if self.scripted.is_some() {
            return;
        }
        self.history_read = false;
        self.history_collected.clear();
        self.history_queue
            .extend(self.migrations.histories.iter().cloned());
        self.send_next_history(source, cx);
    }

    fn send_next_history(&mut self, source: SourceId, cx: &mut Context<Self>) {
        let Some(history) = self.history_queue.pop_front() else {
            let collected = std::mem::take(&mut self.history_collected);
            self.apply_history(collected, cx);
            return;
        };
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        self.history_columns.clear();
        self.history_rows.clear();
        if session
            .send(Command::Query {
                id,
                sql: history.sql,
                timeout: None,
                max_rows: None,
            })
            .is_ok()
        {
            self.history_pending = Some((source, id, history.tool));
        }
        cx.notify();
    }

    /// Asks the language to match the rows of the history tables against the files, in the
    /// background, and shows the result.
    fn apply_history(&mut self, rows: Vec<MigrationHistoryRows>, cx: &mut Context<Self>) {
        if rows.is_empty() {
            return;
        }
        let Some(support) = cx
            .try_global::<ActiveMigrations>()
            .and_then(|active| active.0.clone())
        else {
            return;
        };
        let job = cx
            .background_executor()
            .spawn(async move { support.with_history(&rows) });
        cx.spawn(async move |this, cx| {
            let model = job.await;
            this.update(cx, |this, cx| {
                this.migrations = model;
                this.history_read = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The rows of the history table the helper answered: the id of the statement tells
    /// them from the statements of the console.
    fn history_event(&mut self, source: &SourceId, event: Event, cx: &mut Context<Self>) -> bool {
        let Some((pending_source, id, tool)) = self.history_pending.clone() else {
            return false;
        };
        if &pending_source != source {
            return false;
        }
        match event {
            Event::Columns {
                id: event_id,
                columns,
            } if event_id == id => {
                self.history_columns = columns.into_iter().map(|column| column.name).collect();
                true
            }
            Event::Rows { id: event_id, rows } if event_id == id => {
                for row in rows {
                    self.history_rows
                        .push(row.iter().map(history_text).collect());
                }
                true
            }
            Event::Done { id: event_id, .. } if event_id == id => {
                self.history_pending = None;
                self.history_collected.push(MigrationHistoryRows {
                    tool,
                    columns: std::mem::take(&mut self.history_columns),
                    rows: std::mem::take(&mut self.history_rows),
                });
                self.send_next_history(source.clone(), cx);
                true
            }
            Event::Error {
                id: event_id,
                message,
                ..
            } if event_id == id => {
                self.history_pending = None;
                self.history_queue.clear();
                self.status = Some(format!(
                    "The history of {} is not read: {message}",
                    tool.name()
                ));
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Runs an action of the migration plugin of the project through the Run window.
    fn run_migration_action(
        &mut self,
        action: &lang_api::MigrationAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(runs) = cx
            .try_global::<crate::script::ActiveRunPanel>()
            .map(|panel| panel.0.clone())
        else {
            return;
        };
        if action.unavailable.is_some() {
            return;
        }
        if let Some(goal) = &action.goal {
            runs.update(cx, |runs, cx| runs.run_goal(goal, cx));
        } else if let Some(task) = &action.task {
            runs.update(cx, |runs, cx| runs.run_task(task, cx));
        } else {
            return;
        }
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(workspace::RUN_TOOL, window, cx)
            })
            .ok();
    }

    /// Opens a migration file or the file an include names.
    fn open_migration(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| workspace.open_file(path, window, cx));
    }

    /// A source with a scripted connection, for the window tests: the tables it answers,
    /// the rows of its first result, its error and the flag its close raises.
    #[cfg(test)]
    pub(crate) fn connect_for_test(
        &mut self,
        source: Source,
        tables: Vec<Table>,
        rows: ResultSet,
        error: Option<String>,
        closed: Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let id = source.id.clone();
        self.sources = vec![source];
        self.selected = Some(id.clone());
        self.tables.insert(id.clone(), tables);
        let _ = database::ensure_console(&self.state_dir, &id);
        if let Ok(mut flag) = self.scripted_closed.lock() {
            *flag = Some(closed.clone());
        }
        self.scripted = Some(Scripted {
            source: id,
            rows,
            error,
            closed,
            queries: Arc::new(Mutex::new(Vec::new())),
        });
        cx.notify();
    }

    /// The answers of a scripted connection, as the helper sends them, for the window
    /// tests.
    #[cfg(test)]
    pub(crate) fn connected_for_test(
        &mut self,
        source: SourceId,
        product: &str,
        quote: &str,
        cx: &mut Context<Self>,
    ) {
        self.event(
            source,
            Event::Connected {
                id: 0,
                product: product.to_owned(),
                auto_commit: true,
                quote: quote.to_owned(),
            },
            cx,
        );
    }

    /// The statements a scripted connection was asked to run, with their row limit.
    #[cfg(test)]
    pub(crate) fn queries_for_test(&self) -> Vec<SentQuery> {
        self.scripted
            .as_ref()
            .and_then(|scripted| scripted.queries.lock().ok().map(|queries| queries.clone()))
            .unwrap_or_default()
    }

    /// The application puts the sources, the drivers and the suggestions in place.
    fn refresh_catalog(&mut self) {
        let Some(support) = support() else {
            return;
        };
        self.drivers = support.drivers();
        self.suggestions = support.suggestions();
    }

    fn load_sources(&mut self, cx: &mut Context<Self>) {
        let directory = self.state_dir.clone();
        let job = cx
            .background_executor()
            .spawn(async move { SourceStore::load(&directory) });
        cx.spawn(async move |this, cx| {
            let store = job.await;
            this.update(cx, |this, cx| {
                this.sources = store.sources;
                if this.selected.is_none() {
                    this.selected = this.sources.first().map(|source| source.id.clone());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_sources(&self, cx: &mut Context<Self>) {
        let directory = self.state_dir.clone();
        let store = SourceStore {
            sources: self.sources.clone(),
        };
        let job = cx
            .background_executor()
            .spawn(async move { store.save(&directory) });
        cx.spawn(async move |_, _| {
            let _ = job.await;
        })
        .detach();
    }

    /// Starts the task that reads the answers of the helpers, when a connection needs it.
    fn ensure_poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let more = this.update(cx, |this, cx| {
                    #[cfg(test)]
                    {
                        this.poll_ticks += 1;
                    }
                    this.poll(cx);
                    // Nothing to read: the task ends and the next connection starts it.
                    let more = !this.sessions.is_empty();
                    if !more {
                        this.polling = false;
                    }
                    more
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// How many times the task that reads the answers has run, for the window tests.
    #[cfg(test)]
    pub(crate) fn poll_ticks_for_test(&self) -> usize {
        self.poll_ticks
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.window_closed.load(Ordering::Relaxed) {
            self.sessions.clear();
            self.query_ids.clear();
            self.passwords.clear();
            self.password = TextField::default();
        }
        let mut received = Vec::new();
        let mut gone = Vec::new();
        for (source, session) in &self.sessions {
            loop {
                match session.events().try_recv() {
                    Ok(event) => received.push((source.clone(), event)),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        gone.push(source.clone());
                        break;
                    }
                }
            }
        }
        for (source, event) in received {
            self.event(source, event, cx);
        }
        for source in gone {
            if self.sessions.remove(&source).is_some() {
                self.errors
                    .insert(source, "The connection to the helper is closed.".into());
                cx.notify();
            }
        }
    }

    fn event(&mut self, source: SourceId, event: Event, cx: &mut Context<Self>) {
        if self.history_event(&source, event.clone(), cx) {
            return;
        }
        match event {
            Event::Ready { .. } => {}
            Event::Connected {
                id: _,
                product,
                auto_commit,
                quote,
            } => {
                self.auto_commit = auto_commit;
                self.quotes.insert(source.clone(), quote);
                self.status = Some(format!("Connected to {product}"));
                self.request_history(source.clone(), cx);
                self.request_schema(source, cx);
            }
            Event::Columns { id, columns } => {
                if self.query_ids.get(&source) == Some(&id) {
                    self.result = Some(ResultSet {
                        columns,
                        rows: Vec::new(),
                        update_count: None,
                        truncated: false,
                    });
                    self.result_source = Some(source.clone());
                    self.result_bytes = 0;
                    self.selected_cell = None;
                    cx.notify();
                }
            }
            Event::Rows { id, rows } => {
                if self.query_ids.get(&source) == Some(&id) {
                    self.add_rows(source, id, rows, cx);
                }
            }
            Event::Done {
                id,
                update_count,
                truncated,
                cancelled,
                ..
            } => {
                if self.query_ids.get(&source) == Some(&id) {
                    self.query_ids.remove(&source);
                    if let Some(result) = &mut self.result {
                        result.truncated |= truncated;
                        if cancelled {
                            result.truncated = true;
                        }
                    } else if update_count.is_some() {
                        self.result = Some(ResultSet {
                            columns: Vec::new(),
                            rows: Vec::new(),
                            update_count,
                            truncated,
                        });
                        self.result_source = Some(source.clone());
                    }
                    cx.notify();
                    self.run_next(cx);
                }
            }
            Event::Error {
                id,
                message,
                cancelled,
                ..
            } => {
                if self.query_ids.get(&source) == Some(&id) {
                    self.query_ids.remove(&source);
                    let text = if cancelled {
                        "The statement was cancelled.".to_owned()
                    } else {
                        message
                    };
                    self.errors.insert(source.clone(), text);
                    self.result = None;
                    cx.notify();
                    self.run_next(cx);
                }
            }
            Event::Schema { schema, .. } => {
                let tables = self.tables.entry(source.clone()).or_default();
                for table in schema.tables {
                    if !tables
                        .iter()
                        .any(|known| known.schema == table.schema && known.name == table.name)
                    {
                        tables.push(table);
                    }
                }
                cx.notify();
            }
            Event::SchemaDone {
                truncated, partial, ..
            } => {
                self.schema_loading.remove(&source);
                if truncated || partial {
                    self.status = Some(if truncated {
                        "The schema is longer than the window reads.".to_owned()
                    } else {
                        "The driver could not describe every index or key.".to_owned()
                    });
                }
                cx.notify();
            }
            Event::Exited { .. } => {
                self.sessions.remove(&source);
                self.errors.insert(
                    source,
                    "The helper has ended; connect the source again.".into(),
                );
                cx.notify();
            }
        }
    }

    fn add_rows(
        &mut self,
        source: SourceId,
        id: u64,
        rows: Vec<Vec<Value>>,
        cx: &mut Context<Self>,
    ) {
        let Some(result) = &mut self.result else {
            return;
        };
        let mut truncated = result.truncated;
        for row in rows {
            if result.rows.len() >= MAX_RESULT_ROWS || self.result_bytes >= MAX_RESULT_BYTES {
                truncated = true;
                break;
            }
            self.result_bytes += row.iter().map(value_bytes).sum::<usize>();
            result.rows.push(row);
        }
        result.truncated |= truncated;
        if truncated {
            if let Some(session) = self.sessions.get(&source) {
                let _ = session.send(Command::Cancel { id });
            }
            self.query_ids.remove(&source);
            self.status = Some(format!(
                "The result is longer than {} rows; only its beginning is shown.",
                MAX_RESULT_ROWS
            ));
        } else if let Some(session) = self.sessions.get(&source) {
            let _ = session.send(Command::Fetch { id });
        }
        cx.notify();
    }

    fn request_schema(&mut self, source: SourceId, cx: &mut Context<Self>) {
        if !self.sessions.contains_key(&source) {
            return;
        }
        let id = self.next();
        if let Some(session) = self.sessions.get(&source)
            && session.send(Command::Schema { id }).is_ok()
        {
            self.schema_loading.insert(source.clone());
            self.tables.insert(source, Vec::new());
        }
        cx.notify();
    }

    fn next(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn selected_source(&self) -> Option<&Source> {
        let selected = self.selected.as_ref()?;
        self.sources.iter().find(|source| &source.id == selected)
    }

    fn select(&mut self, source: SourceId, cx: &mut Context<Self>) {
        self.selected = Some(source.clone());
        self.sort = None;
        self.selected_cell = None;
        if !self.tables.contains_key(&source) && self.sessions.contains_key(&source) {
            self.request_schema(source, cx);
        }
        cx.notify();
    }

    fn jars_of(&self, source: &Source) -> Vec<PathBuf> {
        if let Some(jar) = &source.driver_jar {
            return vec![jar.clone()];
        }
        let matching: Vec<&Driver> = self
            .drivers
            .iter()
            .filter(|driver| driver.kind == source.kind)
            .collect();
        let chosen: Vec<&Driver> = matching
            .iter()
            .copied()
            .filter(|driver| {
                source
                    .module
                    .as_ref()
                    .is_none_or(|module| driver.module.as_ref() == Some(module))
            })
            .collect();
        let chosen = if chosen.is_empty() { matching } else { chosen };
        chosen
            .into_iter()
            .flat_map(|driver| driver.jars.iter().cloned())
            .collect()
    }

    fn connect_source(&mut self, source: Source, password: Secret, cx: &mut Context<Self>) {
        if self.sessions.contains_key(&source.id) || self.scripted.is_some() {
            return;
        }
        let jars = self.jars_of(&source);
        if jars.is_empty() {
            self.errors.insert(
                source.id.clone(),
                "No driver in the dependencies of the project; add one or choose its jar.".into(),
            );
            cx.notify();
            return;
        }
        let Some(support) = support() else {
            self.errors.insert(
                source.id.clone(),
                "The language support of the Database window is not ready.".into(),
            );
            cx.notify();
            return;
        };
        let id = self.next();
        self.status = Some("Connecting…".into());
        let requested = source.clone();
        let job = cx.background_executor().spawn(async move {
            let launch = support.helper(&jars)?;
            let session = Session::start(launch).map_err(|error| error.to_string())?;
            session
                .send(Command::Connect {
                    id,
                    url: requested.url,
                    user: requested.user,
                    password,
                    properties: Vec::new(),
                })
                .map_err(|error| error.to_string())?;
            Ok::<Session, String>(session)
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.status = None;
                match result {
                    Ok(session) => {
                        this.sessions.insert(source.id.clone(), session);
                        this.errors.remove(&source.id);
                        this.ensure_poll(cx);
                    }
                    Err(error) => {
                        this.errors.insert(source.id.clone(), error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn disconnect(&mut self, source: SourceId, cx: &mut Context<Self>) {
        self.sessions.remove(&source);
        self.query_ids.remove(&source);
        self.quotes.remove(&source);
        self.schema_loading.remove(&source);
        self.history_queue.clear();
        self.history_pending = None;
        self.history_collected.clear();
        self.history_read = false;
        cx.notify();
    }

    fn run_sql(
        &mut self,
        source: SourceId,
        text: String,
        max_rows: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if let Some(scripted) = &self.scripted {
            if scripted.source != source {
                return;
            }
            if let Ok(mut queries) = scripted.queries.lock() {
                queries.push((text.clone(), max_rows));
            }
            match &scripted.error {
                Some(error) => {
                    self.errors.insert(source, error.clone());
                    self.result = None;
                }
                None => {
                    self.errors.remove(&source);
                    self.result = Some(scripted.rows.clone());
                    self.result_source = Some(scripted.source.clone());
                    self.result_bytes = 0;
                    self.selected_cell = None;
                }
            }
            cx.notify();
            return;
        }
        if let Some(session) = self.sessions.get(&source) {
            if let Some(active) = self.query_ids.get(&source).copied() {
                let _ = session.send(Command::Cancel { id: active });
            }
        } else {
            self.errors.insert(
                source.clone(),
                "Connect the source before running its console.".into(),
            );
            cx.notify();
            return;
        }
        let statements = database::split_statements(&text);
        self.queue.clear();
        for statement in statements {
            self.queue.push_back((source.clone(), statement, max_rows));
        }
        self.run_next(cx);
    }

    fn run_next(&mut self, cx: &mut Context<Self>) {
        while let Some((source, sql, max_rows)) = self.queue.pop_front() {
            if !self.sessions.contains_key(&source) {
                continue;
            }
            let id = self.next();
            let Some(session) = self.sessions.get(&source) else {
                continue;
            };
            self.errors.remove(&source);
            self.result = None;
            self.result_source = Some(source.clone());
            self.result_bytes = 0;
            self.selected_cell = None;
            if session
                .send(Command::Query {
                    id,
                    sql,
                    timeout: None,
                    max_rows,
                })
                .is_ok()
            {
                self.query_ids.insert(source, id);
                cx.notify();
                return;
            }
        }
    }

    fn sample_table(&mut self, source: SourceId, table: Table, cx: &mut Context<Self>) {
        let quote = self.quotes.get(&source).map_or("\"", String::as_str);
        let name = if table.schema.is_empty() {
            quote_identifier(&table.name, quote)
        } else {
            format!(
                "{}.{}",
                quote_identifier(&table.schema, quote),
                quote_identifier(&table.name, quote)
            )
        };
        // The row limit belongs to the statement, so the text runs on every dialect.
        self.run_sql(
            source,
            format!("SELECT * FROM {name}"),
            Some(TABLE_SAMPLE),
            cx,
        );
    }

    fn toggle_table(&mut self, source: SourceId, table: (String, String), cx: &mut Context<Self>) {
        let key = (source, table.0, table.1);
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    fn open_console(&mut self, source: SourceId, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.state_dir.clone();
        let console = source.clone();
        let job = cx
            .background_executor()
            .spawn(async move { database::ensure_console(&directory, &console) });
        let Some(handle) = window.window_handle().downcast::<Workspace>() else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let Ok(path) = job.await else {
                return;
            };
            handle
                .update(cx, |workspace, window, cx| {
                    workspace.open_file(path, window, cx);
                })
                .ok();
        })
        .detach();
    }

    fn toggle_auto_commit(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        if session.send(Command::AutoCommit { id, on }).is_ok() {
            self.auto_commit = on;
            cx.notify();
        }
    }

    fn commit(&mut self, rollback: bool, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        let command = if rollback {
            Command::Rollback { id }
        } else {
            Command::Commit { id }
        };
        let _ = session.send(command);
        cx.notify();
    }

    fn add_source(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let url = dialog.url.text.trim().to_owned();
        if url.is_empty() {
            self.status = Some("The address of a source is required.".into());
            cx.notify();
            return;
        }
        let mut name = dialog.name.text.trim().to_owned();
        if name.is_empty() {
            name = source_name(&url);
        }
        let id = self.unique_id(&name);
        let driver = dialog.driver.text.trim();
        let source = Source {
            id: id.clone(),
            name,
            kind: dialog.kind,
            url,
            user: dialog.user.text.trim().to_owned(),
            module: dialog.module,
            driver_jar: (!driver.is_empty()).then(|| PathBuf::from(driver)),
        };
        if !dialog.password.text.is_empty() {
            self.passwords
                .insert(id.clone(), Secret::new(dialog.password.text.clone()));
        }
        self.sources.push(source.clone());
        self.selected = Some(id.clone());
        self.save_sources(cx);
        let password = self
            .passwords
            .get(&id)
            .cloned()
            .unwrap_or_else(|| Secret::new(""));
        self.connect_source(source, password, cx);
    }

    fn unique_id(&self, name: &str) -> SourceId {
        let base = slug(name);
        let mut id = base.clone();
        let mut serial = 2;
        while self.sources.iter().any(|source| source.id.as_str() == id) {
            id = format!("{base}-{serial}");
            serial += 1;
        }
        SourceId::new(id)
    }

    fn connect_selected(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected_source().cloned() else {
            return;
        };
        self.refresh_catalog();
        let password = self
            .passwords
            .get(&source.id)
            .cloned()
            .unwrap_or_else(|| Secret::new(self.password.text.clone()));
        self.connect_source(source, password, cx);
    }

    fn copy_cell(&mut self, cx: &mut Context<Self>) {
        let Some((row, column)) = self.selected_cell else {
            return;
        };
        let Some(result) = &self.result else {
            return;
        };
        let Some(value) = result.rows.get(row).and_then(|row| row.get(column)) else {
            return;
        };
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(value_text(value)));
    }

    fn copy_row(&mut self, cx: &mut Context<Self>) {
        let Some((row, _)) = self.selected_cell else {
            return;
        };
        let Some(result) = &self.result else {
            return;
        };
        let Some(values) = result.rows.get(row) else {
            return;
        };
        let text = values.iter().map(value_text).collect::<Vec<_>>().join("\t");
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text));
    }

    fn sort_by(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(result) = &mut self.result else {
            return;
        };
        let ascending = !matches!(self.sort, Some((sorted, true)) if sorted == column);
        result.rows.sort_by(|left, right| {
            let left = left.get(column);
            let right = right.get(column);
            compare_values(left, right, ascending)
        });
        self.sort = Some((column, ascending));
        self.selected_cell = None;
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.dialog {
            let stroke = &event.keystroke;
            if stroke.key == "escape" {
                self.dialog = None;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if stroke.key == "enter" {
                self.add_source(cx);
                cx.stop_propagation();
                return;
            }
            if stroke.key == "tab" {
                dialog.field = dialog.field.next();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let field = dialog.field;
            if dialog.field(field).key(event, false) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.password_field {
            if self.password.key(event, false) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let stroke = &event.keystroke;
        if (stroke.modifiers.control || stroke.modifiers.platform) && stroke.key == "c" {
            self.copy_cell(cx);
            cx.stop_propagation();
        }
    }

    fn tree(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = Vec::new();
        rows.extend(self.migration_rows(theme, cx));
        if self.sources.is_empty() {
            rows.push(
                div()
                    .px(px(SPACE_M))
                    .py(px(SPACE_S))
                    .text_color(theme.text_muted)
                    .child("No data sources. Press + to add one.")
                    .into_any_element(),
            );
        }
        for source in &self.sources {
            let selected = self.selected.as_ref() == Some(&source.id);
            let id = source.id.clone();
            let connected = self.sessions.contains_key(&source.id);
            rows.push(
                div()
                    .id(format!("database-source-{}", source.id))
                    .debug_selector({
                        let name = format!("database-source-{}", source.id);
                        move || name.clone()
                    })
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .when(selected, |this| this.bg(theme.tab_selected))
                    .cursor_pointer()
                    .child(icon("toolwindows/services", theme))
                    .child(div().flex_1().overflow_hidden().child(source.name.clone()))
                    .child(
                        div()
                            .text_color(if connected {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .child(if connected { "●" } else { "○" }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .into_any_element(),
            );
            if !selected {
                continue;
            }
            if self.schema_loading.contains(&source.id) {
                rows.push(
                    div()
                        .pl(px(SPACE_L))
                        .text_color(theme.text_muted)
                        .child("Reading the schema…")
                        .into_any_element(),
                );
            }
            let tables = self.tables.get(&source.id).cloned().unwrap_or_default();
            let mut last_schema = None;
            for table in tables {
                if last_schema.as_deref() != Some(table.schema.as_str()) {
                    rows.push(
                        div()
                            .pl(px(SPACE_M))
                            .pt(px(SPACE_XS))
                            .text_color(theme.text_muted)
                            .child(if table.schema.is_empty() {
                                "(no schema)".to_owned()
                            } else {
                                table.schema.clone()
                            })
                            .into_any_element(),
                    );
                    last_schema = Some(table.schema.clone());
                }
                let open = self.expanded.contains(&(
                    source.id.clone(),
                    table.schema.clone(),
                    table.name.clone(),
                ));
                let table_name = table.name.clone();
                let open_source = source.id.clone();
                let open_table = table.clone();
                let toggle = (table.schema.clone(), table.name.clone());
                rows.push(
                    div()
                        .id(format!("database-table-{}", table.name))
                        .debug_selector({
                            let name = format!("database-table-{}", table.name);
                            move || name.clone()
                        })
                        .flex()
                        .items_center()
                        .gap(px(SPACE_XS))
                        .h(px(CONTROL_HEIGHT))
                        .pl(px(SPACE_L))
                        .pr(px(SPACE_S))
                        .rounded(px(CONTROL_RADIUS))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.hover))
                        .child(icon(
                            if open {
                                "general/chevronDown"
                            } else {
                                "general/chevronRight"
                            },
                            theme,
                        ))
                        .child(div().flex_1().overflow_hidden().child(table_name.clone()))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if event.click_count() >= 2 {
                                this.sample_table(open_source.clone(), open_table.clone(), cx);
                            } else {
                                this.toggle_table(open_source.clone(), toggle.clone(), cx);
                            }
                        }))
                        .into_any_element(),
                );
                if !open {
                    continue;
                }
                for column in &table.columns {
                    rows.push(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_XS))
                            .pl(px(SPACE_XL))
                            .py(px(SPACE_XS))
                            .text_size(px(TEXT_SMALL))
                            .child(div().flex_1().child(column_text(column)))
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .child(column.type_name.clone()),
                            )
                            .into_any_element(),
                    );
                }
                for index in &table.indexes {
                    rows.push(
                        div()
                            .pl(px(SPACE_XL))
                            .py(px(SPACE_XS))
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "index {} ({}){}",
                                index.name,
                                index.columns.join(", "),
                                if index.unique { ", unique" } else { "" }
                            ))
                            .into_any_element(),
                    );
                }
                for key in &table.foreign_keys {
                    rows.push(
                        div()
                            .pl(px(SPACE_XL))
                            .py(px(SPACE_XS))
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(format!(
                                "foreign key {} ({}) → {} ({})",
                                key.name,
                                key.columns.join(", "),
                                key.referenced_table,
                                key.referenced_columns.join(", ")
                            ))
                            .into_any_element(),
                    );
                }
            }
        }
        scrollable(
            "database-tree",
            &self.tree_scroll,
            div()
                .id("database-tree")
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .px(px(SPACE_XS))
                .overflow_y_scroll()
                .track_scroll(&self.tree_scroll)
                .children(rows),
            theme,
            window,
            cx,
        )
        .into_any_element()
    }

    /// The rows of the migrations above the sources: the tool, its files in the order it
    /// runs them, and the state of each file when a connection read the history table.
    fn migration_rows(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        if self.migrations.is_empty() {
            return rows;
        }
        let tools = self
            .migrations
            .tools
            .iter()
            .map(|tool| tool.name())
            .collect::<Vec<_>>()
            .join(", ");
        let open = self.migrations_open;
        rows.push(
            div()
                .id("database-migrations")
                .debug_selector(|| "database-migrations".to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .hover(|style| style.bg(theme.hover))
                .child(icon(
                    if open {
                        "general/chevronDown"
                    } else {
                        "general/chevronRight"
                    },
                    theme,
                ))
                .child(div().flex_1().overflow_hidden().child("Migrations"))
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(tools),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.migrations_open = !this.migrations_open;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if !open {
            return rows;
        }
        let model = self.migrations.clone();
        for (index, migration) in model.rows.iter().enumerate() {
            let issue = model.issue_of(&migration.path);
            let state = (self.history_read
                && migration.kind != MigrationKind::Changelog
                && migration.kind != MigrationKind::Include)
                .then_some(migration.state);
            let depth = migration.depth as f32;
            let label = migration.label.clone();
            let row_id = format!("database-migration-{index}");
            let selector = if issue.is_some() {
                format!("database-migration-{index}-issue")
            } else {
                row_id.clone()
            };
            let target = migration
                .target
                .clone()
                .filter(|target| target.is_file())
                .unwrap_or_else(|| migration.path.clone());
            let muted = migration.kind == MigrationKind::ChangeSet
                || migration.kind == MigrationKind::Include;
            let row = div()
                .id(row_id)
                .debug_selector(move || selector.clone())
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .h(px(CONTROL_HEIGHT))
                .pl(px(SPACE_S + depth * SPACE_M))
                .pr(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .hover(|style| style.bg(theme.hover))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .when(muted, |this| this.text_color(theme.text_muted))
                        .when(issue.is_some(), |this| {
                            this.text_color(theme.stripe_error).underline()
                        })
                        .child(label),
                )
                .when_some(state, |this, state| {
                    this.child(
                        div()
                            .text_color(match state {
                                MigrationState::Applied => theme.accent,
                                MigrationState::Failed | MigrationState::Changed => {
                                    theme.stripe_error
                                }
                                MigrationState::Pending => theme.text_muted,
                            })
                            .child(state.glyph()),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if !target.exists() {
                        return;
                    }
                    this.open_migration(target.clone(), window, cx);
                }));
            rows.push(row.into_any_element());
        }
        rows
    }

    fn result_table(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(result) = self.result.clone() else {
            let hint = if self.selected.is_some() {
                "Run a statement in the console. Ctrl+Enter takes the statement at the caret."
            } else {
                "Choose a data source."
            };
            return div()
                .flex_1()
                .p(px(SPACE_L))
                .text_color(theme.text_muted)
                .child(hint)
                .into_any_element();
        };
        if result.columns.is_empty() {
            return div()
                .flex_1()
                .p(px(SPACE_L))
                .child(format!(
                    "{} row(s) changed.",
                    result.update_count.unwrap_or_default()
                ))
                .into_any_element();
        }
        let mut header = div()
            .flex()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .bg(theme.tab_selected);
        for (column, found) in result.columns.iter().enumerate() {
            let arrow = match self.sort {
                Some((sorted, true)) if sorted == column => " ▲",
                Some((sorted, false)) if sorted == column => " ▼",
                _ => "",
            };
            let title = format!("{}{arrow}", found.name);
            header = header.child(
                div()
                    .id(("database-head", column))
                    .w(px(CELL_WIDTH))
                    .flex_none()
                    .px(px(SPACE_S))
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| this.sort_by(column, cx))),
            );
        }
        let selected = self.selected_cell;
        let widths = result.columns.len() as f32 * CELL_WIDTH;
        let mut body = div()
            .id("database-result-body")
            .flex()
            .flex_col()
            .min_w(px(widths));
        for (row_index, row) in result.rows.iter().enumerate() {
            let mut line = div()
                .flex()
                .flex_none()
                .when(row_index % 2 == 1, |this| this.bg(theme.hover.opacity(0.3)));
            for (column, value) in row.iter().enumerate() {
                let is_selected = selected == Some((row_index, column));
                let text = value_text(value);
                line = line.child(
                    div()
                        .id(("database-cell", row_index * result.columns.len() + column))
                        .w(px(CELL_WIDTH))
                        .flex_none()
                        .px(px(SPACE_S))
                        .py(px(SPACE_XS))
                        .overflow_hidden()
                        .border_r_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .when(is_selected, |this| this.bg(theme.selection))
                        .when(matches!(value, Value::Null), |this| {
                            this.text_color(theme.text_muted).italic()
                        })
                        .cursor_pointer()
                        .child(text)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_cell = Some((row_index, column));
                            cx.notify();
                        })),
                );
            }
            body = body.child(line);
        }
        let footer = if result.truncated {
            Some(div().text_color(theme.text_muted).child(format!(
                "Showing the beginning of the result, up to {} rows.",
                MAX_RESULT_ROWS
            )))
        } else {
            None
        };
        scrollable(
            "database-result",
            &self.result_scroll,
            div()
                .id("database-result")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_x_scroll()
                .overflow_y_scroll()
                .track_scroll(&self.result_scroll)
                .child(header)
                .child(body)
                .children(footer),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    fn dialog(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = &self.dialog else {
            return div().into_any_element();
        };
        let vendors = support()
            .map(|support| support.vendors())
            .unwrap_or_default();
        let suggestions = self.suggestions.clone();
        let kind = dialog.kind;
        let fields = [
            (Field::Name, "Name", dialog.name.text.clone(), false),
            (Field::Url, "Address", dialog.url.text.clone(), false),
            (Field::User, "User", dialog.user.text.clone(), false),
            (
                Field::Password,
                "Password",
                "•".repeat(dialog.password.text.chars().count()),
                true,
            ),
            (
                Field::Driver,
                "Driver jar",
                dialog.driver.text.clone(),
                false,
            ),
        ];
        let focused = dialog.field;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(SPACE_L))
            .w(px(520.))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popup_bg);
        body = body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child("Data Source")
                .child(
                    small_button("database-dialog-close", "Close", theme).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.dialog = None;
                            cx.notify();
                        },
                    )),
                ),
        );
        let mut kinds = div().flex().items_center().gap(px(SPACE_XS));
        for vendor in &vendors {
            let chosen = vendor.kind == kind;
            let vendor_kind = vendor.kind;
            let vendor_template = vendor.url_template.clone();
            let host = vendor.host.clone();
            let port = vendor.port;
            kinds = kinds.child(
                small_button(
                    ("database-kind", vendor_kind as usize),
                    &vendor.title,
                    theme,
                )
                .when(chosen, |this| this.bg(theme.tab_selected))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(dialog) = &mut this.dialog {
                        dialog.kind = vendor_kind;
                        let url = vendor_template
                            .replace("{host}", &host)
                            .replace(
                                "{port}",
                                &port.map(|port| port.to_string()).unwrap_or_default(),
                            )
                            .replace("{database}", "database")
                            .replace("{file}", "~/test");
                        dialog.url = TextField::from_text(url);
                    }
                    cx.notify();
                })),
            );
        }
        body = body.child(kinds);
        for (field, label, text, _masked) in fields {
            let mut shown = TextField::from_text(text.clone());
            shown.set_caret(shown.text.len());
            let active = field == focused;
            let mut value = div()
                .id(("database-field", field as usize))
                .debug_selector({
                    let name = format!("database-field-{}", field as usize);
                    move || name.clone()
                })
                .flex()
                .items_center()
                .flex_1()
                .h(px(FIELD_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(if active {
                    theme.accent
                } else {
                    theme.control_border
                })
                .overflow_hidden()
                .cursor_text()
                .children(text_field_children(
                    &shown,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    TEXT_SMALL,
                    active,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        if let Some(dialog) = &mut this.dialog {
                            dialog.field = field;
                        }
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }),
                );
            if text.is_empty() && !active {
                value = value.child(div().text_color(theme.text_muted).child(label.to_owned()));
            }
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .w(px(96.))
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(label.to_owned()),
                    )
                    .child(value),
            );
        }
        if !suggestions.is_empty() {
            let mut list = div().flex().flex_col().gap(px(SPACE_XS));
            list = list.child(
                div()
                    .text_color(theme.text_muted)
                    .child("From the project files"),
            );
            for suggestion in suggestions {
                let url = suggestion.url.clone();
                let user = suggestion.user.clone();
                let kind = suggestion.kind;
                let module = suggestion.module.clone();
                let origin = match (module.is_empty(), suggestion.profile.clone()) {
                    (false, Some(profile)) => format!("{module}, {profile}"),
                    (false, None) => module.clone(),
                    (true, Some(profile)) => profile,
                    (true, None) => String::new(),
                };
                let label = if origin.is_empty() {
                    url.clone()
                } else {
                    format!("{url} ({origin})")
                };
                list = list.child(
                    small_button(format!("database-suggestion-{url}"), &label, theme).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if let Some(dialog) = &mut this.dialog {
                                dialog.url = TextField::from_text(url.clone());
                                dialog.user = TextField::from_text(user.clone());
                                dialog.module = (!module.is_empty()).then(|| module.clone());
                                if let Some(kind) = kind {
                                    dialog.kind = kind;
                                }
                            }
                            cx.notify();
                        }),
                    ),
                );
            }
            body = body.child(list);
        }
        let driver = self
            .drivers
            .iter()
            .find(|driver| driver.kind == kind)
            .map(|driver| {
                format!(
                    "{} ({})",
                    driver.label,
                    driver.module.clone().unwrap_or_default()
                )
            })
            .unwrap_or_else(|| {
                "No driver in the dependencies; add one or give the jar path.".to_owned()
            });
        body = body.child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(driver),
        );
        body = body.child(
            div().flex().justify_end().gap(px(SPACE_S)).child(
                small_button("database-dialog-add", "Add", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.add_source(cx))),
            ),
        );
        div()
            .flex_none()
            .p(px(SPACE_M))
            .child(body)
            .into_any_element()
    }

    /// The question behind Clean: the tool drops every object of the schema, which no
    /// undo brings back.
    fn clean_dialog(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(action) = self.clean_pending.clone() else {
            return div().into_any_element();
        };
        let name = self
            .selected
            .as_ref()
            .and_then(|selected| self.sources.iter().find(|known| &known.id == selected))
            .map_or_else(
                || "the database of the project".to_owned(),
                |source| source.name.clone(),
            );
        div()
            .id("database-clean")
            .debug_selector(|| "database-clean".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(SPACE_L))
            .w(px(520.))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.stripe_error)
            .bg(theme.popup_bg)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.stripe_error)
                    .child(format!("{} {}", action.tool.name(), action.kind.label())),
            )
            .child(format!(
                "This drops every object of the schema of {name} and every row of its \
                 history. The data does not come back."
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_S))
                    .child(
                        small_button("database-clean-cancel", "Cancel", theme).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.clean_pending = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        small_button("database-clean-confirm", "Clean", theme)
                            .text_color(theme.stripe_error)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let Some(action) = this.clean_pending.take() else {
                                    return;
                                };
                                this.run_migration_action(&action, window, cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn toolbar(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected.clone();
        let source = self.selected_source().cloned();
        let connected = selected
            .as_ref()
            .is_some_and(|source| self.sessions.contains_key(source));
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border);
        bar = bar.child(
            icon_button("database-add", "general/add", "Data Source", None, theme).on_click(
                cx.listener(|this, _, _, cx| {
                    let vendors = support()
                        .map(|support| support.vendors())
                        .unwrap_or_default();
                    this.refresh_catalog();
                    this.dialog = Some(Dialog::new(DriverKind::H2, &vendors));
                    cx.notify();
                }),
            ),
        );
        bar = bar.child(
            icon_button(
                "database-refresh",
                "general/refresh",
                "Refresh Schema",
                None,
                theme,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.refresh_catalog();
                if let Some(source) = this.selected.clone() {
                    this.request_schema(source, cx);
                }
                cx.notify();
            })),
        );
        if let Some(source) = &selected {
            let id = source.clone();
            bar =
                bar.child(
                    icon_button(
                        "database-console",
                        "general/openInToolWindow",
                        "Open Console",
                        None,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_console(id.clone(), window, cx)
                    })),
                );
        }
        if connected {
            bar = bar.child(
                icon_button("database-disconnect", "run/stop", "Disconnect", None, theme).on_click(
                    cx.listener(|this, _, _, cx| {
                        if let Some(source) = this.selected.clone() {
                            this.disconnect(source, cx);
                        }
                    }),
                ),
            );
            bar = bar.child(
                small_button(
                    "database-autocommit",
                    &format!(
                        "Auto-commit: {}",
                        if self.auto_commit { "on" } else { "off" }
                    ),
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    let on = !this.auto_commit;
                    this.toggle_auto_commit(on, cx);
                })),
            );
            if !self.auto_commit {
                bar = bar.child(
                    small_button("database-commit", "Commit", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.commit(false, cx))),
                );
                bar = bar.child(
                    small_button("database-rollback", "Rollback", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.commit(true, cx))),
                );
            }
        } else if let Some(source) = &source {
            let id = source.id.clone();
            bar = bar.child(
                div()
                    .id("database-password")
                    .debug_selector(|| "database-password".to_owned())
                    .flex()
                    .items_center()
                    .w(px(180.))
                    .h(px(FIELD_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if self.password_field {
                        theme.accent
                    } else {
                        theme.control_border
                    })
                    .cursor_text()
                    .children(text_field_children(
                        &self.password,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        TEXT_SMALL,
                        self.password_field,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.password_field = true;
                            window.focus(&this.focus, cx);
                            cx.notify();
                        }),
                    ),
            );
            bar = bar.child(
                icon_button("database-connect", "run/run", "Connect", None, theme).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.password_field = false;
                        this.connect_selected(cx);
                        let _ = &id;
                    }),
                ),
            );
        }
        bar = bar.child(div().flex_1());
        for (index, action) in self.migrations.actions.clone().iter().enumerate() {
            let reason = action.unavailable.clone();
            let mut button = small_button(
                ("database-migrate", index),
                &format!("{} {}", action.tool.name(), action.kind.label()),
                theme,
            )
            .debug_selector(move || format!("database-migration-action-{index}"));
            if let Some(reason) = reason {
                let hint = SharedString::from(reason.clone());
                button = button
                    .text_color(theme.text_muted)
                    .cursor_default()
                    .tooltip(move |_, cx| tooltip(hint.clone(), None, cx));
            } else {
                button = button.on_click(cx.listener(move |this, _, window, cx| {
                    let Some(action) = this.migrations.actions.get(index).cloned() else {
                        return;
                    };
                    if action.kind == lang_api::MigrationActionKind::Clean {
                        this.clean_pending = Some(action);
                        cx.notify();
                    } else {
                        this.run_migration_action(&action, window, cx);
                    }
                }));
            }
            bar = bar.child(button);
        }
        if let Some(status) = &self.status {
            bar = bar.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(status.clone()),
            );
        }
        bar = bar.child(
            small_button("database-copy-cell", "Copy cell", theme)
                .on_click(cx.listener(|this, _, _, cx| this.copy_cell(cx))),
        );
        bar = bar.child(
            small_button("database-copy-row", "Copy row", theme)
                .on_click(cx.listener(|this, _, _, cx| this.copy_row(cx))),
        );
        let _ = window;
        bar.into_any_element()
    }
}

impl Drop for DatabasePanel {
    fn drop(&mut self) {
        if let Some(scripted) = &self.scripted {
            scripted.closed.store(true, Ordering::Relaxed);
        }
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(DATABASE_TOOL, window, cx)
                })
                .ok();
        };
        let toolbar = self.toolbar(&theme, window, cx);
        let tree = self.tree(&theme, window, cx);
        let result = self.result_table(&theme, window, cx);
        let dialog = self.dialog(&theme, cx);
        let clean = self.clean_dialog(&theme, cx);
        let tree = div()
            .id("database-tree-pane")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(TREE_WIDTH))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(tree);
        let right = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .children(self.last_error().map(|error| {
                div()
                    .debug_selector(|| "database-error".to_owned())
                    .p(px(SPACE_S))
                    .text_color(theme.stripe_error)
                    .child(error)
            }))
            .child(result);
        island(&theme)
            .id("database-panel")
            .debug_selector(|| "database-panel".to_owned())
            .key_context("Database ToolWindow")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .flex()
            .flex_col()
            .h_full()
            .child(panel_header("Database", "bottom-panel", hide, &theme))
            .child(toolbar)
            .child(dialog)
            .child(clean)
            .child(div().flex().flex_1().min_h_0().child(tree).child(right))
    }
}

/// The plain syntax of a console file: no grammar, but Ctrl+Enter belongs to the window.
pub(crate) fn install(state_dir: PathBuf, cx: &mut App) {
    cx.set_global(DatabaseConsoleRoot(state_dir.clone()));
    cx.set_global(RunFiles(Arc::new(move |path: &Path| {
        database::source_of_console(&state_dir, path).is_some()
    })));
}

/// Runs the selection or the statement at `line` of a `.sql` console. The answer is
/// `false` when the file is not a console, so another tool may take the key.
pub(crate) fn run_line(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let path = view.read(cx).path().to_path_buf();
    let Some(root) = cx
        .try_global::<DatabaseConsoleRoot>()
        .map(|root| root.0.clone())
    else {
        return false;
    };
    let Some(source) = database::source_of_console(&root, &path) else {
        return false;
    };
    let selection = view.read(cx).selected_text();
    let text = selection.or_else(|| {
        view.read(cx)
            .text()
            .and_then(|text| database::statement_at(&text, line))
    });
    let Some(panel) = cx
        .try_global::<ActiveDatabase>()
        .map(|panel| panel.0.clone())
    else {
        return false;
    };
    workspace.show_tool(DATABASE_TOOL, window, cx);
    if let Some(text) = text {
        panel.update(cx, |panel, cx| panel.run_sql(source, text, None, cx));
    }
    true
}

fn small_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
    let hover = theme.hover;
    let pressed = theme.pressed;
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(FIELD_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

fn column_text(column: &Column) -> String {
    let mut text = column.name.clone();
    if column.primary_key {
        text.push_str(" PK");
    }
    if !column.nullable {
        text.push_str(" NOT NULL");
    }
    text
}

/// The text of a cell of a history row. A missing value is empty, so that two texts are
/// compared as the database wrote them and never as `NULL`.
fn history_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => value_text(other),
    }
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(integer) => integer.to_string(),
        Value::Decimal(decimal) => decimal.clone(),
        Value::Real(real) => real.to_string(),
        Value::Boolean(boolean) => boolean.to_string(),
        Value::Text(text) | Value::Date(text) | Value::Time(text) | Value::Timestamp(text) => {
            text.clone()
        }
        Value::Binary { base64, bytes, .. } => {
            if base64.is_empty() {
                format!("[binary {bytes} bytes]")
            } else {
                format!("[binary {bytes} bytes] {base64}")
            }
        }
    }
}

fn value_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Integer(_) | Value::Real(_) | Value::Boolean(_) => 8,
        Value::Decimal(text)
        | Value::Text(text)
        | Value::Date(text)
        | Value::Time(text)
        | Value::Timestamp(text) => text.len(),
        Value::Binary { base64, .. } => base64.len(),
    }
}

fn compare_values(
    left: Option<&Value>,
    right: Option<&Value>,
    ascending: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let order = match (left, right) {
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), _) => Ordering::Greater,
        (_, Some(Value::Null)) => Ordering::Less,
        (Some(Value::Integer(left)), Some(Value::Integer(right))) => left.cmp(right),
        (Some(Value::Real(left)), Some(Value::Real(right))) => {
            left.partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Some(Value::Integer(left)), Some(Value::Real(right))) => {
            (*left as f64).partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Some(Value::Real(left)), Some(Value::Integer(right))) => left
            .partial_cmp(&(*right as f64))
            .unwrap_or(Ordering::Equal),
        (Some(Value::Boolean(left)), Some(Value::Boolean(right))) => left.cmp(right),
        (Some(left), Some(right)) => value_text(left).cmp(&value_text(right)),
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
    };
    if ascending { order } else { order.reverse() }
}

/// Wraps an identifier in the quotes of the connected driver. A driver that quotes no
/// identifier gets the name as the schema wrote it.
fn quote_identifier(name: &str, quote: &str) -> String {
    if quote.trim().is_empty() {
        return name.to_owned();
    }
    format!(
        "{quote}{}{quote}",
        name.replace(quote, &format!("{quote}{quote}"))
    )
}

fn source_name(url: &str) -> String {
    let tail = url
        .rsplit(['/', ':', ';'])
        .find(|part| !part.is_empty() && !part.contains('='))
        .unwrap_or("source");
    let name = tail.trim().replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or("source");
    if name.is_empty() {
        "source".to_owned()
    } else {
        name.to_owned()
    }
}

fn slug(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-').to_owned();
    if id.is_empty() {
        "source".to_owned()
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::{DriverKind, Source, SourceId, Value, compare_values, slug, source_name};

    #[test]
    fn a_source_name_comes_from_the_address() {
        assert_eq!(source_name("jdbc:h2:mem:petclinic"), "petclinic");
        assert_eq!(
            source_name("jdbc:postgresql://localhost:5432/owners"),
            "owners"
        );
        assert_eq!(source_name("jdbc:sqlite:C:/data/pets.db"), "pets.db");
    }

    #[test]
    fn a_slug_is_a_folder_name() {
        assert_eq!(slug("Pet Clinic"), "pet-clinic");
        assert_eq!(slug("../old"), "old");
        assert_eq!(slug(""), "source");
    }

    #[test]
    fn null_sorts_last_and_numbers_compare_as_numbers() {
        let null = Value::Null;
        let ten = Value::Integer(10);
        let two = Value::Integer(2);
        assert_eq!(
            compare_values(Some(&ten), Some(&two), true),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_values(Some(&null), Some(&two), true),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_values(Some(&ten), Some(&two), false),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn the_password_field_is_never_a_source_field() {
        let source = Source {
            id: SourceId::new("owners"),
            name: "Owners".into(),
            kind: DriverKind::H2,
            url: "jdbc:h2:mem:owners".into(),
            user: "sa".into(),
            module: None,
            driver_jar: None,
        };
        let text = serde_json::to_string(&source).unwrap();
        assert!(!text.contains("password"));
    }
}
