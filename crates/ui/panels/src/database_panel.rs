//! The Database tool window: the sources of the project, their schema, the SQL console
//! of each source and the grid of its last result. A table or a view opens as a tab of
//! its own, where its rows are edited in place.
//!
//! The window itself knows no database product and no driver: it asks [`database::Support`]
//! for what the project has and starts a helper through it. The helper and its protocol
//! live in `services/database`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ::base::Cancel;
use database::{
    Column, Command, Dialect, DownloadProgress, Driver, DriverKind, Event, ExplainNode,
    ExplainPlan, ExportFormat, Exporter, MAX_RESULT_BYTES, MAX_RESULT_ROWS, Param, PasswordStore,
    ResultSet, Secret, Session, Source, SourceId, SourceStore, Suggestion, Support,
    SystemPasswords, Table, TableRef, Value, VendorTemplate, asked_parameters, bind_parameters,
};
use editor_view::{EditorView, ExtraTemplates, FileBar, RunFiles};
use lang_api::{
    MigrationHistory, MigrationHistoryRows, MigrationKind, MigrationModel, MigrationState,
    MigrationSupport, MigrationTool,
};
use tool_gateway::{
    DatabaseAnswer, DatabaseConnection, DatabaseDescription, DatabaseIntrospection, DatabaseObject,
    DatabaseObjectKind, DatabaseQuery, DatabaseRecentQuery, DatabaseRows, DatabaseSchema,
    DatabaseTestResult,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DATABASE_TOOL, Settings, Stop, Workspace};

use crate::database_data::DatabaseDataView;
use crate::mcp_database::{DatabaseBridge, Job as DatabaseJob};

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

/// The store of source passwords, put in place before the window is built. A window test
/// gives its own store, so no test writes the password of a user.
pub struct ActivePasswords(pub Arc<dyn PasswordStore>);

impl Global for ActivePasswords {}

/// Whether the machine has a store of passwords, as the window learned it in the
/// background. The reason of a missing store is the text the store gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Secrets {
    #[default]
    Checking,
    Ready,
    Unavailable(String),
}

impl Secrets {
    fn ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }
}

/// The order of the write jobs of the password store. The background calls of one source
/// take the gate one at a time, and a job whose source already has a later job leaves the
/// entry to it: a password written by an add cannot come back after the remove that
/// followed it.
#[derive(Default)]
struct PasswordJobs {
    next: AtomicU64,
    /// The last job of each source.
    latest: Mutex<HashMap<SourceId, u64>>,
    /// One write of the store at a time.
    gate: Mutex<()>,
}

impl PasswordJobs {
    fn begin(&self, source: &SourceId) -> u64 {
        let serial = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if let Ok(mut latest) = self.latest.lock() {
            latest.insert(source.clone(), serial);
        }
        serial
    }

    fn is_latest(&self, source: &SourceId, serial: u64) -> bool {
        self.latest
            .lock()
            .ok()
            .and_then(|latest| latest.get(source).copied())
            == Some(serial)
    }
}

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
/// How often the window reads the bytes of a driver download.
const DOWNLOAD_TICK: Duration = Duration::from_millis(100);
/// The width of a cell of the result grid.
const CELL_WIDTH: f32 = 160.;
/// The width of the tree.
const TREE_WIDTH: f32 = 240.;
/// The height of a field of the form.
const FIELD_HEIGHT: f32 = 26.;

/// One field of the new source form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Url,
    User,
    Password,
    Driver,
    Version,
}

impl Field {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Url,
            Self::Url => Self::User,
            Self::User => Self::Password,
            Self::Password => Self::Driver,
            Self::Driver => Self::Version,
            Self::Version => Self::Name,
        }
    }
}

/// The form behind `+`, kept in the memory of the window: the password reaches the store
/// of the system only when `save_password` is on.
struct Dialog {
    kind: DriverKind,
    module: Option<String>,
    name: TextField,
    url: TextField,
    user: TextField,
    password: TextField,
    driver: TextField,
    /// The version of the driver to download; the newest stable one fills it when the
    /// list of the repository arrives.
    version: TextField,
    /// The stable versions the repository offers, newest first.
    versions: Vec<String>,
    /// The kind whose versions the list holds, so an answer for another kind is dropped.
    versions_for: Option<DriverKind>,
    /// A request for the versions of the kind is in flight.
    versions_loading: bool,
    /// Why the versions could not be read, when they were not.
    versions_error: Option<String>,
    /// Why the last download of the form failed.
    driver_error: Option<String>,
    field: Field,
    save_password: bool,
}

impl Dialog {
    fn new(kind: DriverKind, vendors: &[VendorTemplate], save_password: bool) -> Self {
        let mut dialog = Self {
            kind,
            module: None,
            name: TextField::default(),
            url: TextField::default(),
            user: TextField::default(),
            password: TextField::default(),
            driver: TextField::default(),
            version: TextField::default(),
            versions: Vec::new(),
            versions_for: None,
            versions_loading: false,
            versions_error: None,
            driver_error: None,
            field: Field::Url,
            save_password,
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
            Field::Version => &mut self.version,
        }
    }
}

/// A statement the window sends, with the row limit that goes with it.
type SentQuery = (String, Option<usize>);

/// The command and the key of Explain Plan. The key is clear of the IDEA keymap: Ctrl+Shift+E
/// is Recent Locations there.
pub const EXPLAIN_PLAN: &str = "database.explain_plan";
pub const EXPLAIN_KEY: &str = "alt-shift-e";
/// Run SQL Script of the open `.sql` file, from Find Action and from the menu of the file.
pub const RUN_SQL_SCRIPT: &str = "database.run_script";

/// What the console asks before it runs a statement with placeholders: the value of each
/// name, with the last value of the console already in its field.
struct AskParameters {
    source: SourceId,
    sql: String,
    max_rows: Option<usize>,
    fields: Vec<AskField>,
    focused: usize,
}

/// One statement of the console that waits for its turn.
struct QueuedStatement {
    source: SourceId,
    sql: String,
    max_rows: Option<usize>,
    /// The values of the placeholders, once the user gave them; `None` asks first.
    params: Option<Vec<Param>>,
}

struct AskField {
    key: String,
    label: String,
    value: TextField,
}

/// One value a statement asks for and the field of the dialog.
impl AskParameters {
    fn answered(&self) -> HashMap<String, String> {
        self.fields
            .iter()
            .map(|field| (field.key.clone(), field.value.text.clone()))
            .collect()
    }
}

/// A transaction command the helper has not answered yet, and what waits for it.
struct TransactionCommand {
    source: SourceId,
    kind: Transaction,
    after: Option<PendingAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transaction {
    AutoCommit(bool),
    Commit,
    Rollback,
}

/// What happens once the user decided the open transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingAction {
    CloseConsole(PathBuf),
    Disconnect(SourceId),
    RemoveSource(SourceId),
}

/// The question of a close or a disconnect with a transaction still open. It stands over
/// the console until the user commits, rolls back or cancels.
struct PendingQuestion {
    source: SourceId,
    action: PendingAction,
}

/// The form behind the Export button of a result tab.
struct ExportDialog {
    tab_id: u64,
    format: ExportFormat,
    /// The table of the INSERT statements. A result of one table fills it, and a result
    /// of no table leaves the field for the user.
    table: TextField,
    /// The result named its table, so the form does not ask for it.
    known_table: bool,
}

/// Where an export goes: a file the user picked, or the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ExportTarget {
    File(PathBuf),
    Clipboard,
}

/// An export in flight: the statement runs again without a row limit, and its rows become
/// the text of the format while they arrive.
struct ExportJob {
    tab_id: u64,
    source: SourceId,
    /// The id of the query that fills the text.
    id: u64,
    exporter: Exporter,
    target: ExportTarget,
}

/// What a data tab asked the connection for, so that the answer lands in the right place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataRequest {
    Page,
    Count,
    AutoCommit,
    Statement(usize),
    Commit,
    Rollback,
}

/// One request of a data tab: its own serial and what it asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DataTicket {
    pub serial: u64,
    pub request: DataRequest,
}

/// An answer of the connection, as a data tab reads it.
#[derive(Clone, Debug)]
pub(crate) enum DataEvent {
    Columns(Vec<Column>),
    Rows(Vec<Vec<Value>>),
    Done {
        update_count: Option<u64>,
        cancelled: bool,
    },
    Error(String),
}

/// A transaction command of a data tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataCommand {
    AutoCommit(bool),
    Commit,
    Rollback,
}

/// One answer of a connection that has no helper, for the window tests.
#[derive(Clone, Debug, Default)]
pub(crate) struct ScriptedReply {
    pub result: ResultSet,
    pub error: Option<String>,
}

/// What a scripted connection was asked, in order, for the window tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScriptedCommand {
    Query {
        sql: String,
        max_rows: Option<usize>,
    },
    AutoCommit(bool),
    Commit,
    Rollback,
    Cancel,
}

/// A statement of the console and the values that went with it, for the window tests.
type ParamQuery = (String, Vec<Param>);

/// A connection of the window tests: no process, the answers are known in advance.
struct Scripted {
    source: SourceId,
    rows: ResultSet,
    error: Option<String>,
    closed: Arc<AtomicBool>,
    /// The statements the window would send.
    queries: Arc<Mutex<Vec<SentQuery>>>,
    /// The values sent with the statements of the console, in order.
    param_queries: Arc<Mutex<Vec<ParamQuery>>>,
    /// The answers of the data tab, one per request; an empty queue answers with `rows`.
    replies: VecDeque<ScriptedReply>,
    /// The answers of the console, one per statement; an empty queue answers with `rows`.
    console_replies: VecDeque<ResultSet>,
    /// The answers of a script, one per statement: an `Err` fails the statement.
    console_answers: VecDeque<std::result::Result<ResultSet, String>>,
    /// Every request of the window, with the transaction commands of a data tab.
    commands: Arc<Mutex<Vec<ScriptedCommand>>>,
    /// The statement runs until the window stops it, as a long query does.
    hangs: bool,
    /// The count the connection answers a count of a console result with.
    count: Option<u64>,
}

/// One statement of the console and the tab of its result, under the console.
pub(crate) struct ConsoleResult {
    /// The id of the tab; the query that fills it carries its own one.
    pub id: u64,
    pub statement: String,
    pub source: SourceId,
    pub result: Option<ResultSet>,
    pub error: Option<String>,
    /// When the statement was sent, so its tab shows how long it ran.
    pub started: Instant,
    pub elapsed: Option<Duration>,
    pub cancelled: bool,
    /// The row limit of the last run; `None` when the statement asked for every row.
    pub limit: Option<usize>,
    /// The tab shows less than the statement returned.
    pub truncated: bool,
    /// The rows of the whole statement, when the user asked for their count.
    pub total: Option<u64>,
    /// The count of the statement runs: its tab shows an indicator and Stop stops it.
    pub counting: bool,
    /// A count may be asked for this statement: it yields rows and does not write, so
    /// asking never runs a write twice.
    pub rows_countable: bool,
    /// The bytes of the rows read so far, so a wide result stops before the window holds
    /// too much of it.
    pub bytes: usize,
    /// The tab explains a statement: its answer becomes a plan instead of a grid.
    pub explain: bool,
    /// The plan of an explained statement, once its answer is read.
    pub plan: Option<ExplainPlan>,
}

impl ConsoleResult {
    fn new(id: u64, statement: String, source: SourceId, limit: Option<usize>) -> Self {
        Self {
            id,
            statement,
            source,
            result: None,
            error: None,
            started: Instant::now(),
            elapsed: None,
            cancelled: false,
            limit,
            truncated: false,
            total: None,
            counting: false,
            rows_countable: false,
            bytes: 0,
            explain: false,
            plan: None,
        }
    }

    /// The tab of an explained statement, whose answer becomes a plan.
    fn explain(id: u64, statement: String, source: SourceId) -> Self {
        Self {
            explain: true,
            ..Self::new(id, statement, source, None)
        }
    }

    /// Marks the result complete: a portion of a limited statement is truncated even when
    /// the driver does not say so, and the tab says whether the user may ask for its count.
    fn settle(&mut self, driver_truncated: bool, cancelled: bool) {
        self.cancelled |= cancelled;
        let limit = self.limit;
        let statement = self.statement.as_str();
        let Some(result) = &mut self.result else {
            self.truncated = false;
            self.rows_countable = false;
            return;
        };
        result.truncated |= driver_truncated || cancelled;
        // The driver cuts a limited statement without saying so: a portion is exactly the
        // limit, unless the statement changed rows.
        let portion = result.update_count.is_none()
            && !result.columns.is_empty()
            && limit.is_some_and(|limit| limit > 0 && result.rows.len() >= limit);
        self.truncated = result.truncated || portion;
        self.rows_countable =
            !cancelled && self.truncated && database::countable_statement(statement);
    }

    /// Whether the footer offers a count of the rows of the whole statement.
    pub(crate) fn countable(&self) -> bool {
        self.rows_countable
            && self.total.is_none()
            && !self.counting
            && !self.cancelled
            && self.error.is_none()
    }

    /// The name of the tab: the head of the statement, as a console tab names it.
    pub(crate) fn title(&self) -> String {
        let flat = self
            .statement
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut title: String = flat.chars().take(28).collect();
        if flat.chars().count() > 28 {
            title.push('…');
        }
        if title.is_empty() {
            return if self.explain {
                "Plan".to_owned()
            } else {
                "Statement".to_owned()
            };
        }
        if self.explain {
            format!("Plan: {title}")
        } else {
            title
        }
    }

    /// Whether the footer offers Export: the statement only reads, so running it again
    /// gives the same rows, and a plan has nothing to export.
    pub(crate) fn can_export(&self) -> bool {
        !self.explain
            && self.error.is_none()
            && !self.cancelled
            && self
                .result
                .as_ref()
                .is_some_and(|result| !result.columns.is_empty())
            && database::countable_statement(&self.statement)
    }

    fn running(&self) -> bool {
        self.elapsed.is_none() && self.error.is_none()
    }

    /// How many rows the tab read, or the count the driver changed.
    pub(crate) fn rows_text(&self) -> String {
        if self.running() {
            return "Running…".to_owned();
        }
        if self.error.is_some() {
            return "Failed".to_owned();
        }
        let Some(result) = &self.result else {
            return String::new();
        };
        if let Some(count) = result.update_count {
            return format!("{count} rows affected");
        }
        let rows = result.rows.len();
        let noun = if rows == 1 { "row" } else { "rows" };
        match self.total {
            Some(total) if total as usize > rows => {
                format!("{rows} {noun} of {}", group_digits(total))
            }
            _ if self.truncated => format!("{rows}+ {noun}"),
            _ => format!("{rows} {noun}"),
        }
    }

    /// The time and the rows of the tab, as the row under its grid shows them.
    pub(crate) fn summary(&self) -> String {
        let rows = self.rows_text();
        match self.elapsed {
            Some(elapsed) => format!("{} ms · {rows}", elapsed.as_millis()),
            None => rows,
        }
    }
}

/// The statements of a source that wait for Commit or Rollback, as the console says them.
fn pending_text(pending: u32) -> String {
    match pending {
        0 => "No pending changes".to_owned(),
        1 => "1 pending statement".to_owned(),
        count => format!("{count} pending statements"),
    }
}

/// The steps of a plan as the rows of a tree: each one with how deep it is.
fn plan_rows(node: &ExplainNode, depth: usize, rows: &mut Vec<(usize, String)>) {
    rows.push((depth, node.label.clone()));
    for child in &node.children {
        plan_rows(child, depth + 1, rows);
    }
}

/// A number with a thin space between the groups of three, as the row count of a
/// result reads: `12 840`.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push('\u{a0}');
        }
        grouped.push(character);
    }
    grouped
}

/// The answer of a download: the version and the jar, or the reason it failed.
type DownloadAnswer = Result<(String, PathBuf), String>;

/// A driver download in flight. The window shows its bytes each tick and stops it on
/// request; the work itself is on a background thread and never waits for a frame.
struct DriverDownload {
    /// The source that waits for the driver; `None` for the form.
    source: Option<SourceId>,
    /// The form gets the file in its driver field.
    form: bool,
    kind: DriverKind,
    /// What the button asked for; `None` is the newest stable version.
    version: Option<String>,
    progress: Arc<DownloadProgress>,
    cancel: Cancel,
    /// The version and the jar of a finished download, or the reason it failed.
    result: Arc<Mutex<Option<DownloadAnswer>>>,
}

pub struct DatabasePanel {
    workspace: WeakEntity<Workspace>,
    state_dir: PathBuf,
    focus: FocusHandle,
    /// The language side of the window: its drivers, the drivers of the project and the
    /// download of a driver.
    support: Option<Arc<dyn Support>>,
    sources: Vec<Source>,
    suggestions: Vec<Suggestion>,
    drivers: Vec<Driver>,
    selected: Option<SourceId>,
    tables: HashMap<SourceId, Vec<Table>>,
    expanded: HashSet<(SourceId, String, String)>,
    errors: HashMap<SourceId, String>,
    schema_loading: HashSet<SourceId>,
    /// The tabs of the statements of the console, oldest first.
    results: Vec<ConsoleResult>,
    /// The tab the grid shows.
    active_result: Option<u64>,
    /// The tab of each query in flight, by the id of the query.
    tabs_by_query: HashMap<u64, u64>,
    /// The tab of each count in flight, by the id of the count query.
    count_ids: HashMap<u64, u64>,
    /// The schema the console of a source resolves bare names against, chosen in its
    /// toolbar. A source without one uses the first schema of its tree.
    default_schemas: HashMap<SourceId, String>,
    /// The statements the user ran, newest first, for the history of the console.
    console_history: Vec<String>,
    sort: Option<(usize, bool)>,
    selected_cell: Option<(usize, usize)>,
    /// The sources whose console is in Manual: a statement waits for Commit or Rollback.
    manual: HashMap<SourceId, bool>,
    /// The source the user picked for a `.sql` script of the project, by path.
    file_sources: HashMap<PathBuf, SourceId>,
    /// The statements that changed rows of each source since its last Commit or Rollback.
    pending: HashMap<SourceId, u32>,
    /// The transaction commands the helper has not answered yet.
    transaction_ids: HashMap<u64, TransactionCommand>,
    /// The question of a close or a disconnect with a transaction still open.
    question: Option<PendingQuestion>,
    /// The values the console asks before running a statement with placeholders.
    params_ask: Option<AskParameters>,
    /// The last value of each name of a placeholder, per source.
    param_values: HashMap<(SourceId, String), String>,
    /// The form behind the Export button of a result tab.
    export_dialog: Option<ExportDialog>,
    /// The export in flight, if any.
    export: Option<ExportJob>,
    /// The window of the tab: a console closes itself once its transaction is decided.
    window: Option<WindowHandle<Workspace>>,
    /// The quote of each connected driver, from its `connected` answer.
    quotes: HashMap<SourceId, String>,
    sessions: HashMap<SourceId, Session>,
    query_ids: HashMap<SourceId, u64>,
    /// The connect commands the helper has not answered yet.
    connect_ids: HashMap<SourceId, u64>,
    /// The sources whose driver the helper could not load, so the window offers the
    /// download even when a jar is present.
    driver_failed: HashSet<SourceId>,
    queue: VecDeque<QueuedStatement>,
    /// Run SQL Script stops the queue at the first statement that fails.
    script_stop: bool,
    /// The first failure of a script stopped its queue.
    script_halt: bool,
    next_id: u64,
    passwords: HashMap<SourceId, Secret>,
    password: TextField,
    password_field: bool,
    /// The store of the passwords of the sources, of the machine or of a window test.
    password_store: Arc<dyn PasswordStore>,
    /// The order of the write jobs of the store.
    password_jobs: Arc<PasswordJobs>,
    /// What the window learned about the store when it was built.
    secrets: Secrets,
    status: Option<String>,
    dialog: Option<Dialog>,
    /// The download of a driver that is in flight, if any.
    driver_download: Option<DriverDownload>,
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
    /// The requests of the data tabs that the helper has not answered yet.
    data_pending: HashMap<(SourceId, u64), (Entity<DatabaseDataView>, DataTicket)>,
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
    /// The connections were read from the state folder; a tool may not write before.
    sources_loaded: bool,
    /// The bridge of the MCP database tools, taken in every poll.
    tool_database: Option<Arc<DatabaseBridge>>,
    /// The statements a tool asked for, by the id of their query, until the answer.
    tool_queries: HashMap<u64, DatabaseJob>,
    /// The job of a tool whose statement the console is about to take.
    pending_tool_job: Option<DatabaseJob>,
}

