use super::{Completion, Tool};
use crate::state::{Message, Provider, ProviderKind as Kind, Role, ToolCall};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value as V, json};
use std::collections::BTreeMap;

fn tag(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn function(tool: &Tool) -> V {
    json!({"name":tool.name,"description":tool.description,"parameters":tool.schema})
}

fn completion_message(message: &Message, ollama: bool) -> V {
    let mut item = json!({"role":tag(message.role),"content":message.content()});
    if let Some(file) = &message.attachment
        && let Some(mime) = &file.media_type
    {
        if ollama && mime.starts_with("image/") {
            item["images"] = json!([file.text]);
        } else if !ollama {
            let part = if mime == "application/pdf" {
                json!({"type":"file","file":{"filename":file.name,"file_data":file.data_url()}})
            } else {
                json!({"type":"image_url","image_url":{"url":file.data_url()}})
            };
            item["content"] = json!([{"type":"text","text":message.content()}, part]);
        }
    }
    if message.role == Role::Tool {
        let field = if ollama { "tool_name" } else { "tool_call_id" };
        item[field] = if ollama {
            json!(message.tool_name)
        } else {
            json!(message.tool_id)
        };
    }
    if !message.calls.is_empty() {
        item["tool_calls"] = json!(message.calls.iter().map(|call| {
            if ollama {
                json!({"function":{"name":call.name,"arguments":call.arguments}})
            } else {
                json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}})
            }
        }).collect::<Vec<_>>());
    }
    item
}

pub(super) fn payload(
    provider: &Provider,
    model: &str,
    system: &str,
    messages: &[Message],
    tools: &[Tool],
) -> Result<(&'static str, V)> {
    let limit = provider.token_limit;
    let ollama = matches!(provider.kind, Kind::Ollama | Kind::OllamaCloud);
    let (path, mut body) = match provider.kind {
        Kind::OpenAi => {
            let mut input = Vec::new();
            for message in messages {
                if !message.output_items.is_empty() {
                    input.extend(message.output_items.iter().cloned());
                    continue;
                }
                if message.role == Role::Tool {
                    input.push(json!({"type":"function_call_output","call_id":message.tool_id,"output":message.text}));
                    continue;
                }
                if message.has_content() {
                    let mut content = json!(message.content());
                    if let Some(file) = &message.attachment
                        && let Some(mime) = &file.media_type
                    {
                        let part = if mime == "application/pdf" {
                            json!({"type":"input_file","filename":file.name,"file_data":file.data_url()})
                        } else {
                            json!({"type":"input_image","detail":"auto","image_url":file.data_url()})
                        };
                        content = json!([{"type":"input_text","text":message.content()},part]);
                    }
                    input.push(json!({"role":tag(message.role),"content":content}));
                }
                for call in &message.calls {
                    input.push(json!({"type":"function_call","call_id":call.id,"name":call.name,"arguments":call.arguments.to_string()}));
                }
            }
            let mut body = json!({"model":model,"input":input,"stream":true,"store":false,"include":["reasoning.encrypted_content"]});
            if !system.is_empty() {
                body["instructions"] = json!(system);
            }
            if let Some(limit) = limit {
                body["max_output_tokens"] = json!(limit);
            }
            if !tools.is_empty() {
                body["tools"] = json!(
                    tools
                        .iter()
                        .map(|tool| {
                            let mut spec = function(tool);
                            spec["type"] = json!("function");
                            spec["strict"] = json!(false);
                            spec
                        })
                        .collect::<Vec<_>>()
                );
            }
            ("responses", body)
        }
        Kind::Anthropic => {
            let mut input: Vec<(String, Vec<V>)> = Vec::new();
            for message in messages {
                let role = if message.role == Role::Tool {
                    "user"
                } else {
                    tag(message.role)
                };
                if input.last().is_none_or(|(prev, _)| prev != role) {
                    input.push((role.into(), vec![]));
                }
                let blocks = &mut input.last_mut().expect("inserted role").1;
                if message.role == Role::Tool {
                    blocks.push(json!({"type":"tool_result","tool_use_id":message.tool_id,"content":message.text}));
                    continue;
                }
                if message.has_content() {
                    blocks.push(json!({"type":"text","text":message.content()}));
                }
                if let Some(file) = &message.attachment
                    && let Some(mime) = &file.media_type
                {
                    let kind = if mime == "application/pdf" {
                        "document"
                    } else {
                        "image"
                    };
                    blocks.push(json!({"type":kind,"source":{"type":"base64","media_type":mime,"data":file.text}}));
                }
                for call in &message.calls {
                    blocks.push(json!({"type":"tool_use","id":call.id,"name":call.name,"input":call.arguments}));
                }
            }
            let input: Vec<_> = input
                .into_iter()
                .filter(|(_, blocks)| !blocks.is_empty())
                .map(|(role, content)| json!({"role":role,"content":content}))
                .collect();
            let mut body = json!({"model":model,"messages":input,"max_tokens":limit.unwrap_or(8192),"stream":true});
            if !system.is_empty() {
                body["system"] = json!(system);
            }
            if !tools.is_empty() {
                body["tools"] = json!(tools.iter().map(|tool| {
                    json!({"name":tool.name,"description":tool.description,"input_schema":tool.schema})
                }).collect::<Vec<_>>());
            }
            ("messages", body)
        }
        Kind::OpenAiCompatible | Kind::Ollama | Kind::OllamaCloud => {
            let mut input = Vec::new();
            if !system.is_empty() {
                input.push(json!({"role":"system","content":system}));
            }
            input.extend(
                messages
                    .iter()
                    .map(|message| completion_message(message, ollama)),
            );
            let mut body = json!({"model":model,"messages":input,"stream":true});
            if ollama && let Some(think) = provider.ollama_think {
                body["think"] = json!(think);
            }
            if let Some(limit) = limit {
                if ollama {
                    body["options"] = json!({"num_predict":limit});
                } else {
                    body["max_tokens"] = json!(limit);
                }
            }
            if !tools.is_empty() {
                body["tools"] = json!(
                    tools
                        .iter()
                        .map(|tool| json!({"type":"function","function":function(tool)}))
                        .collect::<Vec<_>>()
                );
            }
            (if ollama { "chat" } else { "chat/completions" }, body)
        }
        Kind::Gemini => bail!("Use the native Gemini endpoint."),
        Kind::ChatGptAccount => bail!("Use the ChatGPT account connection."),
    };
    // A missing optional argument is represented by omitting its field in each format above.
    debug_assert!(body.as_object_mut().is_some());
    Ok((path, body))
}

