//! T-0253: the passwords of the sources in the store of the system, without a window.
//! The Save password flag writes the password of a source, the Connect button takes it
//! back instead of asking, removing a source removes the password, and a machine without
//! a store keeps the flag off with the reason of the store.
//!
//! The store of every test is a stand-in: no test writes a password of a user.
//!
//! `cargo test -p panels window_tests_database_passwords`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use database::{DriverKind, PasswordStore, Secret, Source, SourceId};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::{ActiveDatabase, ActivePasswords, DatabasePanel};
use crate::window_tests::{
    click, draw, open_window, press, sample_project, type_text, wait_for_settled_bounds,
    wait_for_signal,
};

const SERVICE: &str = "Arro/database/owners";
const ACCOUNT: &str = "sa";
const PASSWORD: &str = "hunter2";

fn source() -> Source {
    Source {
        id: SourceId::new("owners"),
        name: "PetClinic".into(),
        kind: DriverKind::H2,
        url: "mem:petclinic".into(),
        user: ACCOUNT.into(),
        module: None,
        driver_jar: None,
    }
}

/// The store of the test: an optional reason that keeps it unavailable and the entries
/// it holds in memory.
struct FakePasswords {
    unavailable: Option<String>,
    entries: Mutex<HashMap<(String, String), String>>,
}

impl FakePasswords {
    fn ready() -> Arc<Self> {
        Arc::new(Self {
            unavailable: None,
            entries: Mutex::new(HashMap::new()),
        })
    }

    fn with_password(service: &str, account: &str, password: &str) -> Arc<Self> {
        let store = Self::ready();
        store
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (service.to_owned(), account.to_owned()),
                password.to_owned(),
            );
        store
    }

    fn unavailable(reason: &str) -> Arc<Self> {
        Arc::new(Self {
            unavailable: Some(reason.to_owned()),
            entries: Mutex::new(HashMap::new()),
        })
    }

    fn password(&self, service: &str, account: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(service.to_owned(), account.to_owned()))
            .cloned()
    }

    fn entries(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl PasswordStore for FakePasswords {
    fn available(&self) -> Result<(), String> {
        match &self.unavailable {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    fn store(&self, service: &str, account: &str, password: &Secret) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (service.to_owned(), account.to_owned()),
                password.expose().to_owned(),
            );
        Ok(())
    }

    fn load(&self, service: &str, account: &str) -> Option<Secret> {
        self.password(service, account).map(Secret::new)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
    store: Arc<FakePasswords>,
) -> (WindowHandle<Workspace>, gpui::Entity<DatabasePanel>) {
    let dynamic: Arc<dyn PasswordStore> = store;
    cx.update(|cx| cx.set_global(ActivePasswords(dynamic)));
    let root = sample_project(name);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveDatabase>().0.clone());
    // The toolbar and the tree need a bottom dock that shows them whole, as a user
    // would drag it.
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    crate::window_tests::with_window(cx, window, |window, cx| window.focus(&focus, cx));
    for _ in 0..6 {
        press(cx, window, "ctrl-shift-up");
    }
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "database-panel");
    draw(cx, window);
    (window, panel)
}

fn open_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "database-add");
    draw(cx, window);
}

/// Fills the form through the fields of the dialog, as the user clicks and types.
fn fill_fields(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for (selector, text) in [
        ("database-field-0", "Owners"),
        ("database-field-1", "mem:owners"),
        ("database-field-2", ACCOUNT),
        ("database-field-3", PASSWORD),
    ] {
        click(cx, window, selector);
        press(cx, window, "ctrl-a");
        type_text(cx, window, text);
        draw(cx, window);
    }
}

#[ui_kit::gpui::test]
fn the_save_password_flag_writes_the_password_of_a_new_source(cx: &mut TestAppContext) {
    let store = FakePasswords::ready();
    let (window, panel) = open(cx, "database-password-save", store.clone());
    wait_for_signal(cx, "the store of passwords", |cx| {
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test())
    });

    open_dialog(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.dialog_save_password_for_test()),
        Some(true),
        "the flag is on while the machine has a store"
    );
    fill_fields(cx, window);
    click(cx, window, "database-dialog-add");

    wait_for_signal(cx, "the saved password", |_| {
        store.password(SERVICE, ACCOUNT).is_some()
    });
    assert_eq!(store.password(SERVICE, ACCOUNT).as_deref(), Some(PASSWORD));
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.sources().len()),
        1,
        "the source is in the tree"
    );
}

