//! Minimal HTTP/1.1 endpoint on 127.0.0.1 that accepts
//! `{ "method": "<lsp-method>", "params": {...} }` and forwards it to the
//! language server as a JSON-RPC request. This is how the Zed extension asks
//! the server for a debug port (`start_debug_server`) — the WASM sandbox has
//! no way to talk to the running server directly.

use crate::framing::{content_length, find_subslice};
use crate::Shared;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

pub fn serve(shared: Arc<Shared>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP endpoint");
    let port = listener.local_addr().expect("HTTP addr").port();

    // Publish our HTTP port: <workdir>/proxy/<hex(workspace-uri)> so the
    // extension (whose cwd is the extension workdir) can find it.
    let hex: String = shared
        .workspace_uri
        .bytes()
        .map(|b| format!("{:02x}", b))
        .collect();
    let proxy_dir = std::path::Path::new(&shared.workdir).join("proxy");
    let _ = std::fs::create_dir_all(&proxy_dir);
    let port_file = proxy_dir.join(hex);
    if let Err(e) = std::fs::write(&port_file, port.to_string()) {
        eprintln!("[intellij-lsp-bridge] failed to write port file: {e}");
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = shared.clone();
                thread::spawn(move || handle_connection(shared, stream));
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(shared: Arc<Shared>, mut stream: TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    while let Ok(n) = stream.read(&mut tmp) {
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(body) = extract_body(&buf) {
            let response = handle_body(&shared, body);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            return;
        }
    }
}

fn extract_body(buf: &[u8]) -> Option<&[u8]> {
    let header_end = find_subslice(buf, b"\r\n\r\n")?;
    let header = std::str::from_utf8(&buf[..header_end]).ok()?;
    let len = content_length(header)?;
    let body_start = header_end + 4;
    if buf.len() < body_start + len {
        return None;
    }
    Some(&buf[body_start..body_start + len])
}

fn handle_body(shared: &Shared, body: &[u8]) -> String {
    let request: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            return json_response(&serde_json::json!({
                "error": { "code": -32700, "message": "invalid JSON request" }
            }))
        }
    };
    let method = request
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let params = request
        .get("params")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let result = crate::lsp::send_lsp_request(shared, method, params);
    json_response(&result)
}

fn json_response(value: &serde_json::Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}
