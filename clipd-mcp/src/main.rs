use clipd_core::{
    all_transforms, apply_transform, compute_sessions, embedding_cosine, generate_embedding,
    is_embedding_available, load_transform_config, ClipEntry, ClipStore, SearchFilters,
    SlotManager, TfIdfIndex, TransformKind,
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
                "description": "Search clipboard history. 'hybrid' (default) merges exact full-text matches with semantic (by-meaning) matches; semantic uses stored embeddings when available, TF-IDF otherwise. Image clips match via their OCR text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query text" },
                        "limit": { "type": "integer", "description": "Max results (default 20)", "default": 20 },
                        "mode": { "type": "string", "enum": ["hybrid", "keyword", "semantic"], "description": "Search mode (default hybrid)" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "set_clipboard",
                "description": "Put text on the user's clipboard so they can paste it anywhere. clipd's history records it automatically.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The text to place on the clipboard" }
                    },
                    "required": ["content"]
                }
            },
            {
                "name": "add_clip",
                "description": "Save text into clipd history WITHOUT touching the live clipboard (e.g. stash a result for later).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "Text to store" },
                        "source": { "type": "string", "description": "Label for where this came from (default 'mcp')" }
                    },
                    "required": ["content"]
                }
            },
            {
                "name": "list_slots",
                "description": "List clipd's multi-copy slots and their contents (slot 1 = latest copy; letter slots A-Z are 31-56).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "list_snippets",
                "description": "List saved snippets (reusable text recalled by trigger word).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "save_snippet",
                "description": "Create or update a snippet: reusable text the user recalls by typing its trigger in clipd's search.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "trigger": { "type": "string", "description": "Short trigger word (e.g. 'sig')" },
                        "name": { "type": "string", "description": "Optional human-readable name" },
                        "body": { "type": "string", "description": "The snippet text" }
                    },
                    "required": ["trigger", "body"]
                }
            },
            {
                "name": "list_collections",
                "description": "List clip collections (named buckets, e.g. pinned clips).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "get_collection",
                "description": "Get the clips inside a collection by its name.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Collection name" }
                    },
                    "required": ["name"]
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
            "set_clipboard" => self.tool_set_clipboard(&arguments),
            "add_clip" => self.tool_add_clip(&arguments),
            "list_slots" => self.tool_list_slots(),
            "list_snippets" => self.tool_list_snippets(),
            "save_snippet" => self.tool_save_snippet(&arguments),
            "list_collections" => self.tool_list_collections(),
            "get_collection" => self.tool_get_collection(&arguments),
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
        // Back-compat: the old boolean `semantic: true` maps to mode=semantic.
        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or(if args.get("semantic").and_then(|v| v.as_bool()) == Some(true) {
                "semantic"
            } else {
                "hybrid"
            });

        let keyword_hits = |limit: usize| -> Vec<ClipEntry> {
            let filters = SearchFilters {
                query: Some(query.to_string()),
                limit,
                ..Default::default()
            };
            self.store.search(&filters).unwrap_or_default()
        };

        let clips: Vec<ClipEntry> = match mode {
            "keyword" => keyword_hits(limit),
            "semantic" => self.semantic_hits(query, limit),
            // Hybrid: exact matches first (they're precise), then by-meaning
            // matches fill the remainder, deduped by id.
            _ => {
                let mut merged = keyword_hits(limit);
                let mut seen: std::collections::HashSet<i64> =
                    merged.iter().map(|c| c.id).collect();
                for c in self.semantic_hits(query, limit) {
                    if merged.len() >= limit {
                        break;
                    }
                    if seen.insert(c.id) {
                        merged.push(c);
                    }
                }
                merged
            }
        };

        let entries: Vec<Value> = clips.iter().map(clip_to_json).collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    /// Semantic (by-meaning) matches over recent history. Prefers stored
    /// vector embeddings (when an embedding API is configured and clips have
    /// been embedded by the daemon); falls back to a local TF-IDF index, which
    /// needs no API and runs fully offline.
    fn semantic_hits(&self, query: &str, limit: usize) -> Vec<ClipEntry> {
        let all = self.store.get_recent(500).unwrap_or_default();
        if all.is_empty() {
            return Vec::new();
        }

        let config = load_transform_config();
        if is_embedding_available(&config) {
            let ids: Vec<i64> = all.iter().map(|c| c.id).collect();
            if let Ok(embs) = self.store.get_embeddings_for_clip_ids(&ids) {
                if !embs.is_empty() {
                    if let Ok(q) = generate_embedding(query, &config) {
                        let by_id: std::collections::HashMap<i64, &ClipEntry> =
                            all.iter().map(|c| (c.id, c)).collect();
                        let mut scored: Vec<(f32, &ClipEntry)> = embs
                            .iter()
                            .filter_map(|(id, e)| {
                                Some((embedding_cosine(&q, e), *by_id.get(id)?))
                            })
                            .collect();
                        scored.sort_by(|a, b| {
                            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        return scored
                            .into_iter()
                            .take(limit)
                            .map(|(_, c)| c.clone())
                            .collect();
                    }
                }
            }
        }

        let docs: Vec<&str> = all.iter().map(|c| c.content.as_str()).collect();
        let index = TfIdfIndex::build(&docs);
        index
            .search(query, limit)
            .iter()
            .filter_map(|r| all.get(r.clip_index).cloned())
            .collect()
    }

    // ── Write tools ──

    fn tool_set_clipboard(&self, args: &Value) -> Result<String, String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'content' parameter")?;
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(content.to_string()).map_err(|e| e.to_string())?;
        Ok(format!(
            "Copied {} characters to the clipboard — ready to paste.",
            content.chars().count()
        ))
    }

    fn tool_add_clip(&self, args: &Value) -> Result<String, String> {
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'content' parameter")?;
        let source = args
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("mcp");
        let entry = ClipEntry::new(content.to_string(), Some(source.to_string()), None);
        let id = self.store.insert(&entry).map_err(|e| e.to_string())?;
        Ok(format!("Saved to clipd history as clip #{}.", id))
    }

    fn tool_list_slots(&self) -> Result<String, String> {
        let mgr = SlotManager::new();
        let slots = mgr.list_slots()?;
        let entries: Vec<Value> = slots
            .iter()
            .map(|(n, content)| {
                let label = if (31..=56).contains(n) {
                    format!("{}", (b'A' + (n - 31)) as char)
                } else {
                    n.to_string()
                };
                let preview: String = content.chars().take(120).collect();
                serde_json::json!({ "slot": label, "preview": preview })
            })
            .collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_list_snippets(&self) -> Result<String, String> {
        let snippets = self.store.list_snippets().map_err(|e| e.to_string())?;
        let entries: Vec<Value> = snippets
            .iter()
            .map(|s| {
                serde_json::json!({
                    "trigger": s.trigger,
                    "name": s.name,
                    "body": s.body,
                })
            })
            .collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_save_snippet(&self, args: &Value) -> Result<String, String> {
        let trigger = args
            .get("trigger")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'trigger' parameter")?;
        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'body' parameter")?;
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
        self.store
            .upsert_snippet(trigger, name, body)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Snippet saved — typing '{}' in clipd's search recalls it.",
            trigger
        ))
    }

    fn tool_list_collections(&self) -> Result<String, String> {
        let collections = self.store.list_collections().map_err(|e| e.to_string())?;
        let entries: Vec<Value> = collections
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "items": c.item_count,
                    "auto_route_app": c.source_app,
                })
            })
            .collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    }

    fn tool_get_collection(&self, args: &Value) -> Result<String, String> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'name' parameter")?;
        let coll = self
            .store
            .get_collection_by_name(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("No collection named '{}'", name))?;
        let items = self
            .store
            .collection_items(coll.id)
            .map_err(|e| e.to_string())?;
        let entries: Vec<Value> = items
            .iter()
            .map(|i| {
                serde_json::json!({
                    "clip_id": i.clip_id,
                    "content": i.content,
                    "added_at": i.added_at.to_rfc3339(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
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
    let mut v = serde_json::json!({
        "id": clip.id,
        "content": clip.content,
        "content_type": clip.content_type.as_str(),
        "source_app": clip.source_app,
        "timestamp": clip.timestamp.to_rfc3339(),
        "preview": clip.preview,
    });
    // Image clips: content already carries the OCR text; expose the file too.
    if let Some(path) = &clip.image_path {
        v["image_path"] = Value::String(path.clone());
    }
    v
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

        // JSON-RPC notifications (no id) must not receive responses.
        if request.id.is_none() && request.method.starts_with("notifications/") {
            continue;
        }

        let response = server.handle_request(&request);
        let _ = writeln!(stdout, "{}", serde_json::to_string(&response).unwrap());
        let _ = stdout.flush();
    }
}
