//! Background `claude` process speaking the stream-json protocol over stdin/stdout.

use futures::SinkExt;
use iced::futures::Stream;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Identifies one Claude process. Changing any field restarts the process.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Config {
    pub command: String,
    pub repo: PathBuf,
    pub generation: u64,
}

/// Messages from the app to the Claude process.
#[derive(Debug, Clone)]
pub enum Outgoing {
    User(String),
    Permission { request_id: String, allow: bool, input: Value, updated_permissions: Option<Value> },
    Interrupt,
}

#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub input: Value,
    pub description: Option<String>,
    pub decision_reason: Option<String>,
    pub suggestions: Option<Value>,
}

/// Events from the Claude process to the app.
#[derive(Debug, Clone)]
pub enum Event {
    Started(mpsc::UnboundedSender<Outgoing>),
    Init { session_id: String, model: String, permission_mode: String },
    /// A new assistant message started streaming.
    MessageStart { id: String },
    TextDelta { text: String },
    /// A complete text block of an assistant message.
    AssistantText { id: String, text: String },
    ToolUse { name: String, input: Value },
    ToolResult { is_error: bool, content: String },
    Result { is_error: bool, subtype: String, text: String, cost_usd: Option<f64> },
    Permission(PermissionRequest),
    CancelPermission { request_id: String },
    Stderr(String),
    Exited(String),
}

/// Environment variables that would make a nested `claude` think it runs inside another
/// Claude Code session (relevant when this app is launched from a Claude Code terminal).
const SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
];

pub fn expand_command(cfg: &Config) -> String {
    cfg.command
        .replace("{repo}", &crate::settings::shell_quote(&cfg.repo.to_string_lossy()))
}

pub fn connect(cfg: &Config) -> impl Stream<Item = Event> + use<> {
    let cfg = cfg.clone();
    iced::stream::channel(256, async move |mut output: futures::channel::mpsc::Sender<Event>| {
        let command = expand_command(&cfg);
        eprintln!("[claude] starting: {command} (cwd {})", cfg.repo.display());
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(&cfg.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for v in SESSION_ENV {
            cmd.env_remove(v);
        }
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = output.send(Event::Exited(format!("Failed to start `{command}`: {e}"))).await;
                std::future::pending::<()>().await;
                return;
            }
        };
        crate::debug::dlog!(
            "[claude] spawned sh pid={:?} pgid={} (generation {}); app {}",
            child.id(),
            child.id().map(crate::debug::pgid_of).unwrap_or_default(),
            cfg.generation,
            crate::debug::self_ids()
        );
        let _group = ProcessGroupGuard(child.id());
        let mut stdin = child.stdin.take().expect("stdin");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout")).lines();
        let mut stderr = BufReader::new(child.stderr.take().expect("stderr")).lines();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _ = output.send(Event::Started(tx)).await;

        let mut stderr_tail: Vec<String> = Vec::new();
        let mut interrupt_seq = 0u64;
        let mut stderr_open = true;

        loop {
            tokio::select! {
                line = stdout.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                                let _ = output.send(Event::Stderr(line)).await;
                                continue;
                            };
                            // Reply to control requests we don't support right away.
                            if let Some(reply) = auto_reply(&v) {
                                let _ = write_json(&mut stdin, &reply).await;
                            }
                            for ev in parse_message(&v) {
                                let _ = output.send(ev).await;
                            }
                        }
                        _ => break,
                    }
                }
                line = stderr.next_line(), if stderr_open => {
                    match line {
                        Ok(Some(line)) => {
                            eprintln!("[claude stderr] {line}");
                            stderr_tail.push(line.clone());
                            if stderr_tail.len() > 20 { stderr_tail.remove(0); }
                            let _ = output.send(Event::Stderr(line)).await;
                        }
                        _ => stderr_open = false,
                    }
                }
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    let v = match msg {
                        Outgoing::User(text) => json!({
                            "type": "user",
                            "message": {"role": "user", "content": text},
                        }),
                        Outgoing::Permission { request_id, allow, input, updated_permissions } => {
                            let mut response = if allow {
                                json!({"behavior": "allow", "updatedInput": input})
                            } else {
                                json!({"behavior": "deny", "message": "The user denied this action in the PR review tool."})
                            };
                            if let (true, Some(p)) = (allow, updated_permissions) {
                                response["updatedPermissions"] = p;
                            }
                            json!({
                                "type": "control_response",
                                "response": {"subtype": "success", "request_id": request_id, "response": response},
                            })
                        }
                        Outgoing::Interrupt => {
                            interrupt_seq += 1;
                            json!({
                                "type": "control_request",
                                "request_id": format!("pr-review-interrupt-{interrupt_seq}"),
                                "request": {"subtype": "interrupt"},
                            })
                        }
                    };
                    if let Err(e) = write_json(&mut stdin, &v).await {
                        let _ = output.send(Event::Stderr(format!("write to claude failed: {e}"))).await;
                    }
                }
            }
        }

        let status = child.wait().await.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
        crate::debug::dlog!("[claude] process exited on its own: {status}");
        // Drain whatever is left on stderr for the error message.
        while let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(200), stderr.next_line()).await
        {
            stderr_tail.push(line);
        }
        let mut msg = format!("Claude process exited ({status}).");
        if !stderr_tail.is_empty() {
            msg.push_str("\n\n");
            msg.push_str(&stderr_tail.join("\n"));
        }
        let _ = output.send(Event::Exited(msg)).await;
        std::future::pending::<()>().await;
    })
}

