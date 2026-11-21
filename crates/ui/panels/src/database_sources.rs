//! The Data Sources window of the Database tool, as IDEA's Data Sources and Drivers dialog:
//! the connections of the project on the left, the form of the chosen one on the right, and
//! Test Connection through the same helper that connects. The window keeps its own copy of
//! the form until OK or Apply hands the changes to the panel, which owns the list, the
//! console files and the passwords. The window never changes the panel itself: it asks the
//! panel and reads it again when the panel says it changed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use database::{
    Driver, DriverKind, DriverPackage, Event, Secret, Source, SourceId, Suggestion, Support,
    VendorTemplate,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::database_panel::{DatabasePanel, small_button};

const WIDTH: f32 = 900.;
/// The share of the window height the window takes, as the Run/Debug dialog does.
const HEIGHT_SHARE: f32 = 0.8;
const MIN_SIZE: (f32, f32) = (640., 420.);
/// The width of the list of connections.
const LIST_WIDTH: f32 = 240.;
/// Every label the form can show. The label column is measured with all of them, so it keeps
/// its width when a row comes and goes.
const FORM_LABELS: &[&str] = &[
    "Name",
    "Database",
    "Host",
    "Port",
    "File",
    "Address",
    "User",
    "Password",
    "Driver jar",
    "Driver version",
];
/// The file a new H2 or SQLite connection points at, until the user picks another one.
const DEFAULT_FILE: &str = "~/test";
/// The products the window offers, in the order of its lists.
const KINDS: [DriverKind; 7] = [
    DriverKind::H2,
    DriverKind::PostgreSql,
    DriverKind::MySql,
    DriverKind::MariaDb,
    DriverKind::Sqlite,
    DriverKind::Oracle,
    DriverKind::SqlServer,
];
/// The fields in the order Tab goes through them.
const ORDER: [Field; 10] = [
    Field::Name,
    Field::Host,
    Field::Port,
    Field::Database,
    Field::File,
    Field::Url,
    Field::User,
    Field::Password,
    Field::Driver,
    Field::Version,
];
const NO_SOURCE: &str = "The address of a source is required.";

/// The text fields of the form. The numbers are the ones the window tests name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Name = 0,
    Url = 1,
    User = 2,
    Password = 3,
    Driver = 4,
    Version = 5,
    Host = 6,
    Port = 7,
    Database = 8,
    File = 9,
}

impl Field {
    /// The part of an address that the field holds, when it holds one.
    fn part(self) -> Option<&'static str> {
        match self {
            Self::Host => Some("host"),
            Self::Port => Some("port"),
            Self::Database => Some("database"),
            Self::File => Some("file"),
            _ => None,
        }
    }
}

/// The icon of a product in the lists of the window.
fn kind_icon(kind: DriverKind) -> &'static str {
    match kind {
        DriverKind::H2 => "databases/h2",
        DriverKind::PostgreSql => "databases/postgresql",
        DriverKind::MySql => "databases/mysql",
        DriverKind::MariaDb => "databases/mariadb",
        DriverKind::Sqlite => "databases/sqlite",
        DriverKind::Oracle => "databases/oracle",
        DriverKind::SqlServer => "databases/sqlserver",
    }
}

/// The address template of a product, as the language layer gives it.
fn template_of(vendor: Option<&VendorTemplate>) -> Option<String> {
    vendor.map(|vendor| vendor.url_template.clone())
}

/// The placeholders of an address template, such as `host` and `port` in
/// `jdbc:postgresql://{host}:{port}/{database}`.
fn placeholders(template: &str) -> Vec<&str> {
    split_template(template).1
}

/// The text between the placeholders of a template, and the placeholders in their order.
/// There is one more text than there are placeholders.
fn split_template(template: &str) -> (Vec<&str>, Vec<&str>) {
    let mut texts = Vec::new();
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(length) = rest[start..].find('}') else {
            break;
        };
        texts.push(&rest[..start]);
        names.push(&rest[start + 1..start + length]);
        rest = &rest[start + length + 1..];
    }
    texts.push(rest);
    (texts, names)
}

/// The address of a template with each placeholder replaced by what `value` gives for it.
fn fill_template(template: &str, value: impl Fn(&str) -> String) -> String {
    let (texts, names) = split_template(template);
    let mut address = String::new();
    for (index, text) in texts.iter().enumerate() {
        address.push_str(text);
        if let Some(name) = names.get(index) {
            address.push_str(&value(name));
        }
    }
    address
}

/// The values of the placeholders of a template, when `url` has the shape of the template.
/// The values are the texts between the fixed parts, so a template reads back the address
/// it wrote; an address of another shape gives `None`.
fn match_template(template: &str, url: &str) -> Option<Vec<(String, String)>> {
    let (texts, names) = split_template(template);
    let mut rest = url.strip_prefix(texts[0])?;
    let mut values = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let next = texts[index + 1];
        // A last placeholder with no text after it takes the rest of the address.
        let at = if index + 1 == names.len() && next.is_empty() {
            rest.len()
        } else {
            rest.find(next)?
        };
        values.push((name.to_string(), rest[..at].to_owned()));
        rest = &rest[at + next.len()..];
    }
    rest.is_empty().then_some(values)
}

/// What a finished probe says: the product of the server when the helper connected, or the
/// reason it did not. An event that is not an answer to the probe is skipped.
pub(crate) fn probe_outcome(events: impl IntoIterator<Item = Event>) -> Result<String, String> {
    for event in events {
        match event {
            Event::Connected { product, .. } => {
                return Ok(if product.is_empty() {
                    "Connected".to_owned()
                } else {
                    product
                });
            }
            Event::Error { message, .. } => return Err(message),
            Event::Exited { code } => {
                return Err(match code {
                    Some(code) => format!("The helper stopped with code {code}."),
                    None => "The helper stopped.".to_owned(),
                });
            }
            _ => {}
        }
    }
    Err("The helper did not answer.".to_owned())
}

