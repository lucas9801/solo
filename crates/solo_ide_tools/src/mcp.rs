//! The MCP messages the server answers (JSON-RPC 2.0): `initialize`, `ping`,
//! `tools/list` and `tools/call`. Everything else gets "method not found";
//! notifications get no answer.

use serde_json::{Value, json};

/// The protocol versions the server speaks; it answers an `initialize` with
/// the client's version when it's one of these, else the first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05", "2025-11-25"];

const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_REQUEST: i64 = -32600;
const INVALID_PARAMS: i64 = -32602;

/// What to do with one message.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// Answer with this.
    Respond(Value),
    /// A notification or a response: nothing to answer.
    Nothing,
    /// Run a tool and answer with [`tool_result`].
    CallTool {
        id: Value,
        name: String,
        arguments: Value,
    },
}

pub fn dispatch(message: &Value, instructions: &str) -> Incoming {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        if message.get("result").is_some() || message.get("error").is_some() {
            return Incoming::Nothing;
        }
        return Incoming::Respond(error(
            Value::Null,
            INVALID_REQUEST,
            "not a JSON-RPC request",
        ));
    };
    let Some(id) = message.get("id").cloned() else {
        return Incoming::Nothing;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            let version = requested
                .filter(|version| PROTOCOL_VERSIONS.contains(version))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            Incoming::Respond(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": terminal::ide_tools::SERVER_NAME, "title": "Solo IDE tools", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": instructions,
                }),
            ))
        }
        "ping" => Incoming::Respond(result(id, json!({}))),
        "tools/list" => Incoming::Respond(result(id, json!({ "tools": tools() }))),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Incoming::Respond(error(id, INVALID_PARAMS, "missing tool name"));
            };
            if !TOOL_NAMES.contains(&name) {
                return Incoming::Respond(error(
                    id,
                    INVALID_PARAMS,
                    &format!("unknown tool: {name}"),
                ));
            }
            Incoming::CallTool {
                id,
                name: name.to_string(),
                arguments: params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            }
        }
        _ => Incoming::Respond(error(
            id,
            METHOD_NOT_FOUND,
            &format!("method not found: {method}"),
        )),
    }
}

/// The answer to a `tools/call`: the tool's text, or its error as a tool
/// error (which the model sees), not a protocol error.
pub fn tool_result(id: Value, output: Result<String, String>) -> Value {
    let (text, is_error) = match output {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    result(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub const TOOL_NAMES: &[&str] = &[
    "workspace_symbols",
    "go_to_definition",
    "find_references",
    "diagnostics",
];

fn locator_properties() -> Value {
    json!({
        "symbol": {
            "type": "string",
            "description": "The symbol's name, as written in the code (e.g. `parse_config`, not `Config::parse_config`)."
        },
        "path": {
            "type": "string",
            "description": "A file in the workspace: relative to the workspace root (the terminal's starting directory) or absolute. May end in `:line` or `:line:column`."
        },
        "line": {
            "type": "integer",
            "minimum": 1,
            "description": "1-based line in `path` where the symbol appears; `symbol` is looked for on it (and a few lines around it)."
        },
        "column": {
            "type": "integer",
            "minimum": 1,
            "description": "1-based column of the symbol on `line`, instead of `symbol`."
        }
    })
}

/// The tools, as `tools/list` gives them. All read-only.
fn tools() -> Value {
    let annotations = |title: &str| {
        json!({
            "title": title,
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        })
    };
    json!([
        {
            "name": "workspace_symbols",
            "title": "Workspace symbols",
            "description": "Search the workspace's symbols (functions, types, methods, constants...) by name with the language servers Solo runs. Faster and more precise than grepping for a definition. Returns `kind name path:line:column (container)`. Fuzzy: `parse cfg` can find `parse_config`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The name, or part of it." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "At most this many results (default 50)." }
                },
                "required": ["query"]
            },
            "annotations": annotations("Workspace symbols"),
        },
        {
            "name": "go_to_definition",
            "title": "Go to definition",
            "description": "Where a symbol is defined, as `path:line:column: source line`, from the language server. Give `path` and `line` with `symbol` (or `column`) to resolve a use at that place exactly; or only `symbol` to look up definitions with that exact name in the workspace.",
            "inputSchema": { "type": "object", "properties": locator_properties() },
            "annotations": annotations("Go to definition"),
        },
        {
            "name": "find_references",
            "title": "Find references",
            "description": "Every reference to a symbol in the workspace (callers, uses, imports), as `path:line:column: source line`, from the language server: unlike grep, it skips other symbols with the same name. Give `path` and `line` with `symbol` (or `column`) for the symbol at that place, or only `symbol` for the workspace symbol with that exact name.",
            "inputSchema": { "type": "object", "properties": locator_properties() },
            "annotations": annotations("Find references"),
        },
        {
            "name": "diagnostics",
            "title": "Diagnostics",
            "description": "Errors and warnings from the language servers Solo runs. With `path`, that file's diagnostics (`path:line:column: severity: message`); without, how many each file in the workspace has. They can lag behind edits made outside Solo (e.g. rust-analyzer's cargo check runs when Solo saves a file), so build or test to be sure.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "A file in the workspace, relative to the workspace root or absolute. Leave out for a summary of the workspace." }
                }
            },
            "annotations": annotations("Diagnostics"),
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_answers_with_the_clients_version() {
        let Incoming::Respond(answer) = dispatch(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            "hi",
        ) else {
            panic!()
        };
        assert_eq!(answer["id"], 1);
        assert_eq!(answer["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(answer["result"]["instructions"], "hi");
        assert!(answer["result"]["capabilities"]["tools"].is_object());

        let Incoming::Respond(answer) = dispatch(
            &json!({"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":"1999-01-01"}}),
            "",
        ) else {
            panic!()
        };
        assert_eq!(answer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn lists_read_only_tools() {
        let Incoming::Respond(answer) =
            dispatch(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), "")
        else {
            panic!()
        };
        let tools = answer["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, TOOL_NAMES);
        for tool in tools {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn calls_and_other_messages() {
        assert_eq!(
            dispatch(
                &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"diagnostics","arguments":{"path":"a.rs"}}}),
                ""
            ),
            Incoming::CallTool {
                id: json!(3),
                name: "diagnostics".to_string(),
                arguments: json!({"path":"a.rs"}),
            }
        );
        let Incoming::Respond(answer) = dispatch(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"edit_file"}}),
            "",
        ) else {
            panic!()
        };
        assert_eq!(answer["error"]["code"], INVALID_PARAMS);
        assert_eq!(
            dispatch(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                ""
            ),
            Incoming::Nothing
        );
        let Incoming::Respond(answer) = dispatch(
            &json!({"jsonrpc":"2.0","id":5,"method":"resources/list"}),
            "",
        ) else {
            panic!()
        };
        assert_eq!(answer["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(
            tool_result(json!(6), Err("no".into()))["result"]["isError"],
            true
        );
    }
}
