use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug)]
pub struct EnscribeConfig {
    pub base_url: String,
    pub api_key: String,
    pub openai_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EnscribeClient {
    config: EnscribeConfig,
    http: reqwest::Client,
}

impl EnscribeClient {
    pub fn new(config: EnscribeConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    pub fn config(&self) -> &EnscribeConfig {
        &self.config
    }

    pub fn ingest_request_for_memory(
        &self,
        tenant_id: &str,
        entry: &MemoryEntry,
        resolution_mode: Option<ResolutionMode>,
    ) -> Result<IngestRequest, EnscribeError> {
        let document_id = entry.lane.document_id(&entry.node_id);
        let content = entry.content_with_header()?;
        let paragraph = ParagraphIn {
            id: entry.paragraph_id.clone(),
            content,
        };

        Ok(IngestRequest {
            tenant_id: tenant_id.to_string(),
            document_id,
            paragraphs: vec![paragraph],
            options: Some(IngestOptions {
                strategy: Some("baseline".to_string()),
                strategy_config: Some(serde_json::json!({
                    "chunk_size": 400,
                    "chunk_overlap": 32
                })),
                strategies: None,
                strategy_configs: None,
            }),
            resolution_mode: resolution_mode.map(|mode| mode.as_str().to_string()),
        })
    }

    pub fn search_request_for_memory(
        &self,
        tenant_id: &str,
        lane: &MemoryLane,
        node_id: &str,
        query: &str,
        limit: u32,
        granularity: Option<SearchGranularity>,
    ) -> SearchRequest {
        SearchRequest {
            query: query.to_string(),
            tenant_id: tenant_id.to_string(),
            filters: Some(SearchFilters {
                document_id: Some(lane.document_id(node_id)),
                user_id: None,
                conversation_id: None,
                layer: Some("baseline".to_string()),
                strategy: Some("baseline".to_string()),
            }),
            limit: Some(limit),
            score_threshold: None,
            include_vectors: Some(false),
            include_timing: Some(false),
            granularity: granularity.map(|g| g.as_str().to_string()),
        }
    }

    pub async fn ingest(
        &self,
        request: &IngestRequest,
        request_id: Option<&str>,
    ) -> Result<IngestResponse, EnscribeError> {
        let url = format!(
            "{}/v1/embeddings/paragraphs",
            self.config.base_url.trim_end_matches('/')
        );
        let mut req = self.http.post(url).json(request);
        req = req.header("X-API-Key", &self.config.api_key);
        if let Some(openai_key) = &self.config.openai_key {
            req = req.header("X-OpenAI-Key", openai_key);
        }
        if let Some(request_id) = request_id {
            req = req.header("X-Request-Id", request_id);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(EnscribeError::Status { status, body });
        }
        Ok(resp.json::<IngestResponse>().await?)
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, EnscribeError> {
        let url = format!(
            "{}/v1/search",
            self.config.base_url.trim_end_matches('/')
        );
        let mut req = self.http.post(url).json(request);
        req = req.header("X-API-Key", &self.config.api_key);
        if let Some(openai_key) = &self.config.openai_key {
            req = req.header("X-OpenAI-Key", openai_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(EnscribeError::Status { status, body });
        }
        Ok(resp.json::<SearchResponse>().await?)
    }

    pub async fn reconstruct(
        &self,
        tenant_id: &str,
        document_id: &str,
        layer: Option<&str>,
        include_metadata: bool,
        deduplicate: bool,
    ) -> Result<ReconstructResponse, EnscribeError> {
        let base = self.config.base_url.trim_end_matches('/');
        let tenant_enc = urlencoding::encode(tenant_id);
        let document_enc = urlencoding::encode(document_id);
        let mut url = format!(
            "{}/v1/embeddings/{}/{}/reconstruct",
            base, tenant_enc, document_enc
        );
        let mut query_parts = Vec::new();
        if let Some(layer) = layer {
            query_parts.push(format!("layer={}", urlencoding::encode(layer)));
        }
        query_parts.push(format!("include_metadata={}", include_metadata));
        query_parts.push(format!("deduplicate={}", deduplicate));
        if !query_parts.is_empty() {
            url.push('?');
            url.push_str(&query_parts.join("&"));
        }

        let mut req = self.http.get(url);
        req = req.header("X-API-Key", &self.config.api_key);
        if let Some(openai_key) = &self.config.openai_key {
            req = req.header("X-OpenAI-Key", openai_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(EnscribeError::Status { status, body });
        }
        Ok(resp.json::<ReconstructResponse>().await?)
    }
}

#[derive(Clone, Debug)]
pub enum MemoryLane {
    Session { session_id: String },
    Project { project_id: String },
    Knowledge,
}

#[derive(Clone, Debug)]
pub enum MemoryKind {
    Decision,
    Summary,
    Error,
    Trace,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Decision => "decision",
            MemoryKind::Summary => "summary",
            MemoryKind::Error => "error",
            MemoryKind::Trace => "trace",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryHeader {
    pub project_id: Option<String>,
    pub node_id: String,
    pub agent_id: String,
    pub memory_type: String,
    pub memory_kind: String,
    pub source: String,
    pub dsl_version: String,
    pub created_at: String,
}

impl MemoryHeader {
    pub fn to_canonical_json(&self) -> Result<String, EnscribeError> {
        let mut map = BTreeMap::new();
        if let Some(project_id) = &self.project_id {
            map.insert("project_id".to_string(), serde_json::Value::String(project_id.clone()));
        }
        map.insert("node_id".to_string(), serde_json::Value::String(self.node_id.clone()));
        map.insert("agent_id".to_string(), serde_json::Value::String(self.agent_id.clone()));
        map.insert(
            "memory_type".to_string(),
            serde_json::Value::String(self.memory_type.clone()),
        );
        map.insert(
            "memory_kind".to_string(),
            serde_json::Value::String(self.memory_kind.clone()),
        );
        map.insert("source".to_string(), serde_json::Value::String(self.source.clone()));
        map.insert(
            "dsl_version".to_string(),
            serde_json::Value::String(self.dsl_version.clone()),
        );
        map.insert(
            "created_at".to_string(),
            serde_json::Value::String(self.created_at.clone()),
        );
        Ok(serde_json::to_string(&map)?)
    }
}

#[derive(Clone, Debug)]
pub struct MemoryEntry {
    pub lane: MemoryLane,
    pub node_id: String,
    pub paragraph_id: String,
    pub header: MemoryHeader,
    pub body: String,
}

impl MemoryEntry {
    pub fn content_with_header(&self) -> Result<String, EnscribeError> {
        let header_json = self.header.to_canonical_json()?;
        Ok(format!("---\n{}\n---\n{}", header_json, self.body))
    }
}

impl MemoryLane {
    pub fn document_id(&self, node_id: &str) -> String {
        match self {
            MemoryLane::Session { session_id } => {
                format!("grepvec::node::{}::session::{}", node_id, session_id)
            }
            MemoryLane::Project { project_id } => {
                format!("grepvec::node::{}::project::{}", node_id, project_id)
            }
            MemoryLane::Knowledge => format!("grepvec::node::{}::knowledge", node_id),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ParagraphIn {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct IngestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_configs: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Serialize)]
pub struct IngestRequest {
    pub tenant_id: String,
    pub document_id: String,
    pub paragraphs: Vec<ParagraphIn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<IngestOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_mode: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct IngestResponse {
    pub processed_count: u32,
    pub new_embeddings_count: u32,
    pub layers_written: Vec<String>,
    pub total_tokens_used: u32,
}

#[derive(Debug, Serialize)]
pub struct SearchFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchRequest {
    pub query: String,
    pub tenant_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<SearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_vectors: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_timing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granularity: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub paragraph_id: String,
    pub document_id: String,
    pub score: f32,
    pub content: String,
    pub metadata: serde_json::Value,
    pub vector: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize)]
pub struct SearchTimingBreakdown {
    pub api_overhead_ms: Option<u64>,
    pub openai_embedding_ms: Option<u64>,
    pub qdrant_search_ms: Option<u64>,
    pub serialization_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub query_vector: Option<Vec<f32>>,
    pub search_time_ms: Option<u64>,
    pub timing_breakdown: Option<SearchTimingBreakdown>,
}

#[derive(Debug, Deserialize)]
pub struct ReconstructChunk {
    pub id: String,
    pub content: String,
    pub source_paragraph_index: u32,
    pub chunk_index: u32,
    pub overlap_tokens: Option<u32>,
    pub token_count: Option<u32>,
    pub layer: String,
}

#[derive(Debug, Deserialize)]
pub struct ReconstructResponse {
    pub document_id: String,
    pub tenant_id: String,
    pub layer: String,
    pub content: String,
    pub character_count: u32,
    pub chunk_count: u32,
    pub paragraph_count: u32,
    pub deduplicated: bool,
    pub chunks: Option<Vec<ReconstructChunk>>,
}

#[derive(Clone, Copy, Debug)]
pub enum ResolutionMode {
    Fixed,
    Adaptive,
    Fast,
    Balanced,
    Precise,
}

impl ResolutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionMode::Fixed => "fixed",
            ResolutionMode::Adaptive => "adaptive",
            ResolutionMode::Fast => "fast",
            ResolutionMode::Balanced => "balanced",
            ResolutionMode::Precise => "precise",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SearchGranularity {
    Topic,
    Context,
    Precise,
    Adaptive,
}

impl SearchGranularity {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchGranularity::Topic => "topic",
            SearchGranularity::Context => "context",
            SearchGranularity::Precise => "precise",
            SearchGranularity::Adaptive => "adaptive",
        }
    }
}

#[derive(Debug)]
pub enum EnscribeError {
    Http(reqwest::Error),
    Json(serde_json::Error),
    Status { status: reqwest::StatusCode, body: String },
}

impl fmt::Display for EnscribeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnscribeError::Http(err) => write!(f, "http error: {}", err),
            EnscribeError::Json(err) => write!(f, "json error: {}", err),
            EnscribeError::Status { status, body } => {
                write!(f, "enscribe error: status={} body={}", status, body)
            }
        }
    }
}

impl std::error::Error for EnscribeError {}

impl From<reqwest::Error> for EnscribeError {
    fn from(err: reqwest::Error) -> Self {
        EnscribeError::Http(err)
    }
}

impl From<serde_json::Error> for EnscribeError {
    fn from(err: serde_json::Error) -> Self {
        EnscribeError::Json(err)
    }
}
