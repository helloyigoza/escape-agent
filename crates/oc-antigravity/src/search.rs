//! Grounded Google Search (`google_search`) and URL context analysis tool via Cloud Code Assist.

use crate::constants::{
    ANTIGRAVITY_PROVIDER_ID, HeaderStyle, SEARCH_MODEL, SEARCH_SYSTEM_INSTRUCTION,
    SEARCH_THINKING_BUDGET_DEEP, SEARCH_THINKING_BUDGET_FAST, SEARCH_TIMEOUT_MS, headers_for_style,
};
use crate::error::{AntigravityError, Result};
use crate::service::AntigravityService;
use async_trait::async_trait;
use oc_tools::{Tool, ToolContext, ToolError, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct GoogleSearchInput {
    query: String,
    #[serde(default)]
    urls: Vec<String>,
    #[serde(default)]
    thinking: Option<bool>,
}

/// Citation returned by Google Search grounding metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchCitation {
    /// Grounding source title.
    pub title: String,
    /// Grounding source URL.
    pub uri: String,
}

/// Structured output returned by executing [`execute_google_search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleSearchOutput {
    /// Answer body synthesized by Gemini 2.5 Flash grounding.
    pub answer: String,
    /// Grounding web citations.
    pub citations: Vec<SearchCitation>,
    /// Grounding search queries issued upstream.
    pub web_search_queries: Vec<String>,
    /// URL retrieval metadata entries when `urls` were requested.
    pub url_statuses: Vec<(String, String)>,
}

impl GoogleSearchOutput {
    /// Format answer + citations + search queries into Markdown output for tool responses.
    #[must_use]
    pub fn to_markdown(&self, max_results: Option<u32>) -> String {
        let mut out = self.answer.trim().to_owned();
        if !self.citations.is_empty() {
            out.push_str("\n\n### Sources\n");
            let limit = max_results.map_or(self.citations.len(), |m| m as usize);
            for (idx, cite) in self.citations.iter().take(limit).enumerate() {
                let label = if cite.title.trim().is_empty() {
                    cite.uri.as_str()
                } else {
                    cite.title.as_str()
                };
                let _ = writeln!(out, "{}. [{}]({})", idx + 1, label, cite.uri);
            }
        }
        if !self.url_statuses.is_empty() {
            out.push_str("\n### URL Context\n");
            for (url, status) in &self.url_statuses {
                let _ = writeln!(out, "- {url}: {status}");
            }
        }
        if !self.web_search_queries.is_empty() {
            out.push_str("\n_Queries searched:_ ");
            out.push_str(&self.web_search_queries.join(", "));
            out.push('\n');
        }
        out
    }
}

/// Native `oc-tools::Tool` exposing Antigravity's grounded `google_search` + `url_context`.
#[derive(Debug, Clone)]
pub struct GoogleSearchTool {
    service: AntigravityService,
}

impl GoogleSearchTool {
    /// Wrap an initialized [`AntigravityService`].
    #[must_use]
    pub const fn new(service: AntigravityService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Tool for GoogleSearchTool {
    fn name(&self) -> &'static str {
        "google_search"
    }

    fn description(&self) -> &'static str {
        "Search the live web using Google Search grounding and optionally analyze specific URLs via Gemini."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query or question to answer using Google Search grounding"
                },
                "urls": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional specific URLs to inspect alongside web search"
                },
                "thinking": {
                    "type": "boolean",
                    "description": "Set true for deeper multi-step reasoning during search"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: Value,
    ) -> std::result::Result<ToolResult, ToolError> {
        let parsed: GoogleSearchInput = serde_json::from_value(input)
            .map_err(|err| ToolError::new(format!("invalid google_search input: {err}")))?;
        if parsed.query.trim().is_empty() {
            return Err(ToolError::new(
                "google_search `query` must not be empty".to_owned(),
            ));
        }

        let output =
            execute_google_search(&self.service, &parsed.query, &parsed.urls, parsed.thinking)
                .await
                .map_err(|err| ToolError::new(err.to_string()))?;

        let max_results = self.service.config().web_search.max_results;
        let rendered = output.to_markdown(max_results);
        Ok(ToolResult::text(rendered).with_metadata(json!({
            "provider": ANTIGRAVITY_PROVIDER_ID,
            "citations": output.citations,
            "queries": output.web_search_queries,
        })))
    }
}

