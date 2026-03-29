//! VectorBackend abstraction for grepvec.
//!
//! Provides a trait for vector search and storage backends, with two implementations:
//! - `EnscribeBackend`: wraps the Enscribe HTTP API (search, ingest-prepared)
//! - `LocalBackend`: talks directly to local Qdrant + BGE inference server

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Result type for vector operations.
pub type VectorResult<T> = Result<T, VectorError>;

#[derive(Debug)]
pub enum VectorError {
    Connection(String),
    Embedding(String),
    Search(String),
    Ingest(String),
}

impl std::fmt::Display for VectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorError::Connection(s) => write!(f, "Connection: {}", s),
            VectorError::Embedding(s) => write!(f, "Embedding: {}", s),
            VectorError::Search(s) => write!(f, "Search: {}", s),
            VectorError::Ingest(s) => write!(f, "Ingest: {}", s),
        }
    }
}

impl std::error::Error for VectorError {}

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// A search result from the vector backend.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub document_id: String,
    pub content: String,
    pub score: f64,
}

/// A document to embed and store.
#[derive(Debug, Clone)]
pub struct VectorDocument {
    pub document_id: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
}

/// Configuration for search operations.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub collection: String,
    pub limit: usize,
    pub score_threshold: f32,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait VectorBackend: Send + Sync {
    /// Search for documents by semantic similarity.
    async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> VectorResult<Vec<VectorSearchResult>>;

    /// Embed and store documents.
    async fn upsert(
        &self,
        collection: &str,
        documents: Vec<VectorDocument>,
    ) -> VectorResult<usize>;

    /// Ensure a collection exists (create if needed).
    async fn ensure_collection(&self, name: &str, dimensions: usize) -> VectorResult<()>;

    /// Check if the backend is reachable.
    async fn health_check(&self) -> VectorResult<()>;

    /// Backend name for display.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// EnscribeBackend
// ---------------------------------------------------------------------------

pub struct EnscribeBackend {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl EnscribeBackend {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            http: reqwest::Client::new(),
        }
    }
}

/// Request body for Enscribe search API.
#[derive(Debug, Serialize)]
struct EnscribeSearchRequest {
    query: String,
    collection_id: String,
    limit: usize,
    score_threshold: f32,
    include_vectors: bool,
}

/// Response from Enscribe search API.
#[derive(Debug, Deserialize)]
struct EnscribeSearchResponse {
    results: Vec<EnscribeSearchResult>,
}

/// A single result from Enscribe search.
#[derive(Debug, Deserialize)]
struct EnscribeSearchResult {
    document_id: String,
    content: String,
    score: f64,
}

/// A segment within an ingest-prepared request.
#[derive(Debug, Serialize)]
struct EnscribeSegment {
    content: String,
    label: String,
    confidence: f64,
    reasoning: String,
    start_paragraph: u32,
    end_paragraph: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

/// Request body for Enscribe ingest-prepared API.
#[derive(Debug, Serialize)]
struct EnscribeIngestPreparedRequest {
    collection_id: String,
    document_id: String,
    segments: Vec<EnscribeSegment>,
}

/// Request body for Enscribe collection creation.
#[derive(Debug, Serialize)]
struct EnscribeCreateCollectionRequest {
    name: String,
}

#[async_trait]
impl VectorBackend for EnscribeBackend {
    async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> VectorResult<Vec<VectorSearchResult>> {
        let url = format!("{}/v1/search", self.base_url.trim_end_matches('/'));

        let body = EnscribeSearchRequest {
            query: query.to_string(),
            collection_id: config.collection.clone(),
            limit: config.limit,
            score_threshold: config.score_threshold,
            include_vectors: false,
        };

        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Search(format!("request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(VectorError::Search(format!(
                "HTTP {} — {}",
                status, err_body
            )));
        }

        let search_resp: EnscribeSearchResponse = resp
            .json()
            .await
            .map_err(|e| VectorError::Search(format!("response parse failed: {}", e)))?;