impl DatabasePanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        state_dir: PathBuf,
        cx: &mut App,
    ) -> Entity<Self> {
        let panel = cx.new(|cx| {
            let password_store: Arc<dyn PasswordStore> = cx
                .try_global::<ActivePasswords>()
                .map(|active| active.0.clone())
                .unwrap_or_else(|| Arc::new(SystemPasswords));
            let mut panel = Self {
                workspace,
                state_dir: state_dir.clone(),
                focus: cx.focus_handle(),
                support: support(),
                sources: Vec::new(),
                suggestions: Vec::new(),
                drivers: Vec::new(),
                selected: None,
                tables: HashMap::new(),
                expanded: HashSet::new(),
                errors: HashMap::new(),
                schema_loading: HashSet::new(),
                results: Vec::new(),
                active_result: None,
                tabs_by_query: HashMap::new(),
                count_ids: HashMap::new(),
                default_schemas: HashMap::new(),
                console_history: Vec::new(),
                sort: None,
                selected_cell: None,
                manual: HashMap::new(),
                file_sources: HashMap::new(),
                pending: HashMap::new(),
                transaction_ids: HashMap::new(),
                question: None,
                params_ask: None,
                param_values: HashMap::new(),
                export_dialog: None,
                export: None,
                window: None,
                quotes: HashMap::new(),
                sessions: HashMap::new(),
                query_ids: HashMap::new(),
                connect_ids: HashMap::new(),
                driver_failed: HashSet::new(),
                queue: VecDeque::new(),
                script_stop: false,
                script_halt: false,
                next_id: 1,
                passwords: HashMap::new(),
                password: TextField::default(),
                password_field: false,
                password_store,
                password_jobs: Arc::new(PasswordJobs::default()),
                secrets: Secrets::Checking,
                status: None,
                dialog: None,
                driver_download: None,
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
                data_pending: HashMap::new(),
                polling: false,
                #[cfg(test)]
                poll_ticks: 0,
                window_closed: Arc::new(AtomicBool::new(false)),
                scripted_closed: Arc::new(Mutex::new(None)),
                tree_scroll: ScrollHandle::new(),
                result_scroll: ScrollHandle::new(),
                sources_loaded: false,
                tool_database: None,
                tool_queries: HashMap::new(),
                pending_tool_job: None,
            };
            panel.load_sources(cx);
            panel.load_migrations(cx);
            panel.refresh_catalog();
            panel.check_passwords(cx);
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

    /// Connects the tools of the MCP server to this window: their statements run through
    /// the same console and the same drivers as the ones of the user.
    pub(crate) fn set_tool_database(
        &mut self,
        bridge: Arc<DatabaseBridge>,
        cx: &mut Context<Self>,
    ) {
        self.tool_database = Some(bridge);
        // The queue has to be read even while no connection is open, so a tool that
        // lists the connections gets an answer.
        let polling = cx.weak_entity();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if polling
                    .update(cx, |panel, cx| {
                        let _ = this;
                        panel.poll_tool_jobs(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// Takes the jobs the MCP tools left and answers the ones the cache can answer.
    fn poll_tool_jobs(&mut self, cx: &mut Context<Self>) {
        let Some(bridge) = self.tool_database.clone() else {
            return;
        };
        while let Some(job) = bridge.take_job() {
            self.take_database_job(job, cx);
        }
    }

    fn take_database_job(&mut self, job: DatabaseJob, cx: &mut Context<Self>) {
        let query = job.query.clone();
        match query {
            DatabaseQuery::ListConnections => job.answer(DatabaseAnswer::Connections(
                self.sources
                    .iter()
                    .map(|source| self.connection_of(source))
                    .collect(),
            )),
            DatabaseQuery::TestConnection { id } => {
                match self.test_connection(&SourceId::new(id)) {
                    Ok(result) => job.answer(DatabaseAnswer::Tested(result)),
                    Err(message) => job.fail(message),
                }
            }
            DatabaseQuery::CreateConnection {
                name,
                dbms,
                url,
                need_to_check,
            } => match self.create_connection(&name, &dbms, &url, need_to_check, cx) {
                Ok(answer) => job.answer(answer),
                Err(message) => job.fail(message),
            },
            DatabaseQuery::EditConnection {
                connection_id,
                dbms,
                url,
                need_to_check,
            } => {
                match self.edit_connection(&connection_id, dbms.as_deref(), &url, need_to_check, cx)
                {
                    Ok(answer) => job.answer(answer),
                    Err(message) => job.fail(message),
                }
            }
            DatabaseQuery::ListSchemas {
                connection_id,
                selected_only,
            } => {
                let source = SourceId::new(connection_id);
                let tables = self.tables(&source);
                let mut schemas: Vec<DatabaseSchema> = Vec::new();
                for table in &tables {
                    let name = table.schema.clone();
                    if name.is_empty() || schemas.iter().any(|known| known.name == name) {
                        continue;
                    }
                    schemas.push(DatabaseSchema {
                        name,
                        database: None,
                    });
                }
                if selected_only && let Some(selected) = self.default_schemas.get(&source) {
                    schemas.retain(|schema| &schema.name == selected);
                }
                job.answer(DatabaseAnswer::Schemas(schemas));
            }
            DatabaseQuery::Introspect {
                connection_id,
                database_name,
                schema_name,
            } => {
                let source = SourceId::new(connection_id);
                if !self.sources.iter().any(|known| known.id == source) {
                    return job.fail(format!("no connection has the id {source}"));
                }
                let status = if self.sessions.contains_key(&source) {
                    self.request_schema(source.clone(), cx);
                    "started"
                } else {
                    "failed"
                };
                job.answer(DatabaseAnswer::Introspected(DatabaseIntrospection {
                    connection_id: source.as_str().to_string(),
                    database_name,
                    schema_name,
                    status: status.to_string(),
                    tables: self.tables(&source).len(),
                    partial: false,
                }));
            }
            DatabaseQuery::ObjectKinds { connection_id } => {
                let source = SourceId::new(connection_id);
                let mut kinds: Vec<DatabaseObjectKind> = Vec::new();
                for table in self.tables(&source) {
                    let code = match table.kind {
                        database::TableKind::View => "VIEW",
                        database::TableKind::Table => "TABLE",
                    };
                    if !kinds.iter().any(|known| known.code == code) {
                        kinds.push(DatabaseObjectKind {
                            code: code.to_string(),
                            name: if code == "VIEW" { "Views" } else { "Tables" }.to_string(),
                        });
                    }
                }
                job.answer(DatabaseAnswer::Kinds(kinds));
            }
            DatabaseQuery::ListObjects {
                connection_id,
                schema_name,
                database_name,
                kind,
            } => {
                let source = SourceId::new(connection_id);
                let objects = self
                    .tables(&source)
                    .into_iter()
                    .filter(|table| {
                        schema_name
                            .as_deref()
                            .is_none_or(|schema| table.schema == schema)
                    })
                    .filter(|table| {
                        kind.as_deref().is_none_or(|kind| {
                            kind.eq_ignore_ascii_case(match table.kind {
                                database::TableKind::View => "VIEW",
                                database::TableKind::Table => "TABLE",
                            })
                        })
                    })
                    .map(|table| DatabaseObject {
                        name: table.name,
                        kind: match table.kind {
                            database::TableKind::View => "VIEW",
                            database::TableKind::Table => "TABLE",
                        }
                        .to_string(),
                        schema: (!table.schema.is_empty()).then_some(table.schema),
                        database: database_name.clone(),
                    })
                    .collect();
                job.answer(DatabaseAnswer::Objects(objects));
            }
            DatabaseQuery::Describe {
                connection_id,
                database_name,
                schema_name,
                kind: _,
                object_name,
            } => {
                let source = SourceId::new(connection_id);
                let Some(table) = self.tables(&source).into_iter().find(|table| {
                    table.name == object_name
                        && schema_name
                            .as_deref()
                            .is_none_or(|schema| table.schema == schema)
                }) else {
                    return job.fail(format!(
                        "the window has no object {object_name} of {source}"
                    ));
                };
                job.answer(DatabaseAnswer::Described(DatabaseDescription {
                    name: table.name.clone(),
                    kind: match table.kind {
                        database::TableKind::View => "VIEW",
                        database::TableKind::Table => "TABLE",
                    }
                    .to_string(),
                    schema: (!table.schema.is_empty()).then_some(table.schema),
                    database: database_name,
                    columns: table
                        .columns
                        .iter()
                        .map(|column| tool_gateway::DescriptionColumn {
                            name: column.name.clone(),
                            type_name: column.type_name.clone(),
                            nullable: column.nullable,
                            primary_key: column.primary_key,
                        })
                        .collect(),
                    indexes: table
                        .indexes
                        .iter()
                        .map(|index| tool_gateway::DescriptionIndex {
                            name: index.name.clone(),
                            unique: index.unique,
                            columns: index.columns.clone(),
                        })
                        .collect(),
                    foreign_keys: table
                        .foreign_keys
                        .iter()
                        .map(|key| tool_gateway::DescriptionKey {
                            name: key.name.clone(),
                            columns: key.columns.clone(),
                            referenced_table: key.referenced_table.clone(),
                            referenced_columns: key.referenced_columns.clone(),
                        })
                        .collect(),
                    partial: table.partial,
                }));
            }
            DatabaseQuery::Execute {
                connection_id,
                query_text,
            } => self.start_tool_statement(job, &connection_id, query_text, None, cx),
            DatabaseQuery::PreviewTable {
                connection_id,
                schema_name,
                database_name,
                table_name,
                max_row_count,
            } => {
                let source = SourceId::new(connection_id);
                let kind = self
                    .sources
                    .iter()
                    .find(|known| known.id == source)
                    .map(|known| known.kind)
                    .unwrap_or_default();
                let dialect = Dialect::of(kind);
                let table = TableRef::new(schema_name.unwrap_or_default(), table_name);
                let sql = format!("SELECT * FROM {}", table.qualified(dialect));
                let _ = database_name;
                self.start_tool_statement(job, source.as_str(), sql, Some(max_row_count), cx)
            }
            DatabaseQuery::RecentQueries { connection_id } => {
                let source = SourceId::new(connection_id);
                let mut queries: Vec<DatabaseRecentQuery> = Vec::new();
                if let Some(active) = self.query_ids.get(&source)
                    && let Some(index) = self.tab_of(*active)
                    && let Some(tab) = self.results.get(index)
                {
                    queries.push(DatabaseRecentQuery {
                        sql: tab.statement.clone(),
                        state: "running".to_string(),
                        elapsed_ms: tab.started.elapsed().as_millis() as u64,
                        connection_id: source.as_str().to_string(),
                    });
                }
                for sql in &self.console_history {
                    queries.push(DatabaseRecentQuery {
                        sql: sql.clone(),
                        state: "finished".to_string(),
                        elapsed_ms: 0,
                        connection_id: source.as_str().to_string(),
                    });
                }
                job.answer(DatabaseAnswer::Recent(queries));
            }
            DatabaseQuery::Cancel { session_id } => {
                let source = SourceId::new(session_id);
                let cancelled = self
                    .query_ids
                    .get(&source)
                    .and_then(|id| {
                        self.sessions
                            .get(&source)
                            .map(|session| session.send(Command::Cancel { id: *id }).is_ok())
                    })
                    .unwrap_or(false);
                job.answer(DatabaseAnswer::Cancelled(cancelled));
            }
        }
    }

    fn connection_of(&self, source: &Source) -> DatabaseConnection {
        let driver = self
            .drivers
            .iter()
            .find(|driver| driver.kind == source.kind)
            .map(|driver| driver.label.clone());
        DatabaseConnection {
            id: source.id.as_str().to_string(),
            name: source.name.clone(),
            dbms: source.kind.title().to_string(),
            driver,
            url: tool_gateway::hide_secrets(&source.url),
        }
    }

    fn test_connection(&self, source: &SourceId) -> Result<DatabaseTestResult, String> {
        let Some(known) = self.sources.iter().find(|known| &known.id == source) else {
            return Err(format!("no connection has the id {source}"));
        };
        let driver = self
            .drivers
            .iter()
            .find(|driver| driver.kind == known.kind)
            .map(|driver| driver.label.clone())
            .or_else(|| {
                known
                    .driver_jar
                    .as_ref()
                    .map(|jar| jar.to_string_lossy().into_owned())
            });
        if self.sessions.contains_key(source) {
            return Ok(DatabaseTestResult {
                problem: false,
                dbms: known.kind.title().to_string(),
                version: self
                    .status
                    .clone()
                    .and_then(|status| status.strip_prefix("Connected to ").map(str::to_string)),
                driver,
                result: self
                    .status
                    .clone()
                    .unwrap_or_else(|| "Connected".to_string()),
            });
        }
        Err(format!(
            "{} is not connected in the Database window: open it there to test the driver \
             and the password",
            known.name
        ))
    }

    fn create_connection(
        &mut self,
        name: &str,
        dbms: &str,
        url: &str,
        need_to_check: bool,
        cx: &mut Context<Self>,
    ) -> Result<DatabaseAnswer, String> {
        let Some(kind) = driver_kind(dbms) else {
            return Err(format!("unknown DBMS {dbms}"));
        };
        if !self.sources_loaded {
            return Err("the connections of the window are still loading".to_string());
        }
        if url.trim().is_empty() {
            return Err("the address of a connection is required".to_string());
        }
        let name = if name.trim().is_empty() {
            source_name(url)
        } else {
            name.trim().to_string()
        };
        let id = self.unique_id(&name);
        let source = Source {
            id: id.clone(),
            name: name.clone(),
            kind,
            // The address a project file keeps carries no password.
            url: tool_gateway::without_password(url),
            user: String::new(),
            module: None,
            driver_jar: None,
        };
        self.sources.push(source.clone());
        self.selected = Some(id.clone());
        self.save_sources(cx);
        let check = need_to_check
            .then(|| self.test_connection(&id).ok())
            .flatten();
        cx.notify();
        Ok(DatabaseAnswer::Saved(self.connection_of(&source), check))
    }

    fn edit_connection(
        &mut self,
        connection_id: &str,
        dbms: Option<&str>,
        url: &str,
        need_to_check: bool,
        cx: &mut Context<Self>,
    ) -> Result<DatabaseAnswer, String> {
        let source = SourceId::new(connection_id);
        let kind = match dbms {
            Some(dbms) => Some(driver_kind(dbms).ok_or_else(|| format!("unknown DBMS {dbms}"))?),
            None => None,
        };
        if !self.sources_loaded {
            return Err("the connections of the window are still loading".to_string());
        }
        if url.trim().is_empty() {
            return Err("the address of a connection is required".to_string());
        }
        let Some(index) = self.sources.iter().position(|known| known.id == source) else {
            return Err(format!("no connection has the id {connection_id}"));
        };
        let changed = self.sources[index].url != url
            || kind.is_some_and(|kind| kind != self.sources[index].kind);
        self.sources[index].url = tool_gateway::without_password(url);
        if let Some(kind) = kind {
            self.sources[index].kind = kind;
        }
        if changed {
            // The address of a live connection is gone: the helper reads a new one on
            // the next open, and the schema of the old address must not be shown.
            self.sessions.remove(&source);
            self.tables.remove(&source);
            self.errors.remove(&source);
        }
        self.save_sources(cx);
        let saved = self.sources[index].clone();
        let check = need_to_check
            .then(|| self.test_connection(&source).ok())
            .flatten();
        cx.notify();
        Ok(DatabaseAnswer::Saved(self.connection_of(&saved), check))
    }

    /// Runs a statement for a tool on the console of its connection and keeps the job
    /// until the console has the result.
    fn start_tool_statement(
        &mut self,
        job: DatabaseJob,
        connection_id: &str,
        sql: String,
        max_rows: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let source = SourceId::new(connection_id);
        if !self.sources.iter().any(|known| known.id == source) {
            return job.fail(format!("no connection has the id {connection_id}"));
        }
        if sql.trim().is_empty() {
            return job.fail("the statement is empty");
        }
        if !self.sessions.contains_key(&source) && self.scripted.is_none() {
            return job.fail(format!(
                "{} is not connected in the Database window",
                source
            ));
        }
        // The statement makes its own id; the job waits for it there.
        self.pending_tool_job = Some(job);
        let _ = self.start_statement(source, sql, max_rows, Vec::new(), false, cx);
    }

    fn tool_rows(&self, index: usize) -> Option<DatabaseRows> {
        let tab = self.results.get(index)?;
        let result = tab.result.clone().unwrap_or_default();
        Some(DatabaseRows {
            columns: result
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
            rows: result
                .rows
                .iter()
                .map(|row| row.iter().map(Value::text).collect())
                .collect(),
            update_count: result.update_count,
            truncated: result.truncated,
            elapsed_ms: tab
                .elapsed
                .map(|elapsed| elapsed.as_millis() as u64)
                .unwrap_or(0),
            error: tab.error.clone(),
        })
    }

    /// Answers a statement a tool asked for, by the tab the console finished.
    fn answer_tool_query(&mut self, query: u64) {
        let Some(job) = self.tool_queries.remove(&query) else {
            return;
        };
        let Some(index) = self.tab_of(query) else {
            return job.fail("the console has no tab for the statement");
        };
        match self.tool_rows(index) {
            Some(rows) => job.answer(DatabaseAnswer::Rows(rows)),
            None => job.fail("the statement left no result"),
        }
    }

    /// The last result of the console or of a double-clicked table: the tab the grid
    /// shows.
    pub fn result(&self) -> Option<ResultSet> {
        self.active().and_then(|tab| tab.result.as_ref()).cloned()
    }

    fn active(&self) -> Option<&ConsoleResult> {
        let id = self.active_result?;
        self.results.iter().find(|tab| tab.id == id)
    }

    fn active_mut(&mut self) -> Option<&mut ConsoleResult> {
        let id = self.active_result?;
        self.results.iter_mut().find(|tab| tab.id == id)
    }

    /// The tab of a query in flight.
    fn tab_of(&self, query: u64) -> Option<usize> {
        let tab = *self.tabs_by_query.get(&query)?;
        self.results.iter().position(|known| known.id == tab)
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

    /// The source a `.sql` script runs against: the console of a source, or the source
    /// the user picked in the bar above a file of the project.
    pub(crate) fn source_of_script(&self, path: &Path) -> Option<SourceId> {
        database::source_of_console(&self.state_dir, path)
            .or_else(|| self.file_sources.get(path).cloned())
    }

    /// The source the user picked for a script of the project.
    #[cfg(test)]
    pub(crate) fn file_source(&self, path: &Path) -> Option<SourceId> {
        self.file_sources.get(path).cloned()
    }

    /// Runs `path` against `source` from now on: its completion, its checks and its
    /// script follow the schema of that source.
    fn select_sql_source(&mut self, path: PathBuf, source: SourceId, cx: &mut Context<Self>) {
        self.file_sources.insert(path.clone(), source.clone());
        if let Some(language) = crate::database_console::language_for(&self.state_dir, cx) {
            language.set_file_source(&path, source.clone());
        }
        self.sync_schema(&source, cx);
        if let Some(view) = self.workspace.upgrade().and_then(|workspace| {
            workspace
                .read(cx)
                .view_of(&path)
                .and_then(|view| view.downcast::<EditorView>().ok())
        }) {
            view.update(cx, |view, cx| view.attach_support(cx));
        }
        cx.notify();
    }

    /// The sources of the window as a menu: the choice of the source a `.sql` script of
    /// the project runs against.
    fn show_sql_source_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.sources.is_empty() {
            self.status = Some("No data sources. Press + to add one.".to_owned());
            cx.notify();
            return;
        }
        let current = self.file_sources.get(&path).cloned();
        let panel = cx.weak_entity();
        let entries: Vec<MenuEntry> = self
            .sources
            .iter()
            .map(|source| {
                let panel = panel.clone();
                let target = path.clone();
                let chosen = source.id.clone();
                let picked = current.as_ref() == Some(&source.id);
                let entry = MenuEntry::new(source.name.clone(), move |_window, cx| {
                    let chosen = chosen.clone();
                    let target = target.clone();
                    let _ = panel.update(cx, |panel, cx| {
                        panel.select_sql_source(target, chosen, cx);
                    });
                });
                if picked {
                    entry.icon("general/check")
                } else {
                    entry
                }
            })
            .collect();
        let position = window.mouse_position();
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            });
        }
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
                params: Vec::new(),
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
            source: id.clone(),
            rows,
            error,
            closed,
            queries: Arc::new(Mutex::new(Vec::new())),
            param_queries: Arc::new(Mutex::new(Vec::new())),
            replies: VecDeque::new(),
            console_replies: VecDeque::new(),
            console_answers: VecDeque::new(),
            commands: Arc::new(Mutex::new(Vec::new())),
            hangs: false,
            count: None,
        });
        self.sync_schema(&id, cx);
        cx.notify();
    }

    /// The count a scripted connection answers with, for the window tests of Count rows.
    #[cfg(test)]
    pub(crate) fn scripted_count_for_test(&mut self, count: u64) {
        if let Some(scripted) = &mut self.scripted {
            scripted.count = Some(count);
        }
    }

    /// A scripted connection whose statement runs until the window stops it, for the
    /// window tests of the Stop button.
    #[cfg(test)]
    pub(crate) fn scripted_hangs_for_test(&mut self, hangs: bool) {
        if let Some(scripted) = &mut self.scripted {
            scripted.hangs = hangs;
        }
    }

    /// The answers a scripted connection gives the console, one per statement.
    #[cfg(test)]
    pub(crate) fn scripted_console_replies_for_test(&mut self, replies: Vec<ResultSet>) {
        if let Some(scripted) = &mut self.scripted {
            scripted.console_replies = replies.into();
        }
    }

    /// The answers a scripted connection gives the console, one per statement.
    #[cfg(test)]
    pub(crate) fn scripted_console_answers_for_test(
        &mut self,
        answers: Vec<std::result::Result<ResultSet, String>>,
    ) {
        if let Some(scripted) = &mut self.scripted {
            scripted.console_answers = answers.into();
        }
    }

    /// The values the window sent with the statements of its console, in order.
    #[cfg(test)]
    pub(crate) fn parameter_queries_for_test(&self) -> Vec<(String, Vec<Param>)> {
        self.scripted
            .as_ref()
            .and_then(|scripted| scripted.param_queries.lock().ok().map(|sent| sent.clone()))
            .unwrap_or_default()
    }

    /// The names of the placeholders the console asks for.
    #[cfg(test)]
    pub(crate) fn asked_parameters_for_test(&self) -> Vec<String> {
        self.params_ask
            .as_ref()
            .map(|ask| ask.fields.iter().map(|field| field.key.clone()).collect())
            .unwrap_or_default()
    }

    /// Whether the console asks Commit, Rollback or Cancel before it goes on.
    #[cfg(test)]
    pub(crate) fn transaction_question_for_test(&self) -> Option<String> {
        self.question
            .as_ref()
            .map(|question| question.source.to_string())
    }

    /// The plans of the result tabs, for the window tests of Explain Plan.
    #[cfg(test)]
    pub(crate) fn plans_for_test(&self) -> Vec<(String, Option<ExplainPlan>)> {
        self.results
            .iter()
            .filter(|tab| tab.explain)
            .map(|tab| (tab.statement.clone(), tab.plan.clone()))
            .collect()
    }

    /// Whether the Export form of a result tab is open, and its format.
    #[cfg(test)]
    pub(crate) fn export_dialog_for_test(&self) -> Option<ExportFormat> {
        self.export_dialog.as_ref().map(|dialog| dialog.format)
    }

    /// The tabs of the statements of the console, for the window tests.
    #[cfg(test)]
    pub(crate) fn results_for_test(&self) -> Vec<(String, String, Option<String>)> {
        self.results
            .iter()
            .map(|tab| (tab.title(), tab.summary(), tab.error.clone()))
            .collect()
    }

    /// The statements the console remembers, for the window tests.
    #[cfg(test)]
    pub(crate) fn console_history_for_test(&self) -> Vec<String> {
        self.console_history.clone()
    }

    /// Whether a statement of the selected source still runs, for the window tests.
    #[cfg(test)]
    pub(crate) fn running_for_test(&self) -> bool {
        !self.query_ids.is_empty()
    }

    /// The language side of the window, for the window tests: the application puts one
    /// in place before the window is built, a test after that.
    #[cfg(test)]
    pub(crate) fn set_support_for_test(&mut self, support: Arc<dyn Support>) {
        self.support = Some(support);
    }

    /// The message of the driver notice of the selected source, for the window tests.
    #[cfg(test)]
    pub(crate) fn driver_message_for_test(&self) -> Option<String> {
        let source = self.selected_source()?;
        (self.driver_missing(source) || self.driver_failed.contains(&source.id))
            .then(|| driver_message(source))
    }

    /// The version of the driver in the open form, for the window tests.
    #[cfg(test)]
    pub(crate) fn dialog_version_for_test(&self) -> Option<String> {
        self.dialog
            .as_ref()
            .map(|dialog| dialog.version.text.trim().to_owned())
            .filter(|version| !version.is_empty())
    }

    /// Whether the store of passwords of the test is ready, for the window tests.
    #[cfg(test)]
    pub(crate) fn password_storage_ready_for_test(&self) -> bool {
        self.secrets.ready()
    }

    /// The reason the store of passwords is off, for the window tests.
    #[cfg(test)]
    pub(crate) fn password_storage_reason_for_test(&self) -> Option<String> {
        self.secrets.reason().map(str::to_owned)
    }

    /// Whether the window holds the password of a source in memory, for the window
    /// tests: a password read from the store is in place before a connection starts.
    #[cfg(test)]
    pub(crate) fn has_cached_password_for_test(&self, source: &SourceId) -> bool {
        self.passwords.contains_key(source)
    }

    /// The password the window holds in memory, for the window tests.
    #[cfg(test)]
    pub(crate) fn cached_password_for_test(&self, source: &SourceId) -> Option<String> {
        self.passwords
            .get(source)
            .map(|password| password.expose().to_owned())
    }

    /// The Save password flag of the open form, for the window tests.
    #[cfg(test)]
    pub(crate) fn dialog_save_password_for_test(&self) -> Option<bool> {
        self.dialog.as_ref().map(|dialog| dialog.save_password)
    }

    /// The sources and the selected one, without a connection, for the window tests.
    #[cfg(test)]
    pub(crate) fn sources_for_test(&mut self, sources: Vec<Source>, cx: &mut Context<Self>) {
        self.selected = sources.first().map(|source| source.id.clone());
        self.sources = sources;
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

    /// The answers the scripted connection gives to the data tab, in order.
    #[cfg(test)]
    pub(crate) fn scripted_replies_for_test(&mut self, replies: Vec<ScriptedReply>) {
        if let Some(scripted) = &mut self.scripted {
            scripted.replies = replies.into();
        }
    }

    /// Every request the scripted connection got, with the transaction commands.
    #[cfg(test)]
    pub(crate) fn scripted_commands_for_test(&self) -> Vec<ScriptedCommand> {
        self.scripted
            .as_ref()
            .and_then(|scripted| {
                scripted
                    .commands
                    .lock()
                    .ok()
                    .map(|commands| commands.clone())
            })
            .unwrap_or_default()
    }

    /// The data tab of a table, when it is open.
    #[cfg(test)]
    pub(crate) fn data_view_for_test(
        &self,
        source: &SourceId,
        table: &Table,
        cx: &App,
    ) -> Option<Entity<DatabaseDataView>> {
        let workspace = self.workspace.upgrade()?;
        let path = data_path(workspace.read(cx).root(), source, table);
        workspace
            .read(cx)
            .view_of(&path)?
            .downcast::<DatabaseDataView>()
            .ok()
    }

    /// The application puts the sources, the drivers and the suggestions in place.
    fn refresh_catalog(&mut self) {
        if self.support.is_none() {
            self.support = support();
        }
        let Some(support) = self.support.as_ref() else {
            return;
        };
        self.drivers = support.drivers();
        self.suggestions = support.suggestions();
    }

    /// The package of a driver the language layer can download, when it has one.
    fn package_of(&self, kind: DriverKind) -> Option<database::DriverPackage> {
        self.support
            .as_ref()?
            .packages()
            .into_iter()
            .find(|package| package.kind == kind)
    }

    fn load_sources(&mut self, cx: &mut Context<Self>) {
        let directory = self.state_dir.clone();
        let job = cx
            .background_executor()
            .spawn(async move { SourceStore::load(&directory) });
        cx.spawn(async move |this, cx| {
            let store = job.await;
            this.update(cx, |this, cx| {
                // A window test hands its source to a scripted connection before the file
                // of the project arrives; the file then has nothing to say about it.
                if this.scripted.is_none() {
                    this.sources = store.sources;
                }
                if this.selected.is_none() {
                    this.selected = this.sources.first().map(|source| source.id.clone());
                }
                this.sources_loaded = true;
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

    /// Asks the store of the machine whether it can keep passwords, in the background:
    /// the answer turns the Save password flag of the form on or gives the reason it is
    /// off.
    fn check_passwords(&mut self, cx: &mut Context<Self>) {
        let store = self.password_store.clone();
        let job = cx
            .background_executor()
            .spawn(async move { store.available() });
        cx.spawn(async move |this, cx| {
            let answer = job.await;
            this.update(cx, |this, cx| {
                this.secrets = match answer {
                    Ok(()) => Secrets::Ready,
                    Err(reason) => Secrets::Unavailable(reason),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Writes the password of a source to the store of the system, in the background.
    fn remember_password(&self, source: &Source, password: Secret, cx: &mut Context<Self>) {
        let serial = self.password_jobs.begin(&source.id);
        let jobs = self.password_jobs.clone();
        let id = source.id.clone();
        let service = database::service_of(&source.id);
        let account = source.user.clone();
        let store = self.password_store.clone();
        let job = cx.background_executor().spawn(async move {
            let _gate = jobs.gate.lock().unwrap_or_else(|error| error.into_inner());
            if !jobs.is_latest(&id, serial) {
                return Ok(());
            }
            store.store(&service, &account, &password)
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = job.await {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("The password is not saved: {error}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Removes the password of a source from the store of the system, in the background.
    fn forget_password(&self, source: &Source, cx: &mut Context<Self>) {
        let serial = self.password_jobs.begin(&source.id);
        let jobs = self.password_jobs.clone();
        let id = source.id.clone();
        let service = database::service_of(&source.id);
        let account = source.user.clone();
        let store = self.password_store.clone();
        let job = cx.background_executor().spawn(async move {
            let _gate = jobs.gate.lock().unwrap_or_else(|error| error.into_inner());
            if !jobs.is_latest(&id, serial) {
                return Ok(());
            }
            store.delete(&service, &account)
        });
        cx.spawn(async move |this, cx| {
            if let Err(error) = job.await {
                this.update(cx, |this, cx| {
                    this.status = Some(format!("The password is not removed: {error}"));
                    cx.notify();
                })
                .ok();
            }
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
                self.errors.insert(
                    source.clone(),
                    "The connection to the helper is closed.".into(),
                );
                // A statement of a tool cannot finish without its helper.
                let waiting: Vec<u64> = self.tool_queries.keys().copied().collect();
                for query in waiting {
                    if let Some(job) = self.tool_queries.remove(&query) {
                        job.fail(format!("the connection to {source} is closed"));
                    }
                }
                cx.notify();
            }
        }
    }

    fn event(&mut self, source: SourceId, event: Event, cx: &mut Context<Self>) {
        if self.history_event(&source, event.clone(), cx) {
            return;
        }
        if self.data_event(&source, event.clone(), cx) {
            return;
        }
        if self.export_event(&source, &event, cx) {
            return;
        }
        if self.count_event(&event, cx) {
            return;
        }
        if self.transaction_event(&source, &event, cx) {
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
                self.connect_ids.remove(&source);
                self.driver_failed.remove(&source);
                // A connection starts in the mode the driver reports; a source opened in
                // Manual keeps it across a reconnect.
                self.manual.entry(source.clone()).or_insert(!auto_commit);
                self.pending.remove(&source);
                self.quotes.insert(source.clone(), quote);
                self.status = Some(format!("Connected to {product}"));
                self.request_history(source.clone(), cx);
                self.request_schema(source, cx);
            }
            Event::Columns { id, columns } => {
                if let Some(index) = self.tab_of(id) {
                    let tab = &mut self.results[index];
                    tab.result = Some(ResultSet {
                        columns,
                        rows: Vec::new(),
                        update_count: None,
                        truncated: false,
                    });
                    tab.bytes = 0;
                    tab.error = None;
                    tab.elapsed = None;
                    if self.active_result != Some(tab.id) {
                        self.active_result = Some(tab.id);
                        self.selected_cell = None;
                    }
                    cx.notify();
                }
            }
            Event::Rows { id, rows } => {
                self.add_rows(source, id, rows, cx);
            }
            Event::Done {
                id,
                update_count,
                truncated,
                cancelled,
                elapsed,
            } => {
                if let Some(index) = self.tab_of(id) {
                    {
                        let tab = &mut self.results[index];
                        tab.elapsed = Some(tab.started.elapsed().max(elapsed));
                        if let Some(count) = update_count {
                            match &mut tab.result {
                                Some(result) => result.update_count = Some(count),
                                None => {
                                    tab.result = Some(ResultSet {
                                        update_count: Some(count),
                                        ..ResultSet::default()
                                    })
                                }
                            }
                        }
                        tab.total = None;
                        tab.counting = false;
                        tab.settle(truncated, cancelled);
                    }
                    self.finish_statement(index, &source, cx);
                    self.tabs_by_query.remove(&id);
                    if self.query_ids.get(&source) == Some(&id) {
                        self.query_ids.remove(&source);
                    }
                    if cancelled && self.script_stop {
                        self.script_halt = true;
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
                if self.connect_ids.get(&source) == Some(&id) {
                    // The helper answered the connect command: the driver or the database
                    // refused it. The text of the driver is the reason the user needs.
                    self.connect_ids.remove(&source);
                    self.errors
                        .insert(source.clone(), format!("The connection failed: {message}"));
                    if driver_refused(&message) {
                        self.driver_failed.insert(source.clone());
                    }
                    // The helper has no connection of its own; the next attempt starts a
                    // fresh one, with the driver the user may download in between.
                    self.sessions.remove(&source);
                    cx.notify();
                    return;
                }
                if let Some(index) = self.tab_of(id) {
                    let text = if cancelled {
                        "The statement was cancelled.".to_owned()
                    } else {
                        message
                    };
                    let tab = &mut self.results[index];
                    tab.elapsed = Some(tab.started.elapsed());
                    tab.cancelled = cancelled;
                    tab.error = Some(text.clone());
                    tab.result = None;
                    // A statement a tool asked for is answered with the error the driver
                    // gave, before the tab loses its place.
                    self.answer_tool_query(id);
                    self.tabs_by_query.remove(&id);
                    if self.query_ids.get(&source) == Some(&id) {
                        self.query_ids.remove(&source);
                    }
                    self.errors.insert(source.clone(), text);
                    if self.script_stop {
                        self.script_halt = true;
                    }
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
                self.sync_schema(&source, cx);
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
                // A helper that never answered leaves its reason on the error stream; a
                // generic "the helper ended" hides the driver class or the server the
                // user has to fix.
                let reason = self
                    .sessions
                    .get(&source)
                    .and_then(|session| database::explain_output(&session.output_tail()));
                self.sessions.remove(&source);
                self.connect_ids.remove(&source);
                self.errors.insert(
                    source,
                    reason.unwrap_or_else(|| {
                        "The helper has ended; connect the source again.".into()
                    }),
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
        let Some(index) = self.tab_of(id) else {
            return;
        };
        let tab = &mut self.results[index];
        let Some(result) = &mut tab.result else {
            return;
        };
        let mut truncated = result.truncated;
        for row in rows {
            if result.rows.len() >= MAX_RESULT_ROWS || tab.bytes >= MAX_RESULT_BYTES {
                truncated = true;
                break;
            }
            tab.bytes += row.iter().map(value_bytes).sum::<usize>();
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

    /// Counts the rows of a limited statement when the user asks for it, as DataGrip
    /// counts on a click: the statement itself ran once, and its count runs here with the
    /// indicator and Stop of the console.
    fn count_rows(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.results.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !tab.countable() {
            return;
        }
        let source = tab.source.clone();
        let Some(sql) = count_statement(&tab.statement) else {
            return;
        };
        // One statement per source runs at a time, and the tab of its answer is one.
        if self.query_ids.contains_key(&source) {
            return;
        }
        let scripted = self.scripted.as_ref().map(|scripted| {
            (
                scripted.source.clone(),
                scripted.hangs,
                scripted.count,
                scripted.queries.clone(),
            )
        });
        if let Some((known, hangs, count, queries)) = scripted {
            if known != source {
                return;
            }
            if let Ok(mut queries) = queries.lock() {
                queries.push((sql.clone(), None));
            }
            if hangs {
                let id = self.next();
                self.count_ids.insert(id, tab_id);
                self.query_ids.insert(source, id);
                self.mark_counting(tab_id, true, cx);
                return;
            }
            // A scripted connection knows its answer: the window test reads the count.
            if let Some(count) = count
                && let Some(tab) = self.results.iter_mut().find(|tab| tab.id == tab_id)
            {
                tab.total = Some(count);
            }
            cx.notify();
            return;
        }
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        if session
            .send(Command::Query {
                id,
                sql,
                timeout: None,
                max_rows: None,
                params: Vec::new(),
            })
            .is_ok()
        {
            self.count_ids.insert(id, tab_id);
            self.query_ids.insert(source, id);
            self.mark_counting(tab_id, true, cx);
        }
    }

    /// Shows or clears the indicator of a count in flight on the tab that asked.
    fn mark_counting(&mut self, tab_id: u64, counting: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.results.iter_mut().find(|tab| tab.id == tab_id) {
            tab.counting = counting;
        }
        cx.notify();
    }

    /// The answer of a count query: the first cell is the number of rows.
    fn count_event(&mut self, event: &Event, cx: &mut Context<Self>) -> bool {
        let id = match event {
            Event::Columns { id, .. }
            | Event::Rows { id, .. }
            | Event::Done { id, .. }
            | Event::Error { id, .. } => *id,
            _ => return false,
        };
        let Some(tab_id) = self.count_ids.get(&id).copied() else {
            return false;
        };
        match event {
            Event::Rows { rows, .. } => {
                let count = rows.first().and_then(|row| row.first()).and_then(count_of);
                if let Some(count) = count
                    && let Some(tab) = self.results.iter_mut().find(|tab| tab.id == tab_id)
                {
                    tab.total = Some(count);
                    tab.counting = false;
                    cx.notify();
                }
            }
            Event::Done { .. } | Event::Error { .. } => {
                self.count_ids.remove(&id);
                self.query_ids.retain(|_, active| *active != id);
                self.mark_counting(tab_id, false, cx);
            }
            _ => {}
        }
        true
    }

    /// Hands the loaded schema of a source to the language of its console, so a keystroke
    /// completes and checks against it without asking the database.
    fn sync_schema(&self, source: &SourceId, cx: &App) {
        let Some(language) = crate::database_console::language_for(&self.state_dir, cx) else {
            return;
        };
        let Some(known) = self.sources.iter().find(|known| &known.id == source) else {
            return;
        };
        let tables = self.tables.get(source).cloned().unwrap_or_default();
        let default = self.default_schemas.get(source).cloned().or_else(|| {
            tables
                .iter()
                .find(|table| !table.schema.is_empty())
                .map(|table| table.schema.clone())
        });
        language.set_source(source.clone(), tables, Dialect::of(known.kind), default);
    }

    /// The schemas of the loaded tree of a source, as the toolbar offers them.
    fn schemas_of(&self, source: &SourceId) -> Vec<String> {
        let mut schemas: Vec<String> = Vec::new();
        for table in self.tables.get(source).into_iter().flatten() {
            if !table.schema.is_empty()
                && !schemas
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&table.schema))
            {
                schemas.push(table.schema.clone());
            }
        }
        schemas.sort();
        schemas
    }

    /// The default schema of a source: the one the user chose, or the first of its tree.
    fn default_schema(&self, source: &SourceId) -> Option<String> {
        self.default_schemas
            .get(source)
            .cloned()
            .or_else(|| self.schemas_of(source).into_iter().next())
    }

    /// Records a statement the user ran, newest first, for the history of the console.
    fn remember_statement(&mut self, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        if self
            .console_history
            .first()
            .is_some_and(|known| known == sql)
        {
            return;
        }
        self.console_history.insert(0, sql.to_owned());
        self.console_history.truncate(50);
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
            self.tables.insert(source.clone(), Vec::new());
            // The tree is being read again: the console completes nothing until the
            // new schema arrives, so it never offers a table that is gone.
            self.sync_schema(&source, cx);
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

    /// Whether the source has no driver the helper could load: no jar at all, or a jar
    /// that is no longer on the disk. The window then offers the download before it
    /// starts a helper that cannot connect.
    fn driver_missing(&self, source: &Source) -> bool {
        let jars = self.jars_of(source);
        jars.is_empty() || jars.iter().any(|jar| !jar.is_file())
    }

    fn connect_source(&mut self, source: Source, password: Secret, cx: &mut Context<Self>) {
        if self.sessions.contains_key(&source.id) || self.scripted.is_some() {
            return;
        }
        if self.driver_missing(&source) {
            // The notice above the result already names the product and offers the
            // download; a helper started without a driver could only fail.
            cx.notify();
            return;
        }
        let jars = self.jars_of(&source);
        let Some(support) = self.support.clone().or_else(support) else {
            self.errors.insert(
                source.id.clone(),
                "The language support of the Database window is not ready.".into(),
            );
            cx.notify();
            return;
        };
        let id = self.next();
        self.connect_ids.insert(source.id.clone(), id);
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
                        this.connect_ids.remove(&source.id);
                        this.errors.insert(source.id.clone(), error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the stable versions of the driver of the form in the background, so the
    /// field shows one and the user may pick another.
    fn load_versions(&mut self, cx: &mut Context<Self>) {
        let Some(support) = self.support.clone() else {
            return;
        };
        let Some(kind) = self.dialog.as_ref().map(|dialog| dialog.kind) else {
            return;
        };
        if self.package_of(kind).is_none() {
            if let Some(dialog) = &mut self.dialog
                && dialog.kind == kind
            {
                dialog.versions.clear();
                dialog.versions_for = None;
                dialog.versions_error = None;
            }
            return;
        }
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        if dialog.versions_loading || dialog.versions_for == Some(kind) {
            return;
        }
        dialog.versions_loading = true;
        dialog.versions_for = Some(kind);
        dialog.versions.clear();
        dialog.versions_error = None;
        let job = cx
            .background_executor()
            .spawn(async move { support.versions(kind) });
        cx.spawn(async move |this, cx| {
            let answer = job.await;
            this.update(cx, |this, cx| {
                let Some(dialog) = &mut this.dialog else {
                    return;
                };
                if dialog.kind != kind {
                    return;
                }
                dialog.versions_loading = false;
                match answer {
                    Ok(versions) => {
                        if dialog.version.text.trim().is_empty()
                            && let Some(newest) = versions.first()
                        {
                            dialog.version = TextField::from_text(newest.clone());
                        }
                        dialog.versions = versions;
                    }
                    Err(reason) => dialog.versions_error = Some(reason),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Downloads the driver of the source, or of the form when `form` is set. The work
    /// is on a background thread; the window shows its bytes and stops it on request.
    fn start_download(
        &mut self,
        source: Option<SourceId>,
        form: bool,
        version: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.driver_download.is_some() {
            return;
        }
        let kind = if form {
            self.dialog.as_ref().map(|dialog| dialog.kind)
        } else {
            source
                .as_ref()
                .and_then(|source| self.sources.iter().find(|known| &known.id == source))
                .map(|source| source.kind)
        };
        let Some(kind) = kind else {
            return;
        };
        let Some(support) = self.support.clone().or_else(support) else {
            self.status = Some("The language support of the Database window is not ready.".into());
            cx.notify();
            return;
        };
        if self.package_of(kind).is_none() {
            self.status = Some(format!("No {} driver can be downloaded.", kind.title()));
            cx.notify();
            return;
        }
        if form && let Some(dialog) = &mut self.dialog {
            dialog.driver_error = None;
        }
        let progress = Arc::new(DownloadProgress::new());
        let cancel = Cancel::new();
        let done = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let requested = version.clone();
        let job_progress = progress.clone();
        let job_cancel = cancel.clone();
        let job_done = done.clone();
        let job_result = result.clone();
        let watch_done = done.clone();
        self.driver_download = Some(DriverDownload {
            source,
            form,
            kind,
            version,
            progress,
            cancel,
            result,
        });
        // The cancel token stops the transfer; the task is detached because the window
        // watches the flag rather than the task.
        cx.background_executor()
            .spawn(async move {
                let answer = support
                    .download(kind, requested.as_deref(), &job_progress, &job_cancel)
                    .map(|jar| {
                        let version = requested
                            .or_else(|| {
                                jar.parent()
                                    .and_then(|parent| parent.file_name())
                                    .and_then(|name| name.to_str())
                                    .map(str::to_owned)
                            })
                            .unwrap_or_default();
                        (version, jar)
                    });
                if let Ok(mut slot) = job_result.lock() {
                    *slot = Some(answer);
                }
                job_done.store(true, Ordering::Release);
            })
            .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DOWNLOAD_TICK).await;
                let finished = watch_done.load(Ordering::Acquire);
                let keep = this.update(cx, |this, cx| {
                    if finished {
                        this.finish_download(cx);
                    } else {
                        cx.notify();
                    }
                    this.driver_download.is_some()
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Puts the answer of a finished download in its source or its form.
    fn finish_download(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.driver_download.take() else {
            return;
        };
        let answer = run.result.lock().ok().and_then(|mut slot| slot.take());
        match answer {
            Some(Ok((version, jar))) => {
                if let Some(id) = &run.source {
                    if let Some(source) = self.sources.iter_mut().find(|source| &source.id == id) {
                        source.driver_jar = Some(jar.clone());
                    }
                    self.errors.remove(id);
                    self.driver_failed.remove(id);
                    self.save_sources(cx);
                    // A source that waited for its driver goes on to the helper as soon
                    // as the file is in place.
                    let password = self
                        .passwords
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| Secret::new(""));
                    if let Some(source) =
                        self.sources.iter().find(|source| &source.id == id).cloned()
                    {
                        self.connect_source(source, password, cx);
                    }
                }
                if run.form
                    && let Some(dialog) = &mut self.dialog
                {
                    dialog.version = TextField::from_text(version);
                    dialog.driver = TextField::from_text(jar.display().to_string());
                    dialog.driver_error = None;
                }
                self.status = Some("The driver is downloaded.".to_owned());
            }
            Some(Err(error)) => {
                if let Some(id) = &run.source {
                    self.errors.insert(id.clone(), error.clone());
                }
                if run.form
                    && let Some(dialog) = &mut self.dialog
                {
                    dialog.driver_error = Some(error.clone());
                }
                self.status = Some(error);
            }
            None => {
                let message = "The download ended without a file.".to_owned();
                if let Some(id) = run.source {
                    self.errors.insert(id, message.clone());
                }
                self.status = Some(message);
            }
        }
        cx.notify();
    }

    fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.driver_download {
            run.cancel.cancel();
            self.status = Some("Stopping the download…".into());
            cx.notify();
        }
    }

    /// Disconnects a source as its toolbar button does: an open transaction asks first.
    fn disconnect_source(&mut self, source: SourceId, cx: &mut Context<Self>) {
        if self.pending_count(&source) > 0 {
            self.question = Some(PendingQuestion {
                source: source.clone(),
                action: PendingAction::Disconnect(source),
            });
            cx.notify();
            return;
        }
        self.disconnect(source, cx);
    }

    /// Removes a source as its menu does: an open transaction asks first, because the
    /// connection goes away and the database rolls it back.
    fn remove_source_asked(&mut self, source: SourceId, cx: &mut Context<Self>) {
        if self.pending_count(&source) > 0 {
            self.question = Some(PendingQuestion {
                source: source.clone(),
                action: PendingAction::RemoveSource(source),
            });
            cx.notify();
            return;
        }
        self.remove_source(source, cx);
    }

    fn disconnect(&mut self, source: SourceId, cx: &mut Context<Self>) {
        self.sessions.remove(&source);
        self.query_ids.remove(&source);
        self.connect_ids.remove(&source);
        self.quotes.remove(&source);
        self.schema_loading.remove(&source);
        // The connection is gone, so its transaction is over: the database rolled it
        // back when the helper ended.
        self.pending.remove(&source);
        self.transaction_ids
            .retain(|_, command| command.source != source);
        if self.export.as_ref().is_some_and(|job| job.source == source) {
            self.export = None;
        }
        self.history_queue.clear();
        self.history_pending = None;
        self.history_collected.clear();
        self.history_read = false;
        // A statement of the closed connection will not answer: its tab says so.
        for tab in &mut self.results {
            if tab.source == source && tab.running() {
                tab.elapsed = Some(tab.started.elapsed());
                tab.error = Some("The connection is closed.".to_owned());
            }
        }
        self.tabs_by_query.clear();
        self.count_ids.clear();
        if let Some(language) = crate::database_console::language_for(&self.state_dir, cx) {
            language.clear_source(&source);
        }
        cx.notify();
    }

    /// Removes a source from the project and its password from the store of the system.
    /// The console file stays where it is: it holds the statements of the user, which
    /// removing a connection does not throw away.
    pub(crate) fn remove_source(&mut self, source: SourceId, cx: &mut Context<Self>) {
        let Some(index) = self.sources.iter().position(|known| known.id == source) else {
            return;
        };
        let removed = self.sources.remove(index);
        self.disconnect(source.clone(), cx);
        self.tables.remove(&source);
        self.errors.remove(&source);
        self.driver_failed.remove(&source);
        self.expanded.retain(|(known, _, _)| known != &source);
        self.passwords.remove(&source);
        if self.selected.as_ref() == Some(&source) {
            self.selected = self
                .sources
                .get(index.min(self.sources.len().saturating_sub(1)))
                .map(|next| next.id.clone());
        }
        self.save_sources(cx);
        if self.secrets.ready() {
            self.forget_password(&removed, cx);
        }
        cx.notify();
    }

    fn run_sql(
        &mut self,
        source: SourceId,
        text: String,
        max_rows: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.start_queue(source, text, max_rows, false, cx);
    }

    /// Run SQL Script: the whole file, with the choice of the settings about a failure.
    fn run_script(&mut self, source: SourceId, text: String, cx: &mut Context<Self>) {
        let stop = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.sql_script_stop_on_error);
        let rows = Some(self.console_rows(cx));
        self.start_queue(source, text, rows, stop, cx);
    }

    /// Queues the statements of `text` and runs the first one.
    fn start_queue(
        &mut self,
        source: SourceId,
        text: String,
        max_rows: Option<usize>,
        stop_on_error: bool,
        cx: &mut Context<Self>,
    ) {
        let scripted_source = self
            .scripted
            .as_ref()
            .is_some_and(|scripted| scripted.source == source);
        if self.scripted.is_some() && !scripted_source {
            return;
        }
        if self.scripted.is_none() {
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
        }
        let statements = database::split_statements(&text);
        if statements.is_empty() {
            return;
        }
        self.queue.clear();
        self.script_stop = stop_on_error;
        self.script_halt = false;
        for statement in statements {
            self.queue.push_back(QueuedStatement {
                source: source.clone(),
                sql: statement,
                max_rows,
                params: None,
            });
        }
        self.run_next(cx);
    }

    /// Runs the next statement of the queue. A statement with placeholders is not sent
    /// until the user gave their values: the dialog opens and the queue waits.
    fn run_next(&mut self, cx: &mut Context<Self>) {
        loop {
            if self.script_halt {
                self.queue.clear();
                self.script_halt = false;
                self.script_stop = false;
                cx.notify();
                return;
            }
            let Some(mut queued) = self.queue.pop_front() else {
                self.script_stop = false;
                return;
            };
            if self.scripted.is_none() && !self.sessions.contains_key(&queued.source) {
                continue;
            }
            if queued.params.is_none() {
                let asked = asked_parameters(&queued.sql);
                if !asked.is_empty() {
                    self.ask_parameters(queued, asked, cx);
                    return;
                }
                queued.params = Some(self.remembered_params(&queued.source, &queued.sql));
            }
            let params = queued.params.unwrap_or_default();
            if !self.start_statement(
                queued.source,
                queued.sql,
                queued.max_rows,
                params,
                false,
                cx,
            ) {
                return;
            }
        }
    }

    /// Sets up the tab of one statement and sends it to the helper. The answer is `false`
    /// when the statement runs and the console waits for it.
    fn start_statement(
        &mut self,
        source: SourceId,
        sql: String,
        max_rows: Option<usize>,
        params: Vec<Param>,
        explain: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let id = self.next();
        if let Some(job) = self.pending_tool_job.take() {
            self.tool_queries.insert(id, job);
        }
        let tab = if explain {
            ConsoleResult::explain(id, sql.clone(), source.clone())
        } else {
            ConsoleResult::new(id, sql.clone(), source.clone(), max_rows)
        };
        let tab_id = tab.id;
        self.results.push(tab);
        self.active_result = Some(tab_id);
        self.selected_cell = None;
        self.sort = None;
        self.errors.remove(&source);
        if !explain {
            self.remember_statement(&sql);
        }
        self.tabs_by_query.insert(id, tab_id);
        if self.scripted.is_some() {
            let rows = {
                let Some(scripted) = self.scripted.as_mut() else {
                    return true;
                };
                if let Ok(mut queries) = scripted.queries.lock() {
                    queries.push((sql.clone(), max_rows));
                }
                if let Ok(mut sent) = scripted.param_queries.lock() {
                    sent.push((sql.clone(), params.clone()));
                }
                if scripted.hangs {
                    self.query_ids.insert(source.clone(), id);
                    cx.notify();
                    return false;
                }
                let answer = scripted.console_answers.pop_front();
                let error = scripted.error.clone();
                let reply = scripted.console_replies.pop_front();
                match answer {
                    Some(answer) => answer,
                    None => match (error, reply) {
                        (Some(error), _) => Err(error),
                        (None, reply) => Ok(reply.unwrap_or_else(|| scripted.rows.clone())),
                    },
                }
            };
            let index = self.results.len() - 1;
            let tab = &mut self.results[index];
            tab.elapsed = Some(tab.started.elapsed());
            match rows {
                Err(error) => {
                    tab.error = Some(error.clone());
                    self.errors.insert(source.clone(), error);
                    if self.script_stop {
                        self.script_halt = true;
                    }
                }
                Ok(result) => {
                    tab.result = Some(result);
                    tab.bytes = 0;
                    tab.settle(false, false);
                }
            }
            self.finish_statement(index, &source, cx);
            self.tabs_by_query.remove(&id);
            return true;
        }
        let Some(session) = self.sessions.get(&source) else {
            self.tabs_by_query.remove(&id);
            return true;
        };
        if session
            .send(Command::Query {
                id,
                sql,
                timeout: None,
                max_rows,
                params,
            })
            .is_ok()
        {
            self.query_ids.insert(source, id);
            cx.notify();
            return false;
        }
        self.tabs_by_query.remove(&id);
        true
    }

    /// The last thing a statement does with its tab: a plan becomes a tree, and a change
    /// of a source in Manual has to be committed or rolled back.
    fn finish_statement(&mut self, index: usize, source: &SourceId, cx: &mut Context<Self>) {
        let Some(tab) = self.results.get(index) else {
            return;
        };
        let explain = tab.explain;
        let changed = tab
            .result
            .as_ref()
            .and_then(|result| result.update_count)
            .is_some();
        if explain {
            self.build_plan(index);
        }
        if changed && !self.auto_commit(source) {
            *self.pending.entry(source.clone()).or_insert(0) += 1;
        }
        // A statement a tool asked for is answered by the tab the console finished.
        let tab_id = self.results.get(index).map(|tab| tab.id);
        if let Some(query) = tab_id.and_then(|tab_id| {
            self.tabs_by_query
                .iter()
                .find(|(_, tab)| **tab == tab_id)
                .map(|(query, _)| *query)
        }) {
            self.answer_tool_query(query);
        }
        cx.notify();
    }

    /// Turns the rows of an explained statement into the plan of its tab.
    fn build_plan(&mut self, index: usize) {
        let Some(tab) = self.results.get(index) else {
            return;
        };
        if !tab.explain {
            return;
        }
        let dialect = self.dialect_of(&tab.source);
        let (columns, rows) = match &tab.result {
            Some(result) => (result.columns.clone(), result.rows.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let plan: ExplainPlan = database::plan_of(dialect, &columns, &rows);
        if let Some(tab) = self.results.get_mut(index) {
            tab.plan = Some(plan);
        }
    }

    /// The dialect of the kind of a source.
    fn dialect_of(&self, source: &SourceId) -> Dialect {
        self.sources
            .iter()
            .find(|known| &known.id == source)
            .map(|known| Dialect::of(known.kind))
            .unwrap_or_default()
    }

    /// The values the console remembers for the placeholders of a source.
    fn remembered_values(&self, source: &SourceId) -> HashMap<String, String> {
        self.param_values
            .iter()
            .filter(|((known, _), _)| known == source)
            .map(|((_, key), value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The remembered values as the driver binds them, for a statement that is sent
    /// without a question: Explain Plan of a statement with placeholders.
    fn remembered_params(&self, source: &SourceId, sql: &str) -> Vec<Param> {
        bind_parameters(sql, &self.remembered_values(source))
    }

    /// Asks for the values of a statement before it runs, with the last values in place.
    fn ask_parameters(
        &mut self,
        queued: QueuedStatement,
        asked: Vec<database::Parameter>,
        cx: &mut Context<Self>,
    ) {
        let remembered = self.remembered_values(&queued.source);
        let fields = asked
            .into_iter()
            .map(|parameter| AskField {
                value: TextField::from_text(
                    remembered.get(&parameter.key).cloned().unwrap_or_default(),
                ),
                label: parameter.label,
                key: parameter.key,
            })
            .collect();
        self.params_ask = Some(AskParameters {
            source: queued.source,
            sql: queued.sql,
            max_rows: queued.max_rows,
            fields,
            focused: 0,
        });
        // The fields take the keys at once, as a dialog of the console does.
        if let Some(window) = self.window {
            let focus = self.focus.clone();
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, cx| window.focus(&focus, cx))
                    .ok();
            });
        }
        cx.notify();
    }

    /// Runs the statement that waited for its values, or drops the run when the user
    /// cancelled.
    fn answer_parameters(&mut self, run: bool, cx: &mut Context<Self>) {
        let Some(ask) = self.params_ask.take() else {
            return;
        };
        if !run {
            self.queue.clear();
            cx.notify();
            return;
        }
        let values = ask.answered();
        for (key, value) in &values {
            self.param_values
                .insert((ask.source.clone(), key.clone()), value.clone());
        }
        // The statement is answered: it joins the queue with its values, so it does not
        // ask again on the way to the helper.
        let params = bind_parameters(&ask.sql, &values);
        self.queue.push_front(QueuedStatement {
            source: ask.source,
            sql: ask.sql,
            max_rows: ask.max_rows,
            params: Some(params),
        });
        self.run_next(cx);
    }

    /// Explains the statement of a console: its plan opens as a tab of the console.
    pub(crate) fn explain(&mut self, source: SourceId, statement: String, cx: &mut Context<Self>) {
        let Some(sql) = database::split_statements(&statement).into_iter().next() else {
            return;
        };
        if self.scripted.is_none() && !self.sessions.contains_key(&source) {
            self.errors.insert(
                source.clone(),
                "Connect the source before explaining its console.".into(),
            );
            cx.notify();
            return;
        }
        let params = self.remembered_params(&source, &sql);
        let query = database::explain_statement(self.dialect_of(&source), &sql);
        self.start_statement(source, query, None, params, true, cx);
    }

    /// Opens the Export form of a result tab. The statement is not run again here: an
    /// export is a command of the user, and a statement that writes has no export.
    fn open_export(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.results.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !tab.can_export() {
            return;
        }
        let table = database::single_table(&tab.statement);
        self.export_dialog = Some(ExportDialog {
            tab_id,
            format: ExportFormat::Csv,
            table: TextField::from_text(
                table
                    .as_ref()
                    .map(|table| table.name.clone())
                    .unwrap_or_default(),
            ),
            known_table: table.is_some(),
        });
        cx.notify();
    }

    /// Starts the export the form asked for: the statement runs again in full, and the
    /// rows become the text of the format while they arrive.
    fn start_export(&mut self, target: ExportTarget, cx: &mut Context<Self>) {
        let Some(dialog) = self.export_dialog.take() else {
            return;
        };
        let Some(tab) = self.results.iter().find(|tab| tab.id == dialog.tab_id) else {
            return;
        };
        let source = tab.source.clone();
        let statement = tab.statement.clone();
        let table = if dialog.known_table {
            database::single_table(&statement).unwrap_or_default()
        } else {
            TableRef::new("", dialog.table.text.trim())
        };
        if dialog.format == ExportFormat::SqlInsert && table.name.is_empty() {
            self.status = Some("Name the table of the INSERT statements.".to_owned());
            self.export_dialog = Some(dialog);
            cx.notify();
            return;
        }
        if self.scripted.is_none() && !self.sessions.contains_key(&source) {
            self.status = Some("Connect the source before exporting its result.".to_owned());
            cx.notify();
            return;
        }
        if self.query_ids.contains_key(&source) {
            self.status = Some("A statement of this source still runs.".to_owned());
            cx.notify();
            return;
        }
        let mut exporter = Exporter::new(dialog.format, self.dialect_of(&source), table);
        if let Some(scripted) = self.scripted.as_mut() {
            if scripted.source != source {
                return;
            }
            if let Ok(mut queries) = scripted.queries.lock() {
                queries.push((statement.clone(), None));
            }
            let answer = scripted
                .console_replies
                .pop_front()
                .unwrap_or_else(|| scripted.rows.clone());
            exporter.start(&answer.columns);
            for row in &answer.rows {
                exporter.push(row);
            }
            self.finish_export(exporter, target, cx);
            return;
        }
        let id = self.next();
        let params = self.remembered_params(&source, &statement);
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        let sent = session
            .send(Command::Query {
                id,
                sql: statement,
                timeout: None,
                max_rows: None,
                params,
            })
            .is_ok();
        if sent {
            self.export = Some(ExportJob {
                tab_id: dialog.tab_id,
                source: source.clone(),
                id,
                exporter,
                target,
            });
            self.query_ids.insert(source, id);
            cx.notify();
        }
    }

    /// Writes the text of a finished export to its file or its clipboard.
    fn finish_export(&mut self, exporter: Exporter, target: ExportTarget, cx: &mut Context<Self>) {
        let rows = exporter.rows();
        let text = exporter.finish();
        match target {
            ExportTarget::Clipboard => {
                cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text));
                self.status = Some(format!("{rows} rows copied."));
                cx.notify();
            }
            ExportTarget::File(path) => {
                let job = cx
                    .background_executor()
                    .spawn(async move { std::fs::write(&path, text) });
                cx.spawn(async move |this, cx| {
                    let result = job.await;
                    this.update(cx, |this, cx| {
                        this.status = Some(match &result {
                            Ok(()) => format!("{rows} rows exported."),
                            Err(error) => format!("The export failed: {error}"),
                        });
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// The answers of an export: its columns, its rows and its end.
    fn export_event(&mut self, source: &SourceId, event: &Event, cx: &mut Context<Self>) -> bool {
        let id = match event {
            Event::Columns { id, .. }
            | Event::Rows { id, .. }
            | Event::Done { id, .. }
            | Event::Error { id, .. } => *id,
            _ => return false,
        };
        let Some(job) = self.export.as_ref().filter(|job| job.id == id) else {
            return false;
        };
        if &job.source != source {
            return false;
        }
        match event {
            Event::Columns { columns, .. } => {
                if let Some(job) = &mut self.export {
                    job.exporter.start(columns);
                }
                cx.notify();
            }
            Event::Rows { rows, .. } => {
                if let Some(job) = &mut self.export {
                    for row in rows {
                        job.exporter.push(row);
                    }
                }
                if let Some(session) = self.sessions.get(source) {
                    // A full batch waits for a fetch, as it does for a console result.
                    let _ = session.send(Command::Fetch { id });
                }
                cx.notify();
            }
            Event::Done {
                cancelled: true, ..
            } => {
                let rows = self.export.take().map_or(0, |job| job.exporter.rows());
                self.query_ids.remove(source);
                self.status = Some(format!("The export was stopped after {rows} rows."));
                cx.notify();
            }
            Event::Done { .. } => {
                let Some(job) = self.export.take() else {
                    return true;
                };
                self.query_ids.remove(source);
                self.finish_export(job.exporter, job.target, cx);
            }
            Event::Error {
                message, cancelled, ..
            } => {
                self.export = None;
                self.query_ids.remove(source);
                self.status = Some(if *cancelled {
                    "The export was stopped.".to_owned()
                } else {
                    format!("The export failed: {message}")
                });
                cx.notify();
            }
            _ => {}
        }
        true
    }

    /// The tab an export was made for: the form and the progress name it.
    fn export_title(&self, tab_id: u64) -> Option<String> {
        self.results
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(ConsoleResult::title)
    }

    /// Runs the statement or the selection of the open console of a source, as its
    /// toolbar Execute button does.
    fn execute_console(&mut self, source: SourceId, cx: &mut Context<Self>) {
        let Some(path) = self.console_path(&source) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let view = workspace
            .read(cx)
            .view_of(&path)
            .and_then(|view| view.downcast::<EditorView>().ok());
        let Some(view) = view else {
            // No tab of the console is open: the file still holds the statements the
            // button runs.
            if let Ok(text) = std::fs::read_to_string(&path) {
                self.run_sql(source, text, Some(self.console_rows(cx)), cx);
            }
            return;
        };
        let selection = view.read(cx).selected_text();
        let text = selection.or_else(|| {
            let caret = view.read(cx).caret().0;
            view.read(cx)
                .text()
                .and_then(|text| database::statement_at(&text, caret))
        });
        if let Some(text) = text {
            self.run_sql(source, text, Some(self.console_rows(cx)), cx);
        }
    }

    /// The rows a console statement reads at a time.
    fn console_rows(&self, cx: &App) -> usize {
        cx.try_global::<Settings>()
            .map_or(workspace::DEFAULT_DATABASE_PAGE_ROWS, |settings| {
                settings.database_page_rows
            }) as usize
    }

    /// Stops the statement of the selected source: the helper calls `cancel` on it and
    /// answers the tab, while the window keeps drawing.
    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        self.stop_source(source, cx);
    }

    /// Stops the statement of `source`, which a console or a script of the project runs.
    fn stop_source(&mut self, source: SourceId, cx: &mut Context<Self>) {
        let Some(id) = self.query_ids.get(&source).copied() else {
            return;
        };
        if let Some(scripted) = &self.scripted {
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(ScriptedCommand::Cancel);
            }
            self.finish_cancelled(&source, id, cx);
            return;
        }
        if let Some(session) = self.sessions.get(&source) {
            let _ = session.send(Command::Cancel { id });
        }
        self.status = Some("Stopping the statement…".to_owned());
        cx.notify();
    }

    /// The answer of a stopped statement that has no helper, as a scripted connection.
    fn finish_cancelled(&mut self, source: &SourceId, id: u64, cx: &mut Context<Self>) {
        // An export has no tab of its own: what it read so far stays unwritten.
        if self.export.as_ref().is_some_and(|job| job.id == id) {
            let rows = self.export.take().map_or(0, |job| job.exporter.rows());
            self.query_ids.remove(source);
            self.status = Some(format!("The export was stopped after {rows} rows."));
            cx.notify();
            return;
        }
        // A count has no tab of its own: the tab that asked shows its indicator again.
        if let Some(tab_id) = self.count_ids.remove(&id) {
            self.query_ids.retain(|_, active| *active != id);
            self.status = Some("The count was stopped.".to_owned());
            self.mark_counting(tab_id, false, cx);
            self.run_next(cx);
            return;
        }
        let Some(index) = self.tab_of(id) else {
            return;
        };
        let tab = &mut self.results[index];
        tab.elapsed = Some(tab.started.elapsed());
        tab.cancelled = true;
        tab.error = Some("The statement was cancelled.".to_owned());
        tab.result = None;
        self.tabs_by_query.remove(&id);
        if self.query_ids.get(source) == Some(&id) {
            self.query_ids.remove(source);
        }
        self.errors
            .insert(source.clone(), "The statement was cancelled.".to_owned());
        cx.notify();
        self.run_next(cx);
    }

    /// Runs the statement of a result tab again with a larger limit: the tab then shows
    /// more of it in place.
    fn load_more(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.results.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let source = tab.source.clone();
        let statement = tab.statement.clone();
        let shown = tab.result.as_ref().map_or(0, |result| result.rows.len());
        let step = self.console_rows(cx);
        let limit = shown.max(step) + step;
        if self.scripted.is_some() {
            // The scripted answer never grows; the tab still shows how many rows it read.
            return;
        }
        let params = self.remembered_params(&source, &statement);
        let id = self.next();
        if let Some(index) = self.results.iter().position(|tab| tab.id == tab_id) {
            let tab = &mut self.results[index];
            tab.limit = Some(limit);
            tab.elapsed = None;
            tab.error = None;
            tab.total = None;
            tab.truncated = false;
            tab.bytes = 0;
            tab.started = Instant::now();
            let result = tab.result.take().unwrap_or_default();
            tab.result = Some(ResultSet {
                columns: result.columns,
                rows: Vec::new(),
                update_count: None,
                truncated: false,
            });
        }
        let sent = self.sessions.get(&source).is_some_and(|session| {
            session
                .send(Command::Query {
                    id,
                    sql: statement,
                    timeout: None,
                    max_rows: Some(limit),
                    params,
                })
                .is_ok()
        });
        if sent {
            self.tabs_by_query.insert(id, tab_id);
            self.query_ids.insert(source, id);
            self.active_result = Some(tab_id);
            self.selected_cell = None;
            cx.notify();
        }
    }

    /// Opens the tab of a table or a view: its rows, edited in place.
    pub(crate) fn open_data(
        &mut self,
        source: SourceId,
        table: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(kind) = self
            .sources
            .iter()
            .find(|known| known.id == source)
            .map(|known| known.kind)
        else {
            self.errors
                .insert(source, "The source of the table is gone.".into());
            cx.notify();
            return;
        };
        let rows = cx
            .try_global::<Settings>()
            .map_or(workspace::DEFAULT_DATABASE_PAGE_ROWS, |settings| {
                settings.database_page_rows
            }) as usize;
        let file = data_path(workspace.read(cx).root(), &source, &table);
        let existing = workspace
            .read(cx)
            .view_of(&file)
            .and_then(|view| view.downcast::<DatabaseDataView>().ok());
        let mut guard = None;
        let view = match existing {
            Some(view) => view,
            None => {
                let panel = cx.entity().downgrade();
                let weak = self.workspace.clone();
                let handle = window.window_handle().downcast::<Workspace>();
                let view = cx.new(|cx| {
                    DatabaseDataView::create(
                        weak,
                        panel,
                        handle,
                        file.clone(),
                        source.clone(),
                        table.clone(),
                        Dialect::of(kind),
                        rows,
                        cx,
                    )
                });
                guard = Some(view.downgrade());
                let started = view.clone();
                cx.defer(move |cx| {
                    started.update(cx, |view, cx| view.start(cx));
                });
                view
            }
        };
        let title = table.name.clone();
        let focus = view.read(cx).focus();
        workspace.update(cx, |workspace, cx| {
            workspace.open_view_tab(
                file.clone(),
                title,
                "toolwindows/database",
                view.into(),
                focus,
                window,
                cx,
            );
            // The tab is in place now, so the guard finds it: a close with unsaved edits
            // asks the view instead of dropping them.
            if let Some(weak_view) = guard {
                workspace.set_tab_can_close(
                    &file,
                    std::rc::Rc::new(move |cx| {
                        weak_view
                            .update(cx, |view, cx| view.ask_to_close(cx))
                            .unwrap_or(true)
                    }),
                );
            }
        });
    }

    /// The menu of a source in the tree: its console, or its removal with its password.
    fn source_menu(&mut self, source: Source, position: Point<Pixels>, cx: &mut Context<Self>) {
        let panel = cx.entity();
        let console = panel.clone();
        let console_source = source.id.clone();
        let remove = panel;
        let remove_source = source.id.clone();
        let entries = vec![
            MenuEntry::new("Open Console", move |window, cx| {
                console.update(cx, |panel, cx| {
                    panel.open_console(console_source.clone(), window, cx)
                });
            }),
            MenuEntry::new("Remove", move |_, cx| {
                remove.update(cx, |panel, cx| {
                    panel.remove_source_asked(remove_source.clone(), cx)
                });
            }),
        ];
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    /// The menu of a table in the tree: Open Data, or the console of its source.
    fn table_menu(
        &mut self,
        source: SourceId,
        table: Table,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity();
        let data = panel.clone();
        let source_of_data = source.clone();
        let table_of_data = table.clone();
        let console = panel.clone();
        let source_of_console = source.clone();
        let entries = vec![
            MenuEntry::new("Open Data", move |window, cx| {
                data.update(cx, |panel, cx| {
                    panel.open_data(source_of_data.clone(), table_of_data.clone(), window, cx)
                });
            })
            .shortcut("Enter"),
            MenuEntry::new("Open Console", move |window, cx| {
                console.update(cx, |panel, cx| {
                    panel.open_console(source_of_console.clone(), window, cx)
                });
            }),
        ];
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    /// Sends a statement of a data tab and answers it through the tab.
    pub(crate) fn data_query(
        &mut self,
        source: SourceId,
        sql: String,
        max_rows: Option<usize>,
        ticket: DataTicket,
        reader: Entity<DatabaseDataView>,
        cx: &mut Context<Self>,
    ) {
        let scripted = if let Some(scripted) = &mut self.scripted {
            if scripted.source != source {
                return;
            }
            if let Ok(mut queries) = scripted.queries.lock() {
                queries.push((sql.clone(), max_rows));
            }
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(ScriptedCommand::Query {
                    sql: sql.clone(),
                    max_rows,
                });
            }
            Some(
                scripted
                    .replies
                    .pop_front()
                    .unwrap_or_else(|| ScriptedReply {
                        result: scripted.rows.clone(),
                        error: scripted.error.clone(),
                    }),
            )
        } else {
            None
        };
        if let Some(reply) = scripted {
            self.answer_scripted(reader, ticket, reply, cx);
            return;
        }
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            self.answer_error(
                reader,
                ticket,
                "Connect the source before reading its data.".to_owned(),
                cx,
            );
            return;
        };
        let command = Command::Query {
            id,
            sql,
            timeout: None,
            max_rows,
            params: Vec::new(),
        };
        if let Err(error) = session.send(command) {
            self.answer_error(reader, ticket, error.to_string(), cx);
            return;
        }
        self.data_pending.insert((source, id), (reader, ticket));
        cx.notify();
    }

    /// Sends a transaction command of a data tab: auto-commit, commit or rollback.
    pub(crate) fn data_command(
        &mut self,
        source: SourceId,
        command: DataCommand,
        ticket: DataTicket,
        reader: Entity<DatabaseDataView>,
        cx: &mut Context<Self>,
    ) {
        match command {
            // The data tab and the console share the one connection of a source: its
            // transaction commands end the pending changes of both.
            DataCommand::AutoCommit(on) => {
                self.manual.insert(source.clone(), !on);
                self.pending.remove(&source);
            }
            DataCommand::Commit | DataCommand::Rollback => {
                self.pending.remove(&source);
            }
        }
        if let Some(scripted) = &self.scripted {
            if scripted.source != source {
                return;
            }
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(match command {
                    DataCommand::AutoCommit(on) => ScriptedCommand::AutoCommit(on),
                    DataCommand::Commit => ScriptedCommand::Commit,
                    DataCommand::Rollback => ScriptedCommand::Rollback,
                });
            }
            cx.defer(move |cx| {
                reader.update(cx, |view, cx| {
                    view.event(
                        ticket,
                        DataEvent::Done {
                            update_count: None,
                            cancelled: false,
                        },
                        cx,
                    )
                });
            });
            return;
        }
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            self.answer_error(reader, ticket, "The connection is closed.".to_owned(), cx);
            return;
        };
        let command = match command {
            DataCommand::AutoCommit(on) => Command::AutoCommit { id, on },
            DataCommand::Commit => Command::Commit { id },
            DataCommand::Rollback => Command::Rollback { id },
        };
        if let Err(error) = session.send(command) {
            self.answer_error(reader, ticket, error.to_string(), cx);
            return;
        }
        self.data_pending.insert((source, id), (reader, ticket));
        cx.notify();
    }

    /// Asks the helper to stop the statement of a data tab.
    pub(crate) fn data_cancel(
        &mut self,
        source: SourceId,
        ticket: DataTicket,
        cx: &mut Context<Self>,
    ) {
        if let Some(scripted) = &self.scripted {
            // A scripted answer arrives on the next turn of the window: nothing to stop,
            // but the test still sees that the tab asked.
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(ScriptedCommand::Cancel);
            }
            return;
        }
        let found = self
            .data_pending
            .iter()
            .find(|((known, _), (_, waiting))| known == &source && *waiting == ticket)
            .map(|((_, id), _)| *id);
        if let (Some(id), Some(session)) = (found, self.sessions.get(&source)) {
            let _ = session.send(Command::Cancel { id });
        }
        cx.notify();
    }

    /// The answers of a scripted connection, as the helper sends them.
    fn answer_scripted(
        &mut self,
        reader: Entity<DatabaseDataView>,
        ticket: DataTicket,
        reply: ScriptedReply,
        cx: &mut Context<Self>,
    ) {
        let columns = reply.result.columns.clone();
        let rows = reply.result.rows.clone();
        let update_count = reply.result.update_count;
        cx.defer(move |cx| {
            if !columns.is_empty() {
                let columns = columns.clone();
                reader.update(cx, |view, cx| {
                    view.event(ticket, DataEvent::Columns(columns), cx)
                });
            }
            if !rows.is_empty() {
                let rows = rows.clone();
                reader.update(cx, |view, cx| view.event(ticket, DataEvent::Rows(rows), cx));
            }
            let event = match reply.error {
                Some(message) => DataEvent::Error(message),
                None => DataEvent::Done {
                    update_count,
                    cancelled: false,
                },
            };
            reader.update(cx, |view, cx| view.event(ticket, event, cx));
        });
    }

    fn answer_error(
        &mut self,
        reader: Entity<DatabaseDataView>,
        ticket: DataTicket,
        message: String,
        cx: &mut Context<Self>,
    ) {
        cx.defer(move |cx| {
            reader.update(cx, |view, cx| {
                view.event(ticket, DataEvent::Error(message), cx)
            });
        });
    }

    /// The answer of the helper that belongs to a data tab, when the id is one of theirs.
    fn data_event(&mut self, source: &SourceId, event: Event, cx: &mut Context<Self>) -> bool {
        let id = match &event {
            Event::Columns { id, .. }
            | Event::Rows { id, .. }
            | Event::Done { id, .. }
            | Event::Error { id, .. } => *id,
            _ => return false,
        };
        let Some((reader, ticket)) = self.data_pending.remove(&(source.clone(), id)) else {
            return false;
        };
        let terminal = matches!(event, Event::Done { .. } | Event::Error { .. });
        let batches = matches!(event, Event::Rows { .. });
        let event = match event {
            Event::Columns { columns, .. } => DataEvent::Columns(columns),
            Event::Rows { rows, .. } => DataEvent::Rows(rows),
            Event::Done {
                update_count,
                cancelled,
                ..
            } => DataEvent::Done {
                update_count,
                cancelled,
            },
            Event::Error {
                message, cancelled, ..
            } => DataEvent::Error(if cancelled {
                "The statement was cancelled.".to_owned()
            } else {
                message
            }),
            _ => return false,
        };
        if !terminal {
            self.data_pending
                .insert((source.clone(), id), (reader.clone(), ticket));
        }
        if batches && let Some(session) = self.sessions.get(source) {
            // A full batch waits for a fetch, as it does for the console.
            let _ = session.send(Command::Fetch { id });
        }
        cx.defer(move |cx| {
            reader.update(cx, |view, cx| view.event(ticket, event, cx));
        });
        true
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
        self.window = Some(handle);
        let guard = cx.entity().downgrade();
        let guard_source = source.clone();
        cx.spawn(async move |_, cx| {
            let Ok(path) = job.await else {
                return;
            };
            let guard_path = path.clone();
            let guard_file = guard_path.clone();
            handle
                .update(cx, |workspace, window, cx| {
                    workspace.open_file(path, window, cx);
                    // A close with a transaction still open asks before it drops it.
                    workspace.set_tab_can_close(
                        &guard_path,
                        Rc::new(move |cx| {
                            guard
                                .update(cx, |panel, cx| {
                                    panel.ask_console_close(
                                        guard_source.clone(),
                                        guard_file.clone(),
                                        cx,
                                    )
                                })
                                .unwrap_or(true)
                        }),
                    );
                })
                .ok();
        })
        .detach();
    }

    /// Whether the console of a source runs its statements at once.
    fn auto_commit(&self, source: &SourceId) -> bool {
        !self.manual.get(source).copied().unwrap_or(false)
    }

    /// The statements of a source that wait for Commit or Rollback.
    pub(crate) fn pending_count(&self, source: &SourceId) -> u32 {
        self.pending.get(source).copied().unwrap_or(0)
    }

    /// Switches the console of a source between Auto-commit and Manual. A driver that
    /// turns auto-commit back on commits the transaction it finds, as the driver does.
    fn set_auto_commit(&mut self, source: SourceId, on: bool, cx: &mut Context<Self>) {
        if self.auto_commit(&source) == on {
            return;
        }
        self.manual.insert(source.clone(), !on);
        if on {
            self.pending.remove(&source);
        }
        if let Some(scripted) = &self.scripted {
            if scripted.source != source {
                return;
            }
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(ScriptedCommand::AutoCommit(on));
            }
            cx.notify();
            return;
        }
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            cx.notify();
            return;
        };
        if session.send(Command::AutoCommit { id, on }).is_ok() {
            self.transaction_ids.insert(
                id,
                TransactionCommand {
                    source,
                    kind: Transaction::AutoCommit(on),
                    after: None,
                },
            );
        }
        cx.notify();
    }

    /// Commits or rolls back the transaction of a source, then does `after`. The answer
    /// of the helper decides: a commit that fails keeps the transaction and says why.
    fn decide_transaction(
        &mut self,
        source: SourceId,
        rollback: bool,
        after: Option<PendingAction>,
        cx: &mut Context<Self>,
    ) {
        if let Some(scripted) = &self.scripted {
            if scripted.source != source {
                return;
            }
            if let Ok(mut commands) = scripted.commands.lock() {
                commands.push(if rollback {
                    ScriptedCommand::Rollback
                } else {
                    ScriptedCommand::Commit
                });
            }
            self.pending.remove(&source);
            if let Some(action) = after {
                self.perform(action, cx);
            }
            cx.notify();
            return;
        }
        let id = self.next();
        let Some(session) = self.sessions.get(&source) else {
            return;
        };
        let command = if rollback {
            Command::Rollback { id }
        } else {
            Command::Commit { id }
        };
        if session.send(command).is_err() {
            self.status = Some("The connection is closed.".to_owned());
            cx.notify();
            return;
        }
        self.transaction_ids.insert(
            id,
            TransactionCommand {
                source,
                kind: if rollback {
                    Transaction::Rollback
                } else {
                    Transaction::Commit
                },
                after,
            },
        );
        cx.notify();
    }

    /// The answer of a transaction command: the pending changes are over, or the reason
    /// the driver gave stays in place.
    fn transaction_event(
        &mut self,
        source: &SourceId,
        event: &Event,
        cx: &mut Context<Self>,
    ) -> bool {
        let id = match event {
            Event::Done { id, .. } | Event::Error { id, .. } => *id,
            _ => return false,
        };
        let Some(command) = self.transaction_ids.remove(&id) else {
            return false;
        };
        if command.source != *source {
            return false;
        }
        match event {
            Event::Done { .. } => {
                if !matches!(command.kind, Transaction::AutoCommit(false)) {
                    self.pending.remove(source);
                }
                if let Some(action) = command.after {
                    self.perform(action, cx);
                }
            }
            Event::Error { message, .. } => {
                self.errors
                    .insert(source.clone(), format!("The transaction failed: {message}"));
                self.status = Some("The transaction of the console is still open.".to_owned());
            }
            _ => {}
        }
        cx.notify();
        true
    }

    /// Does what waited for the user to decide the transaction of a source.
    fn perform(&mut self, action: PendingAction, cx: &mut Context<Self>) {
        match action {
            PendingAction::CloseConsole(path) => self.close_console(&path, cx),
            PendingAction::Disconnect(source) => self.disconnect(source, cx),
            PendingAction::RemoveSource(source) => self.remove_source(source, cx),
        }
    }

    /// The guard of a console tab: a transaction still open asks Commit, Rollback or
    /// Cancel before the tab goes.
    pub(crate) fn ask_console_close(
        &mut self,
        source: SourceId,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.pending_count(&source) == 0 {
            return true;
        }
        self.question = Some(PendingQuestion {
            source,
            action: PendingAction::CloseConsole(path),
        });
        cx.notify();
        false
    }

    /// Closes the console tab of `path`, once no transaction waits for its answer.
    fn close_console(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let path = path.to_path_buf();
        // The guard of the tab asks again, and this time nothing waits.
        cx.defer(move |cx| {
            let _ = window.update(cx, |workspace, window, cx| {
                workspace.close_tab_of(&path, window, cx);
            });
        });
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
        let entered = Secret::new(dialog.password.text.clone());
        if !entered.expose().is_empty() {
            self.passwords.insert(id.clone(), entered.clone());
        }
        let save = dialog.save_password && self.secrets.ready();
        self.sources.push(source.clone());
        self.selected = Some(id.clone());
        self.save_sources(cx);
        if self.secrets.ready() {
            if save && !entered.expose().is_empty() {
                self.remember_password(&source, entered, cx);
            } else {
                // The flag is off, or the password was cleared: the old entry of the
                // store must not come back on the next connection.
                self.forget_password(&source, cx);
            }
        }
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
        if let Some(password) = self.passwords.get(&source.id).cloned() {
            self.connect_source(source, password, cx);
            return;
        }
        // The field of the toolbar is the question about the password, as it always was:
        // what the user typed there wins over the store.
        if !self.password.text.is_empty() {
            let password = Secret::new(self.password.text.clone());
            self.connect_source(source, password, cx);
            return;
        }
        if !self.secrets.ready() {
            self.connect_source(source, Secret::new(""), cx);
            return;
        }
        let service = database::service_of(&source.id);
        let account = source.user.clone();
        let store = self.password_store.clone();
        let jobs = self.password_jobs.clone();
        let job = cx.background_executor().spawn(async move {
            let _gate = jobs.gate.lock().unwrap_or_else(|error| error.into_inner());
            store.load(&service, &account)
        });
        cx.spawn(async move |this, cx| {
            let password = job.await;
            this.update(cx, |this, cx| {
                // The source may have gone while the store was read.
                if !this.sources.iter().any(|known| known.id == source.id) {
                    return;
                }
                match password {
                    Some(password) => {
                        this.passwords.insert(source.id.clone(), password.clone());
                        this.connect_source(source, password, cx);
                    }
                    // Nothing is stored: the empty field asks for the password the way
                    // it did before the store existed.
                    None => this.connect_source(source, Secret::new(""), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn copy_cell(&mut self, cx: &mut Context<Self>) {
        let Some((row, column)) = self.selected_cell else {
            return;
        };
        let Some(result) = self.active().and_then(|tab| tab.result.as_ref()) else {
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
        let Some(result) = self.active().and_then(|tab| tab.result.as_ref()) else {
            return;
        };
        let Some(values) = result.rows.get(row) else {
            return;
        };
        let text = values.iter().map(value_text).collect::<Vec<_>>().join("\t");
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text));
    }

    fn sort_by(&mut self, column: usize, cx: &mut Context<Self>) {
        let ascending = !matches!(self.sort, Some((sorted, true)) if sorted == column);
        let Some(result) = self.active_mut().and_then(|tab| tab.result.as_mut()) else {
            return;
        };
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
        if self.params_ask.is_some() {
            let stroke = &event.keystroke;
            if stroke.key == "escape" {
                self.answer_parameters(false, cx);
                cx.stop_propagation();
                return;
            }
            if stroke.key == "enter" {
                self.answer_parameters(true, cx);
                cx.stop_propagation();
                return;
            }
            let mut changed = false;
            if let Some(ask) = &mut self.params_ask {
                if stroke.key == "tab" && !ask.fields.is_empty() {
                    ask.focused = (ask.focused + 1) % ask.fields.len();
                    changed = true;
                } else if let Some(field) = ask.fields.get_mut(ask.focused) {
                    changed = field.value.key(event, false);
                }
            }
            if changed {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if let Some(dialog) = &mut self.export_dialog {
            let stroke = &event.keystroke;
            if stroke.key == "escape" {
                self.export_dialog = None;
                cx.stop_propagation();
                cx.notify();
                return;
            }
            // The name of the table is the only field of the form; the buttons take the
            // pointer.
            if !dialog.known_table && dialog.format == ExportFormat::SqlInsert {
                if dialog.table.key(event, false) {
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
        }
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
        if (stroke.modifiers.control || stroke.modifiers.platform) && stroke.key == "f2" {
            self.stop(cx);
            cx.stop_propagation();
            return;
        }
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
            let menu_source = source.clone();
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
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.select(menu_source.id.clone(), cx);
                            this.source_menu(menu_source.clone(), event.position, cx);
                        }),
                    )
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
                let menu_source = source.id.clone();
                let menu_table = table.clone();
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
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if event.click_count() >= 2 {
                                this.open_data(open_source.clone(), open_table.clone(), window, cx);
                            } else {
                                this.toggle_table(open_source.clone(), toggle.clone(), cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.table_menu(
                                    menu_source.clone(),
                                    menu_table.clone(),
                                    event.position,
                                    cx,
                                );
                            }),
                        )
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

    /// The statements of the console as tabs: the one the grid shows is under the caret.
    fn result_tabs(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut strip = div()
            .id("database-result-tabs")
            .debug_selector(|| "database-result-tabs".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border);
        if self.results.is_empty() {
            return div().into_any_element();
        }
        let mut close = Vec::new();
        for (index, tab) in self.results.iter().enumerate() {
            let id = tab.id;
            let active = self.active_result == Some(id);
            let running = tab.running();
            let title = tab.title();
            let button = div()
                .id(("database-result-tab", index))
                .debug_selector({
                    let name = format!("database-result-tab-{index}");
                    move || name.clone()
                })
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .when(active, |this| this.bg(theme.tab_selected))
                .cursor_pointer()
                .child(if running {
                    icon_sized("general/refresh", TEXT_SMALL, theme)
                } else {
                    icon_sized("toolwindows/database", TEXT_SMALL, theme)
                })
                .child(div().overflow_hidden().child(title))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.active_result = Some(id);
                    this.sort = None;
                    this.selected_cell = None;
                    cx.notify();
                }));
            strip = strip.child(button);
            close.push(
                icon_button(
                    ("database-result-close", index),
                    "general/closeSmall",
                    "Close Result",
                    None,
                    theme,
                )
                .debug_selector({
                    let name = format!("database-result-close-{index}");
                    move || name.clone()
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let was_active = this.active_result == Some(id);
                    this.results.retain(|tab| tab.id != id);
                    this.tabs_by_query.retain(|_, tab| *tab != id);
                    if was_active {
                        this.active_result = this.results.last().map(|tab| tab.id);
                        this.sort = None;
                        this.selected_cell = None;
                    }
                    cx.notify();
                })),
            );
        }
        strip.children(close).into_any_element()
    }

    /// The row above the results: Execute, Stop, the default schema and the history of
    /// the statements of the console of the selected source.
    fn console_toolbar(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut bar = div()
            .id("database-console-toolbar")
            .debug_selector(|| "database-console-toolbar".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border);
        let selected = self.selected.clone();
        let running = selected
            .as_ref()
            .is_some_and(|source| self.query_ids.contains_key(source));
        let connected = selected
            .as_ref()
            .is_some_and(|source| self.scripted.is_some() || self.sessions.contains_key(source));
        if let Some(source) = &selected {
            let id = source.clone();
            bar = bar.child(
                icon_button(
                    "database-console-execute",
                    "run/run",
                    "Execute",
                    Some("Ctrl+Enter".into()),
                    theme,
                )
                .debug_selector(|| "database-console-execute".to_owned())
                .when(connected, |this| {
                    this.on_click(
                        cx.listener(move |this, _, _, cx| this.execute_console(id.clone(), cx)),
                    )
                }),
            );
        }
        bar = bar.child(
            icon_button(
                "database-console-stop",
                "run/stop",
                "Stop",
                Some("Ctrl+F2".into()),
                theme,
            )
            .debug_selector(|| "database-console-stop".to_owned())
            .when(running, |this| {
                this.on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
            }),
        );
        if let Some(source) = &selected {
            let id = source.clone();
            let manual = !self.auto_commit(source);
            let pending = self.pending_count(source);
            let auto_id = id.clone();
            let manual_id = id.clone();
            let commit_id = id.clone();
            let rollback_id = id.clone();
            bar = bar.child(
                small_button("database-console-auto-commit", "Auto-commit", theme)
                    .debug_selector(|| "database-console-auto-commit".to_owned())
                    .when(!manual, |this| this.bg(theme.tab_selected))
                    .when(connected, |this| {
                        this.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_auto_commit(auto_id.clone(), true, cx)
                        }))
                    }),
            );
            bar = bar.child(
                small_button("database-console-manual", "Manual", theme)
                    .debug_selector(|| "database-console-manual".to_owned())
                    .when(manual, |this| this.bg(theme.tab_selected))
                    .when(connected, |this| {
                        this.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_auto_commit(manual_id.clone(), false, cx)
                        }))
                    }),
            );
            if manual {
                bar = bar.child(
                    small_button("database-console-commit", "Commit", theme)
                        .debug_selector(|| "database-console-commit".to_owned())
                        .when(connected, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                this.decide_transaction(commit_id.clone(), false, None, cx)
                            }))
                        }),
                );
                bar = bar.child(
                    small_button("database-console-rollback", "Rollback", theme)
                        .debug_selector(|| "database-console-rollback".to_owned())
                        .when(connected, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                this.decide_transaction(rollback_id.clone(), true, None, cx)
                            }))
                        }),
                );
                bar = bar.child(
                    div()
                        .debug_selector(|| "database-console-pending".to_owned())
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(pending_text(pending)),
                );
            }
            bar = bar.child(
                small_button("database-console-explain", "Explain Plan", theme)
                    .debug_selector(|| "database-console-explain".to_owned())
                    .when(connected, |this| {
                        this.on_click(
                            cx.listener(move |this, _, _, cx| this.explain_console(id.clone(), cx)),
                        )
                    }),
            );
        }
        if let Some(source) = &selected {
            let schema = self
                .default_schema(source)
                .unwrap_or_else(|| "default".to_owned());
            let schema_source = source.clone();
            let history_source = source.clone();
            bar = bar.child(
                small_button("database-console-schema", &schema, theme)
                    .debug_selector(|| "database-console-schema".to_owned())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.schema_menu(schema_source.clone(), event.position, cx)
                        }),
                    ),
            );
            bar = bar.child(
                small_button("database-console-history", "History", theme)
                    .debug_selector(|| "database-console-history".to_owned())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.history_menu(history_source.clone(), event.position, cx)
                        }),
                    ),
            );
        }
        if let Some(job) = &self.export {
            let title = self
                .export_title(job.tab_id)
                .unwrap_or_else(|| "Result".to_owned());
            bar = bar.child(
                div()
                    .debug_selector(|| "database-export-progress".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(format!("Exporting {title}… {} rows", job.exporter.rows())),
            );
        } else if running {
            bar = bar.child(
                div()
                    .debug_selector(|| "database-console-running".to_owned())
                    .text_color(theme.text_muted)
                    .child("Running…"),
            );
        }
        let _ = window;
        bar.into_any_element()
    }

    /// Explains the statement at the caret of the open console, as the toolbar button
    /// does.
    fn explain_console(&mut self, source: SourceId, cx: &mut Context<Self>) {
        let Some(path) = self.console_path(&source) else {
            return;
        };
        let view = self.workspace.upgrade().and_then(|workspace| {
            workspace
                .read(cx)
                .view_of(&path)
                .and_then(|view| view.downcast::<EditorView>().ok())
        });
        let statement = match view {
            Some(view) => {
                let selection = view.read(cx).selected_text();
                selection.or_else(|| {
                    let caret = view.read(cx).caret().0;
                    view.read(cx)
                        .text()
                        .and_then(|text| database::statement_at(&text, caret))
                })
            }
            None => std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| database::statement_at(&text, 0)),
        };
        if let Some(statement) = statement {
            self.explain(source, statement, cx);
        }
    }

    /// The schemas of the tree as a menu: the console resolves bare names against the
    /// schema the user picks.
    fn schema_menu(&mut self, source: SourceId, position: Point<Pixels>, cx: &mut Context<Self>) {
        let schemas = self.schemas_of(&source);
        if schemas.is_empty() {
            return;
        }
        let panel = cx.entity();
        let entries: Vec<MenuEntry> = schemas
            .into_iter()
            .map(|schema| {
                let panel = panel.clone();
                let source = source.clone();
                let chosen = schema.clone();
                MenuEntry::new(schema, move |_, cx| {
                    panel.update(cx, |panel, cx| {
                        panel.default_schemas.insert(source.clone(), chosen.clone());
                        panel.sync_schema(&source, cx);
                        cx.notify();
                    });
                })
            })
            .collect();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    /// The statements the user ran as a menu: Enter on an entry runs it again.
    fn history_menu(&mut self, source: SourceId, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.console_history.is_empty() {
            return;
        }
        let rows = cx
            .try_global::<Settings>()
            .map_or(workspace::DEFAULT_DATABASE_PAGE_ROWS, |settings| {
                settings.database_page_rows
            }) as usize;
        let panel = cx.entity();
        let entries: Vec<MenuEntry> = self
            .console_history
            .iter()
            .take(20)
            .cloned()
            .map(|sql| {
                let panel = panel.clone();
                let source = source.clone();
                let run = sql.clone();
                MenuEntry::new(sql, move |_, cx| {
                    panel.update(cx, |panel, cx| {
                        panel.run_sql(source.clone(), run.clone(), Some(rows), cx)
                    });
                })
                .shortcut("Enter")
            })
            .collect();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    fn result_table(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(tab) = self.active() else {
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
        let result = tab.result.clone();
        let error = tab.error.clone();
        let summary = tab.summary();
        let plan = tab.plan.clone();
        let tab_id = tab.id;
        let cancelled = tab.cancelled;
        let counting = tab.counting;
        let count = tab.countable() && !self.query_ids.contains_key(&tab.source);
        let exportable = tab.can_export() && self.export.is_none();
        let load_more = tab.truncated
            && !cancelled
            && error.is_none()
            && self.scripted.is_none()
            && result
                .as_ref()
                .is_some_and(|result| !result.columns.is_empty());
        let footer = div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_muted)
            .child(
                div()
                    .debug_selector(|| "database-result-summary".to_owned())
                    .child(summary),
            )
            .children(count.then(|| {
                small_button("database-result-count", "Count rows", theme)
                    .debug_selector(|| "database-result-count".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| this.count_rows(tab_id, cx)))
            }))
            .children(counting.then(|| {
                div()
                    .debug_selector(|| "database-result-counting".to_owned())
                    .child("Counting…")
            }))
            .children(load_more.then(|| {
                small_button("database-result-load-more", "Load More", theme)
                    .debug_selector(|| "database-result-load-more".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| this.load_more(tab_id, cx)))
            }))
            .children(exportable.then(|| {
                small_button("database-result-export", "Export", theme)
                    .debug_selector(|| "database-result-export".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| this.open_export(tab_id, cx)))
            }));
        if let Some(error) = error {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .debug_selector(|| "database-result-error".to_owned())
                        .p(px(SPACE_L))
                        .text_color(theme.stripe_error)
                        .child(error),
                )
                .child(footer)
                .into_any_element();
        }
        let Some(result) = result else {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .p(px(SPACE_L))
                        .text_color(theme.text_muted)
                        .child("Running…"),
                )
                .child(footer)
                .into_any_element();
        };
        if let Some(plan) = plan {
            let mut body = div().id("database-result-plan").flex().flex_col().w_full();
            if plan.is_tree() {
                let mut rows = Vec::new();
                for node in &plan.nodes {
                    plan_rows(node, 0, &mut rows);
                }
                for (depth, label) in rows {
                    body = body.child(
                        div()
                            .debug_selector(|| "database-result-plan-row".to_owned())
                            .pl(px(SPACE_S + SPACE_S * depth as f32))
                            .py(px(SPACE_XS))
                            .child(label),
                    );
                }
            } else {
                body = body.child(
                    div()
                        .debug_selector(|| "database-result-plan-text".to_owned())
                        .p(px(SPACE_S))
                        .child(plan.text),
                );
            }
            let grid = scrollable(
                "database-result-plan-scroll",
                &self.result_scroll,
                body.overflow_x_scroll().overflow_y_scroll().min_h_0(),
                theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0();
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(grid)
                .child(footer)
                .into_any_element();
        }
        if result.columns.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .debug_selector(|| "database-result-affected".to_owned())
                        .p(px(SPACE_L))
                        .child(format!(
                            "{} row(s) changed.",
                            result.update_count.unwrap_or_default()
                        )),
                )
                .child(footer)
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
        let grid = scrollable(
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
                .child(body),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(grid)
            .child(footer)
            .into_any_element()
    }

    /// The error of the selected source and, when it has no driver, the way to download
    /// one: the message names the product before a helper would start without its driver.
    fn notice(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.selected.clone()?;
        let source = self
            .sources
            .iter()
            .find(|source| source.id == selected)
            .cloned()?;
        let error = self.errors.get(&selected).cloned();
        let missing = self.driver_missing(&source) || self.driver_failed.contains(&selected);
        if error.is_none() && !missing {
            return None;
        }
        let mut block = div().flex().flex_col().gap(px(SPACE_XS)).p(px(SPACE_S));
        if let Some(error) = error {
            block = block.child(
                div()
                    .debug_selector(|| "database-error".to_owned())
                    .text_color(theme.stripe_error)
                    .child(error),
            );
        }
        if missing {
            let running = self
                .driver_download
                .as_ref()
                .is_some_and(|run| run.source.as_ref() == Some(&selected));
            let mut line = div().flex().items_center().gap(px(SPACE_S));
            if running {
                if let Some(run) = &self.driver_download {
                    line = line.child(
                        div()
                            .debug_selector(|| "database-download-progress".to_owned())
                            .text_color(theme.text_muted)
                            .child(download_text(run)),
                    );
                }
                line = line.child(
                    small_button("database-download-cancel", "Cancel", theme)
                        .debug_selector(|| "database-download-cancel".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_download(cx))),
                );
            } else if let Some(package) = self.package_of(source.kind) {
                let id = selected.clone();
                line = line.child(
                    small_button("database-download-driver", "Download driver", theme)
                        .debug_selector(|| "database-download-driver".to_owned())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_download(Some(id.clone()), false, None, cx)
                        })),
                );
                line = line.child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(format!(
                            "{} · {} · {}",
                            package.coordinate, package.license, package.repository
                        )),
                );
            } else {
                line = line.child(
                    div()
                        .text_color(theme.text_muted)
                        .child("This product has no download."),
                );
            }
            block = block.child(
                div()
                    .debug_selector(|| "database-driver-missing".to_owned())
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_color(theme.stripe_error)
                            .child(driver_message(&source)),
                    )
                    .child(line),
            );
        }
        Some(block.into_any_element())
    }

    /// The question of a console close or a disconnect with a transaction still open.
    fn transaction_band(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(question) = &self.question else {
            return div().into_any_element();
        };
        let pending = self.pending_count(&question.source);
        let name = self
            .sources
            .iter()
            .find(|source| source.id == question.source)
            .map_or_else(|| question.source.to_string(), |source| source.name.clone());
        let text = format!(
            "{name} has {}. Commit them before it closes?",
            pending_text(pending)
        );
        let commit = small_button("database-transaction-commit", "Commit", theme)
            .debug_selector(|| "database-transaction-commit".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                let Some(question) = this.question.take() else {
                    return;
                };
                this.decide_transaction(question.source, false, Some(question.action), cx);
            }));
        let rollback = small_button("database-transaction-rollback", "Rollback", theme)
            .debug_selector(|| "database-transaction-rollback".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                let Some(question) = this.question.take() else {
                    return;
                };
                this.decide_transaction(question.source, true, Some(question.action), cx);
            }));
        let cancel = small_button("database-transaction-cancel", "Cancel", theme)
            .debug_selector(|| "database-transaction-cancel".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.question = None;
                cx.notify();
            }));
        div()
            .id("database-transaction-question")
            .debug_selector(|| "database-transaction-question".to_owned())
            .flex()
            .items_center()
            .justify_between()
            .gap(px(SPACE_M))
            .px(px(DIALOG_PADDING))
            .py(px(SPACE_S))
            .flex_none()
            .bg(theme.hover)
            .child(div().child(text))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(commit)
                    .child(rollback)
                    .child(cancel),
            )
            .into_any_element()
    }

    /// The fields of the values a statement asks for.
    fn params_band(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(ask) = &self.params_ask else {
            return div().into_any_element();
        };
        let focused = ask.focused;
        let mut fields = Vec::new();
        for (index, field) in ask.fields.iter().enumerate() {
            let mut shown = TextField::from_text(field.value.text.clone());
            shown.set_caret(shown.text.len());
            let active = index == focused;
            fields.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .w(px(120.))
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(field.label.clone()),
                    )
                    .child(
                        div()
                            .id(("database-parameter", index))
                            .debug_selector({
                                let name = format!("database-parameter-{index}");
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
                                    if let Some(ask) = &mut this.params_ask {
                                        ask.focused = index;
                                    }
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        }
        div()
            .id("database-parameters")
            .debug_selector(|| "database-parameters".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(DIALOG_PADDING))
            .flex_none()
            .bg(theme.hover)
            .child("Values of the statement")
            .children(fields)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_S))
                    .child(
                        small_button("database-parameters-run", "Run", theme)
                            .debug_selector(|| "database-parameters-run".to_owned())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.answer_parameters(true, cx)),
                            ),
                    )
                    .child(
                        small_button("database-parameters-cancel", "Cancel", theme)
                            .debug_selector(|| "database-parameters-cancel".to_owned())
                            .on_click(
                                cx.listener(|this, _, _, cx| this.answer_parameters(false, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The form behind Export: the format, the table of the INSERT statements and where
    /// the text goes.
    fn export_band(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(dialog) = &self.export_dialog else {
            return div().into_any_element();
        };
        let title = self
            .export_title(dialog.tab_id)
            .unwrap_or_else(|| "the result".to_owned());
        let format = dialog.format;
        let known_table = dialog.known_table;
        let table = dialog.table.text.clone();
        let mut formats = div().flex().items_center().gap(px(SPACE_XS));
        for choice in ExportFormat::all() {
            formats = formats.child(
                small_button(
                    ("database-export-format", choice as usize),
                    choice.title(),
                    theme,
                )
                .debug_selector({
                    let name = format!("database-export-format-{}", choice.extension());
                    move || name.clone()
                })
                .when(choice == format, |this| this.bg(theme.tab_selected))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(dialog) = &mut this.export_dialog {
                        dialog.format = choice;
                    }
                    cx.notify();
                })),
            );
        }
        let mut body = div()
            .id("database-export")
            .debug_selector(|| "database-export".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(DIALOG_PADDING))
            .flex_none()
            .bg(theme.hover)
            .child(format!("Export {title}"));
        if format == ExportFormat::SqlInsert && known_table {
            body = body.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(format!("INSERT INTO {table}")),
            );
        }
        body = body.child(formats);
        if format == ExportFormat::SqlInsert && !known_table {
            let mut shown = TextField::from_text(table);
            shown.set_caret(shown.text.len());
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .w(px(120.))
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child("Table"),
                    )
                    .child(
                        div()
                            .id("database-export-table")
                            .debug_selector(|| "database-export-table".to_owned())
                            .flex()
                            .items_center()
                            .flex_1()
                            .h(px(FIELD_HEIGHT))
                            .px(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.control_border)
                            .overflow_hidden()
                            .cursor_text()
                            .children(text_field_children(
                                &shown,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                TEXT_SMALL,
                                false,
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                }),
                            ),
                    ),
            );
        }
        body = body.child(
            div()
                .flex()
                .justify_end()
                .gap(px(SPACE_S))
                .child(
                    small_button("database-export-save", "Save…", theme)
                        .debug_selector(|| "database-export-save".to_owned())
                        .on_click(cx.listener(|this, _, window, cx| this.save_export(window, cx))),
                )
                .child(
                    small_button("database-export-copy", "Copy", theme)
                        .debug_selector(|| "database-export-copy".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_export(ExportTarget::Clipboard, cx)
                        })),
                )
                .child(
                    small_button("database-export-cancel", "Cancel", theme)
                        .debug_selector(|| "database-export-cancel".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.export_dialog = None;
                            cx.notify();
                        })),
                ),
        );
        body.into_any_element()
    }

    /// Asks the user for the file of the export, then starts it.
    fn save_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let Some(dialog) = &self.export_dialog else {
            return;
        };
        let directory = workspace.read(cx).root().to_path_buf();
        let name = format!("result.{}", dialog.format.extension());
        let chosen = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.start_export(ExportTarget::File(path), cx)
            })
            .ok();
        })
        .detach();
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
        let package = self.package_of(kind);
        let mut fields = vec![
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
        if package.is_some() {
            fields.push((
                Field::Version,
                "Driver version",
                dialog.version.text.clone(),
                false,
            ));
        }
        let focused = dialog.field;
        let version_text = dialog.version.text.trim().to_owned();
        let versions = dialog.versions.clone();
        let versions_loading = dialog.versions_loading;
        let versions_error = dialog.versions_error.clone();
        let driver_error = dialog.driver_error.clone();
        let storage_ready = self.secrets.ready();
        let storage_reason = self.secrets.reason().map(str::to_owned);
        let save_password = dialog.save_password && storage_ready;
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
                        dialog.versions.clear();
                        dialog.versions_for = None;
                        dialog.versions_error = None;
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
                    this.load_versions(cx);
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
        let mut save = div()
            .id("database-save-password")
            .debug_selector(|| "database-save-password".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .pl(px(96. + SPACE_M))
            .text_size(px(TEXT_SMALL))
            .child(checkbox(save_password, theme))
            .child("Save password");
        if storage_ready {
            save = save
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(dialog) = &mut this.dialog {
                        dialog.save_password = !dialog.save_password;
                    }
                    cx.notify();
                }));
        } else {
            save = save.text_color(theme.text_muted).cursor_default();
            if let Some(reason) = storage_reason {
                let hint = SharedString::from(reason);
                save = save.tooltip(move |_, cx| tooltip(hint.clone(), None, cx));
            }
        }
        body = body.child(save);
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
                            this.load_versions(cx);
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
                if package.is_some() {
                    "No driver in the dependencies; download one or give the jar path.".to_owned()
                } else {
                    "No driver in the dependencies; add one or give the jar path.".to_owned()
                }
            });
        body = body.child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(driver),
        );
        if let Some(package) = package {
            if !versions.is_empty() {
                let mut choices = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .pl(px(96. + SPACE_M));
                for (index, version) in versions.iter().take(6).enumerate() {
                    let chosen = version_text == *version;
                    let picked = version.clone();
                    choices = choices.child(
                        small_button(("database-version", index), version, theme)
                            .when(chosen, |this| this.bg(theme.tab_selected))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(dialog) = &mut this.dialog {
                                    dialog.version = TextField::from_text(picked.clone());
                                }
                                cx.notify();
                            })),
                    );
                }
                body = body.child(choices);
            }
            if versions_loading {
                body = body.child(
                    div()
                        .pl(px(96. + SPACE_M))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child("Reading the versions of the repository…"),
                );
            } else if let Some(reason) = versions_error {
                body = body.child(
                    div()
                        .pl(px(96. + SPACE_M))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(reason),
                );
            }
            let running = self.driver_download.as_ref().is_some_and(|run| run.form);
            let mut download = div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .pl(px(96. + SPACE_M));
            if running {
                if let Some(run) = &self.driver_download {
                    download = download.child(
                        div()
                            .debug_selector(|| "database-download-progress".to_owned())
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(download_text(run)),
                    );
                }
                download = download.child(
                    small_button("database-download-cancel", "Cancel", theme)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_download(cx))),
                );
            } else {
                let asked = (!version_text.is_empty()).then(|| version_text.clone());
                download = download.child(
                    small_button("database-dialog-download", "Download driver", theme)
                        .debug_selector(|| "database-dialog-download".to_owned())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_download(None, true, asked.clone(), cx)
                        })),
                );
            }
            download = download.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{} · {} · {}",
                        package.coordinate, package.license, package.repository
                    )),
            );
            body = body.child(download);
            if let Some(error) = driver_error {
                body = body.child(
                    div()
                        .pl(px(96. + SPACE_M))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.stripe_error)
                        .child(error),
                );
            }
        }
        body = body.child(
            div().flex().justify_end().gap(px(SPACE_S)).child(
                small_button("database-dialog-add", "Add", theme)
                    .debug_selector(|| "database-dialog-add".to_owned())
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
            icon_button("database-add", "general/add", "Data Source", None, theme)
                .debug_selector(|| "database-add".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    let vendors = this
                        .support
                        .as_ref()
                        .map(|support| support.vendors())
                        .unwrap_or_default();
                    this.refresh_catalog();
                    this.dialog = Some(Dialog::new(DriverKind::H2, &vendors, this.secrets.ready()));
                    this.load_versions(cx);
                    cx.notify();
                })),
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
                    .debug_selector(|| "database-console".to_owned())
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
                            this.disconnect_source(source, cx);
                        }
                    }),
                ),
            );
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
                icon_button("database-connect", "run/run", "Connect", None, theme)
                    .debug_selector(|| "database-connect".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.password_field = false;
                        this.connect_selected(cx);
                        let _ = &id;
                    })),
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
        // A download of a closed window stops before it writes the rest of the file.
        if let Some(run) = &self.driver_download {
            run.cancel.cancel();
        }
    }
}

