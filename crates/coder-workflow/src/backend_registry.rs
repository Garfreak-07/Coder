use std::sync::Arc;

use async_trait::async_trait;
use coder_harness::{HarnessBackend, HarnessError, HarnessRunRequest, HarnessRunResult};
#[cfg(test)]
use coder_store::RunStore;

#[cfg(test)]
use crate::DeterministicNativeBackend;
use crate::NativeMockBackend;

#[derive(Clone)]
pub struct BackendRegistry {
    native_rust: Arc<dyn HarnessBackend>,
    native_mock: Arc<dyn HarnessBackend>,
}

impl BackendRegistry {
    pub fn for_host() -> Self {
        Self {
            native_rust: Arc::new(UnavailableHostBackend("native-rust")),
            native_mock: Arc::new(NativeMockBackend::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_deterministic_tests(store: RunStore) -> Self {
        Self {
            native_rust: Arc::new(DeterministicNativeBackend::new(store)),
            native_mock: Arc::new(NativeMockBackend::default()),
        }
    }

    pub fn native_only() -> Self {
        Self {
            native_rust: Arc::new(NativeMockBackend::default()),
            native_mock: Arc::new(NativeMockBackend::default()),
        }
    }

    pub fn with_native_backend(mut self, backend: Arc<dyn HarnessBackend>) -> Self {
        self.native_rust = backend;
        self
    }

    pub fn backend_for(&self, backend: &str) -> Option<Arc<dyn HarnessBackend>> {
        match backend {
            "native-rust" => Some(Arc::clone(&self.native_rust)),
            "native_mock" | "mock" => Some(Arc::clone(&self.native_mock)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct UnavailableHostBackend(&'static str);

#[async_trait]
impl HarnessBackend for UnavailableHostBackend {
    async fn run(&self, _request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        Err(HarnessError::Failed(format!(
            "backend '{}' must be injected by the runtime host",
            self.0
        )))
    }
}