        Ok(search_resp
            .results
            .into_iter()
            .map(|r| VectorSearchResult {
                document_id: r.document_id,
                content: r.content,
                score: r.score,
            })
            .collect())
    }

    async fn upsert(
        &self,
        collection: &str,
        documents: Vec<VectorDocument>,
    ) -> VectorResult<usize> {
        let url = format!(
            "{}/v1/ingest-prepared",
            self.base_url.trim_end_matches('/')
        );
        let mut ingested = 0usize;

        for doc in &documents {
            let request = EnscribeIngestPreparedRequest {
                collection_id: collection.to_string(),
                document_id: doc.document_id.clone(),
                segments: vec![EnscribeSegment {
                    content: doc.content.clone(),
                    label: "content".to_string(),
                    confidence: 1.0,
                    reasoning: "vector backend upsert".to_string(),
                    start_paragraph: 0,
                    end_paragraph: 0,
                    metadata: doc.metadata.clone(),
                }],
            };

            let resp = self
                .http
                .post(&url)
                .header("X-API-Key", &self.api_key)
                .json(&request)
                .send()
                .await
                .map_err(|e| VectorError::Ingest(format!("request failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err_body = resp.text().await.unwrap_or_default();
                return Err(VectorError::Ingest(format!(
                    "HTTP {} for document {}: {}",
                    status, doc.document_id, err_body
                )));
            }

            ingested += 1;
        }

        Ok(ingested)
    }

    async fn ensure_collection(&self, name: &str, _dimensions: usize) -> VectorResult<()> {
        // Enscribe manages dimensions internally based on the embedding model.
        // We POST to create the collection; if it already exists, Enscribe returns success.
        let url = format!(
            "{}/v1/collections",
            self.base_url.trim_end_matches('/')
        );

        let body = EnscribeCreateCollectionRequest {
            name: name.to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Connection(format!("request failed: {}", e)))?;

        // 2xx or 409 (already exists) are both acceptable.
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::CONFLICT {
            Ok(())
        } else {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            Err(VectorError::Connection(format!(
                "create collection HTTP {} — {}",
                status, err_body
            )))
        }
    }

    async fn health_check(&self) -> VectorResult<()> {
        let url = format!("{}/health", self.base_url.trim_end_matches('/'));

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| VectorError::Connection(format!("health check failed: {}", e)))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(VectorError::Connection(format!(
                "health check returned HTTP {}",
                resp.status()
            )))
        }
    }

    fn name(&self) -> &str {
        "enscribe"
    }
}

// ---------------------------------------------------------------------------
// LocalBackend (Qdrant REST + BGE inference server)
// ---------------------------------------------------------------------------

pub struct LocalBackend {
    qdrant_url: String,
    bge_url: String,
    http: reqwest::Client,
}

impl LocalBackend {
    pub fn new(qdrant_url: String, bge_url: String) -> Self {
        Self {
            qdrant_url,
            bge_url,
            http: reqwest::Client::new(),
        }
    }

    /// Embed one or more texts via the BGE inference server.
    async fn embed_texts(&self, texts: &[&str]) -> VectorResult<Vec<Vec<f32>>> {
        let url = format!("{}/embed", self.bge_url.trim_end_matches('/'));

        let body = serde_json::json!({ "inputs": texts });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Embedding(format!("BGE request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(VectorError::Embedding(format!(
                "BGE HTTP {} — {}",
                status, err_body
            )));
        }

        let vectors: Vec<Vec<f32>> = resp
            .json()
            .await
            .map_err(|e| VectorError::Embedding(format!("BGE response parse failed: {}", e)))?;

        Ok(vectors)
    }
}

/// Qdrant search request body.
#[derive(Debug, Serialize)]
struct QdrantSearchRequest {
    vector: Vec<f32>,
    limit: usize,
    score_threshold: f32,
    with_payload: bool,
}

/// Qdrant search response.
#[derive(Debug, Deserialize)]
struct QdrantSearchResponse {
    result: Vec<QdrantSearchHit>,
}

/// A single Qdrant search hit.
#[derive(Debug, Deserialize)]
struct QdrantSearchHit {
    #[allow(dead_code)]
    id: serde_json::Value,
    score: f64,
    payload: Option<QdrantPayload>,
}

/// Qdrant point payload.
#[derive(Debug, Deserialize)]
struct QdrantPayload {
    document_id: Option<String>,
    content: Option<String>,
}

/// A single Qdrant point for upsert.
#[derive(Debug, Serialize)]
struct QdrantPoint {
    id: String,
    vector: Vec<f32>,
    payload: serde_json::Value,
}

/// Qdrant upsert request body.
#[derive(Debug, Serialize)]
struct QdrantUpsertRequest {
    points: Vec<QdrantPoint>,
}

/// Qdrant create collection request body.
#[derive(Debug, Serialize)]
struct QdrantCreateCollectionRequest {
    vectors: QdrantVectorConfig,
}

/// Qdrant vector configuration.
#[derive(Debug, Serialize)]
struct QdrantVectorConfig {
    size: usize,
    distance: String,
}