/// Kills the whole process group (sh + claude, or docker exec) when dropped.
struct ProcessGroupGuard(Option<u32>);

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        crate::debug::dlog!("[claude] ProcessGroupGuard dropped, pid={:?}", self.0);
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            crate::debug::dlog!(
                "[claude] about to killpg({pid}, SIGTERM); target pid {pid} is now in pgid {}; app {}",
                crate::debug::pgid_of(pid),
                crate::debug::self_ids()
            );
            // Signal the group directly rather than running `kill -TERM -<pid>`: `kill`
            // implementations parse a negative pid differently, and one of them sent the
            // signal to its own process group, which is ours, killing the app.
            let Ok(pgid) = libc::pid_t::try_from(pid) else { return };
            // SAFETY: getpgrp has no preconditions.
            if pgid <= 1 || pgid == unsafe { libc::getpgrp() } {
                crate::debug::dlog!("[claude] refusing to signal pgid {pgid} (ours or invalid)");
                return;
            }
            // SAFETY: killpg has no memory-safety preconditions.
            let rc = unsafe { libc::killpg(pgid, libc::SIGTERM) };
            crate::debug::dlog!(
                "[claude] killpg returned {rc} {}",
                if rc < 0 { std::io::Error::last_os_error().to_string() } else { String::new() }
            );
        }
    }
}

async fn write_json(stdin: &mut tokio::process::ChildStdin, v: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(v).unwrap_or_default();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await
}