impl Render for DatabasePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        self.window = window.window_handle().downcast::<Workspace>();
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
        let question = self.transaction_band(&theme, cx);
        let parameters = self.params_band(&theme, cx);
        let export = self.export_band(&theme, cx);
        let console = self.console_toolbar(&theme, window, cx);
        let tabs = self.result_tabs(&theme, cx);
        let result = self.result_table(&theme, window, cx);
        let notice = self.notice(&theme, cx);
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
            .children(notice)
            .child(question)
            .child(parameters)
            .child(export)
            .child(console)
            .child(tabs)
            .child(result);
        island(&theme)
            .id("database-panel")
            .debug_selector(|| "database-panel".to_owned())
            .key_context("Database ToolWindow")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &Stop, _, cx| this.stop(cx)))
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

/// Alt+Shift+E: explains the statement at the caret of the open console in a tab of its
/// own. The key is clear of the IDEA keymap, where Ctrl+Shift+E is Recent Locations.
pub(crate) fn explain_plan(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = match workspace.active_path() {
        Some(path) => path,
        None => return,
    };
    let Some(root) = cx
        .try_global::<DatabaseConsoleRoot>()
        .map(|root| root.0.clone())
    else {
        return;
    };
    let Some(source) = database::source_of_console(&root, &path) else {
        return;
    };
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        return;
    };
    let selection = view.read(cx).selected_text();
    let text = selection.or_else(|| {
        let caret = view.read(cx).caret().0;
        view.read(cx)
            .text()
            .and_then(|text| database::statement_at(&text, caret))
    });
    let Some(text) = text else {
        return;
    };
    let Some(panel) = cx
        .try_global::<ActiveDatabase>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    workspace.show_tool(DATABASE_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.explain(source, text, cx));
}

