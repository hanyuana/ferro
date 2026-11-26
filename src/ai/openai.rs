//! OpenAI 兼容协议（OpenAI / DeepSeek / Qwen / Moonshot / OpenRouter / Ollama / vLLM ...）

use super::sse::stream_events;
use super::*;
use serde_json::{json, Map};

pub struct OpenAiProvider {
    pub client: reqwest::Client,
    pub base_url: String,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    /// 是否在历史消息中回传 reasoning_content（DeepSeek 思考模式 + 工具调用时需要）
    pub reasoning_passback: bool,
}

impl OpenAiProvider {
    fn build_messages(&self, system: &str, messages: &[Message]) -> Vec<Value> {
        let mut out = vec![json!({ "role": "system", "content": system })];
        for m in messages {
            match m.role {
                Role::User => {
                    let mut text = String::new();
                    let mut images: Vec<(String, String)> = Vec::new();
                    for b in &m.content {
                        match b {
                            Block::ToolResult { tool_use_id, content, .. } => out.push(json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content,
                            })),
                            Block::Text { text: t } => {
                                if !text.is_empty() {
                                    text.push_str("\n\n");
                                }
                                text.push_str(t);
                            }
                            Block::Image { media_type, data } => {
                                images.push((media_type.clone(), data.clone()));
                            }
                            _ => {}
                        }
                    }
                    if images.is_empty() {
                        if !text.is_empty() {
                            out.push(json!({ "role": "user", "content": text }));
                        }
                    } else {
                        let mut parts = Vec::new();
                        if !text.is_empty() {
                            parts.push(json!({ "type": "text", "text": text }));
                        }
                        for (mt, b64) in images {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{mt};base64,{b64}") }
                            }));
                        }
                        out.push(json!({ "role": "user", "content": parts }));
                    }
                }
                Role::Assistant => {
                    let mut text = String::new();
                    let mut reasoning = String::new();
                    let mut calls = Vec::new();
                    for b in &m.content {
                        match b {
                            Block::Text { text: t } => text.push_str(t),
                            Block::Reasoning { text: t, .. } => reasoning.push_str(t),
                            Block::ToolUse { id, name, input } => calls.push(json!({
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": input.to_string() },
                            })),
                            _ => {}
                        }
                    }
                    let mut msg = Map::new();
                    msg.insert("role".into(), json!("assistant"));
                    msg.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
                    if !calls.is_empty() {
                        msg.insert("tool_calls".into(), json!(calls));
                    }
                    if self.reasoning_passback && !reasoning.is_empty() {
                        msg.insert("reasoning_content".into(), json!(reasoning));
                    }
                    out.push(Value::Object(msg));
                }
            }
        }
        out
    }
}

struct PendingCall {
    id: String,
    name: String,
    args: String,
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn chat(&self, req: ChatRequest<'_>, on_delta: DeltaSink<'_>) -> Result<ChatResponse> {
        let mut body = json!({
            "model": req.model,
            "messages": self.build_messages(req.system, req.messages),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !req.tools.is_empty() {
            body["tools"] = json!(req
                .tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.schema },
                }))
                .collect::<Vec<_>>());
        }
        if let Some(mt) = self.max_tokens {
            body["max_tokens"] = json!(mt);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut rb = self.client.post(url).json(&body);
        if let Some(key) = &self.api_key {
            rb = rb.bearer_auth(key);
        }

        let mut asm = Assembler::default();
        let mut calls: Vec<PendingCall> = Vec::new();
        let mut usage = Usage::default();
        let mut finish: Option<String> = None;

        stream_events(rb, |ev| {
            if ev.data.trim() == "[DONE]" {
                return Ok(());
            }
            let v: Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            if let Some(err) = v.get("error") {
                anyhow::bail!("模型返回错误: {}", err);
            }
            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                let cached = u["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
                    .unwrap_or(0);
                usage.input = u["prompt_tokens"].as_u64().unwrap_or(0).saturating_sub(cached);
                usage.cache_read = cached;
                usage.output = u["completion_tokens"].as_u64().unwrap_or(0);
            }
            let Some(choice) = v["choices"].get(0) else { return Ok(()) };
            let d = &choice["delta"];
            let reasoning = d["reasoning_content"].as_str().or_else(|| d["reasoning"].as_str());
            if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
                asm.push_reasoning(r);
                on_delta(Delta::Reasoning(r.to_string()));
            }
            if let Some(t) = d["content"].as_str().filter(|s| !s.is_empty()) {
                asm.push_text(t);
                on_delta(Delta::Text(t.to_string()));
            }
            if let Some(tcs) = d["tool_calls"].as_array() {
                for tc in tcs {
                    let id = tc["id"].as_str().filter(|s| !s.is_empty());
                    let idx = match tc["index"].as_u64() {
                        Some(i) => i as usize,
                        None => match id {
                            Some(id) => calls.iter().position(|c| c.id == id).unwrap_or(calls.len()),
                            None => calls.len().saturating_sub(1),
                        },
                    };
                    while calls.len() <= idx {
                        calls.push(PendingCall { id: String::new(), name: String::new(), args: String::new() });
                    }
                    let c = &mut calls[idx];
                    if let Some(id) = id {
                        c.id = id.to_string();
                    }
                    if let Some(n) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                        c.name.push_str(n);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        c.args.push_str(a);
                    }
                }
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                finish = Some(f.to_string());
            }
            Ok(())
        })
        .await?;

        for (i, c) in calls.into_iter().enumerate() {
            if c.name.is_empty() {
                continue;
            }
            let id = if c.id.is_empty() { format!("call_{}", i) } else { c.id };
            asm.blocks.push(Block::ToolUse { id, name: c.name, input: parse_tool_args(&c.args) });
        }
        let has_tools = asm.blocks.iter().any(|b| matches!(b, Block::ToolUse { .. }));
        let stop = match finish.as_deref() {
            _ if has_tools => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("stop") | None => StopReason::EndTurn,
            _ => StopReason::Other,
        };
        Ok(ChatResponse {
            message: Message { role: Role::Assistant, content: asm.blocks },
            usage,
            stop,
        })
    }
}
