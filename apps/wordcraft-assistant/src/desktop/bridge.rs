//! The tool bridge between a plan's command-line tool (Codex, Claude Code) and the open
//! document.
//!
//! The CLI starts this app's own executable with `--assistant-mcp` as its only MCP server
//! ([`serve_stdio`]). That process speaks MCP on stdin/stdout and forwards each tool call over a
//! localhost socket to the running app ([`Bridge`]), which runs it on the UI thread through
//! [`crate::agent::run_tool`]: the same two tools, allow list and limits as the API path.
//!
//! The socket listens on 127.0.0.1 only and every line must carry a random token. The port and
//! token are in a file in the user's settings folder, named by the MCP server's environment
//! (`CRAFT_ASSISTANT_BRIDGE`), so no secret appears on a command line.
#![forbid(unsafe_code)]

use crate::protocol;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

/// The argument that turns the app's executable into the MCP server.
pub const MCP_FLAG: &str = "--assistant-mcp";
/// The environment variable naming the bridge file.
pub const BRIDGE_ENV: &str = "CRAFT_ASSISTANT_BRIDGE";
/// The MCP server's name, so the tools are `mcp__craft__list_commands` and so on.
pub const SERVER: &str = "craft";
/// The longest a tool call may take on the UI thread before the CLI hears an error.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest line accepted on the socket or on stdin (a tool call's arguments).
const MAX_LINE: usize = 4 * 1024 * 1024;

/// A tool call waiting for the UI thread.
pub struct Call {
    pub name: String,
    pub args: Value,
    reply: Sender<Value>,
}
impl Call {
    /// `content` is the JSON text the model receives; `error` marks a failed call.
    pub fn answer(self, content: String, error: bool) {
        let _ = self.reply.send(json!({"content":content,"error":error}));
    }
}

/// The app's end: a localhost listener whose calls the UI thread takes with [`Bridge::calls`].
pub struct Bridge {
    calls: Receiver<Call>,
    file: PathBuf,
}

impl Bridge {
    pub fn start(dir: &Path) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let file = dir.join(format!("assistant-bridge-{}.json", std::process::id()));
        write_private(&file, &json!({"port":port,"token":token}).to_string())?;
        let (tx, calls) = mpsc::channel();
        std::thread::Builder::new()
            .name("craft-assistant-bridge".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let tx = tx.clone();
                    let token = token.clone();
                    let _ = std::thread::Builder::new().name("craft-assistant-call".into()).spawn(move || serve_connection(stream, &token, &tx));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { calls, file })
    }

    /// The file the MCP server reads the port and token from.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Tool calls that arrived since the last frame.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.try_iter().collect()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Constant-time comparison: the token must not leak through timing.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn read_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = Vec::new();
    let read = Read::take(&mut *reader, MAX_LINE as u64 + 1).read_until(b'\n', &mut line).ok()?;
    if read == 0 || line.len() > MAX_LINE {
        return None;
    }
    String::from_utf8(line).ok()
}

fn serve_connection(stream: TcpStream, token: &str, calls: &Sender<Call>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));
    let Ok(mut out) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_line(&mut reader) {
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let reply = if !request["token"].as_str().is_some_and(|t| same(t, token)) {
            // A wrong token ends the connection: nothing else is read from it.
            let _ = writeln!(out, "{}", json!({"content":json!({"error":"unauthorized"}).to_string(),"error":true}));
            return;
        } else {
            let (tx, rx) = mpsc::channel();
            let call = Call { name: request["name"].as_str().unwrap_or("").to_owned(), args: request["args"].clone(), reply: tx };
            if calls.send(call).is_err() {
                return;
            }
            rx.recv_timeout(CALL_TIMEOUT)
                .unwrap_or_else(|_| json!({"content":json!({"error":"The editor did not answer in time"}).to_string(),"error":true}))
        };
        if writeln!(out, "{reply}").is_err() {
            return;
        }
    }
}

/// The MCP server: `<app> --assistant-mcp`, started by Codex or Claude Code. Returns when
/// stdin closes. Speaks JSON-RPC 2.0, one message per line (the MCP stdio transport).
pub fn serve_stdio() {
    let file = std::env::var_os(BRIDGE_ENV).map(PathBuf::from);
    let bridge: Option<(u16, String)> = file
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| Some((u16::try_from(v["port"].as_u64()?).ok()?, v["token"].as_str()?.to_owned())));
    let mut connection: Option<(TcpStream, BufReader<TcpStream>)> = None;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    while let Some(line) = read_line(&mut input) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(reply) = answer(&message, &mut |name, args| forward(&bridge, &mut connection, name, args)) else { continue };
        let mut out = stdout.lock();
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            return;
        }
    }
}

