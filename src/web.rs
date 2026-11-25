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
    Searxng {
        endpoint: String,
    },
    Ollama {
        provider: Uuid,
    },
    /// API-key services. The key lives in the system credential store under
    /// `id`, never in workspace.json.
    Brave {
        id: Uuid,
    },
    Tavily {
        id: Uuid,
    },
}

impl Config {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Searxng { .. } => "SearXNG",
            Self::Ollama { .. } => "Ollama Cloud",
            Self::Brave { .. } => "Brave Search",
            Self::Tavily { .. } => "Tavily",
        }
    }

    /// Configuration id used for the credential-store entry, when the service
    /// needs a key.
    pub fn secret_id(&self) -> Option<Uuid> {
        match self {
            Self::Brave { id } | Self::Tavily { id } => Some(*id),
            _ => None,
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
            Self::Brave { id } => Ok(Service::Brave {
                key: search_key(*id)?,
            }),
            Self::Tavily { id } => Ok(Service::Tavily {
                key: search_key(*id)?,
            }),
        }
    }
}

const BRAVE_SEARCH: &str = "https://api.search.brave.com/res/v1/web/search";
const TAVILY_SEARCH: &str = "https://api.tavily.com/search";

fn search_key(id: Uuid) -> Result<String> {
    crate::state::read_secret(id, "web-search", true)?
        .context("Save an API key for this search service in Settings.")
}

pub enum Service {
    Searxng(Url),
    Ollama(Provider),
    Brave { key: String },
    Tavily { key: String },
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
            Self::Brave { .. } => "Brave Search".into(),
            Self::Tavily { .. } => "Tavily".into(),
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
                let sources = self.search(client, &query, limit).await?;
                Ok(Output {
                    query: Some(query),
                    sources,
                })
            }
            Action::Fetch { url } => {
                let Self::Ollama(provider) = self else {
                    bail!("This search service does not support fetching pages.");
                };
                let request =
                    ollama_request(client, provider, FETCH, json!({"url":url.as_str()})).await?;
                let response = response(request, false).await?;
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

impl Service {
    async fn search(&self, client: &Client, query: &str, limit: u64) -> Result<Vec<Source>> {
        match self {
            Self::Searxng(url) => {
                let request = client
                    .get(url.clone())
                    .query(&[("q", query), ("format", "json")]);
                let response = response(request, true).await?;
                let results = response["results"]
                    .as_array()
                    .context("The search service did not return a results array.")?;
                Ok(collect(results, "content", limit))
            }
            Self::Ollama(provider) => {
                let request = ollama_request(
                    client,
                    provider,
                    SEARCH,
                    json!({"query":query,"max_results":limit}),
                )
                .await?;
                let response = response(request, false).await?;
                let results = response["results"]
                    .as_array()
                    .context("The search service did not return a results array.")?;
                Ok(collect(results, "content", limit))
            }
            Self::Brave { key } => {
                let count = limit.to_string();
                let request = client
                    .get(BRAVE_SEARCH)
                    .query(&[("q", query), ("count", count.as_str())])
                    .header("X-Subscription-Token", key)
                    .header("Accept", "application/json");
                let response = response(request, false)
                    .await
                    .context("Brave Search did not respond. Check the API key and plan.")?;
                let results = response["web"]["results"]
                    .as_array()
                    .context("Brave Search did not return web.results.")?;
                Ok(collect(results, "description", limit))
            }
            Self::Tavily { key } => {
                let request = client
                    .post(TAVILY_SEARCH)
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
                    .json(&json!({"query": query, "max_results": limit, "include_answer": false}));
                let response = response(request, false)
                    .await
                    .context("Tavily did not respond. Check the API key and plan.")?;
                let results = response["results"]
                    .as_array()
                    .context("Tavily did not return a results array.")?;
                Ok(collect(results, "content", limit))
            }
        }
    }
}

/// Map a provider-specific results array to sources, keeping the first
/// occurrence of each URL and reading the text from `content_key`.
fn collect(items: &[Value], content_key: &str, limit: u64) -> Vec<Source> {
    let mut urls = HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            let url = http_url(item["url"].as_str()?).ok()?.to_string();
            if !urls.insert(url.clone()) {
                return None;
            }
            let title = item["title"].as_str().unwrap_or_default().trim();
            let content = item[content_key].as_str().unwrap_or_default().trim();
            Some(Source {
                title: if title.is_empty() {
                    url.clone()
                } else {
                    title.chars().take(250).collect()
                },
                url,
                content: content.chars().take(1_600).collect(),
            })
        })
        .take(limit as usize)
        .collect()
}