/// The plain syntax of a console file: no grammar, but Ctrl+Enter belongs to the window.
pub(crate) fn install(state_dir: PathBuf, cx: &mut App) {
    cx.set_global(DatabaseConsoleRoot(state_dir.clone()));
    let console_dir = state_dir.clone();
    cx.set_global(RunFiles(Arc::new(move |path: &Path| {
        database::source_of_console(&console_dir, path).is_some()
            || crate::database_console::is_sql_file(path)
    })));
    crate::database_console::install(state_dir.clone(), cx);
    let mut extra = cx
        .try_global::<ExtraTemplates>()
        .map(|extra| extra.0.clone())
        .unwrap_or_default();
    let sql_templates: fn(&Path) -> Option<&'static editor_view::TemplateLanguage> =
        crate::database_console::templates_for;
    if !extra.contains(&sql_templates) {
        extra.push(sql_templates);
        cx.set_global(ExtraTemplates(extra));
    }
    let bar = cx.try_global::<FileBar>().map(|bar| bar.0.clone());
    cx.set_global(FileBar(Rc::new(
        move |path: &Path, window: &mut Window, cx: &mut Context<EditorView>| {
            sql_file_bar(path, window, cx)
                .or_else(|| bar.as_ref().and_then(|bar| bar(path, window, cx)))
        },
    )));
}