/// One tool call through the socket; (content, is_error).
fn forward(bridge: &Option<(u16, String)>, connection: &mut Option<(TcpStream, BufReader<TcpStream>)>, name: &str, args: &Value) -> (String, bool) {
    let fail = |text: &str| (json!({"error":text}).to_string(), true);
    let Some((port, token)) = bridge else { return fail("The editor is not reachable: open the assistant in the app and ask again") };
    for _attempt in 0..2 {
        if connection.is_none() {
            let Ok(stream) = TcpStream::connect(("127.0.0.1", *port)) else { return fail("The editor closed") };
            let Ok(reader) = stream.try_clone() else { return fail("The editor closed") };
            *connection = Some((stream, BufReader::new(reader)));
        }
        let Some((stream, reader)) = connection.as_mut() else { return fail("The editor closed") };
        let request = json!({"token":token,"name":name,"args":args});
        if writeln!(stream, "{request}").is_ok()
            && let Some(line) = read_line(reader)
            && let Ok(reply) = serde_json::from_str::<Value>(&line)
        {
            return (reply["content"].as_str().unwrap_or("{}").to_owned(), reply["error"].as_bool().unwrap_or(true));
        }
        // A dropped connection is opened once more before giving up.
        *connection = None;
    }
    fail("The editor closed")
}

/// The reply to one JSON-RPC message (None for notifications).
fn answer(message: &Value, call: &mut dyn FnMut(&str, &Value) -> (String, bool)) -> Option<Value> {
    let id = message.get("id").filter(|id| !id.is_null())?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => {
            let version = message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Tools that read and edit the document open in the user's office app."
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let tools: Vec<Value> = protocol::tools()
                .into_iter()
                .map(|t| {
                    let read_only = t["name"] == "list_commands";
                    json!({
                        "name": t["name"],
                        "description": t["description"],
                        "inputSchema": t["parameters"],
                        "annotations": {"readOnlyHint": read_only, "destructiveHint": false, "openWorldHint": false}
                    })
                })
                .collect();
            json!({"tools": tools})
        }
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or("");
            let args = message["params"].get("arguments").cloned().unwrap_or(json!({}));
            let (text, error) = call(name, &args);
            json!({"content":[{"type":"text","text":text}],"isError":error})
        }
        "resources/list" => json!({"resources": []}),
        "prompts/list" => json!({"prompts": []}),
        _ => return Some(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Method not found: {method}")}})),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_lists_the_two_tools_and_forwards_calls() {
        let mut seen = Vec::new();
        let mut call = |name: &str, args: &Value| {
            seen.push((name.to_owned(), args.clone()));
            ("{\"ok\":true}".to_owned(), false)
        };
        let init = answer(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}), &mut call).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(answer(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &mut call).is_none());
        let list = answer(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), &mut call).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["list_commands", "execute_command"]);
        let reply = answer(
            &json!({"jsonrpc":"2.0","id":"c","method":"tools/call","params":{"name":"execute_command","arguments":{"command":"format.bold","params":{}}}}),
            &mut call,
        )
        .unwrap();
        assert_eq!(reply["id"], "c");
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["content"][0]["text"], "{\"ok\":true}");
        assert_eq!(answer(&json!({"jsonrpc":"2.0","id":3,"method":"sampling/create"}), &mut call).unwrap()["error"]["code"], -32601);
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn the_socket_needs_the_token_and_reaches_the_ui_thread() {
        let dir = std::env::temp_dir().join(format!("craft-bridge-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bridge = Bridge::start(&dir).unwrap();
        let info: Value = serde_json::from_slice(&std::fs::read(bridge.file()).unwrap()).unwrap();
        let port = info["port"].as_u64().unwrap() as u16;
        let token = info["token"].as_str().unwrap().to_owned();

        let mut wrong = TcpStream::connect(("127.0.0.1", port)).unwrap();
        writeln!(wrong, "{}", json!({"token":"nope","name":"list_commands","args":{}})).unwrap();
        let mut line = String::new();
        BufReader::new(wrong).read_line(&mut line).unwrap();
        assert!(line.contains("unauthorized"), "{line}");

        let ui = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(call) = bridge.calls().pop() {
                    assert_eq!(call.name, "list_commands");
                    call.answer("{\"total\":0}".into(), false);
                    return bridge;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no call arrived");
        });
        let mut connection = None;
        let (text, error) = forward(&Some((port, token)), &mut connection, "list_commands", &json!({}));
        assert_eq!((text.as_str(), error), ("{\"total\":0}", false));
        let bridge = ui.join().unwrap();
        let file = bridge.file().to_owned();
        drop(bridge);
        assert!(!file.exists(), "the bridge file goes with the app");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
