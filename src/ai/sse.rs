//! 极简 Server-Sent Events 解析器（按字节缓冲，避免 UTF-8 被切断）

use anyhow::{bail, Result};
use futures_util::StreamExt;

#[derive(Debug)]
pub struct SseEvent {
    #[allow(dead_code)]
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent { event: self.event.take(), data: self.data.join("\n") });
                    self.data.clear();
                }
                self.event = None;
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = Some(v.trim().to_string());
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            return None;
        }
        let ev = SseEvent { event: self.event.take(), data: self.data.join("\n") };
        self.data.clear();
        Some(ev)
    }
}

/// 发送请求并逐个回调 SSE 事件
pub async fn stream_events(
    req: reqwest::RequestBuilder,
    mut on_event: impl FnMut(SseEvent) -> Result<()>,
) -> Result<()> {
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("API 请求失败 ({}): {}", status, truncate(&body, 2000));
    }
    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        for ev in parser.push(&chunk?) {
            on_event(ev)?;
        }
    }
    if let Some(ev) = parser.finish() {
        on_event(ev)?;
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: a\r\ndata: {\"x\"").is_empty());
        let evs = p.push(b":1}\r\n\r\ndata: [DONE]\n\n");
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event.as_deref(), Some("a"));
        assert_eq!(evs[0].data, "{\"x\":1}");
        assert_eq!(evs[1].data, "[DONE]");
    }
}
