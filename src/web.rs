//! Web tools use the configured search service, independently of the model endpoint.

use crate::{
    providers::{self, Tool},
    state::{Provider, ProviderKind, endpoint},
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};
use uuid::Uuid;

pub const SEARCH: &str = "web_search";
pub const FETCH: &str = "web_fetch";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULTS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Config {
    Searxng { endpoint: String },
    Ollama { provider: Uuid },
}

impl Config {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Searxng { .. } => "SearXNG",
            Self::Ollama { .. } => "Ollama Cloud",
        }
    }

    pub fn resolve<'a>(
        &self,
        providers: impl IntoIterator<Item = &'a Provider>,
    ) -> Result<Service> {
        match self {
            Self::Searxng { endpoint: address } => Ok(Service::Searxng(endpoint(address.trim())?)),
            Self::Ollama { provider: id } => {
                let provider = providers
                    .into_iter()
                    .find(|item| item.id == *id && item.kind == ProviderKind::OllamaCloud)
                    .context("Choose a saved Ollama Cloud connection.")?;
                endpoint(&provider.endpoint)?;
                Ok(Service::Ollama(provider.clone()))
            }
        }
    }
}

pub enum Service {
    Searxng(Url),
    Ollama(Provider),
}

#[derive(Debug)]
pub enum Action {
    Search { query: String, limit: u64 },
    Fetch { url: Url },
}

impl Action {
    pub fn parse(name: &str, arguments: &Value) -> Result<Self> {
        match name {
            SEARCH => {
                let query = arguments["query"]
                    .as_str()
                    .context("Enter a web search query.")?
                    .trim();
                ensure!(
                    !query.is_empty()
                        && query.len() <= 1_024
                        && !query.chars().any(char::is_control),
                    "Use a single-line web search query up to 1024 bytes."
                );
                let limit = arguments
                    .get("max_results")
                    .map_or(Some(5), Value::as_u64)
                    .filter(|limit| (1..=MAX_RESULTS).contains(limit))
                    .context("Request between one and ten search results.")?;
                Ok(Self::Search {
                    query: query.into(),
                    limit,
                })
            }
            FETCH => {
                let url = arguments["url"].as_str().context("Enter a page URL.")?;
                Ok(Self::Fetch {
                    url: http_url(url)?,
                })
            }
            _ => bail!("Unknown web tool."),
        }
    }
}

pub fn http_url(raw: &str) -> Result<Url> {
    ensure!(raw.len() <= 2_048, "The URL is too long.");
    let url = Url::parse(raw).context("Enter a valid source URL.")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "Use an HTTP or HTTPS URL without credentials."
    );
    Ok(url)
}

#[derive(Debug, Serialize)]
pub struct Source {
    pub title: String,
    pub url: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct Output {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub sources: Vec<Source>,
}

impl Service {
    pub fn label(&self) -> String {
        match self {
            Self::Searxng(url) => format!("SearXNG · {}", url.host_str().unwrap_or_default()),
            Self::Ollama(provider) => format!("{} · {}", provider.name, provider.endpoint),
        }
    }

    pub fn tools(&self) -> Vec<Tool> {
        let mut tools = vec![Tool {
            name: SEARCH.into(),
            description: "Search the web for public information. Send a concise query and cite original result URLs in your answer.".into(),
            schema: json!({"type":"object","properties":{
                "query":{"type":"string"},
                "max_results":{"type":"integer","minimum":1,"maximum":10}
            },"required":["query"]}),
        }];
        if matches!(self, Self::Ollama(_)) {
            tools.push(Tool {
                name: FETCH.into(),
                description: "Read the main text of an HTTP or HTTPS page through the configured Ollama web service.".into(),
                schema: json!({"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}),
            });
        }
        tools
    }

    pub async fn execute(&self, client: &Client, action: Action) -> Result<Output> {
        match action {
            Action::Search { query, limit } => {
                let request = match self {
                    Self::Searxng(url) => client
                        .get(url.clone())
                        .query(&[("q", query.as_str()), ("format", "json")]),
                    Self::Ollama(provider) => ollama_request(
                        client,
                        provider,
                        SEARCH,
                        json!({"query":query,"max_results":limit}),
                    )?,
                };
                let response = response(request).await?;
                let results = response["results"]
                    .as_array()
                    .context("The search service did not return a results array.")?;
                let mut urls = HashSet::new();
                let sources = results
                    .iter()
                    .filter_map(|item| {
                        let url = http_url(item["url"].as_str()?).ok()?.to_string();
                        if !urls.insert(url.clone()) {
                            return None;
                        }
                        Some(source(item, url, 1_600))
                    })
                    .take(limit as usize)
                    .collect();
                Ok(Output {
                    query: Some(query),
                    sources,
                })
            }
            Action::Fetch { url } => {
                let Self::Ollama(provider) = self else {
                    bail!("This search service does not support fetching pages.");
                };
                let request = ollama_request(client, provider, FETCH, json!({"url":url.as_str()}))?;
                let response = response(request).await?;
                ensure!(
                    response["content"].is_string(),
                    "The web service did not return page text."
                );
                Ok(Output {
                    query: None,
                    sources: vec![source(&response, url.to_string(), 12_000)],
                })
            }
        }
    }
}

fn ollama_request(
    client: &Client,
    provider: &Provider,
    path: &str,
    payload: Value,
) -> Result<RequestBuilder> {
    let mut url = endpoint(&provider.endpoint)?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Ollama API endpoint."))?
        .pop_if_empty()
        .push(path);
    providers::request(provider, client.post(url).json(&payload))
}

async fn response(request: RequestBuilder) -> Result<Value> {
    let mut response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| error.without_url())
        .context("Could not reach the web service.")?;
    ensure!(
        response.status().is_success(),
        "The web service returned HTTP {}.",
        response.status()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url())?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "The web service response is too large."
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("The web service response was not JSON.")
}

fn source(item: &Value, url: String, limit: usize) -> Source {
    let title = item["title"].as_str().unwrap_or_default().trim();
    let content = item["content"].as_str().unwrap_or_default().trim();
    Source {
        title: if title.is_empty() {
            url.clone()
        } else {
            title.chars().take(250).collect()
        },
        url,
        content: content.chars().take(limit).collect(),
    }
}