#[ui_kit::gpui::test]
fn the_connect_button_takes_the_password_from_the_store(cx: &mut TestAppContext) {
    let store = FakePasswords::with_password(SERVICE, ACCOUNT, "from-the-store");
    let (window, panel) = open(cx, "database-password-connect", store);
    wait_for_signal(cx, "the store of passwords", |cx| {
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test())
    });
    panel.update(cx, |panel, cx| {
        panel.sources_for_test(vec![source()], cx);
    });
    draw(cx, window);
    assert!(
        !panel.read_with(cx, |panel, _| panel
            .has_cached_password_for_test(&SourceId::new("owners"))),
        "the window starts without a password"
    );

    click(cx, window, "database-connect");

    wait_for_signal(cx, "the password from the store", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .cached_password_for_test(&SourceId::new("owners"))
                .as_deref()
                == Some("from-the-store")
        })
    });
    assert!(
        panel.read_with(cx, |panel, _| panel
            .has_cached_password_for_test(&SourceId::new("owners"))),
        "the password is in place before a connection starts"
    );
}

#[ui_kit::gpui::test]
fn removing_a_source_removes_its_password(cx: &mut TestAppContext) {
    let store = FakePasswords::with_password(SERVICE, ACCOUNT, PASSWORD);
    let (_, panel) = open(cx, "database-password-remove", store.clone());
    wait_for_signal(cx, "the store of passwords", |cx| {
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test())
    });
    panel.update(cx, |panel, cx| {
        panel.sources_for_test(vec![source()], cx);
        panel.remove_source(SourceId::new("owners"), cx);
    });

    wait_for_signal(cx, "the removed password", |_| store.entries() == 0);
    assert!(
        panel.read_with(cx, |panel, _| panel.sources().is_empty()),
        "the source left the tree"
    );
}

#[ui_kit::gpui::test]
fn an_off_flag_removes_the_stored_password_of_the_source(cx: &mut TestAppContext) {
    let store = FakePasswords::with_password(SERVICE, ACCOUNT, "old-password");
    let (window, panel) = open(cx, "database-password-flag", store.clone());
    wait_for_signal(cx, "the store of passwords", |cx| {
        panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test())
    });

    open_dialog(cx, window);
    click(cx, window, "database-save-password");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.dialog_save_password_for_test()),
        Some(false),
        "the flag of the form is off"
    );
    fill_fields(cx, window);
    click(cx, window, "database-dialog-add");

    wait_for_signal(cx, "the removed password", |_| store.entries() == 0);
}

#[ui_kit::gpui::test]
fn without_a_store_the_save_password_flag_stays_off(cx: &mut TestAppContext) {
    let reason = "The password storage needs the `secret-tool` program. \
                  Install the `libsecret-tools` package for the Secret Service.";
    let store = FakePasswords::unavailable(reason);
    let (window, panel) = open(cx, "database-password-missing", store.clone());

    wait_for_signal(cx, "the reason of the missing store", |cx| {
        panel
            .read_with(cx, |panel, _| panel.password_storage_reason_for_test())
            .is_some()
    });
    assert!(
        !panel.read_with(cx, |panel, _| panel.password_storage_ready_for_test()),
        "the window knows the store is missing"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.password_storage_reason_for_test()),
        Some(reason.to_owned())
    );

    open_dialog(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.dialog_save_password_for_test()),
        Some(false),
        "the flag starts off"
    );
    click(cx, window, "database-save-password");
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.dialog_save_password_for_test()),
        Some(false),
        "the flag of a missing store does not turn on"
    );

    fill_fields(cx, window);
    click(cx, window, "database-dialog-add");
    wait_for_signal(cx, "the added source", |cx| {
        panel.read_with(cx, |panel, _| panel.sources().len() == 1)
    });
    assert_eq!(store.entries(), 0, "nothing was written to the store");
}
