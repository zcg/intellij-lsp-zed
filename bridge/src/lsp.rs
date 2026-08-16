//! LSP message routing: requests the bridge issues (via HTTP), server-initiated
//! requests, and notifications destined for Zed.

use crate::framing::encode_frame;
use crate::{PendingRequest, Shared, REQUEST_TIMEOUT};
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
    shared
        .pending
        .lock()
        .unwrap()
        .insert(id, PendingRequest {
            method: method.to_string(),
            tx,
        });
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
            // the server's real port to forward connections to.
            if pending.method == "workspace/executeCommand"
                && msg.get("result").and_then(|r| r.as_u64()).is_some()
            {
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
                forward_to_zed(&msg);
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

    // Notification or a response meant for Zed — forward unchanged.
    forward_to_zed(&msg);
}

fn forward_to_zed(msg: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&encode_frame(msg.to_string().as_bytes()));
    let _ = out.flush();
}