/// The row above a `.sql` script of the project: the source its statements run against.
fn sql_file_bar(
    path: &Path,
    _window: &mut Window,
    cx: &mut Context<EditorView>,
) -> Option<AnyElement> {
    if !crate::database_console::is_sql_file(path) {
        return None;
    }
    let root = cx.try_global::<DatabaseConsoleRoot>()?.0.clone();
    if database::source_of_console(&root, path).is_some() {
        return None;
    }
    let panel = cx.try_global::<ActiveDatabase>()?.0.clone();
    let theme = *cx.global::<Theme>();
    let label = panel
        .read_with(cx, |panel, _| {
            panel
                .file_sources
                .get(path)
                .and_then(|id| panel.sources.iter().find(|source| &source.id == id))
                .map(|source| source.name.clone())
        })
        .unwrap_or_else(|| "No data source".to_owned());
    let target = path.to_path_buf();
    let click = panel.clone();
    let button = toolbar_dropdown("database-sql-source", None, label, &theme)
        .debug_selector(|| "database-sql-source".to_owned())
        .on_click(move |_, window, cx| {
            let target = target.clone();
            click.update(cx, |panel, cx| {
                panel.show_sql_source_menu(target, window, cx)
            });
        });
    Some(
        div()
            .id("database-sql-file-bar")
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border)
            .child(button)
            .into_any_element(),
    )
}