/// Execute a Cloud Code Assist `/v1internal:generateContent` grounded search request.
pub async fn execute_google_search(
    service: &AntigravityService,
    query: &str,
    urls: &[String],
    deep_thinking: Option<bool>,
) -> Result<GoogleSearchOutput> {
    let creds = service.acquire_any_token_for_control_plane().await?;
    let cfg = service.config();
    let deep = deep_thinking.unwrap_or(matches!(
        cfg.web_search.default_mode,
        crate::config::SearchMode::Deep
    ));
    let thinking_budget = if deep {
        SEARCH_THINKING_BUDGET_DEEP
    } else {
        SEARCH_THINKING_BUDGET_FAST
    };

    let mut prompt = query.trim().to_owned();
    if !urls.is_empty() {
        prompt.push_str("\n\nURLs to inspect:\n");
        for u in urls {
            let _ = writeln!(prompt, "- {u}");
        }
    }

    let mut tools = vec![json!({ "googleSearch": {} })];
    if !urls.is_empty() {
        tools.push(json!({ "urlContext": {} }));
    }

    let payload = json!({
        "project": creds.project_id,
        "model": SEARCH_MODEL,
        "request": {
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }]
            }],
            "systemInstruction": {
                "role": "user",
                "parts": [{ "text": SEARCH_SYSTEM_INSTRUCTION }]
            },
            "tools": tools,
            "generationConfig": {
                "thinkingConfig": {
                    "thinkingBudget": thinking_budget,
                    "includeThoughts": false
                }
            }
        },
        "requestType": "agent",
        "userAgent": "antigravity"
    });

    let hub_version = service.version().current();
    let url = format!(
        "{}/v1internal:generateContent",
        service.endpoints().antigravity_endpoint
    );
    let headers = headers_for_style(HeaderStyle::Antigravity, &hub_version);
    let mut req = service
        .http_client()
        .post(&url)
        .timeout(Duration::from_millis(SEARCH_TIMEOUT_MS))
        .bearer_auth(&creds.access_token)
        .header("Content-Type", "application/json")
        .json(&payload);
    for (k, v) in headers.to_pairs() {
        req = req.header(k, v);
    }

    let resp = req.send().await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AntigravityError::Upstream { status, body });
    }

    let body_json: Value = resp.json().await?;
    Ok(parse_grounding_response(&body_json))
}

fn parse_grounding_response(body: &Value) -> GoogleSearchOutput {
    let inner = body.get("response").unwrap_or(body);
    let candidate = inner
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first());

    let mut answer = String::new();
    if let Some(parts) = candidate
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if part
                .get("thought")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                answer.push_str(text);
            }
        }
    }

    let mut citations = Vec::new();
    let mut web_search_queries = Vec::new();
    if let Some(meta) = candidate.and_then(|c| c.get("groundingMetadata")) {
        if let Some(chunks) = meta.get("groundingChunks").and_then(Value::as_array) {
            for chunk in chunks {
                if let Some(web) = chunk.get("web")
                    && let Some(uri) = web.get("uri").and_then(Value::as_str)
                {
                    let title = web
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    citations.push(SearchCitation {
                        title,
                        uri: uri.to_owned(),
                    });
                }
            }
        }
        if let Some(queries) = meta.get("webSearchQueries").and_then(Value::as_array) {
            for q in queries {
                if let Some(s) = q.as_str() {
                    web_search_queries.push(s.to_owned());
                }
            }
        }
    }

    let mut url_statuses = Vec::new();
    if let Some(entries) = candidate
        .and_then(|c| c.get("urlContextMetadata"))
        .and_then(|m| m.get("urlMetadata"))
        .and_then(Value::as_array)
    {
        for entry in entries {
            if let (Some(u), Some(status)) = (
                entry.get("retrievedUrl").and_then(Value::as_str),
                entry.get("urlRetrievalStatus").and_then(Value::as_str),
            ) {
                url_statuses.push((u.to_owned(), status.to_owned()));
            }
        }
    }

    GoogleSearchOutput {
        answer,
        citations,
        web_search_queries,
        url_statuses,
    }
}
