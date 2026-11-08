//! Minimal LSP client: launches a server, tracks progress, and serves
//! "go to definition" requests (results are printed to stdout for now).

use crate::mason::Server;
use futures::SinkExt;
use iced::futures::Stream;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Config {
    pub server: Server,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Request {
    /// `line` is 0-based, `character` is a UTF-16 offset.
    GotoDefinition { path: String, line: u32, character: u32, word: String },
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(Server, String),
    Ready(Server, mpsc::UnboundedSender<Request>),
    Progress { server: Server, token: String, title: String, message: String, percentage: Option<u32>, done: bool },
    Definition { server: Server, summary: String },
    Exited(Server, String),
}

enum Pending {
    Initialize,
    Definition { path: String, line: u32, character: u32, word: String },
}

pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn uri_to_display(uri: &str, root: &Path) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    // Percent-decode.
    let bytes = path.as_bytes();
    let mut dec = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                dec.push(b);
                i += 3;
                continue;
            }
        dec.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8_lossy(&dec).into_owned();
    let root = root.to_string_lossy();
    path.strip_prefix(&format!("{root}/")).map(str::to_owned).unwrap_or(path)
}

async fn write_msg(stdin: &mut tokio::process::ChildStdin, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_string(v).unwrap_or_default();
    stdin.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes()).await?;
    stdin.write_all(body.as_bytes()).await?;
    stdin.flush().await
}

async fn read_msg<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).await.ok()?;
    serde_json::from_slice(&buf).ok()
}