#[derive(Default)]
struct Pending {
    id: String,
    name: String,
    arguments: String,
    object: Option<V>,
}

pub(super) struct Decoder {
    kind: Kind,
    text: String,
    calls: BTreeMap<u64, Pending>,
    output_items: Vec<V>,
    done: bool,
}

fn string(value: &V) -> &str {
    value.as_str().unwrap_or_default()
}

impl Decoder {
    pub(super) fn new(kind: Kind) -> Self {
        Self {
            kind,
            text: String::new(),
            calls: BTreeMap::new(),
            output_items: vec![],
            done: false,
        }
    }

    pub(super) fn finish(&mut self) {
        self.done = true;
    }
    pub(super) fn finished(&self) -> bool {
        self.done
    }

    fn push(&mut self, value: &V, delta: &mut impl FnMut(&str)) -> Result<()> {
        let text = string(value);
        if !text.is_empty() {
            ensure!(
                self.text.len() + text.len() <= 4 * 1024 * 1024,
                "The response exceeds OpenDex's size limit."
            );
            self.text.push_str(text);
            delta(text);
        }
        Ok(())
    }

    pub(super) fn event(&mut self, part: &V, delta: &mut impl FnMut(&str)) -> Result<()> {
        if !part["error"].is_null() || part["type"] == "error" {
            let error = part["error"]["message"]
                .as_str()
                .or(part["error"].as_str())
                .unwrap_or("The provider reported a stream error.");
            bail!("{error}");
        }
        match self.kind {
            Kind::OpenAi => match string(&part["type"]) {
                "response.output_text.delta" | "response.refusal.delta" => {
                    self.push(&part["delta"], delta)?
                }
                "response.output_item.done" if part["item"]["type"] == "function_call" => {
                    let call = &part["item"];
                    self.calls.insert(
                        part["output_index"].as_u64().unwrap_or(0),
                        Pending {
                            id: string(&call["call_id"]).into(),
                            name: string(&call["name"]).into(),
                            arguments: string(&call["arguments"]).into(),
                            object: None,
                        },
                    );
                }
                "response.completed" => {
                    if let Some(items) = part["response"]["output"].as_array() {
                        self.output_items.clone_from(items);
                    }
                    self.finish();
                }
                "response.incomplete" => bail!(
                    "OpenAI returned an incomplete response ({})",
                    part["response"]["incomplete_details"]["reason"]
                        .as_str()
                        .unwrap_or("unknown reason")
                ),
                "response.failed" => bail!(
                    "{}",
                    part["response"]["error"]["message"]
                        .as_str()
                        .unwrap_or("OpenAI reported a failed response.")
                ),
                _ => {}
            },
            Kind::Anthropic => match string(&part["type"]) {
                "content_block_start" => {
                    let block = &part["content_block"];
                    if block["type"] == "tool_use" {
                        self.calls.insert(
                            part["index"].as_u64().context("Missing tool index.")?,
                            Pending {
                                id: string(&block["id"]).into(),
                                name: string(&block["name"]).into(),
                                object: block.get("input").cloned(),
                                arguments: String::new(),
                            },
                        );
                    } else if block["type"] == "text" {
                        self.push(&block["text"], delta)?;
                    }
                }
                "content_block_delta" => {
                    let value = &part["delta"];
                    if value["type"] == "text_delta" {
                        self.push(&value["text"], delta)?;
                    }
                    if value["type"] == "input_json_delta" {
                        let index = part["index"].as_u64().context("Missing tool index.")?;
                        let call = self
                            .calls
                            .get_mut(&index)
                            .context("Tool delta arrived before its header.")?;
                        call.arguments.push_str(string(&value["partial_json"]));
                    }
                }
                "message_delta" if part["delta"]["stop_reason"] == "max_tokens" => {
                    bail!("Claude reached the output limit. Increase it in provider settings.")
                }
                "message_stop" => self.finish(),
                _ => {}
            },
            Kind::Ollama | Kind::OllamaCloud => {
                self.push(&part["message"]["content"], delta)?;
                if let Some(calls) = part["message"]["tool_calls"].as_array() {
                    for call in calls {
                        let function = &call["function"];
                        let index = self.calls.last_key_value().map_or(0, |(last, _)| last + 1);
                        self.calls.insert(
                            index,
                            Pending {
                                name: string(&function["name"]).into(),
                                object: function.get("arguments").cloned(),
                                ..Pending::default()
                            },
                        );
                    }
                }
                if part["done"] == true {
                    self.finish();
                }
            }
            Kind::OpenAiCompatible => {
                let choice = &part["choices"][0];
                self.push(&choice["delta"]["content"], delta)?;
                self.push(&choice["delta"]["refusal"], delta)?;
                if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
                    for call in calls {
                        let pending = self
                            .calls
                            .entry(call["index"].as_u64().unwrap_or(0))
                            .or_default();
                        pending.id.push_str(string(&call["id"]));
                        pending.name.push_str(string(&call["function"]["name"]));
                        pending
                            .arguments
                            .push_str(string(&call["function"]["arguments"]));
                    }
                }
                if choice["finish_reason"] == "length" {
                    bail!("The model reached the output limit.");
                }
                if choice["finish_reason"].is_string() {
                    self.finish();
                }
            }
            Kind::Gemini => bail!("Use the native Gemini stream."),
            Kind::ChatGptAccount => bail!("Unsupported account stream."),
        }
        ensure!(
            self.calls.len() <= 64
                && self
                    .calls
                    .values()
                    .all(|call| call.arguments.len() < 1024 * 1024),
            "The model sent too many tool calls or oversized arguments."
        );
        Ok(())
    }

    pub(super) fn complete(self) -> Result<Completion> {
        ensure!(self.done, "The model stream closed before its final event.");
        let mut calls = Vec::new();
        for (_, call) in self.calls {
            ensure!(
                !call.name.is_empty(),
                "The model returned a tool call without a name."
            );
            let arguments = if call.arguments.is_empty() {
                call.object.unwrap_or_else(|| json!({}))
            } else {
                serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| json!({"_invalid_arguments":call.arguments}))
            };
            let id = if call.id.is_empty() {
                format!("call_{}", uuid::Uuid::new_v4().simple())
            } else {
                call.id
            };
            calls.push(ToolCall {
                id,
                name: call.name,
                arguments,
            });
        }
        ensure!(
            !self.text.is_empty() || !calls.is_empty(),
            "The model returned an empty reply."
        );
        Ok(Completion {
            text: self.text,
            calls,
            output_items: self.output_items,
        })
    }
}

