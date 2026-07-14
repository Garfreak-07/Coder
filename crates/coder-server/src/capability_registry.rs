use std::{collections::BTreeMap, sync::Arc};

use coder_config::ProjectConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityKind {
    Code,
}

#[derive(Debug, Clone)]
pub(crate) struct CapabilityRegistry {
    entries: Arc<BTreeMap<String, CapabilityKind>>,
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self {
            entries: Arc::new(BTreeMap::from([("code".to_owned(), CapabilityKind::Code)])),
        }
    }
}

impl CapabilityRegistry {
    pub(crate) fn resolve(&self, capability_id: &str) -> Option<CapabilityKind> {
        self.entries.get(capability_id).copied()
    }

    pub(crate) fn resolve_task_profile(
        &self,
        config: &ProjectConfig,
        task_profile_id: &str,
    ) -> Option<CapabilityKind> {
        config
            .task_profiles
            .contains_key(task_profile_id)
            .then(|| self.resolve("code"))
            .flatten()
    }

    pub(crate) fn ids(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }
}
