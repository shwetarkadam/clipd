use clipd_core::{
    all_transforms, apply_transform, compute_sessions, load_transform_config, ClipStore,
    SearchFilters, TfIdfIndex, TransformKind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

// ── JSON-RPC types ──

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

fn ok_response(id: Option<Value>, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

fn err_response(id: Option<Value>, code: i32, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
        }),
    }
}

// ── MCP Tool definitions ──

fn tool_definitions() -> Value {
    serde_json::json!({
        "tools": [
            {
                "name": "search_clips",
                "description": "Search clipboard history by text or meaning. Uses FTS5 full-text search with semantic fallback.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query text" },
                        "limit": { "type": "integer", "description": "Max results (default 20)", "default": 20 },
                        "semantic": { "type": "boolean", "description": "Use semantic (TF-IDF) search instead of text match", "default": false }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "get_recent",
                "description": "Get the most recent clipboard entries.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "Number of entries (default 10)", "default": 10 }
                    }
                }
            },
            {
                "name": "get_clip",
                "description": "Get a specific clipboard entry by ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "description": "Clip ID" }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "transform",
                "description": "Apply a transformation to text content (pretty JSON, strip HTML, translate, etc.)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The text content to transform" },
                        "transform": { "type": "string", "description": "Transform name (e.g. 'pretty_json', 'html_to_markdown', 'uppercase', 'translate_to_english')" }
                    },
                    "required": ["content", "transform"]
                }
            },
            {
                "name": "list_transforms",
                "description": "List all available content transformations.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "get_sessions",
                "description": "Get clipboard history grouped into time-based sessions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "window_minutes": { "type": "integer", "description": "Session gap in minutes (default 30)", "default": 30 }
                    }
                }
            },
            {
                "name": "stats",
                "description": "Get clipboard store statistics.",
                "inputSchema": { "type": "object", "properties": {} }
            }
        ]
    })
}

// ── MCP Server ──

struct McpServer {
    store: ClipStore,
}

impl McpServer {
    fn new() -> Self {
        let db_path = ClipStore::default_path();
        let store = ClipStore::new(&db_path).expect("Failed to open clip database");
        Self { store }
    }

