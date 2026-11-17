//! The passwords of the sources, kept by the store of the system rather than in the
//! state folder of the project. The window reaches the system through [`PasswordStore`],
//! so a test gives its own store and no test writes the passwords of a user.

use crate::sources::{Secret, SourceId};

/// The product part of the service names of these entries.
const APPLICATION: &str = "Arro";
/// The group part of the service names of these entries.
const GROUP: &str = "database";

/// The name of the password of one source: `Arro/database/<id>`, the service every
/// platform stores it under.
pub fn service_of(id: &SourceId) -> String {
    platform::secrets::service_name(APPLICATION, GROUP, id.as_str())
}

/// The store of passwords of the machine.
pub trait PasswordStore: Send + Sync {
    /// Whether the machine has a store, with the reason and the package to install when
    /// it has none.
    fn available(&self) -> Result<(), String>;

    /// Writes the password of the account under the service.
    fn store(&self, service: &str, account: &str, password: &Secret) -> Result<(), String>;

    /// Reads the password of the account, or nothing when the store has none.
    fn load(&self, service: &str, account: &str) -> Option<Secret>;

    /// Removes the password of the account; a missing entry is not an error.
    fn delete(&self, service: &str, account: &str) -> Result<(), String>;
}

/// The store of the system: Credential Manager, Keychain or Secret Service.
pub struct SystemPasswords;

impl PasswordStore for SystemPasswords {
    fn available(&self) -> Result<(), String> {
        platform::secrets::available()
    }

    fn store(&self, service: &str, account: &str, password: &Secret) -> Result<(), String> {
        platform::secrets::store(service, account, password)
    }

    fn load(&self, service: &str, account: &str) -> Option<Secret> {
        platform::secrets::load(service, account)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), String> {
        platform::secrets::delete(service, account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_has_a_service_of_its_own() {
        assert_eq!(service_of(&SourceId::new("owners")), "Arro/database/owners");
        assert_ne!(
            service_of(&SourceId::new("owners")),
            service_of(&SourceId::new("owners-2"))
        );
    }

    #[test]
    fn a_password_shows_itself_nowhere() {
        let secret = Secret::new("hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret}").contains("hunter2"));
    }
}