#[async_trait]
impl VectorBackend for LocalBackend {
    async fn search(
        &self,
        query: &str,
        config: &SearchConfig,
    ) -> VectorResult<Vec<VectorSearchResult>> {
        // Step 1: embed the query via BGE
        let embeddings = self.embed_texts(&[query]).await?;
        let query_vector = embeddings
            .into_iter()
            .next()
            .ok_or_else(|| VectorError::Embedding("BGE returned no vectors".to_string()))?;

        // Step 2: search Qdrant
        let url = format!(
            "{}/collections/{}/points/search",
            self.qdrant_url.trim_end_matches('/'),
            urlencoding::encode(&config.collection)
        );

        let body = QdrantSearchRequest {
            vector: query_vector,
            limit: config.limit,
            score_threshold: config.score_threshold,
            with_payload: true,
        };

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Search(format!("Qdrant search failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(VectorError::Search(format!(
                "Qdrant HTTP {} — {}",
                status, err_body
            )));
        }

        let search_resp: QdrantSearchResponse = resp
            .json()
            .await
            .map_err(|e| VectorError::Search(format!("Qdrant response parse failed: {}", e)))?;

        Ok(search_resp
            .result
            .into_iter()
            .map(|hit| {
                let payload = hit.payload.unwrap_or(QdrantPayload {
                    document_id: None,
                    content: None,
                });
                VectorSearchResult {
                    document_id: payload.document_id.unwrap_or_default(),
                    content: payload.content.unwrap_or_default(),
                    score: hit.score,
                }
            })
            .collect())
    }

    async fn upsert(
        &self,
        collection: &str,
        documents: Vec<VectorDocument>,
    ) -> VectorResult<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        // Step 1: embed all document contents via BGE (batch)
        let texts: Vec<&str> = documents.iter().map(|d| d.content.as_str()).collect();
        let embeddings = self.embed_texts(&texts).await?;

        if embeddings.len() != documents.len() {
            return Err(VectorError::Embedding(format!(
                "BGE returned {} vectors for {} documents",
                embeddings.len(),
                documents.len()
            )));
        }

        // Step 2: create Qdrant points and upsert
        let points: Vec<QdrantPoint> = documents
            .iter()
            .zip(embeddings.into_iter())
            .map(|(doc, vec)| {
                let mut payload = serde_json::json!({
                    "document_id": doc.document_id,
                    "content": doc.content,
                });
                if let Some(ref meta) = doc.metadata {
                    if let serde_json::Value::Object(map) = meta {
                        if let serde_json::Value::Object(ref mut p) = payload {
                            for (k, v) in map {
                                p.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                QdrantPoint {
                    id: uuid::Uuid::new_v4().to_string(),
                    vector: vec,
                    payload,
                }
            })
            .collect();

        let count = points.len();

        let url = format!(
            "{}/collections/{}/points",
            self.qdrant_url.trim_end_matches('/'),
            urlencoding::encode(collection)
        );

        let body = QdrantUpsertRequest { points };

        let resp = self
            .http
            .put(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Ingest(format!("Qdrant upsert failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Err(VectorError::Ingest(format!(
                "Qdrant HTTP {} — {}",
                status, err_body
            )));
        }

        Ok(count)
    }

    async fn ensure_collection(&self, name: &str, dimensions: usize) -> VectorResult<()> {
        // Check if the collection already exists
        let check_url = format!(
            "{}/collections/{}",
            self.qdrant_url.trim_end_matches('/'),
            urlencoding::encode(name)
        );

        let resp = self
            .http
            .get(&check_url)
            .send()
            .await
            .map_err(|e| VectorError::Connection(format!("Qdrant check failed: {}", e)))?;

        if resp.status().is_success() {
            // Collection already exists
            return Ok(());
        }

        // Create the collection
        let create_url = format!(
            "{}/collections/{}",
            self.qdrant_url.trim_end_matches('/'),
            urlencoding::encode(name)
        );

        let body = QdrantCreateCollectionRequest {
            vectors: QdrantVectorConfig {
                size: dimensions,
                distance: "Cosine".to_string(),
            },
        };

        let resp = self
            .http
            .put(&create_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::Connection(format!("Qdrant create failed: {}", e)))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            Err(VectorError::Connection(format!(
                "Qdrant create collection HTTP {} — {}",
                status, err_body
            )))
        }
    }

    async fn health_check(&self) -> VectorResult<()> {
        let url = format!("{}/healthz", self.qdrant_url.trim_end_matches('/'));

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| VectorError::Connection(format!("Qdrant health check failed: {}", e)))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(VectorError::Connection(format!(
                "Qdrant health check returned HTTP {}",
                resp.status()
            )))
        }
    }

    fn name(&self) -> &str {
        "local"
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Backend configuration.
#[derive(Debug, Clone)]
pub struct BackendConfig {
    pub backend_type: BackendType,
    pub enscribe_url: Option<String>,
    pub enscribe_key: Option<String>,
    pub qdrant_url: Option<String>,
    pub bge_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendType {
    Enscribe,
    Local,
}

/// Create the appropriate backend from config.
pub fn create_backend(config: &BackendConfig) -> Option<Box<dyn VectorBackend>> {
    match config.backend_type {
        BackendType::Enscribe => {
            let url = config.enscribe_url.as_ref()?;
            let key = config.enscribe_key.as_ref()?;
            if key.is_empty() {
                return None;
            }
            Some(Box::new(EnscribeBackend::new(url.clone(), key.clone())))
        }
        BackendType::Local => {
            let qdrant = config.qdrant_url.as_ref()?;
            let bge = config.bge_url.as_ref()?;
            Some(Box::new(LocalBackend::new(qdrant.clone(), bge.clone())))
        }
    }
}