fn auto_reply(v: &Value) -> Option<Value> {
    if v["type"] != "control_request" {
        return None;
    }
    let subtype = v["request"]["subtype"].as_str().unwrap_or_default();
    if subtype == "can_use_tool" {
        return None;
    }
    Some(json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": v["request_id"],
            "error": format!("Unsupported control request: {subtype}"),
        }
    }))
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Translates one stream-json message into app events.
pub fn parse_message(v: &Value) -> Vec<Event> {
    let mut out = Vec::new();
    match v["type"].as_str().unwrap_or_default() {
        "system" if v["subtype"] == "init" => out.push(Event::Init {
            session_id: str_of(&v["session_id"]),
            model: str_of(&v["model"]),
            permission_mode: str_of(&v["permissionMode"]),
        }),
        "stream_event" => {
            // Only top-level (non sub-agent) output is streamed into chats.
            if !v["parent_tool_use_id"].is_null() {
                return out;
            }
            let e = &v["event"];
            match e["type"].as_str().unwrap_or_default() {
                "message_start" => out.push(Event::MessageStart { id: str_of(&e["message"]["id"]) }),
                "content_block_delta" if e["delta"]["type"] == "text_delta" => {
                    out.push(Event::TextDelta { text: str_of(&e["delta"]["text"]) })
                }
                _ => {}
            }
        }
        "assistant" => {
            if !v["parent_tool_use_id"].is_null() {
                return out;
            }
            let id = str_of(&v["message"]["id"]);
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks {
                    match b["type"].as_str().unwrap_or_default() {
                        "text" => out.push(Event::AssistantText { id: id.clone(), text: str_of(&b["text"]) }),
                        "tool_use" => out.push(Event::ToolUse {
                            name: str_of(&b["name"]),
                            input: b["input"].clone(),
                        }),
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            if !v["parent_tool_use_id"].is_null() {
                return out;
            }
            if let Some(blocks) = v["message"]["content"].as_array() {
                for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                    let content = match &b["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                        _ => String::new(),
                    };
                    out.push(Event::ToolResult {
                        is_error: b["is_error"].as_bool().unwrap_or(false),
                        content,
                    });
                }
            }
        }
        "result" => {
            let mut text = str_of(&v["result"]);
            if text.is_empty()
                && let Some(errs) = v["errors"].as_array() {
                    text = errs.iter().map(|e| e.as_str().map(str::to_owned).unwrap_or_else(|| e.to_string())).collect::<Vec<_>>().join("\n");
                }
            out.push(Event::Result {
                is_error: v["is_error"].as_bool().unwrap_or(false),
                subtype: str_of(&v["subtype"]),
                text,
                cost_usd: v["total_cost_usd"].as_f64(),
            })
        }
        "control_request" if v["request"]["subtype"] == "can_use_tool" => {
            let r = &v["request"];
            out.push(Event::Permission(PermissionRequest {
                request_id: str_of(&v["request_id"]),
                tool_name: str_of(&r["tool_name"]),
                input: r["input"].clone(),
                description: r["description"].as_str().map(str::to_owned),
                decision_reason: r["decision_reason"].as_str().map(str::to_owned),
                suggestions: r["permission_suggestions"]
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .map(|a| Value::Array(a.clone())),
            }))
        }
        "control_cancel_request" => out.push(Event::CancelPermission { request_id: str_of(&v["request_id"]) }),
        _ => {}
    }
    out
}

/// Short human readable summary of a tool call for chat logs.
pub fn tool_summary(name: &str, input: &Value) -> String {
    let detail = ["command", "file_path", "path", "pattern", "url", "query", "description"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .unwrap_or_default();
    let detail: String = detail.lines().next().unwrap_or_default().chars().take(140).collect();
    if detail.is_empty() { name.to_string() } else { format!("{name}: {detail}") }
}

/// Extracts the annotation map from Claude's answer to the annotate prompt.
pub fn parse_annotations(text: &str) -> Result<std::collections::BTreeMap<u32, String>, String> {
    let start = text.find('{').ok_or("No JSON object found in Claude's response")?;
    let end = text.rfind('}').ok_or("No JSON object found in Claude's response")?;
    if end < start {
        return Err("Malformed JSON in Claude's response".into());
    }
    let v: Value = serde_json::from_str(&text[start..=end])
        .map_err(|e| format!("Could not parse annotations JSON: {e}"))?;
    let obj = v.get("annotations").unwrap_or(&v);
    let mut map = std::collections::BTreeMap::new();
    match obj {
        Value::Object(o) => {
            for (k, val) in o {
                if let (Ok(n), Some(s)) = (k.trim().parse::<u32>(), val.as_str())
                    && !s.trim().is_empty() {
                        map.insert(n, s.trim().to_string());
                    }
            }
        }
        Value::Array(items) => {
            for it in items {
                if let (Some(n), Some(s)) = (it["line"].as_u64(), it["comment"].as_str())
                    && !s.trim().is_empty() {
                        map.insert(n as u32, s.trim().to_string());
                    }
            }
        }
        _ => return Err("Unexpected annotations format".into()),
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations() {
        let m = parse_annotations("Here:\n```json\n{\"annotations\": {\"1\": \"a\", \"2\": \"\", \"x\": \"b\"}}\n```").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[&1], "a");
        let m = parse_annotations("{\"annotations\": [{\"line\": 3, \"comment\": \"c\"}]}").unwrap();
        assert_eq!(m[&3], "c");
    }

    #[test]
    fn permission() {
        let v: Value = serde_json::from_str(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"permission_suggestions":[]}}"#).unwrap();
        let evs = parse_message(&v);
        assert!(matches!(&evs[0], Event::Permission(p) if p.tool_name == "Bash" && p.suggestions.is_none()));
        assert!(auto_reply(&v).is_none());
    }
}
