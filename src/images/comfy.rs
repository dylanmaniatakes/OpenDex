//! ComfyUI's workflow library and installed node catalog.
use super::{
    Source, SourceKind, limited_body,
    sources::{MAX_OPTIONS, validate_workflow},
};
use crate::{providers, state::endpoint};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use zeroize::Zeroizing;

const MAX_TOOL_GRAPH: usize = 48 * 1024;
const MAX_CATALOG: usize = 16 * 1024 * 1024;

pub struct Library<'a> {
    client: &'a Client,
    source: &'a Source,
    base: Url,
    key: Option<Zeroizing<String>>,
}

impl<'a> Library<'a> {
    pub fn new(client: &'a Client, source: &'a Source) -> Result<Self> {
        ensure!(
            source.kind == SourceKind::ComfyUi,
            "Choose a saved ComfyUI connection."
        );
        Ok(Self {
            client,
            source,
            base: endpoint(&source.endpoint)?,
            key: source.secret()?.map(Zeroizing::new),
        })
    }

    pub async fn workflows(&self) -> Result<Vec<String>> {
        let mut url = self.route(&["userdata"])?;
        url.query_pairs_mut()
            .extend_pairs([("dir", "workflows"), ("recurse", "true")]);
        let response = self.request(self.client.get(url)).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        let paths: Vec<String> = serde_json::from_slice(
            &limited_body(providers::checked(response).await?, MAX_OPTIONS).await?,
        )
        .context("ComfyUI returned an invalid workflows list.")?;
        let mut paths = paths
            .into_iter()
            .filter_map(|path| workflow_path(&path).ok())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        paths.truncate(300);
        Ok(paths)
    }

    pub async fn read(&self, path: &str) -> Result<Value> {
        let path = workflow_path(path)?;
        let url = self.workflow_url(&path)?;
        self.json(self.client.get(url), MAX_OPTIONS).await
    }

