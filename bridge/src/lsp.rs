//! LSP message routing: requests the bridge issues (via HTTP), server-initiated
//! requests, and notifications destined for Zed.

use crate::framing::encode_frame;
use crate::{jars, PendingRequest, Shared, REQUEST_TIMEOUT};
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::mpsc::channel;

/// Sends an LSP request to the server and blocks until its response arrives.
/// Returns the full response JSON (`{ result | error }` shape).
pub fn send_lsp_request(
    shared: &Shared,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = channel();
    let command = params.get("command").and_then(|c| c.as_str()).map(str::to_string);
    shared.pending.lock().unwrap().insert(
        id,
        PendingRequest {
            method: method.to_string(),
            command,
            tx,
        },
    );
    let msg = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    {
        let mut stdin = shared.server_stdin.lock().unwrap();
        if stdin.write_all(&encode_frame(msg.to_string().as_bytes())).is_err() {
            shared.pending.lock().unwrap().remove(&id);
            return serde_json::json!({
                "error": { "code": -1, "message": "failed to write to language server" }
            });
        }
        let _ = stdin.flush();
    }
    match rx.recv_timeout(REQUEST_TIMEOUT) {
        Ok(message) => message,
        Err(_) => {
            shared.pending.lock().unwrap().remove(&id);
            serde_json::json!({
                "error": { "code": -1, "message": format!("timeout waiting for {method}") }
            })
        }
    }
}

/// Routes one LSP message received from the server.
pub fn handle_server_message(shared: &Shared, body: &[u8]) {
    let msg: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return,
    };
    let id = msg.get("id").and_then(|v| v.as_u64());

    if let Some(id) = id {
        // Response to a request the bridge itself issued → resolve the HTTP caller.
        if let Some(pending) = shared.pending.lock().unwrap().remove(&id) {
            // `start_debug_server`: hand Zed *our* DAP proxy port and remember
            // the server's real port to forward connections to. Only this
            // specific command's numeric result is rewritten — any other
            // `workspace/executeCommand` passes through untouched.
            let is_start_debug_server = pending.method == "workspace/executeCommand"
                && pending.command.as_deref() == Some("start_debug_server");
            if is_start_debug_server && msg.get("result").and_then(|r| r.as_u64()).is_some() {
                if let Some(real) = msg.get("result").and_then(|r| r.as_u64()) {
                    *shared.real_dap_port.lock().unwrap() = Some(real as u16);
                }
                let mut rewritten = msg.clone();
                rewritten["result"] = serde_json::json!(shared.dap_proxy_port);
                let _ = pending.tx.send(rewritten);
            } else {
                let _ = pending.tx.send(msg.clone());
            }
            return;
        }

        if msg.get("method").is_some() {
            // Server-initiated request.
            if msg.get("method").and_then(|m| m.as_str()) == Some("window/showMessageRequest") {
                // The server wants to ask the user something (e.g. which build
                // tool to use). Forward it to Zed, which renders the prompt and
                // replies. Intercepting it would swallow the prompt.
                forward_to_zed(shared, &msg);
            } else {
                // Acknowledge other requests with a null result.
                let reply = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": null });
                let mut stdin = shared.server_stdin.lock().unwrap();
                let _ = stdin.write_all(&encode_frame(reply.to_string().as_bytes()));
                let _ = stdin.flush();
            }
            return;
        }
    }

    // Notification or a response meant for Zed — forward (rewriting any
    // `jar://` source URIs into local files so definition jumps into JDK and
    // third-party libraries open in Zed).
    forward_to_zed(shared, &msg);
}

/// 转发消息给 Zed。定义/悬停等响应里若含 `jar://` / `jrt://` URI(第三方库
/// 与 JDK 源码),先尝试本地提取改写为 `file://`;本地拿不到的交给后台 worker
/// 向 IntelliJ 服务器要文本(`workspace/textDocumentContent`)后再转发。
fn forward_to_zed(shared: &Shared, msg: &serde_json::Value) {
    let mut uris = Vec::new();
    jars::collect_virtual_uris(msg, &mut uris);
    if uris.is_empty() {
        write_stdout(msg);
        return;
    }

    let mut out = msg.clone();
    let mut pending = Vec::new();
    for uri in &uris {
        if let Some(file_uri) = shared.jars.rewrite(uri) {
            jars::replace_uri(&mut out, uri, &file_uri);
        } else if let Some(cached) = shared.jars.cached(uri) {
            jars::replace_uri(&mut out, uri, &cached);
        } else {
            pending.push(uri.clone());
        }
    }

    if pending.is_empty() {
        write_stdout(&out);
    } else {
        // 入队,worker 拿到文本、落盘、改写后再转发(保持顺序)。
        shared
            .rewrite_queue
            .lock()
            .unwrap()
            .push_back((out, pending));
    }
}

fn write_stdout(msg: &serde_json::Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&encode_frame(msg.to_string().as_bytes()));
    let _ = stdout.flush();
}