pub(super) fn whole(kind: Kind, body: &V) -> Result<Completion> {
    let mut decoder = Decoder::new(kind);
    let mut ignore = |_: &str| {};
    match kind {
        Kind::OpenAi => {
            decoder.output_items = body["output"].as_array().cloned().unwrap_or_default();
            if body["status"] != "completed" {
                bail!("OpenAI returned an incomplete response.");
            }
            for (index, item) in body["output"]
                .as_array()
                .context("Missing response output.")?
                .iter()
                .enumerate()
            {
                decoder.event(
                    &json!({"type":"response.output_item.done","output_index":index,"item":item}),
                    &mut ignore,
                )?;
                if let Some(content) = item["content"].as_array() {
                    for block in content {
                        decoder
                            .push(block.get("text").unwrap_or(&block["refusal"]), &mut ignore)?;
                    }
                }
            }
        }
        Kind::Anthropic => {
            for (index, block) in body["content"]
                .as_array()
                .context("Missing Claude response content.")?
                .iter()
                .enumerate()
            {
                decoder.event(
                    &json!({"type":"content_block_start","index":index,"content_block":block}),
                    &mut ignore,
                )?;
            }
        }
        Kind::OpenAiCompatible => {
            let message = &body["choices"][0]["message"];
            let mut item = message.clone();
            if let Some(calls) = item.get_mut("tool_calls").and_then(V::as_array_mut) {
                for (index, call) in calls.iter_mut().enumerate() {
                    call["index"] = json!(index);
                }
            }
            decoder.event(&json!({"choices":[{"delta":item,"finish_reason":body["choices"][0]["finish_reason"]}]}), &mut ignore)?;
        }
        _ => bail!("Unexpected provider response."),
    }
    decoder.finish();
    decoder.complete()
}