    pub async fn node_classes(&self, query: &str) -> Result<Vec<String>> {
        let query = query.trim().to_lowercase();
        ensure!(query.len() <= 120, "Enter a shorter node search.");
        let url = self.route(&["object_info"])?;
        let body = self.json(self.client.get(url), MAX_CATALOG).await?;
        let catalog = body
            .as_object()
            .context("ComfyUI returned an invalid node catalog.")?;
        let mut matches = catalog
            .keys()
            .filter(|name| name.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_unstable();
        matches.truncate(80);
        Ok(matches)
    }

    pub async fn node(&self, name: &str) -> Result<Value> {
        ensure!(
            !name.is_empty()
                && name.len() <= 160
                && name != "."
                && name != ".."
                && !name
                    .chars()
                    .any(|ch| ch.is_control() || "/\\%".contains(ch)),
            "Enter an installed ComfyUI node class."
        );
        let url = self.route(&["object_info", name])?;
        let mut body = self.json(self.client.get(url), MAX_OPTIONS).await?;
        let schema = body
            .as_object_mut()
            .and_then(|nodes| nodes.remove(name))
            .context("This node class is not installed on ComfyUI.")?;
        Ok(schema)
    }

    /// Validate an authored graph before asking the user to save it.
    pub async fn validate_new_graph(&self, graph: &Map<String, Value>) -> Result<()> {
        validate_workflow(graph)?;
        ensure!(
            graph.len() <= 80 && serde_json::to_vec(graph)?.len() <= MAX_TOOL_GRAPH,
            "Use up to 80 nodes and 48 KiB for a workflow draft."
        );
        ensure!(
            graph.values().any(|node| node["class_type"] == "SaveImage"),
            "Include a SaveImage node to make the workflow usable from OpenDex."
        );
        let classes = graph
            .values()
            .filter_map(|node| node["class_type"].as_str())
            .collect::<BTreeSet<_>>();
        ensure!(classes.len() <= 32, "Use up to 32 distinct node classes.");
        let mut definitions = std::collections::HashMap::new();
        for class in classes {
            definitions.insert(
                class,
                self.node(class)
                    .await
                    .with_context(|| format!("Check the {class} node class."))?,
            );
        }
        for (id, node) in graph {
            ensure!(
                !id.is_empty() && id.len() <= 100 && !id.chars().any(char::is_control),
                "Invalid workflow node ID."
            );
            let class = node["class_type"].as_str().expect("validated workflow");
            let inputs = node["inputs"].as_object().expect("validated workflow");
            if let Some(required) = definitions[class]["input"]["required"].as_object() {
                for name in required.keys() {
                    ensure!(
                        inputs.contains_key(name),
                        "Node {id} ({class}) is missing required input {name}."
                    );
                }
            }
            for (name, value) in inputs {
                if let Some(link) = value.as_array()
                    && let [source, output] = link.as_slice()
                    && let (Some(source), Some(output)) = (source.as_str(), output.as_u64())
                {
                    let class = graph
                        .get(source)
                        .and_then(|node| node["class_type"].as_str())
                        .with_context(|| {
                            format!("Node {id} input {name} links to missing node {source}.")
                        })?;
                    ensure!(source != id, "Node {id} links to itself.");
                    if let Some(outputs) = definitions[class]["output"].as_array() {
                        ensure!(
                            output < outputs.len() as u64,
                            "Node {id} input {name} links to missing output {output}."
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn save_new(&self, path: &str, graph: &Map<String, Value>) -> Result<()> {
        let mut url = self.workflow_url(path)?;
        url.query_pairs_mut().append_pair("overwrite", "false");
        let response = self.request(self.client.post(url).json(graph)).await?;
        ensure!(
            response.status() != StatusCode::CONFLICT,
            "That workflow already exists. Choose a new filename."
        );
        providers::checked(response).await?;
        Ok(())
    }

    fn workflow_url(&self, path: &str) -> Result<Url> {
        // ComfyUI's userdata router takes the encoded file path as one segment.
        self.route(&["userdata", &format!("workflows/{}", workflow_path(path)?)])
    }

    fn route(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Invalid ComfyUI base URL."))?
            .pop_if_empty()
            .extend(segments.iter().copied());
        Ok(url)
    }

    async fn request(&self, request: RequestBuilder) -> Result<Response> {
        Ok(self
            .source
            .authorized(request, self.key.as_deref().map(String::as_str))?
            .send()
            .await
            .map_err(|error| error.without_url())?)
    }

    async fn json(&self, request: RequestBuilder, limit: usize) -> Result<Value> {
        let response = providers::checked(self.request(request).await?).await?;
        serde_json::from_slice(&limited_body(response, limit).await?)
            .context("ComfyUI returned invalid JSON.")
    }
}

pub fn workflow_path(path: &str) -> Result<String> {
    ensure!(
        path.len() <= 300
            && path.to_lowercase().ends_with(".json")
            && path.split('/').all(|part| !part.is_empty()
                && !part.starts_with('.')
                && !part
                    .chars()
                    .any(|ch| ch.is_control() || "\\%:#?<>|\"".contains(ch))),
        "Use a JSON filename relative to the ComfyUI workflows folder."
    );
    Ok(path.to_owned())
}

pub fn new_workflow_path(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(
        !name.is_empty() && !name.contains('/'),
        "Enter a single new workflow filename."
    );
    if name.to_lowercase().ends_with(".json") {
        workflow_path(name)
    } else {
        workflow_path(&format!("{name}.json"))
    }
}

pub fn api_graph(mut value: Value) -> Result<Map<String, Value>> {
    if value.get("prompt").is_some_and(Value::is_object) {
        value = value["prompt"].take();
    }
    let Value::Object(graph) = value else {
        bail!("Use a ComfyUI API-format workflow.");
    };
    ensure!(
        !graph.is_empty()
            && graph
                .values()
                .all(|node| { node["class_type"].is_string() && node["inputs"].is_object() }),
        "This is a ComfyUI editor workflow. Export it in API format to use it for generation."
    );
    Ok(graph)
}

pub fn with_workflow(source: Source, workflow: Value) -> Result<Source> {
    let mut source = source;
    source.options = api_graph(workflow)?;
    source.validate()?;
    Ok(source)
}

/// Keep tool output bounded and avoid relaying common credential fields in
/// existing workflows or schemas to the conversation's model provider.
pub fn model_view(mut value: Value) -> Result<String> {
    fn redact(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    let key = key.to_ascii_lowercase();
                    if [
                        "api_key",
                        "apikey",
                        "token",
                        "password",
                        "authorization",
                        "secret",
                        "cookie",
                    ]
                    .iter()
                    .any(|part| key.contains(part))
                    {
                        *item = json!("[redacted]");
                    } else {
                        redact(item);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(redact),
            Value::String(text) if text.len() > 4_000 => *text = "[long value omitted]".into(),
            _ => {}
        }
    }
    redact(&mut value);
    let json = serde_json::to_string(&value)?;
    ensure!(
        json.len() <= MAX_TOOL_GRAPH,
        "This ComfyUI result is too large for the model. Open it in Settings → Images."
    );
    Ok(json)
}
