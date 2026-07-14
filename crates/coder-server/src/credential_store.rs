use std::{error::Error, fmt, sync::Arc};

use keyring::{Entry, Error as KeyringError};

pub(crate) const PROVIDER_KEYRING_SERVICE: &str = "Coder Provider Credentials";

#[derive(Debug)]
pub(crate) struct CredentialStoreError(KeyringError);

impl CredentialStoreError {
    fn new(error: KeyringError) -> Self {
        Self(error)
    }
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for CredentialStoreError {}

pub(crate) trait KeyringStore: fmt::Debug + Send + Sync {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError>;
    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError>;
    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DefaultKeyringStore;

impl KeyringStore for DefaultKeyringStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(CredentialStoreError::new(error)),
        }
    }

    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError> {
        Entry::new(service, account)
            .map_err(CredentialStoreError::new)?
            .set_password(value)
            .map_err(CredentialStoreError::new)
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(KeyringError::NoEntry) => Ok(false),
            Err(error) => Err(CredentialStoreError::new(error)),
        }
    }
}

pub(crate) fn default_keyring_store() -> Arc<dyn KeyringStore> {
    Arc::new(DefaultKeyringStore)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use super::{CredentialStoreError, KeyringError, KeyringStore};

    #[derive(Debug, Clone, Default)]
    pub(crate) struct MemoryKeyringStore {
        values: Arc<Mutex<BTreeMap<String, String>>>,
        failure: Arc<Mutex<Option<String>>>,
    }

    impl MemoryKeyringStore {
        pub(crate) fn value(&self, account: &str) -> Option<String> {
            self.values.lock().unwrap().get(account).cloned()
        }

        pub(crate) fn fail_with(&self, message: impl Into<String>) {
            *self.failure.lock().unwrap() = Some(message.into());
        }

        pub(crate) fn clear_failure(&self) {
            *self.failure.lock().unwrap() = None;
        }

        fn failure(&self) -> Result<(), CredentialStoreError> {
            match self.failure.lock().unwrap().clone() {
                Some(message) => Err(CredentialStoreError::new(KeyringError::PlatformFailure(
                    Box::new(std::io::Error::other(message)),
                ))),
                None => Ok(()),
            }
        }
    }

    impl KeyringStore for MemoryKeyringStore {
        fn load(
            &self,
            _service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            self.failure()?;
            Ok(self.value(account))
        }

        fn save(
            &self,
            _service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            self.failure()?;
            self.values
                .lock()
                .unwrap()
                .insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, _service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            self.failure()?;
            Ok(self.values.lock().unwrap().remove(account).is_some())
        }
    }
}