async fn ollama_request(
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
    providers::request(client, provider, client.post(url).json(&payload)).await
}

async fn response(request: RequestBuilder, searxng: bool) -> Result<Value> {
    let mut response = request
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| error.without_url())
        .context("Could not reach the web service.")?;
    if searxng && response.status() == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "SearXNG returned HTTP 403. Check that JSON is enabled in search.formats and that the server allows access from this computer."
        );
    }
    if searxng {
        match response.status() {
            reqwest::StatusCode::UNAUTHORIZED => bail!(
                "SearXNG returned HTTP 401. The instance or reverse proxy requires authentication."
            ),
            reqwest::StatusCode::TOO_MANY_REQUESTS => bail!(
                "SearXNG returned HTTP 429. The instance rate-limited this computer. Retry later or check its limiter settings."
            ),
            status if status.is_redirection() => {
                let location = response
                    .headers()
                    .get(&reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let hint = if location.is_empty() {
                    String::new()
                } else {
                    format!(" The server sent Location: {location}.")
                };
                bail!(
                    "SearXNG redirected the request (HTTP {status}). Enter the final search URL, including its reverse-proxy path.{hint}"
                );
            }
            _ => {}
        }
    }
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
    serde_json::from_slice(&bytes).context(if searxng {
        "SearXNG did not return JSON. Check the search URL, enable json in search.formats, and check whether the reverse proxy shows a login page."
    } else {
        "The web service response was not JSON."
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[tokio::test]
    async fn searxng_errors_explain_instance_and_proxy_failures() {
        for (status, body, expected, location) in [
            ("401 Unauthorized", "", "requires authentication", ""),
            ("403 Forbidden", "", "search.formats", ""),
            ("429 Too Many Requests", "", "rate-limited", ""),
            (
                "302 Found",
                "",
                "final search URL",
                "/search?q=OpenDex&format=json",
            ),
            ("200 OK", "<html>Sign in</html>", "did not return JSON", ""),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/search", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&stream);
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let headers = if location.is_empty() {
                    String::new()
                } else {
                    format!("Location: {location}\r\n")
                };
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let client = providers::client().unwrap();
            let error = response(client.get(url), true).await.unwrap_err();
            let message = error.to_string();
            assert!(message.contains(expected), "{message:#}");
            if !location.is_empty() {
                assert!(message.contains(location), "{message:#}");
            }
            server.join().unwrap();
        }
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    #[test]
    fn brave_results_use_the_description_field() {
        let payload: Value = serde_json::from_str(
            r#"{"web":{"results":[
            {"title":"Rust","url":"https://www.rust-lang.org/","description":"A language."},
            {"title":"Rust","url":"https://www.rust-lang.org/","description":"Duplicate."},
            {"title":"Docs","url":"https://doc.rust-lang.org/","description":"Reference."}
        ]}}"#,
        )
        .unwrap();
        let results = collect(
            payload["web"]["results"].as_array().unwrap(),
            "description",
            5,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "A language.");
    }

    #[test]
    fn tavily_results_use_the_content_field() {
        let payload: Value = serde_json::from_str(
            r#"{"results":[
            {"title":"OpenDex","url":"https://example.org/","content":"Desktop app."}
        ]}"#,
        )
        .unwrap();
        let results = collect(payload["results"].as_array().unwrap(), "content", 5);
        assert_eq!(results[0].title, "OpenDex");
        assert_eq!(results[0].content, "Desktop app.");
    }

    #[test]
    fn non_http_urls_are_dropped() {
        let payload: Value = serde_json::from_str(
            r#"{"results":[
            {"title":"Local","url":"file:///etc/passwd","content":"x"}
        ]}"#,
        )
        .unwrap();
        assert!(collect(payload["results"].as_array().unwrap(), "content", 5).is_empty());
    }
}
