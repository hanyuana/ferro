//! 原生 AI 驱动层：统一的消息模型 + Provider 抽象。
//! 不依赖任何第三方 AI SDK，直接实现各家流式协议。

pub mod anthropic;
pub mod openai;
pub mod sse;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// 思考过程（DeepSeek reasoning_content / Claude thinking）
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

impl Message {
    pub fn tool_uses(&self) -> Vec<(String, String, Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

/// 流式增量，推送给 UI 实时显示
#[derive(Debug, Clone)]
pub enum Delta {
    Text(String),
    Reasoning(String),
}

pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolDef],
}

pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub stop: StopReason,
}

pub type DeltaSink<'a> = &'a (dyn Fn(Delta) + Send + Sync);

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, req: ChatRequest<'_>, on_delta: DeltaSink<'_>) -> Result<ChatResponse>;
}

/// 流式拼装助手消息时的公共工具
#[derive(Default)]
pub(crate) struct Assembler {
    pub blocks: Vec<Block>,
}

impl Assembler {
    pub fn push_text(&mut self, s: &str) {
        if let Some(Block::Text { text }) = self.blocks.last_mut() {
            text.push_str(s);
        } else {
            self.blocks.push(Block::Text { text: s.to_string() });
        }
    }

    pub fn push_reasoning(&mut self, s: &str) {
        if let Some(Block::Reasoning { text, .. }) = self.blocks.last_mut() {
            text.push_str(s);
        } else {
            self.blocks.push(Block::Reasoning { text: s.to_string(), signature: None });
        }
    }
}

/// 解析流式拼接出来的工具参数 JSON（容忍空字符串）
pub(crate) fn parse_tool_args(raw: &str) -> Value {
    let t = raw.trim();
    if t.is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(t).unwrap_or_else(|_| serde_json::json!({ "__raw": raw }))
}