    fn handle_request(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id.clone()),
            "notifications/initialized" => ok_response(req.id.clone(), Value::Null),
            "tools/list" => self.handle_tools_list(req.id.clone()),
            "tools/call" => self.handle_tools_call(req.id.clone(), &req.params),
            "resources/list" => self.handle_resources_list(req.id.clone()),
            "resources/read" => self.handle_resources_read(req.id.clone(), &req.params),
            _ => err_response(req.id.clone(), -32601, &format!("Method not found: {}", req.method)),
        }
    }

    fn handle_initialize(&self, id: Option<Value>) -> JsonRpcResponse {
        ok_response(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "resources": {}
                },
                "serverInfo": {
                    "name": "clipd-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
    }

    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        ok_response(id, tool_definitions())
    }

    fn handle_tools_call(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let tool_name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));

        let result = match tool_name {
            "search_clips" => self.tool_search_clips(&arguments),
            "get_recent" => self.tool_get_recent(&arguments),
            "get_clip" => self.tool_get_clip(&arguments),
            "transform" => self.tool_transform(&arguments),
            "list_transforms" => self.tool_list_transforms(),
            "get_sessions" => self.tool_get_sessions(&arguments),
            "stats" => self.tool_stats(),
            _ => Err(format!("Unknown tool: {}", tool_name)),
        };

        match result {
            Ok(content) => ok_response(
                id,
                serde_json::json!({
                    "content": [{ "type": "text", "text": content }]
                }),
            ),
            Err(msg) => ok_response(
                id,
                serde_json::json!({
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true
                }),
            ),
        }
    }

    fn handle_resources_list(&self, id: Option<Value>) -> JsonRpcResponse {
        ok_response(
            id,
            serde_json::json!({
                "resources": [
                    {
                        "uri": "clipd://history",
                        "name": "Clipboard History",
                        "description": "Recent clipboard entries",
                        "mimeType": "application/json"
                    }
                ]
            }),
        )
    }

    fn handle_resources_read(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let uri = params
            .get("uri")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match uri {
            "clipd://history" => {
                let clips = self.store.get_recent(50).unwrap_or_default();
                let entries: Vec<Value> = clips
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "id": c.id,
                            "preview": c.preview,
                            "content_type": c.content_type.as_str(),
                            "source_app": c.source_app,
                            "timestamp": c.timestamp.to_rfc3339(),
                        })
                    })
                    .collect();

                ok_response(
                    id,
                    serde_json::json!({
                        "contents": [{
                            "uri": "clipd://history",
                            "mimeType": "application/json",
                            "text": serde_json::to_string_pretty(&entries).unwrap_or_default()
                        }]
                    }),
                )
            }
            _ => err_response(id, -32602, &format!("Unknown resource: {}", uri)),
        }
    }

    // ── Tool implementations ──

    fn tool_search_clips(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'query' parameter")?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let semantic = args
            .get("semantic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if semantic {
            let all = self.store.get_recent(500).unwrap_or_default();
            let docs: Vec<&str> = all.iter().map(|c| c.content.as_str()).collect();
            let index = TfIdfIndex::build(&docs);
            let results = index.search(query, limit);

            let entries: Vec<Value> = results
                .iter()
                .filter_map(|r| all.get(r.clip_index))
                .map(|c| clip_to_json(c))
                .collect();

            serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
        } else {
            let filters = SearchFilters {
                query: Some(query.to_string()),
                limit,
                ..Default::default()
            };
            let clips = self.store.search(&filters).map_err(|e| e.to_string())?;
            let entries: Vec<Value> = clips.iter().map(clip_to_json).collect();
            serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
        }
    }

    fn tool_get_recent(&self, args: &Value) -> Result<String, String> {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        let clips = self.store.get_recent(limit).map_err(|e| e.to_string())?;
        let entries: Vec<Value> = clips.iter().map(clip_to_json).collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_get_clip(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or("Missing 'id' parameter")?;

        let clip = self.store.get_by_id(id).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&clip_to_json(&clip)).map_err(|e| e.to_string())
    }

    fn tool_transform(&self, args: &Value) -> Result<String, String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'content' parameter")?;
        let transform_name = args
            .get("transform")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'transform' parameter")?;

        let config = load_transform_config();
        let kind = parse_transform_name(transform_name)?;
        apply_transform(&kind, content, &config)
    }

    fn tool_list_transforms(&self) -> Result<String, String> {
        let transforms = all_transforms();
        let entries: Vec<Value> = transforms
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": transform_name(t),
                    "label": t.label(),
                    "category": t.category(),
                    "ai": t.is_ai(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_get_sessions(&self, args: &Value) -> Result<String, String> {
        let window = args
            .get("window_minutes")
            .and_then(|v| v.as_i64())
            .unwrap_or(30);

        let clips = self.store.get_recent(500).unwrap_or_default();
        let sessions = compute_sessions(&clips, window);

        let entries: Vec<Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "started_at": s.started_at.to_rfc3339(),
                    "ended_at": s.ended_at.to_rfc3339(),
                    "clip_count": s.clip_count(),
                    "duration_mins": s.duration_mins(),
                    "apps": s.top_apps,
                    "clip_ids": s.clip_ids,
                })
            })
            .collect();

        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_stats(&self) -> Result<String, String> {
        let stats = self.store.stats().map_err(|e| e.to_string())?;
        let result = serde_json::json!({
            "total_clips": stats.total_clips,
            "unique_apps": stats.unique_apps,
            "db_size_bytes": stats.db_size_bytes,
            "oldest_clip": stats.oldest_clip.map(|d| d.to_rfc3339()),
            "newest_clip": stats.newest_clip.map(|d| d.to_rfc3339()),
            "top_apps": stats.top_apps,
            "type_counts": stats.type_counts,
        });
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }
}

fn clip_to_json(clip: &clipd_core::ClipEntry) -> Value {
    serde_json::json!({
        "id": clip.id,
        "content": clip.content,
        "content_type": clip.content_type.as_str(),
        "source_app": clip.source_app,
        "timestamp": clip.timestamp.to_rfc3339(),
        "preview": clip.preview,
    })
}