/// One connection of the list as the window edits it: its own fields, the password that
/// the user typed, and what it was when the window read it.
#[derive(Clone)]
struct Draft {
    /// The connection in the list of the panel; `None` for a connection the window adds.
    id: Option<SourceId>,
    /// The user the connection had when the window read it: a change of it moves the
    /// password to the new account.
    account: String,
    name: TextField,
    kind: DriverKind,
    /// The address template of the kind, when the language layer has one.
    template: Option<String>,
    url: TextField,
    host: TextField,
    port: TextField,
    database: TextField,
    file: TextField,
    user: TextField,
    password: TextField,
    /// The password field changed: the window writes the password then, and not before.
    password_edited: bool,
    save_password: bool,
    /// The user turned the Save password flag, so the store follows it on the next apply.
    save_changed: bool,
    driver: TextField,
    version: TextField,
    module: Option<String>,
}

/// One connection the window saves: the connection as the window holds it, the password the
/// user typed when it did, and the account the connection had when the window read it.
pub(crate) struct SourceEdit {
    /// The connection in the list of the panel; `None` for a connection the window adds.
    pub id: Option<SourceId>,
    pub source: Source,
    /// The password the user typed; an empty one clears it.
    pub password: Option<Secret>,
    pub save_password: bool,
    pub save_changed: bool,
    pub account: String,
}

/// A change of the list that the window hands to the panel with OK or Apply.
pub(crate) enum SourceChange {
    Save(Box<SourceEdit>),
    Remove(SourceId),
}

/// What the window starts from: the connections, the language side and the passwords the
/// panel holds, read before the window opens.
pub(crate) struct Snapshot {
    pub sources: Vec<Source>,
    pub support: Option<Arc<dyn Support>>,
    pub drivers: Vec<Driver>,
    pub suggestions: Vec<Suggestion>,
    pub secrets_ready: bool,
    pub secrets_reason: Option<String>,
    /// The connections whose password the panel holds.
    pub stored: HashSet<SourceId>,
}

/// The connection the window opens on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    /// `+`: a new H2 connection.
    New,
    /// F4, Properties and a double click: the connection with this id.
    Edit(SourceId),
}

/// The state of Test Connection of the selected connection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TestState {
    Idle,
    Running,
    /// The product of the server, or the reason it did not connect.
    Done(Result<String, String>),
}

impl Draft {
    /// A connection of a product with the address its template gives, and no name yet.
    fn blank(kind: DriverKind, vendor: Option<&VendorTemplate>, save_password: bool) -> Self {
        let mut draft = Self {
            id: None,
            account: String::new(),
            name: TextField::default(),
            kind,
            template: template_of(vendor),
            url: TextField::default(),
            host: TextField::default(),
            port: TextField::default(),
            database: TextField::default(),
            file: TextField::default(),
            user: TextField::default(),
            password: TextField::default(),
            password_edited: false,
            save_password,
            save_changed: true,
            driver: TextField::default(),
            version: TextField::default(),
            module: None,
        };
        draft.reset_address(vendor);
        draft
    }

    /// The connection of the panel, as the window reads it.
    fn from_source(source: &Source, stored: bool, vendor: Option<&VendorTemplate>) -> Self {
        let mut draft = Self::blank(source.kind, vendor, stored);
        draft.id = Some(source.id.clone());
        draft.account = source.user.clone();
        draft.name = TextField::from_text(source.name.clone());
        draft.url = TextField::from_text(source.url.clone());
        draft.user = TextField::from_text(source.user.clone());
        draft.driver = TextField::from_text(driver_text(source.driver_jar.as_ref()));
        draft.module = source.module.clone();
        draft.save_changed = false;
        draft.read_address();
        draft
    }

    /// A new connection that a project file suggests.
    fn from_suggestion(
        suggestion: &Suggestion,
        vendor: Option<&VendorTemplate>,
        save: bool,
    ) -> Self {
        let kind = suggestion.kind.unwrap_or(DriverKind::H2);
        let mut draft = Self::blank(kind, vendor, save);
        draft.url = TextField::from_text(suggestion.url.clone());
        draft.user = TextField::from_text(suggestion.user.clone());
        draft.driver = TextField::from_text(driver_text(suggestion.driver_jar.as_ref()));
        draft.module = (!suggestion.module.is_empty()).then(|| suggestion.module.clone());
        draft.read_address();
        draft
    }

    /// The same connection as a new one, with a name of its own.
    fn copied(&self) -> Self {
        let mut copy = self.clone();
        copy.id = None;
        copy.account = String::new();
        copy.name = TextField::from_text(format!("{} (copy)", self.name.text.trim()));
        copy.save_changed = true;
        copy
    }

    fn field(&self, field: Field) -> &TextField {
        match field {
            Field::Name => &self.name,
            Field::Url => &self.url,
            Field::User => &self.user,
            Field::Password => &self.password,
            Field::Driver => &self.driver,
            Field::Version => &self.version,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Database => &self.database,
            Field::File => &self.file,
        }
    }

    fn field_mut(&mut self, field: Field) -> &mut TextField {
        match field {
            Field::Name => &mut self.name,
            Field::Url => &mut self.url,
            Field::User => &mut self.user,
            Field::Password => &mut self.password,
            Field::Driver => &mut self.driver,
            Field::Version => &mut self.version,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Database => &mut self.database,
            Field::File => &mut self.file,
        }
    }

    /// Whether the template of the connection has the part.
    fn holds(&self, part: &str) -> bool {
        self.template
            .as_deref()
            .is_some_and(|template| placeholders(template).contains(&part))
    }

    fn part(&self, part: &str) -> String {
        match part {
            "host" => self.host.text.clone(),
            "port" => self.port.text.clone(),
            "database" => self.database.text.clone(),
            "file" => self.file.text.clone(),
            _ => String::new(),
        }
    }

    /// Puts the defaults of the product in the parts of the address and writes the address.
    fn reset_address(&mut self, vendor: Option<&VendorTemplate>) {
        self.host =
            TextField::from_text(vendor.map(|vendor| vendor.host.clone()).unwrap_or_default());
        self.port = TextField::from_text(
            vendor
                .and_then(|vendor| vendor.port)
                .map(|port| port.to_string())
                .unwrap_or_default(),
        );
        self.database = TextField::default();
        self.file = TextField::from_text(DEFAULT_FILE);
        self.rebuild_url();
    }

