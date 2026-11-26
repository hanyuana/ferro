//! Anthropic Messages API 原生协议（含 Prompt Caching 与 Extended Thinking）

use super::sse::stream_events;
use super::*;
use serde_json::json;

pub struct AnthropicProvider {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    /// >0 时开启 extended thinking
    pub thinking_budget: u32,
}

impl AnthropicProvider {
    fn build_messages(&self, messages: &[Message]) -> Vec<Value> {
        let thinking = self.thinking_budget > 0;
        let mut out: Vec<Value> = Vec::with_capacity(messages.len());
        for m in messages {
            let mut blocks = Vec::new();
            for b in &m.content {
                match b {
                    Block::Text { text } if !text.trim().is_empty() => {
                        blocks.push(json!({ "type": "text", "text": text }))
                    }
                    Block::Image { media_type, data } => {
                        blocks.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        }))
                    }
                    Block::Reasoning { text, signature: Some(sig) } if thinking => {
                        blocks.push(json!({ "type": "thinking", "thinking": text, "signature": sig }))
                    }
                    Block::ToolUse { id, name, input } => {
                        blocks.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }))
                    }
                    Block::ToolResult { tool_use_id, content, is_error } => blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": if content.is_empty() { "(no output)" } else { content.as_str() },
                        "is_error": is_error,
                    })),
                    _ => {}
                }
            }
            if blocks.is_empty() {
                blocks.push(json!({ "type": "text", "text": "(empty)" }));
            }
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            out.push(json!({ "role": role, "content": blocks }));
        }
        // 在最后一条消息的最后一个块上打缓存断点，实现增量 Prompt Caching
        if let Some(last) = out.last_mut() {
            if let Some(arr) = last["content"].as_array_mut() {
                if let Some(b) = arr.iter_mut().rev().find(|b| b["type"] != "thinking") {
                    b["cache_control"] = json!({ "type": "ephemeral" });
                }
            }
        }
        out
    }
}

enum Building {
    Text,
    Thinking { text: String, sig: String },
    Tool { id: String, name: String, json: String },
    Ignore,
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(&self, req: ChatRequest<'_>, on_delta: DeltaSink<'_>) -> Result<ChatResponse> {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema }))
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        let mut max_tokens = self.max_tokens;
        let mut body = json!({
            "model": req.model,
            "system": [{ "type": "text", "text": req.system, "cache_control": { "type": "ephemeral" } }],
            "messages": self.build_messages(req.messages),
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        if self.thinking_budget > 0 {
            max_tokens = max_tokens.max(self.thinking_budget + 4096);
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": self.thinking_budget });
        }
        body["max_tokens"] = json!(max_tokens);

        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let mut rb = self
            .client
            .post(url)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.header("x-api-key", key);
        }

        let mut asm = Assembler::default();
        let mut building: Vec<Building> = Vec::new();
        let mut usage = Usage::default();
        let mut stop_reason = String::new();

        stream_events(rb, |ev| {
            let v: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            match v["type"].as_str().unwrap_or_default() {
                "message_start" => {
                    let u = &v["message"]["usage"];
                    usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    usage.output = u["output_tokens"].as_u64().unwrap_or(0);
                }
                "content_block_start" => {
                    let cb = &v["content_block"];
                    let b = match cb["type"].as_str().unwrap_or_default() {
                        "text" => Building::Text,
                        "thinking" => Building::Thinking { text: String::new(), sig: String::new() },
                        "tool_use" => {
                            let name = cb["name"].as_str().unwrap_or_default().to_string();
                            Building::Tool {
                                id: cb["id"].as_str().unwrap_or_default().to_string(),
                                name,
                                json: String::new(),
                            }
                        }
                        _ => Building::Ignore,
                    };
                    building.push(b);
                }
                "content_block_delta" => {
                    let d = &v["delta"];
                    let Some(cur) = building.last_mut() else { return Ok(()) };
                    match (d["type"].as_str().unwrap_or_default(), cur) {
                        ("text_delta", Building::Text) => {
                            let t = d["text"].as_str().unwrap_or_default();
                            asm.push_text(t);
                            on_delta(Delta::Text(t.to_string()));
                        }
                        ("thinking_delta", Building::Thinking { text, .. }) => {
                            let t = d["thinking"].as_str().unwrap_or_default();
                            text.push_str(t);
                            on_delta(Delta::Reasoning(t.to_string()));
                        }
                        ("signature_delta", Building::Thinking { sig, .. }) => {
                            sig.push_str(d["signature"].as_str().unwrap_or_default());
                        }
                        ("input_json_delta", Building::Tool { json, .. }) => {
                            json.push_str(d["partial_json"].as_str().unwrap_or_default());
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => match building.pop() {
                    Some(Building::Thinking { text, sig }) => asm.blocks.push(Block::Reasoning {
                        text,
                        signature: if sig.is_empty() { None } else { Some(sig) },
                    }),
                    Some(Building::Tool { id, name, json }) => {
                        asm.blocks.push(Block::ToolUse { id, name, input: parse_tool_args(&json) })
                    }
                    Some(Building::Text) => {
                        // 文本已增量写入；插入分隔防止后续文本块被合并
                        if let Some(Block::Text { .. }) = asm.blocks.last() {
                            asm.blocks.push(Block::Text { text: String::new() });
                        }
                    }
                    _ => {}
                },
                "message_delta" => {
                    if let Some(s) = v["delta"]["stop_reason"].as_str() {
                        stop_reason = s.to_string();
                    }
                    if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                        usage.output = o;
                    }
                }
                "error" => anyhow::bail!("Anthropic 返回错误: {}", v["error"]),
                _ => {}
            }
            Ok(())
        })
        .await?;

        asm.blocks.retain(|b| !matches!(b, Block::Text { text } if text.is_empty()));
        let stop = match stop_reason.as_str() {
            "tool_use" => StopReason::ToolUse,
            "max_tokens" => StopReason::MaxTokens,
            "end_turn" | "stop_sequence" | "" => StopReason::EndTurn,
            _ => StopReason::Other,
        };
        Ok(ChatResponse {
            message: Message { role: Role::Assistant, content: asm.blocks },
            usage,
            stop,
        })
    }
}