fn transform_name(t: &TransformKind) -> String {
    match t {
        TransformKind::PrettyJson => "pretty_json".into(),
        TransformKind::MinifyJson => "minify_json".into(),
        TransformKind::SortLines => "sort_lines".into(),
        TransformKind::UniqueLines => "unique_lines".into(),
        TransformKind::ReverseLines => "reverse_lines".into(),
        TransformKind::TrimWhitespace => "trim_whitespace".into(),
        TransformKind::AddLineNumbers => "add_line_numbers".into(),
        TransformKind::RemoveLineNumbers => "remove_line_numbers".into(),
        TransformKind::HtmlToMarkdown => "html_to_markdown".into(),
        TransformKind::StripHtml => "strip_html".into(),
        TransformKind::Base64Encode => "base64_encode".into(),
        TransformKind::Base64Decode => "base64_decode".into(),
        TransformKind::UrlEncode => "url_encode".into(),
        TransformKind::UrlDecode => "url_decode".into(),
        TransformKind::Uppercase => "uppercase".into(),
        TransformKind::Lowercase => "lowercase".into(),
        TransformKind::TitleCase => "title_case".into(),
        TransformKind::CamelToSnake => "camel_to_snake".into(),
        TransformKind::SnakeToCamel => "snake_to_camel".into(),
        TransformKind::TranslateToEnglish => "translate_to_english".into(),
        TransformKind::FixGrammar => "fix_grammar".into(),
        TransformKind::Summarize => "summarize".into(),
        TransformKind::CodeToTypeScript => "code_to_typescript".into(),
        TransformKind::CodeToPython => "code_to_python".into(),
        TransformKind::CodeToRust => "code_to_rust".into(),
        TransformKind::ExplainCode => "explain_code".into(),
        TransformKind::CustomPrompt(_) => "custom_prompt".into(),
    }
}

fn parse_transform_name(name: &str) -> Result<TransformKind, String> {
    match name {
        "pretty_json" => Ok(TransformKind::PrettyJson),
        "minify_json" => Ok(TransformKind::MinifyJson),
        "sort_lines" => Ok(TransformKind::SortLines),
        "unique_lines" => Ok(TransformKind::UniqueLines),
        "reverse_lines" => Ok(TransformKind::ReverseLines),
        "trim_whitespace" => Ok(TransformKind::TrimWhitespace),
        "add_line_numbers" => Ok(TransformKind::AddLineNumbers),
        "remove_line_numbers" => Ok(TransformKind::RemoveLineNumbers),
        "html_to_markdown" => Ok(TransformKind::HtmlToMarkdown),
        "strip_html" => Ok(TransformKind::StripHtml),
        "base64_encode" => Ok(TransformKind::Base64Encode),
        "base64_decode" => Ok(TransformKind::Base64Decode),
        "url_encode" => Ok(TransformKind::UrlEncode),
        "url_decode" => Ok(TransformKind::UrlDecode),
        "uppercase" => Ok(TransformKind::Uppercase),
        "lowercase" => Ok(TransformKind::Lowercase),
        "title_case" => Ok(TransformKind::TitleCase),
        "camel_to_snake" => Ok(TransformKind::CamelToSnake),
        "snake_to_camel" => Ok(TransformKind::SnakeToCamel),
        "translate_to_english" => Ok(TransformKind::TranslateToEnglish),
        "fix_grammar" => Ok(TransformKind::FixGrammar),
        "summarize" => Ok(TransformKind::Summarize),
        "code_to_typescript" => Ok(TransformKind::CodeToTypeScript),
        "code_to_python" => Ok(TransformKind::CodeToPython),
        "code_to_rust" => Ok(TransformKind::CodeToRust),
        "explain_code" => Ok(TransformKind::ExplainCode),
        other => {
            if other.starts_with("custom:") {
                Ok(TransformKind::CustomPrompt(other[7..].to_string()))
            } else {
                Err(format!("Unknown transform: '{}'. Use list_transforms to see available options.", other))
            }
        }
    }
}

// ── Main loop ──

fn main() {
    let server = McpServer::new();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    eprintln!("clipd-mcp: MCP server started (stdio mode)");

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err = err_response(None, -32700, &format!("Parse error: {}", e));
                let _ = writeln!(stdout, "{}", serde_json::to_string(&err).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        if request.jsonrpc != "2.0" {
            let err = err_response(request.id, -32600, "Invalid JSON-RPC version");
            let _ = writeln!(stdout, "{}", serde_json::to_string(&err).unwrap());
            let _ = stdout.flush();
            continue;
        }

        let response = server.handle_request(&request);
        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}