    /// Writes the address from the parts, when the product has a template.
    fn rebuild_url(&mut self) {
        let Some(template) = self.template.clone() else {
            return;
        };
        let address = fill_template(&template, |part| self.part(part));
        self.url = TextField::from_text(address);
    }

    /// Reads the parts from the address, when it has the shape of the template. An address
    /// of another shape keeps the parts as they were.
    fn read_address(&mut self) {
        let Some(template) = self.template.clone() else {
            return;
        };
        let Some(values) = match_template(&template, &self.url.text) else {
            return;
        };
        for (part, value) in values {
            let text = TextField::from_text(value);
            match part.as_str() {
                "host" => self.host = text,
                "port" => self.port = text,
                "database" => self.database = text,
                "file" => self.file = text,
                _ => {}
            }
        }
    }

    /// The connection the window saves, with the id it has in the panel.
    fn source(&self, id: SourceId) -> Source {
        let driver = self.driver.text.trim();
        Source {
            id,
            name: self.name.text.trim().to_owned(),
            kind: self.kind,
            url: self.url.text.trim().to_owned(),
            user: self.user.text.trim().to_owned(),
            module: self.module.clone(),
            driver_jar: (!driver.is_empty()).then(|| PathBuf::from(driver)),
        }
    }

    fn edit(&self) -> SourceEdit {
        let id = self.id.clone();
        let source = self.source(id.clone().unwrap_or_else(|| SourceId::new("")));
        SourceEdit {
            id,
            source,
            password: self
                .password_edited
                .then(|| Secret::new(self.password.text.clone())),
            save_password: self.save_password,
            save_changed: self.save_changed,
            account: self.account.clone(),
        }
    }

    /// Takes the answer of the panel: the id of a new connection, and the state after a save.
    fn saved(&mut self, id: Option<SourceId>) {
        if let Some(id) = id {
            self.id = Some(id);
            self.account = self.user.text.trim().to_owned();
        }
        self.password_edited = false;
        self.save_changed = false;
    }
}

fn driver_text(jar: Option<&PathBuf>) -> String {
    jar.map(|jar| jar.display().to_string()).unwrap_or_default()
}

/// The Data Sources window: a modal dialog over the workspace.
pub(crate) struct DataSourcesDialog {
    panel: Entity<DatabasePanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    support: Option<Arc<dyn Support>>,
    vendors: Vec<VendorTemplate>,
    packages: Vec<DriverPackage>,
    drivers: Vec<Driver>,
    suggestions: Vec<Suggestion>,
    secrets_ready: bool,
    secrets_reason: Option<String>,
    drafts: Vec<Draft>,
    /// The connections the window removed from the list, applied with OK or Apply.
    removed: Vec<SourceId>,
    selected: Option<usize>,
    /// The field that takes the typed text.
    active: Field,
    /// The product list of the form is open.
    kind_open: bool,
    /// The product list of `+` is open.
    adding_open: bool,
    error: Option<String>,
    /// The stable versions of the driver of the selected product, newest first.
    versions: Vec<String>,
    versions_for: Option<DriverKind>,
    versions_loading: bool,
    versions_error: Option<String>,
    driver_error: Option<String>,
    /// The serial of the last download answer the window has taken.
    form_serial: u64,
    test: TestState,
    /// The serial of the last Test Connection; an older answer is dropped.
    test_serial: u64,
    list_scroll: ScrollHandle,
    form_scroll: ScrollHandle,
    _panel: Subscription,
}

impl DataSourcesDialog {
    /// Opens the window over `workspace`, on the connection that `opening` names.
    pub(crate) fn show(
        panel: Entity<DatabasePanel>,
        snapshot: Snapshot,
        opening: Opening,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self::new(panel, snapshot, opening, weak, cx));
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn new(
        panel: Entity<DatabasePanel>,
        snapshot: Snapshot,
        opening: Opening,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let vendors = snapshot
            .support
            .as_ref()
            .map(|support| support.vendors())
            .unwrap_or_default();
        let packages = snapshot
            .support
            .as_ref()
            .map(|support| support.packages())
            .unwrap_or_default();
        let vendor_of = |kind: DriverKind| vendors.iter().find(|vendor| vendor.kind == kind);
        let mut drafts: Vec<Draft> = snapshot
            .sources
            .iter()
            .map(|source| {
                let stored = snapshot.secrets_ready && snapshot.stored.contains(&source.id);
                Draft::from_source(source, stored, vendor_of(source.kind))
            })
            .collect();
        let selected = match opening {
            Opening::New => {
                drafts.push(Draft::blank(
                    DriverKind::H2,
                    vendor_of(DriverKind::H2),
                    snapshot.secrets_ready,
                ));
                Some(drafts.len() - 1)
            }
            Opening::Edit(id) => drafts
                .iter()
                .position(|draft| draft.id.as_ref() == Some(&id)),
        };
        let form_serial = panel
            .read(cx)
            .form_download()
            .answer
            .map_or(0, |(serial, _, _)| serial);
        let observe = cx.observe(&panel, |this: &mut Self, _, cx| this.panel_changed(cx));
        let mut dialog = Self {
            panel,
            workspace,
            focus: cx.focus_handle(),
            support: snapshot.support,
            vendors,
            packages,
            drivers: snapshot.drivers,
            suggestions: snapshot.suggestions,
            secrets_ready: snapshot.secrets_ready,
            secrets_reason: snapshot.secrets_reason,
            drafts,
            removed: Vec::new(),
            selected,
            active: Field::Name,
            kind_open: false,
            adding_open: false,
            error: None,
            versions: Vec::new(),
            versions_for: None,
            versions_loading: false,
            versions_error: None,
            driver_error: None,
            form_serial,
            test: TestState::Idle,
            test_serial: 0,
            list_scroll: ScrollHandle::new(),
            form_scroll: ScrollHandle::new(),
            _panel: observe,
        };
        dialog.load_versions(cx);
        dialog
    }

    fn selected_draft(&self) -> Option<&Draft> {
        self.selected.and_then(|index| self.drafts.get(index))
    }