/// Runs the selection or the statement at `line` of a `.sql` console or script. The
/// answer is `false` when the file is not SQL, so another tool may take the key.
pub(crate) fn run_line(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let path = view.read(cx).path().to_path_buf();
    let sql_file = crate::database_console::is_sql_file(&path);
    let Some(panel) = cx
        .try_global::<ActiveDatabase>()
        .map(|panel| panel.0.clone())
    else {
        return false;
    };
    let source = panel.read(cx).source_of_script(&path);
    let Some(source) = source else {
        if sql_file {
            workspace.notify_user(
                "Run SQL Script",
                "Choose a data source in the bar above the file.",
                cx,
            );
            return true;
        }
        return false;
    };
    let selection = view.read(cx).selected_text();
    let text = selection.or_else(|| {
        view.read(cx)
            .text()
            .and_then(|text| database::statement_at(&text, line))
    });
    workspace.show_tool(DATABASE_TOOL, window, cx);
    // The console keeps the focus: the next statement is typed where the last one was
    // written, as in a console that runs what its editor holds.
    let focus = view.read(cx).focus_handle(cx);
    window.focus(&focus, cx);
    if let Some(text) = text {
        let rows = cx
            .try_global::<Settings>()
            .map_or(workspace::DEFAULT_DATABASE_PAGE_ROWS, |settings| {
                settings.database_page_rows
            }) as usize;
        panel.update(cx, |panel, cx| panel.run_sql(source, text, Some(rows), cx));
    }
    true
}

