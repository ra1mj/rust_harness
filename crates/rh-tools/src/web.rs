//! Web capability tools: `web_fetch` and `web_search` (dsh web capability /
//! grok web_search/web_fetch analogues).

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

use rh_tool::{Tool, ToolCallContext, ToolDescription, ToolError, ToolId};

const UA: &str = "Mozilla/5.0 (compatible; rh-harness/0.1)";

/// Fetch a URL and return its text content (HTML stripped to text).
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn id(&self) -> ToolId {
        "web_fetch".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "web_fetch",
            "Fetch a URL and return its text content (HTML is stripped).",
            json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "the URL to fetch" } },
                "required": ["url"]
            }),
        )
    }

    async fn run(&self, _ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 url 参数"))?;
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| ToolError::execution(e.to_string()))?;
        let response = client
            .get(url)
            .header("user-agent", UA)
            .send()
            .await
            .map_err(|e| ToolError::execution(format!("抓取失败：{e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::execution(format!("HTTP {status}")));
        }
        let body = response
            .text()
            .await
            .map_err(|e| ToolError::execution(e.to_string()))?;
        let text = html_to_text(&body);
        Ok(json!({ "url": url, "text": truncate(&text, 8000) }))
    }
}

/// Search the web via DuckDuckGo's HTML endpoint (no API key).
pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn id(&self) -> ToolId {
        "web_search".to_string()
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::new(
            "web_search",
            "Search the web and return the top results (title, URL, snippet).",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "the search query" } },
                "required": ["query"]
            }),
        )
    }

    async fn run(&self, _ctx: &ToolCallContext, args: Value) -> Result<Value, ToolError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::execution("缺少 query 参数"))?;
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            url_encode(query)
        );
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| ToolError::execution(e.to_string()))?;
        let response = client
            .get(&url)
            .header("user-agent", UA)
            .send()
            .await
            .map_err(|e| ToolError::execution(format!("搜索失败：{e}")))?;
        let body = response
            .text()
            .await
            .map_err(|e| ToolError::execution(e.to_string()))?;
        let results = parse_ddg_results(&body);
        Ok(json!({ "query": query, "results": results }))
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript"] {
        let re = Regex::new(&format!(r"(?is)<{tag}[^>]*>.*?</{tag}>")).unwrap();
        s = re.replace_all(&s, " ").to_string();
    }
    s = Regex::new(r"(?s)<!--.*?-->").unwrap().replace_all(&s, "").to_string();
    s = Regex::new(r"(?s)<[^>]+>").unwrap().replace_all(&s, " ").to_string();
    let s = decode_entities(&s);
    Regex::new(r"[ \t]+").unwrap().replace_all(s.trim(), " ").to_string()
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn parse_ddg_results(html: &str) -> Vec<Value> {
    let mut results = Vec::new();
    let link_re = Regex::new(r#"class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap();
    for cap in link_re.captures_iter(html).take(8) {
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let title = decode_entities(&html_to_text(cap.get(2).map(|m| m.as_str()).unwrap_or("")));
        results.push(json!({ "title": title, "url": href }));
    }
    results
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}