    fn selected_draft_mut(&mut self) -> Option<&mut Draft> {
        self.selected.and_then(|index| self.drafts.get_mut(index))
    }

    /// The text field of the selected draft that `field` names, for the mouse.
    fn draft_field_mut(&mut self, field: Field) -> Option<&mut TextField> {
        self.selected_draft_mut()
            .map(|draft| draft.field_mut(field))
    }

    fn package_of(&self, kind: DriverKind) -> Option<&DriverPackage> {
        self.packages.iter().find(|package| package.kind == kind)
    }

    /// The fields the form shows for a connection, in the order Tab goes through them.
    fn visible_fields(&self, draft: &Draft) -> Vec<Field> {
        let downloadable = self.package_of(draft.kind).is_some();
        ORDER
            .into_iter()
            .filter(|field| match field {
                Field::Version => downloadable,
                other => other.part().is_none_or(|part| draft.holds(part)),
            })
            .collect()
    }

    /// The panel changed, as a download or a save of the panel does: the window takes the
    /// answer of a driver download that it started, and redraws.
    fn panel_changed(&mut self, cx: &mut Context<Self>) {
        let download = self.panel.read(cx).form_download();
        if let Some((serial, kind, answer)) = download.answer
            && serial > self.form_serial
        {
            self.form_serial = serial;
            match answer {
                Ok((version, jar)) => {
                    self.driver_error = None;
                    if let Some(draft) = self.selected_draft_mut()
                        && draft.kind == kind
                    {
                        draft.version = TextField::from_text(version);
                        draft.driver = TextField::from_text(jar.display().to_string());
                    }
                }
                Err(error) => self.driver_error = Some(error),
            }
        }
        cx.notify();
    }