pub fn connect(cfg: &Config) -> impl Stream<Item = Event> + use<> {
    let cfg = cfg.clone();
    iced::stream::channel(256, async move |mut output: futures::channel::mpsc::Sender<Event>| {
        let server = cfg.server;
        let (status_tx, mut status_rx) = mpsc::unbounded_channel::<String>();
        let resolve = crate::mason::resolve(server, move |s| {
            let _ = status_tx.send(s);
        });
        tokio::pin!(resolve);
        let resolved = loop {
            tokio::select! {
                r = &mut resolve => break r,
                Some(s) = status_rx.recv() => { let _ = output.send(Event::Status(server, s)).await; }
            }
        };
        while let Ok(s) = status_rx.try_recv() {
            let _ = output.send(Event::Status(server, s)).await;
        }
        let (bin, source) = match resolved {
            Ok(r) => r,
            Err(e) => {
                let _ = output.send(Event::Exited(server, e)).await;
                std::future::pending::<()>().await;
                return;
            }
        };
        println!("[lsp] starting {} ({source}) in {}", bin.display(), cfg.root.display());
        let _ = output.send(Event::Status(server, format!("starting ({source})…"))).await;

        let mut child = match tokio::process::Command::new(&bin)
            .current_dir(&cfg.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = output.send(Event::Exited(server, format!("failed to start {}: {e}", bin.display()))).await;
                std::future::pending::<()>().await;
                return;
            }
        };
        crate::debug::dlog!(
            "[lsp] spawned {server:?} pid={:?} pgid={}; app {}",
            child.id(),
            child.id().map(crate::debug::pgid_of).unwrap_or_default(),
            crate::debug::self_ids()
        );
        let _drop_log = crate::debug::DropLog(format!("[lsp] {server:?} stream (pid {:?}, killed by kill_on_drop)", child.id()));
        let mut stdin = child.stdin.take().expect("stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let stderr = child.stderr.take().expect("stderr");
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                eprintln!("[{server} stderr] {l}");
            }
        });

        // Reading happens in a separate task so that `select!` never cancels a partial read.
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(async move {
            while let Some(v) = read_msg(&mut stdout).await {
                if in_tx.send(v).is_err() {
                    break;
                }
            }
        });

        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
        let mut next_id = 1u64;
        let mut pending: HashMap<u64, Pending> = HashMap::new();
        let mut opened: HashMap<String, ()> = HashMap::new();
        let mut initialized = false;
        let mut queued: Vec<Request> = Vec::new();
        let root_uri = path_to_uri(&cfg.root);
        let root_name = cfg.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

        let init = json!({
            "jsonrpc": "2.0", "id": next_id, "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "clientInfo": {"name": "reviewfren", "version": env!("CARGO_PKG_VERSION")},
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": root_name}],
                "capabilities": {
                    "general": {"positionEncodings": ["utf-16"]},
                    "window": {"workDoneProgress": true, "showMessage": {}},
                    "workspace": {"configuration": true, "workspaceFolders": true},
                    "textDocument": {
                        "synchronization": {"dynamicRegistration": false},
                        "definition": {"linkSupport": true},
                        "hover": {"contentFormat": ["markdown", "plaintext"]},
                    },
                    "experimental": {"serverStatusNotification": true},
                },
            }
        });
        pending.insert(next_id, Pending::Initialize);
        next_id += 1;
        if let Err(e) = write_msg(&mut stdin, &init).await {
            let _ = output.send(Event::Exited(server, format!("could not talk to {server}: {e}"))).await;
            std::future::pending::<()>().await;
        }
        let _ = output.send(Event::Status(server, "initializing…".into())).await;

        loop {
            tokio::select! {
                msg = in_rx.recv() => {
                    let Some(msg) = msg else { break };
                    let has_id = msg.get("id").is_some();
                    let method = msg["method"].as_str().map(str::to_owned);
                    match (has_id, method) {
                        // Server -> client request.
                        (true, Some(method)) => {
                            let result = match method.as_str() {
                                "workspace/configuration" => {
                                    let n = msg["params"]["items"].as_array().map(|a| a.len()).unwrap_or(0);
                                    Value::Array(vec![Value::Null; n])
                                }
                                "workspace/workspaceFolders" => json!([{"uri": root_uri, "name": root_name}]),
                                _ => Value::Null,
                            };
                            let _ = write_msg(&mut stdin, &json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})).await;
                        }
                        // Notification.
                        (false, Some(method)) => {
                            let p = &msg["params"];
                            match method.as_str() {
                                "$/progress" => {
                                    let v = &p["value"];
                                    let kind = v["kind"].as_str().unwrap_or_default();
                                    let token = match &p["token"] { Value::String(s) => s.clone(), t => t.to_string() };
                                    let _ = output.send(Event::Progress {
                                        server,
                                        token,
                                        title: v["title"].as_str().unwrap_or_default().to_string(),
                                        message: v["message"].as_str().unwrap_or_default().to_string(),
                                        percentage: v["percentage"].as_u64().map(|x| x as u32),
                                        done: kind == "end",
                                    }).await;
                                }
                                "experimental/serverStatus" => {
                                    let health = p["health"].as_str().unwrap_or("ok");
                                    let quiescent = p["quiescent"].as_bool().unwrap_or(false);
                                    let mut s = if quiescent { "ready".to_string() } else { "working…".to_string() };
                                    if health != "ok" { s = format!("{s} ({health})"); }
                                    if let Some(m) = p["message"].as_str() { s = format!("{s}: {}", m.lines().next().unwrap_or_default()); }
                                    let _ = output.send(Event::Status(server, s)).await;
                                }
                                // Only errors and warnings (type 1/2) are worth printing.
                                "window/showMessage" | "window/logMessage" if p["type"].as_u64().unwrap_or(4) <= 2 => {
                                    eprintln!("[{server}] {}", p["message"].as_str().unwrap_or_default());
                                }
                                _ => {}
                            }
                        }
                        // Response to one of our requests.
                        (true, None) => {
                            let id = msg["id"].as_u64().unwrap_or(0);
                            match pending.remove(&id) {
                                Some(Pending::Initialize) => {
                                    let _ = write_msg(&mut stdin, &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}})).await;
                                    initialized = true;
                                    let name = msg["result"]["serverInfo"]["name"].as_str().unwrap_or(server.name()).to_string();
                                    let version: String = msg["result"]["serverInfo"]["version"].as_str().unwrap_or_default().chars().take(60).collect();
                                    println!("[lsp] {name} {version} initialized");
                                    let _ = output.send(Event::Ready(server, req_tx.clone())).await;
                                    let _ = output.send(Event::Status(server, "running".into())).await;
                                    for r in std::mem::take(&mut queued) {
                                        let _ = req_tx.send(r);
                                    }
                                }
                                Some(Pending::Definition { path, line, character, word }) => {
                                    let pretty = serde_json::to_string_pretty(&msg).unwrap_or_default();
                                    println!("[lsp] {server} textDocument/definition for `{word}` at {path}:{}:{} ->\n{pretty}", line + 1, character + 1);
                                    let summary = summarize_locations(&msg, &cfg.root);
                                    let _ = output.send(Event::Definition { server, summary: format!("`{word}` → {summary}") }).await;
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    }
                }
                req = req_rx.recv() => {
                    let Some(req) = req else { continue };
                    if !initialized {
                        queued.push(req);
                        continue;
                    }
                    match req {
                        Request::GotoDefinition { path, line, character, word } => {
                            let abs = cfg.root.join(&path);
                            let uri = path_to_uri(&abs);
                            if !opened.contains_key(&uri) {
                                let text = tokio::fs::read_to_string(&abs).await.unwrap_or_default();
                                let _ = write_msg(&mut stdin, &json!({
                                    "jsonrpc": "2.0", "method": "textDocument/didOpen",
                                    "params": {"textDocument": {"uri": uri, "languageId": server.language_id(), "version": 1, "text": text}}
                                })).await;
                                opened.insert(uri.clone(), ());
                            }
                            let id = next_id;
                            next_id += 1;
                            pending.insert(id, Pending::Definition { path: path.clone(), line, character, word });
                            let _ = write_msg(&mut stdin, &json!({
                                "jsonrpc": "2.0", "id": id, "method": "textDocument/definition",
                                "params": {"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}
                            })).await;
                        }
                    }
                }
            }
        }
        let status = child.wait().await.map(|s| s.to_string()).unwrap_or_default();
        let _ = output.send(Event::Exited(server, format!("{server} exited ({status})"))).await;
        std::future::pending::<()>().await;
    })
}