/// Run SQL Script: every statement of the open `.sql` file, with the choice of the
/// settings about the first failure.
pub(crate) fn run_script(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        return;
    };
    let path = view.read(cx).path().to_path_buf();
    if !crate::database_console::is_sql_file(&path) {
        return;
    }
    let Some(panel) = cx
        .try_global::<ActiveDatabase>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    let source = panel.read(cx).source_of_script(&path);
    let Some(source) = source else {
        workspace.notify_user(
            "Run SQL Script",
            "Choose a data source in the bar above the file.",
            cx,
        );
        return;
    };
    let Some(text) = view.read(cx).text() else {
        return;
    };
    workspace.show_tool(DATABASE_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.run_script(source, text, cx));
}

/// Stops the statement of the console or the script of `view`, as Ctrl+F2 does. The
/// answer is `false` when the file is not SQL, so another tool may take the key.
pub(crate) fn stop_line(
    view: &Entity<EditorView>,
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> bool {
    let path = view.read(cx).path().to_path_buf();
    let Some(panel) = cx
        .try_global::<ActiveDatabase>()
        .map(|panel| panel.0.clone())
    else {
        return false;
    };
    let source = panel.read(cx).source_of_script(&path);
    let Some(source) = source else {
        return false;
    };
    let _ = workspace;
    panel.update(cx, |panel, cx| panel.stop_source(source, cx));
    true
}

/// The line of a download in flight: the version when it is known, the percent when the
/// server announced the size, the bytes otherwise.
fn download_text(run: &DriverDownload) -> String {
    let version = run
        .version
        .as_deref()
        .map(|version| format!(" {version}"))
        .unwrap_or_default();
    match run.progress.total() {
        Some(total) if total > 0 => format!(
            "Downloading the {} driver{version}: {}%",
            run.kind.title(),
            run.progress.written() * 100 / total
        ),
        _ => format!(
            "Downloading the {} driver{version}: {} KB",
            run.kind.title(),
            run.progress.written() / 1024
        ),
    }
}

/// The message of a source that has no driver to start the helper with.
fn driver_message(source: &Source) -> String {
    format!("No {} driver.", source.kind.title())
}

/// Whether the answer of a helper says that it found no driver to load.
fn driver_refused(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "no suitable driver",
        "classnotfoundexception",
        "no driver",
        "driver class",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

pub(crate) fn small_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
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

/// A `COUNT(*)` of a limited statement, so its tab can say how many rows it has in all
/// when the user asks. Only a statement that yields rows is counted, and only a plain
/// derived table is asked, which every dialect here understands. A `WITH` that writes is
/// left alone: the count would run its `INSERT`, `UPDATE` or `DELETE` a second time.
fn count_statement(statement: &str) -> Option<String> {
    let trimmed = statement.trim().trim_end_matches(';').trim();
    if !database::countable_statement(trimmed) {
        return None;
    }
    Some(format!("SELECT COUNT(*) FROM (\n{trimmed}\n) arro_count"))
}

/// The number a count row carries.
fn count_of(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(count) => u64::try_from(*count).ok(),
        Value::Decimal(text) | Value::Text(text) => text.trim().parse().ok(),
        _ => None,
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

/// The tab of a table: a name inside the state folder of the project that no file has.
fn data_path(root: &Path, source: &SourceId, table: &Table) -> PathBuf {
    root.join(".database").join(format!(
        "{}-{}-{}.data",
        slug(source.as_str()),
        slug(&table.schema),
        slug(&table.name)
    ))
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

/// The product a tool names, in the kinds the Database window offers.
fn driver_kind(name: &str) -> Option<DriverKind> {
    match name.trim().to_ascii_lowercase().as_str() {
        "h2" => Some(DriverKind::H2),
        "postgresql" | "postgres" => Some(DriverKind::PostgreSql),
        "mysql" => Some(DriverKind::MySql),
        "mariadb" => Some(DriverKind::MariaDb),
        "sqlite" => Some(DriverKind::Sqlite),
        "oracle" => Some(DriverKind::Oracle),
        "sqlserver" | "sql server" | "mssql" => Some(DriverKind::SqlServer),
        _ => None,
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
    use super::{
        Column, ConsoleResult, DriverKind, ResultSet, Source, SourceId, Value, compare_values,
        count_statement, driver_message, driver_refused, group_digits, slug, source_name,
    };
    use std::time::Duration;

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
    fn a_driver_that_the_helper_could_not_load_offers_its_download() {
        let source = Source {
            id: SourceId::new("owners"),
            name: "Owners".into(),
            kind: DriverKind::PostgreSql,
            url: "jdbc:postgresql://localhost:5432/owners".into(),
            user: "pet".into(),
            module: None,
            driver_jar: None,
        };

        assert_eq!(driver_message(&source), "No PostgreSQL driver.");
        assert!(driver_refused(
            "org.postgresql.util.PSQLException: No suitable driver found"
        ));
        assert!(driver_refused(
            "ClassNotFoundException: org.postgresql.Driver"
        ));
        assert!(!driver_refused(
            "FATAL: password authentication failed for user \"pet\""
        ));
        assert!(!driver_refused("Connection refused (Connection refused)"));
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

    fn finished(update_count: Option<u64>, rows: usize, total: Option<u64>) -> ConsoleResult {
        let mut tab =
            ConsoleResult::new(1, "SELECT ID FROM OWNERS".into(), SourceId::new("s"), None);
        tab.elapsed = Some(Duration::from_millis(12));
        tab.total = total;
        tab.result = Some(ResultSet {
            columns: vec![Column::default()],
            rows: vec![vec![Value::Null]; rows],
            update_count,
            truncated: false,
        });
        tab
    }

    #[test]
    fn a_tab_says_how_many_rows_it_changed_or_read() {
        let changed = finished(Some(3), 0, None);
        assert_eq!(changed.rows_text(), "3 rows affected");
        assert_eq!(changed.summary(), "12 ms · 3 rows affected");

        let read = finished(None, 1, None);
        assert_eq!(read.rows_text(), "1 row");

        let portion = finished(None, 500, Some(12_840));
        assert_eq!(portion.rows_text(), "500 rows of 12\u{a0}840");

        // Until the user asks for the count, the tab only says that there is more.
        let mut limited = ConsoleResult::new(
            1,
            "SELECT ID FROM OWNERS".into(),
            SourceId::new("s"),
            Some(500),
        );
        limited.result = Some(ResultSet {
            columns: vec![Column::default()],
            rows: vec![vec![Value::Null]; 500],
            update_count: None,
            truncated: false,
        });
        limited.settle(false, false);
        limited.elapsed = Some(Duration::from_millis(12));
        assert_eq!(limited.rows_text(), "500+ rows");
        assert!(limited.countable());
    }

    #[test]
    fn a_limited_select_is_countable_and_an_update_is_not() {
        assert_eq!(
            count_statement("SELECT ID FROM OWNERS;\n").as_deref(),
            Some("SELECT COUNT(*) FROM (\nSELECT ID FROM OWNERS\n) arro_count")
        );
        assert!(count_statement("WITH o AS (SELECT 1) SELECT * FROM o").is_some());
        // A CTE that writes would run its write again inside the count.
        assert_eq!(
            count_statement("WITH moved AS (DELETE FROM OWNERS RETURNING ID) SELECT * FROM moved"),
            None
        );
        assert_eq!(count_statement("UPDATE OWNERS SET NAME = 'x'"), None);
        assert_eq!(count_statement("   "), None);
    }

    #[test]
    fn a_row_count_groups_its_digits() {
        assert_eq!(group_digits(1), "1");
        assert_eq!(group_digits(12_840), "12\u{a0}840");
        assert_eq!(group_digits(1_234_567), "1\u{a0}234\u{a0}567");
    }
}