    /// Reads the stable versions of the driver of the selected product in the background,
    /// so the field shows the newest one and the user may pick another.
    fn load_versions(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = self.selected_draft().map(|draft| draft.kind) else {
            return;
        };
        if self.package_of(kind).is_none() {
            self.versions.clear();
            self.versions_for = None;
            self.versions_error = None;
            return;
        }
        if self.versions_for == Some(kind) {
            return;
        }
        let Some(support) = self.support.clone() else {
            return;
        };
        self.versions_for = Some(kind);
        self.versions.clear();
        self.versions_error = None;
        self.versions_loading = true;
        let job = cx
            .background_executor()
            .spawn(async move { support.versions(kind) });
        cx.spawn(async move |this, cx| {
            let answer = job.await;
            this.update(cx, |this, cx| {
                if this.versions_for != Some(kind) {
                    return;
                }
                this.versions_loading = false;
                match answer {
                    Ok(versions) => {
                        if let Some(newest) = versions.first()
                            && let Some(draft) = this.selected_draft_mut()
                            && draft.kind == kind
                            && draft.version.text.trim().is_empty()
                        {
                            draft.version = TextField::from_text(newest.clone());
                        }
                        this.versions = versions;
                    }
                    Err(reason) => this.versions_error = Some(reason),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Selects a connection of the list and shows its form.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.drafts.len() {
            return;
        }
        self.selected = Some(index);
        self.active = Field::Name;
        self.kind_open = false;
        self.adding_open = false;
        self.test = TestState::Idle;
        self.load_versions(cx);
        cx.notify();
    }

    fn add_draft(&mut self, draft: Draft, cx: &mut Context<Self>) {
        self.drafts.push(draft);
        self.adding_open = false;
        let index = self.drafts.len() - 1;
        self.select(index, cx);
    }

    /// `+` with a product: a new connection of that product.
    fn add_kind(&mut self, kind: DriverKind, cx: &mut Context<Self>) {
        let draft = Draft::blank(
            kind,
            self.vendors.iter().find(|vendor| vendor.kind == kind),
            self.secrets_ready,
        );
        self.add_draft(draft, cx);
    }

    /// A connection that a project file suggests becomes a new connection of the list.
    fn add_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(suggestion) = self.suggestions.get(index).cloned() else {
            return;
        };
        let vendor = self
            .vendors
            .iter()
            .find(|vendor| Some(vendor.kind) == suggestion.kind.or(Some(DriverKind::H2)));
        let draft = Draft::from_suggestion(&suggestion, vendor, self.secrets_ready);
        self.add_draft(draft, cx);
    }

    /// The product of the form changes: the address takes the template of the product.
    fn set_kind(&mut self, kind: DriverKind, cx: &mut Context<Self>) {
        let vendor = self
            .vendors
            .iter()
            .find(|vendor| vendor.kind == kind)
            .cloned();
        let Some(draft) = self.selected_draft_mut() else {
            return;
        };
        draft.kind = kind;
        draft.template = template_of(vendor.as_ref());
        draft.reset_address(vendor.as_ref());
        self.kind_open = false;
        self.active = Field::Name;
        self.test = TestState::Idle;
        self.load_versions(cx);
        cx.notify();
    }

    fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        if index >= self.drafts.len() {
            return;
        }
        let draft = self.drafts.remove(index);
        if let Some(id) = draft.id {
            self.removed.push(id);
        }
        self.error = None;
        if self.drafts.is_empty() {
            self.selected = None;
            cx.notify();
            return;
        }
        self.select(index.min(self.drafts.len() - 1), cx);
    }

    fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(copy) = self.selected_draft().map(Draft::copied) else {
            return;
        };
        self.add_draft(copy, cx);
    }

    fn focus_field(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        self.active = field;
        self.kind_open = false;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn next_field(&mut self) {
        let Some(draft) = self.selected_draft() else {
            return;
        };
        let fields = self.visible_fields(draft);
        if fields.is_empty() {
            return;
        }
        let at = fields
            .iter()
            .position(|field| *field == self.active)
            .map_or(0, |at| (at + 1) % fields.len());
        self.active = fields[at];
    }

    /// Sends a key to the field that has the focus. A password counts as edited only when its
    /// text changed, so that a move of the caret never writes the store.
    fn type_into(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let field = self.active;
        let visible = match self.selected_draft() {
            Some(draft) => self.visible_fields(draft),
            None => return,
        };
        if !visible.contains(&field) {
            return;
        }
        let Some(draft) = self.selected_draft_mut() else {
            return;
        };
        let before = draft.field(field).text.clone();
        let handled = if field == Field::Password {
            draft.field_mut(field).key_secret(event, cx)
        } else {
            draft.field_mut(field).key_with_clipboard(event, false, cx)
        };
        if !handled {
            return;
        }
        if draft.field(field).text != before {
            match field {
                Field::Password => draft.password_edited = true,
                Field::Url => draft.read_address(),
                other if other.part().is_some() => draft.rebuild_url(),
                _ => {}
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" if self.kind_open || self.adding_open => {
                self.kind_open = false;
                self.adding_open = false;
                cx.stop_propagation();
                cx.notify();
            }
            "escape" => {
                cx.stop_propagation();
                self.close(window, cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.apply(true, window, cx);
            }
            "tab" => {
                cx.stop_propagation();
                self.next_field();
                cx.notify();
            }
            _ => self.type_into(event, cx),
        }
    }

    /// Runs Test Connection of the selected connection in the background: the helper of the
    /// connection answers, and the answer is the product of the server or the reason.
    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.selected_draft() else {
            return;
        };
        if draft.url.text.trim().is_empty() {
            self.test = TestState::Done(Err(NO_SOURCE.to_owned()));
            cx.notify();
            return;
        }
        let id = draft.id.clone().unwrap_or_else(|| SourceId::new(""));
        let source = draft.source(id);
        let password = draft
            .password_edited
            .then(|| Secret::new(draft.password.text.clone()));
        self.test_serial += 1;
        let serial = self.test_serial;
        self.test = TestState::Running;
        cx.notify();
        let task = self
            .panel
            .update(cx, |panel, cx| panel.probe_source(source, password, cx));
        cx.spawn(async move |this, cx| {
            let answer = task.await;
            this.update(cx, |this, cx| {
                if this.test_serial == serial {
                    this.test = TestState::Done(answer);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts the download of the driver of the selected product; the answer comes back
    /// through the panel.
    fn download(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.selected_draft() else {
            return;
        };
        let kind = draft.kind;
        let asked = draft.version.text.trim().to_owned();
        let version = (!asked.is_empty()).then_some(asked);
        self.driver_error = None;
        self.panel
            .update(cx, |panel, cx| panel.start_form_download(kind, version, cx));
        cx.notify();
    }

    /// A field with the button that picks a file on the disk: the file of the database or
    /// the jar of a driver.
    fn with_browse(
        &self,
        field: Field,
        input: Stateful<Div>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let (id, selector, label, prompt) = match field {
            Field::Driver => (
                "database-browse-driver",
                "data-sources-browse-driver",
                "…",
                "Choose Driver Jar",
            ),
            _ => (
                "database-browse-file",
                "data-sources-browse",
                "Browse…",
                "Choose Database File",
            ),
        };
        let browse = small_button(id, label, theme)
            .debug_selector(move || selector.to_owned())
            .tooltip(move |_, cx| tooltip(prompt.to_owned(), None, cx))
            .on_click(cx.listener(move |this, _, _, cx| this.browse_file(field, cx)));
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .child(div().flex_1().min_w(px(0.)).child(input))
            .child(browse)
    }

    /// Asks for the file of the database or the jar of a driver with the system dialog. The
    /// file the user picks goes into the field, and an address is built again from its parts.
    fn browse_file(&mut self, field: Field, cx: &mut Context<Self>) {
        let prompt = if field == Field::Driver {
            "Choose Driver Jar"
        } else {
            "Choose Database File"
        };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(chosen) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.picked(field, chosen, cx))
                .ok();
        })
        .detach();
    }

    /// Puts the file the user picked into `field` of the selected connection.
    fn picked(&mut self, field: Field, chosen: PathBuf, cx: &mut Context<Self>) {
        if let Some(draft) = self.selected_draft_mut() {
            *draft.field_mut(field) = TextField::from_text(chosen.display().to_string());
            if field == Field::File {
                draft.rebuild_url();
            }
        }
        cx.notify();
    }

    /// Puts the file the user picked into the File field of the selected connection.
    #[cfg(test)]
    pub(crate) fn file_picked(&mut self, chosen: PathBuf, cx: &mut Context<Self>) {
        self.picked(Field::File, chosen, cx);
    }

    /// OK and Apply: every connection of the list goes to the panel, after the removed ones.
    /// A connection without an address keeps the window open with the reason.
    fn apply(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self
            .drafts
            .iter()
            .position(|draft| draft.url.text.trim().is_empty())
        {
            self.select(index, cx);
            self.error = Some(NO_SOURCE.to_owned());
            cx.notify();
            return;
        }
        let mut changes: Vec<SourceChange> = self
            .removed
            .iter()
            .cloned()
            .map(SourceChange::Remove)
            .collect();
        changes.extend(
            self.drafts
                .iter()
                .map(|draft| SourceChange::Save(Box::new(draft.edit()))),
        );
        let assigned = self
            .panel
            .update(cx, |panel, cx| panel.apply_data_sources(changes, close, cx));
        for (draft, id) in self.drafts.iter_mut().zip(assigned) {
            draft.saved(id);
        }
        self.removed.clear();
        self.error = None;
        if close {
            self.close(window, cx);
        } else {
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// The text field of the form, with its focus on click. A password shows dots.
    fn text_field(
        &self,
        field: Field,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let draft = self.selected_draft()?;
        let active = self.active == field;
        let shown = if field == Field::Password {
            TextField::from_text("•".repeat(draft.password.text.chars().count()))
        } else {
            draft.field(field).clone()
        };
        Some(
            div()
                .id(("database-field", field as usize))
                .debug_selector(move || format!("database-field-{}", field as usize))
                .flex()
                .items_center()
                .flex_1()
                .min_w(px(0.))
                .h(px(CONTROL_HEIGHT))
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
                .text_color(theme.text)
                .children(text_field_children_named(
                    if field == Field::Password {
                        FieldMouse::off()
                    } else {
                        FieldMouse::new(cx, move |this| this.draft_field_mut(field))
                    },
                    "data-sources-field",
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
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        this.focus_field(field, window, cx)
                    }),
                ),
        )
    }

    /// A label and a field of the form on one row, the label in the column of `width`.
    fn row(width: f32, label: &str, field: impl IntoElement, theme: &Theme) -> Div {
        form_row(width, label, field, theme)
    }

    /// The list of connections with its toolbar, the `+` products and the suggestions.
    fn list_column(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(
                icon_button(
                    "data-sources-add",
                    "general/add",
                    "Add Data Source",
                    None,
                    theme,
                )
                .debug_selector(|| "data-sources-add".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.adding_open = !this.adding_open;
                    this.kind_open = false;
                    cx.notify();
                })),
            )
            .child(
                icon_button(
                    "data-sources-remove",
                    "general/minus",
                    "Remove Data Source",
                    None,
                    theme,
                )
                .debug_selector(|| "data-sources-remove".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.remove_selected(cx))),
            )
            .child(
                icon_button(
                    "data-sources-copy",
                    "general/copy",
                    "Copy Data Source",
                    None,
                    theme,
                )
                .debug_selector(|| "data-sources-copy".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.copy_selected(cx))),
            );
        let chooser = self.adding_open.then(|| {
            div()
                .flex()
                .flex_col()
                .children(KINDS.iter().enumerate().map(|(index, kind)| {
                    let kind = *kind;
                    div()
                        .id(("data-sources-new", index))
                        .debug_selector(move || format!("data-sources-new-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(CONTROL_HEIGHT))
                        .px(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .text_color(theme.text)
                        .hover({
                            let hover = theme.hover;
                            move |style| style.bg(hover)
                        })
                        .child(icon(kind_icon(kind), theme))
                        .child(kind.title())
                        .on_click(cx.listener(move |this, _, _, cx| this.add_kind(kind, cx)))
                }))
        });
        let rows = self.drafts.iter().enumerate().map(|(index, draft)| {
            let selected = self.selected == Some(index);
            let label = if draft.name.text.trim().is_empty() {
                draft.kind.title().to_owned()
            } else {
                draft.name.text.trim().to_owned()
            };
            div()
                .id(("data-sources-item", index))
                .debug_selector(move || format!("data-sources-item-{index}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .text_color(if selected {
                    theme.text
                } else {
                    theme.text_secondary
                })
                .when(selected, |this| this.bg(theme.selection))
                .when(!selected, |this| {
                    let hover = theme.hover;
                    this.hover(move |style| style.bg(hover))
                })
                .child(icon(kind_icon(draft.kind), theme))
                .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(label))
                .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
        });
        let suggestions = (!self.suggestions.is_empty()).then(|| {
            let heading = div()
                .px(px(SPACE_M))
                .pt(px(SPACE_S))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child("From the project files");
            let buttons = self
                .suggestions
                .iter()
                .enumerate()
                .map(|(index, suggestion)| {
                    let label = if suggestion.module.is_empty() {
                        suggestion.url.clone()
                    } else {
                        format!("{} ({})", suggestion.url, suggestion.module)
                    };
                    let url = suggestion.url.clone();
                    small_button(("data-sources-suggestion", index), &label, theme)
                        .debug_selector(move || format!("database-suggestion-{url}"))
                        .on_click(cx.listener(move |this, _, _, cx| this.add_suggestion(index, cx)))
                });
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .child(heading)
                .children(buttons)
        });
        let list = scrollable(
            "data-sources-list",
            &self.list_scroll,
            div()
                .id("data-sources-list-body")
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .overflow_y_scroll()
                .track_scroll(&self.list_scroll)
                .children(rows),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_WIDTH))
            .min_h_0()
            .gap(px(SPACE_S))
            .child(toolbar)
            .children(chooser)
            .child(list)
            .children(suggestions)
    }

    /// The form of the selected connection, or the words that tell there is none.
    fn form_column(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = form_label_width(window, FORM_LABELS);
        let body = match self.selected_draft() {
            Some(draft) => self.form_body(draft, width, theme, cx),
            None => div()
                .debug_selector(|| "data-sources-empty".to_owned())
                .text_color(theme.text_muted)
                .child("No data sources. Press + to add one."),
        };
        scrollable(
            "data-sources-form",
            &self.form_scroll,
            div()
                .id("data-sources-form-body")
                .flex()
                .flex_col()
                .gap(px(SPACE_M))
                .overflow_y_scroll()
                .track_scroll(&self.form_scroll)
                .child(body),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_w(px(0.))
        .into_any_element()
    }

    fn form_body(&self, draft: &Draft, width: f32, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let visible = self.visible_fields(draft);
        let shows = |field: Field| visible.contains(&field);
        let mut body = div().flex().flex_col().gap(px(SPACE_M)).min_w(px(0.));
        let name = self.text_field(Field::Name, theme, cx);
        body = body.children(name.map(|field| Self::row(width, "Name", field, theme)));

        let kind = self.kind_row(draft, theme, cx);
        body = body.child(Self::row(width, "Database", kind, theme));
        if self.kind_open {
            let mut list = div().flex().flex_col();
            for (index, kind) in KINDS.iter().enumerate() {
                let kind = *kind;
                let chosen = kind == draft.kind;
                list = list.child(
                    div()
                        .id(("data-sources-kind-item", index))
                        .debug_selector(move || format!("data-sources-kind-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(CONTROL_HEIGHT))
                        .px(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .text_color(theme.text)
                        .when(chosen, |this| this.bg(theme.tab_selected))
                        .hover({
                            let hover = theme.hover;
                            move |style| style.bg(hover)
                        })
                        .child(icon(kind_icon(kind), theme))
                        .child(kind.title())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_kind(kind, cx))),
                );
            }
            body = body.child(form_aligned(width, list));
        }

        for (field, label) in [
            (Field::Host, "Host"),
            (Field::Port, "Port"),
            (Field::Database, "Database"),
            (Field::File, "File"),
        ] {
            if shows(field)
                && let Some(input) = self.text_field(field, theme, cx)
            {
                let input = if field == Field::File {
                    self.with_browse(field, input, theme, cx).into_any_element()
                } else {
                    input.into_any_element()
                };
                body = body.child(Self::row(width, label, input, theme));
            }
        }
        if let Some(input) = self.text_field(Field::Url, theme, cx) {
            body = body.child(Self::row(width, "Address", input, theme));
        }
        if let Some(input) = self.text_field(Field::User, theme, cx) {
            body = body.child(Self::row(width, "User", input, theme));
        }
        if let Some(input) = self.text_field(Field::Password, theme, cx) {
            body = body.child(Self::row(width, "Password", input, theme));
        }
        body = body.child(Self::row(
            width,
            "",
            self.save_password_row(draft, theme, cx),
            theme,
        ));
        body = body.child(self.driver_section(draft, shows(Field::Version), width, theme, cx));
        body
    }

    /// The product of the connection, as a button that opens the list of the products.
    fn kind_row(&self, draft: &Draft, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("data-sources-kind")
            .debug_selector(|| "data-sources-kind".to_owned())
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center()
            .justify_between()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.control_border)
            .text_color(theme.text)
            .cursor_pointer()
            .child(draft.kind.title())
            .child(icon("general/chevronDown", theme))
            .on_click(cx.listener(|this, _, _, cx| {
                this.kind_open = !this.kind_open;
                cx.notify();
            }))
    }

    /// Save password: a checkbox, off while the machine has no store of passwords, with the
    /// reason of the store as its tooltip.
    fn save_password_row(
        &self,
        draft: &Draft,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let on = draft.save_password && self.secrets_ready;
        let row = div()
            .id("database-save-password")
            .debug_selector(|| "database-save-password".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .text_size(px(TEXT_SMALL))
            .child(checkbox(on, theme))
            .child("Save password");
        if self.secrets_ready {
            row.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                if let Some(draft) = this.selected_draft_mut() {
                    draft.save_password = !draft.save_password;
                    draft.save_changed = true;
                }
                cx.notify();
            }))
        } else {
            let row = row.text_color(theme.text_muted).cursor_default();
            match self.secrets_reason.clone() {
                Some(reason) => {
                    let hint = SharedString::from(reason);
                    row.tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
                }
                None => row,
            }
        }
    }

    /// The driver of the product: whether the project has one, its jar, the download of a
    /// version that the repository offers, and the error of the last download.
    fn driver_section(
        &self,
        draft: &Draft,
        versions_shown: bool,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let package = self.package_of(draft.kind).cloned();
        let status = match self.drivers.iter().find(|driver| driver.kind == draft.kind) {
            Some(driver) => format!(
                "{} ({})",
                driver.label,
                driver.module.clone().unwrap_or_default()
            ),
            None if package.is_some() => {
                "No driver in the dependencies; download one or give the jar path.".to_owned()
            }
            None => "No driver in the dependencies; add one or give the jar path.".to_owned(),
        };
        let mut section = div().flex().flex_col().gap(px(SPACE_S)).child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(status),
        );
        if let Some(input) = self.text_field(Field::Driver, theme, cx) {
            let input = self
                .with_browse(Field::Driver, input, theme, cx)
                .into_any_element();
            section = section.child(Self::row(width, "Driver jar", input, theme));
        }
        let Some(package) = package else {
            return section;
        };
        if versions_shown && let Some(input) = self.text_field(Field::Version, theme, cx) {
            section = section.child(Self::row(width, "Driver version", input, theme));
        }
        if !self.versions.is_empty() {
            let mut choices = div().flex().flex_wrap().items_center().gap(px(SPACE_XS));
            for (index, version) in self.versions.iter().take(6).enumerate() {
                let picked = version.clone();
                let chosen = draft.version.text.trim() == version.as_str();
                choices = choices.child(
                    small_button(("database-version", index), version, theme)
                        .debug_selector(move || format!("database-version-{index}"))
                        .when(chosen, |this| this.bg(theme.tab_selected))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(draft) = this.selected_draft_mut() {
                                draft.version = TextField::from_text(picked.clone());
                            }
                            cx.notify();
                        })),
                );
            }
            section = section.child(Self::row(width, "", choices, theme));
        }
        if self.versions_loading {
            section = section.child(Self::row(
                width,
                "",
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child("Reading the versions of the repository…"),
                theme,
            ));
        } else if let Some(reason) = self.versions_error.clone() {
            section = section.child(Self::row(
                width,
                "",
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(reason),
                theme,
            ));
        }
        let download = self.panel.read(cx).form_download();
        let mut line = div().flex().items_center().gap(px(SPACE_S));
        if download.running {
            if let Some(text) = download.text {
                line = line.child(
                    div()
                        .debug_selector(|| "database-download-progress".to_owned())
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(text),
                );
            }
            line = line.child(
                small_button("database-download-cancel", "Cancel", theme)
                    .debug_selector(|| "database-download-cancel".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.panel.update(cx, |panel, cx| panel.cancel_download(cx));
                    })),
            );
        } else {
            line = line.child(
                small_button("database-dialog-download", "Download driver", theme)
                    .debug_selector(|| "database-dialog-download".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
            );
        }
        line = line.child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(format!(
                    "{} · {} · {}",
                    package.coordinate, package.license, package.repository
                )),
        );
        section = section.child(Self::row(width, "", line, theme));
        if let Some(error) = self.driver_error.clone() {
            section = section.child(Self::row(
                width,
                "",
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.stripe_error)
                    .child(error),
                theme,
            ));
        }
        section
    }

    /// The answer of the last Test Connection, shown beside its button in the footer.
    fn test_answer(&self, theme: &Theme) -> Option<Div> {
        match &self.test {
            TestState::Idle => None,
            TestState::Running => Some(
                div()
                    .debug_selector(|| "data-sources-test-result".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child("Testing the connection…"),
            ),
            TestState::Done(Ok(product)) => Some(
                div()
                    .debug_selector(|| "data-sources-test-result".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text)
                    .child(format!("Connected: {product}")),
            ),
            TestState::Done(Err(error)) => Some(
                div()
                    .debug_selector(|| "data-sources-test-result".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.stripe_error)
                    .child(error.clone()),
            ),
        }
    }

    /// The bottom row: Test Connection at the left edge, then OK, Cancel and Apply. Above the row
    /// stand the answer of Test Connection and the error of the last OK or Apply. The footer
    /// stays out of the scroll area, so these buttons are always where the user sees them.
    fn footer(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "data-sources-error".to_owned())
                .text_color(theme.stripe_error)
                .child(error)
        });
        let selected = self.selected_draft().is_some();
        let test = selected.then(|| {
            let running = self.test == TestState::Running;
            let mut button = dialog_button(
                "data-sources-test",
                "data-sources-test".to_owned(),
                "Test Connection",
                false,
                !running,
                theme,
            );
            if !running {
                button = button.on_click(cx.listener(|this, _, _, cx| this.test_connection(cx)));
            }
            button
        });
        let answer = if selected {
            self.test_answer(theme)
        } else {
            None
        };
        let ok = dialog_button(
            "database-dialog-add",
            "database-dialog-add".to_owned(),
            "OK",
            true,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx)));
        let cancel = dialog_button(
            "database-dialog-cancel",
            "database-dialog-cancel".to_owned(),
            "Cancel",
            false,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let apply = dialog_button(
            "database-dialog-apply",
            "database-dialog-apply".to_owned(),
            "Apply",
            false,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx)));
        let mut buttons = DialogButtons::new();
        if let Some(test) = test {
            buttons = buttons.extra(test);
        }
        let row = buttons.primary(ok).other(cancel).apply(apply).into_row();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .children(error)
            .children(answer)
            .child(row)
    }
}