fn summarize_locations(msg: &Value, root: &Path) -> String {
    if let Some(err) = msg.get("error") {
        return format!("error: {}", err["message"].as_str().unwrap_or("unknown"));
    }
    let result = &msg["result"];
    let items: Vec<&Value> = match result {
        Value::Array(a) => a.iter().collect(),
        Value::Null => vec![],
        v => vec![v],
    };
    if items.is_empty() {
        return "no definition found".into();
    }
    items
        .iter()
        .map(|l| {
            let uri = l["targetUri"].as_str().or(l["uri"].as_str()).unwrap_or_default();
            let range = if l.get("targetSelectionRange").is_some() { &l["targetSelectionRange"] } else { &l["range"] };
            let line = range["start"]["line"].as_u64().unwrap_or(0) + 1;
            format!("{}:{line}", uri_to_display(uri, root))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        let root = Path::new("/tmp/my repo");
        let u = path_to_uri(&root.join("src/a.rs"));
        assert_eq!(u, "file:///tmp/my%20repo/src/a.rs");
        assert_eq!(uri_to_display(&u, root), "src/a.rs");
    }

    /// Needs Go + network (installs gopls through Mason). Run with
    /// `cargo test -- --ignored gopls_definition` after creating the module below.
    #[tokio::test]
    #[ignore]
    async fn gopls_definition() {
        use futures::StreamExt;
        let root = std::env::temp_dir().join("prt-gopls-test");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("go.mod"), "module example.com/demo\n\ngo 1.21\n").unwrap();
        std::fs::write(
            root.join("main.go"),
            "package main\n\nimport \"fmt\"\n\nfunc greet(name string) string {\n\treturn \"hello \" + name\n}\n\nfunc main() {\n\tfmt.Println(greet(\"world\"))\n}\n",
        )
        .unwrap();
        let mut events = Box::pin(connect(&Config { server: Server::Gopls, root }));
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            let ev = tokio::time::timeout_at(deadline, events.next()).await.expect("timeout").expect("stream ended");
            match ev {
                Event::Ready(_, tx) => tx
                    .send(Request::GotoDefinition { path: "main.go".into(), line: 9, character: 13, word: "greet".into() })
                    .unwrap(),
                Event::Definition { summary, .. } => {
                    assert!(summary.contains("main.go:5"), "{summary}");
                    break;
                }
                Event::Exited(_, e) => panic!("{e}"),
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn framing() {
        let data = b"Content-Length: 13\r\n\r\n{\"id\":1,\"a\":2}".to_vec();
        // Content length deliberately counts 13 bytes of a 14 byte body -> parse fails gracefully.
        let mut r = BufReader::new(&data[..]);
        assert!(read_msg(&mut r).await.is_none());
        let data = b"Content-Length: 14\r\n\r\n{\"id\":1,\"a\":2}".to_vec();
        let mut r = BufReader::new(&data[..]);
        assert_eq!(read_msg(&mut r).await.unwrap()["a"], 2);
    }
}
