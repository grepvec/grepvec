use crate::enscribe_embed::{
    EnscribeClient, EnscribeError, IngestResponse, MemoryEntry, MemoryHeader, MemoryKind,
    MemoryLane, ResolutionMode, SearchGranularity, SearchResponse,
};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub struct MemoryWriteInput {
    pub node_id: String,
    pub agent_id: String,
    pub lane: MemoryLane,
    pub kind: MemoryKind,
    pub body: String,
    pub source: String,
    pub dsl_version: String,
    pub created_at: String,
    pub timestamp_ms: u64,
}

impl MemoryWriteInput {
    pub fn into_entry(self) -> MemoryEntry {
        let paragraph_id = format!(
            "mem::{}::{}::{}",
            self.node_id,
            self.timestamp_ms,
            self.kind.as_str()
        );
        let header = MemoryHeader {
            project_id: project_id_for_lane(&self.lane).map(|s| s.to_string()),
            node_id: self.node_id.clone(),
            agent_id: self.agent_id,
            memory_type: lane_label(&self.lane).to_string(),
            memory_kind: self.kind.as_str().to_string(),
            source: self.source,
            dsl_version: self.dsl_version,
            created_at: self.created_at,
        };
        MemoryEntry {
            lane: self.lane,
            node_id: self.node_id,
            paragraph_id,
            header,
            body: self.body,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemorySnippet {
    pub paragraph_id: String,
    pub document_id: String,
    pub score: f32,
    pub body: String,
    pub header: Option<MemoryHeader>,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MemoryRecallFailure {
    pub lane: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct MemoryRecallReport {
    pub results: Vec<MemorySnippet>,
    pub failures: Vec<MemoryRecallFailure>,
}

#[derive(Clone, Debug)]
pub struct MemoryStore {
    client: EnscribeClient,
    tenant_id: String,
}

impl MemoryStore {
    pub fn new(client: EnscribeClient, tenant_id: impl Into<String>) -> Self {
        Self {
            client,
            tenant_id: tenant_id.into(),
        }
    }

    pub fn client(&self) -> &EnscribeClient {
        &self.client
    }

    pub async fn write(
        &self,
        input: MemoryWriteInput,
        resolution_mode: Option<ResolutionMode>,
    ) -> Result<IngestResponse, EnscribeError> {
        let entry = input.into_entry();
        let request = self
            .client
            .ingest_request_for_memory(&self.tenant_id, &entry, resolution_mode)?;
        let request_id = request_id_for(&request.document_id, &entry.paragraph_id);
        self.client.ingest(&request, Some(&request_id)).await
    }

    pub async fn recall_lane(
        &self,
        lane: &MemoryLane,
        node_id: &str,
        query: &str,
        limit: u32,
        granularity: Option<SearchGranularity>,
    ) -> Result<Vec<MemorySnippet>, EnscribeError> {
        let request = self.client.search_request_for_memory(
            &self.tenant_id,
            lane,
            node_id,
            query,
            limit,
            granularity,
        );
        let response = self.client.search(&request).await?;
        Ok(parse_search_results(&response))
    }

    pub async fn recall_node(
        &self,
        node_id: &str,
        session_id: Option<&str>,
        project_id: Option<&str>,
        query: &str,
    ) -> MemoryRecallReport {
        let mut results = Vec::new();
        let mut failures = Vec::new();

        if let Some(session_id) = session_id {
            let lane = MemoryLane::Session {
                session_id: session_id.to_string(),
            };
            match self
                .recall_lane(&lane, node_id, query, 5, Some(SearchGranularity::Topic))
                .await
            {
                Ok(mut lane_results) => results.append(&mut lane_results),
                Err(err) => failures.push(MemoryRecallFailure {
                    lane: lane_label(&lane).to_string(),
                    message: err.to_string(),
                }),
            }
        }

        if let Some(project_id) = project_id {
            let lane = MemoryLane::Project {
                project_id: project_id.to_string(),
            };
            match self
                .recall_lane(&lane, node_id, query, 8, Some(SearchGranularity::Context))
                .await
            {
                Ok(mut lane_results) => results.append(&mut lane_results),
                Err(err) => failures.push(MemoryRecallFailure {
                    lane: lane_label(&lane).to_string(),
                    message: err.to_string(),
                }),
            }
        }

        let lane = MemoryLane::Knowledge;
        match self
            .recall_lane(&lane, node_id, query, 8, Some(SearchGranularity::Precise))
            .await
        {
            Ok(mut lane_results) => results.append(&mut lane_results),
            Err(err) => failures.push(MemoryRecallFailure {
                lane: lane_label(&lane).to_string(),
                message: err.to_string(),
            }),
        }

        sort_snippets(&mut results);

        MemoryRecallReport { results, failures }
    }
}

fn request_id_for(document_id: &str, paragraph_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document_id.as_bytes());
    hasher.update(b"::");
    hasher.update(paragraph_id.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)
}

fn parse_search_results(response: &SearchResponse) -> Vec<MemorySnippet> {
    response
        .results
        .iter()
        .map(|result| {
            let (header, body) = split_envelope(&result.content);
            let created_at = header.as_ref().map(|h| h.created_at.clone());
            MemorySnippet {
                paragraph_id: result.paragraph_id.clone(),
                document_id: result.document_id.clone(),
                score: result.score,
                body,
                header,
                created_at,
            }
        })
        .collect()
}

fn split_envelope(content: &str) -> (Option<MemoryHeader>, String) {
    let prefix = "---\n";
    let delimiter = "\n---\n";
    if let Some(rest) = content.strip_prefix(prefix) {
        if let Some(idx) = rest.find(delimiter) {
            let header_str = &rest[..idx];
            let body_start = idx + delimiter.len();
            let body = rest[body_start..].to_string();
            let header = serde_json::from_str::<MemoryHeader>(header_str).ok();
            return (header, body);
        }
    }
    (None, content.to_string())
}

fn sort_snippets(snippets: &mut [MemorySnippet]) {
    snippets.sort_by(|a, b| {
        let score_cmp = b
            .score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal);
        if score_cmp != Ordering::Equal {
            return score_cmp;
        }
        let a_ts = a.created_at.as_deref().unwrap_or("");
        let b_ts = b.created_at.as_deref().unwrap_or("");
        b_ts.cmp(a_ts)
    });
}

fn lane_label(lane: &MemoryLane) -> &'static str {
    match lane {
        MemoryLane::Session { .. } => "session",
        MemoryLane::Project { .. } => "project",
        MemoryLane::Knowledge => "knowledge",
    }
}

fn project_id_for_lane(lane: &MemoryLane) -> Option<&str> {
    match lane {
        MemoryLane::Project { project_id } => Some(project_id.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enscribe_embed::MemoryHeader;

    #[test]
    fn memory_write_input_builds_paragraph_id() {
        let input = MemoryWriteInput {
            node_id: "Button--Login--Primary--submit".to_string(),
            agent_id: "agent-1".to_string(),
            lane: MemoryLane::Knowledge,
            kind: MemoryKind::Decision,
            body: "Decision body".to_string(),
            source: "agent".to_string(),
            dsl_version: "0.1".to_string(),
            created_at: "0000000000001".to_string(),
            timestamp_ms: 1,
        };
        let entry = input.into_entry();
        assert_eq!(
            entry.paragraph_id,
            "mem::Button--Login--Primary--submit::1::decision"
        );
    }

    #[test]
    fn split_envelope_parses_header_and_body() {
        let header = MemoryHeader {
            project_id: Some("proj-1".to_string()),
            node_id: "Node--A--B--C".to_string(),
            agent_id: "agent-9".to_string(),
            memory_type: "project".to_string(),
            memory_kind: "summary".to_string(),
            source: "agent".to_string(),
            dsl_version: "0.1".to_string(),
            created_at: "0000000000002".to_string(),
        };
        let header_json = header.to_canonical_json().expect("header json");
        let content = format!("---\n{}\n---\n{}", header_json, "Body text");

        let (parsed_header, body) = split_envelope(&content);
        let parsed_header = parsed_header.expect("header");

        assert_eq!(body, "Body text");
        assert_eq!(parsed_header.node_id, "Node--A--B--C");
        assert_eq!(parsed_header.memory_kind, "summary");
    }

    #[test]
    fn request_id_is_deterministic() {
        let id_a = request_id_for("doc-1", "para-1");
        let id_b = request_id_for("doc-1", "para-1");
        let id_c = request_id_for("doc-2", "para-1");

        assert_eq!(id_a, id_b);
        assert_ne!(id_a, id_c);
    }

    #[test]
    fn sort_snippets_orders_by_score_then_timestamp() {
        let mut snippets = vec![
            MemorySnippet {
                paragraph_id: "a".to_string(),
                document_id: "doc".to_string(),
                score: 0.8,
                body: "a".to_string(),
                header: None,
                created_at: Some("0000000000002".to_string()),
            },
            MemorySnippet {
                paragraph_id: "b".to_string(),
                document_id: "doc".to_string(),
                score: 0.9,
                body: "b".to_string(),
                header: None,
                created_at: Some("0000000000001".to_string()),
            },
            MemorySnippet {
                paragraph_id: "c".to_string(),
                document_id: "doc".to_string(),
                score: 0.8,
                body: "c".to_string(),
                header: None,
                created_at: Some("0000000000003".to_string()),
            },
        ];

        sort_snippets(&mut snippets);
        assert_eq!(snippets[0].paragraph_id, "b");
        assert_eq!(snippets[1].paragraph_id, "c");
        assert_eq!(snippets[2].paragraph_id, "a");
    }
}