impl Render for DataSourcesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let (view_width, view_height) = (f32::from(viewport.width), f32::from(viewport.height));
        let width = WIDTH.min(view_width - 2. * DIALOG_PADDING);
        let height = (view_height * HEIGHT_SHARE).min(view_height - 2. * DIALOG_PADDING);
        let place = DialogPlace {
            left: ((view_width - width) / 2.).max(0.),
            top: ((view_height - height) / 2.).max(0.),
            width,
            height: Some(height),
        };

        let list = self.list_column(&theme, window, cx);
        let form = self.form_column(&theme, window, cx);
        let footer = self.footer(&theme, cx);
        let shell = surface(&theme)
            .id("data-sources")
            .debug_selector(|| "data-sources".to_owned())
            .key_context("DataSources")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)),
            );
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(DIALOG_GAP))
                    .child(list)
                    .child(form),
            )
            .child(footer);
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "data-sources",
                title: "Data Sources and Drivers".into(),
                extra: None,
                place,
                min: MIN_SIZE,
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

#[cfg(test)]
impl DataSourcesDialog {
    /// The text of a field of the selected connection, for the window tests.
    pub(crate) fn text_for_test(&self, field: Field) -> Option<String> {
        self.selected_draft()
            .map(|draft| draft.field(field).text.clone())
    }

