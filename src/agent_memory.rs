use crate::enscribe_embed::{
    EnscribeClient, EnscribeError, IngestResponse, MemoryKind, MemoryLane, ResolutionMode,
};
use crate::memory::{MemoryRecallReport, MemoryStore, MemoryWriteInput};

#[derive(Clone, Debug)]
pub struct AgentMemoryConfig {
    pub node_id: String,
    pub agent_id: String,
    pub dsl_version: String,
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct AgentMemory {
    store: MemoryStore,
    config: AgentMemoryConfig,
}

impl AgentMemory {
    pub fn new(store: MemoryStore, config: AgentMemoryConfig) -> Self {
        Self { store, config }
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    pub fn client(&self) -> &EnscribeClient {
        self.store.client()
    }

    pub async fn record(
        &self,
        lane: MemoryLane,
        kind: MemoryKind,
        body: impl Into<String>,
        created_at: String,
        timestamp_ms: u64,
        resolution_mode: Option<ResolutionMode>,
    ) -> Result<IngestResponse, EnscribeError> {
        let input = MemoryWriteInput {
            node_id: self.config.node_id.clone(),
            agent_id: self.config.agent_id.clone(),
            lane,
            kind,
            body: body.into(),
            source: self.config.source.clone(),
            dsl_version: self.config.dsl_version.clone(),
            created_at,
            timestamp_ms,
        };
        self.store.write(input, resolution_mode).await
    }

    pub async fn recall(
        &self,
        session_id: Option<&str>,
        project_id: Option<&str>,
        query: &str,
    ) -> MemoryRecallReport {
        self.store
            .recall_node(&self.config.node_id, session_id, project_id, query)
            .await
    }
}