    /// The Save password flag of the selected connection, for the window tests.
    pub(crate) fn save_password_for_test(&self) -> Option<bool> {
        self.selected_draft().map(|draft| draft.save_password)
    }

    /// The version of the driver the form shows, for the window tests.
    pub(crate) fn version_for_test(&self) -> Option<String> {
        self.text_for_test(Field::Version)
    }

    /// The reason of the last OK or Apply that did not save, for the window tests.
    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }

    /// The answer of the last Test Connection, for the window tests.
    pub(crate) fn test_for_test(&self) -> Option<Result<String, String>> {
        match &self.test {
            TestState::Done(answer) => Some(answer.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fill_template, match_template, placeholders, probe_outcome};
    use database::Event;

    const POSTGRESQL: &str = "jdbc:postgresql://{host}:{port}/{database}";

    #[test]
    fn host_port_and_database_write_the_postgresql_address() {
        let address = fill_template(POSTGRESQL, |part| match part {
            "host" => "localhost".to_owned(),
            "port" => "5432".to_owned(),
            _ => "pet".to_owned(),
        });
        assert_eq!(address, "jdbc:postgresql://localhost:5432/pet");
        assert_eq!(placeholders(POSTGRESQL), ["host", "port", "database"]);
    }

    #[test]
    fn an_address_of_the_template_reads_back_its_parts() {
        let values = match_template(POSTGRESQL, "jdbc:postgresql://db.local:6543/owners")
            .expect("the address has the shape of the template");
        assert_eq!(
            values,
            [
                ("host".to_owned(), "db.local".to_owned()),
                ("port".to_owned(), "6543".to_owned()),
                ("database".to_owned(), "owners".to_owned()),
            ]
        );
    }

    #[test]
    fn an_address_of_another_shape_reads_nothing() {
        assert_eq!(match_template(POSTGRESQL, "mem:owners"), None);
        assert_eq!(
            match_template("jdbc:h2:{file}", "jdbc:h2:~/test").as_deref(),
            Some([("file".to_owned(), "~/test".to_owned())].as_slice())
        );
        assert_eq!(match_template("jdbc:h2:{file}", "jdbc:sqlite:x"), None);
    }

    #[test]
    fn the_answer_of_the_helper_is_the_product_or_the_reason() {
        let connected = Event::Connected {
            id: 1,
            product: "PostgreSQL 16.2".into(),
            auto_commit: true,
            quote: "\"".into(),
        };
        assert_eq!(
            probe_outcome([
                Event::Ready {
                    protocol: 1,
                    vendor: "x".into(),
                    version: "1".into(),
                },
                connected
            ]),
            Ok("PostgreSQL 16.2".to_owned())
        );
        let refused = Event::Error {
            id: 1,
            sql_state: None,
            code: None,
            message: "password authentication failed".into(),
            cancelled: false,
        };
        assert_eq!(
            probe_outcome([refused]),
            Err("password authentication failed".to_owned())
        );
        assert_eq!(
            probe_outcome([Event::Exited { code: Some(1) }]),
            Err("The helper stopped with code 1.".to_owned())
        );
        assert_eq!(
            probe_outcome([]),
            Err("The helper did not answer.".to_owned())
        );
    }
}
