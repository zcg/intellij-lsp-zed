//! intellij-lsp-bridge — Rust replacement for the Node proxy.
//!
//! Wraps the IntelliJ language server (`intellij-server --stdio`) and gives
//! the Zed extension three things the WASM sandbox cannot do itself:
//!
//! 1. **LSP stdio forwarding** between Zed and the server (transparent).
//! 2. **An HTTP endpoint** (127.0.0.1) the extension can POST LSP requests to
//!    (`start_debug_server` etc.) — the WASM sandbox has no way to talk to
//!    the running server directly.
//! 3. **A DAP TCP proxy**: Zed connects to our port; we forward to the
//!    server's real DAP port, rewriting `file://` source URIs in
//!    `stackTrace` frames to the absolute paths Zed's Variables pane needs.
//!
//! The HTTP port is published at `<workdir>/proxy/<hex(workspace-uri)>` so the
//! extension can find it (its cwd is the extension workdir, while ours is the
//! *project* root). Server stderr is appended to
//! `<workdir>/intellij-lsp-bridge.log` to diagnose crashes.
//!
//! Usage: `intellij-lsp-bridge <intellij-server> <workspace-uri> <workdir>`

mod dap;
mod framing;
mod http;
mod jars;
mod lsp;

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// LSP request IDs the bridge issues start here so they never collide with
/// Zed's own IDs (Zed uses small sequential IDs).
pub const ID_BASE: u64 = 1_000_000;
/// Timeout for LSP requests issued over the HTTP endpoint.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// An LSP request issued by the bridge, waiting for its response.
pub struct PendingRequest {
    pub method: String,
    /// The `command` name for `workspace/executeCommand` requests (used to
    /// detect `start_debug_server` responses), `None` otherwise.
    pub command: Option<String>,
    pub tx: Sender<serde_json::Value>,
}

/// Everything the worker threads share.
pub struct Shared {
    pub next_id: AtomicU64,
    pub pending: Mutex<HashMap<u64, PendingRequest>>,
    pub server_stdin: Mutex<ChildStdin>,
    pub child: Mutex<Child>,
    pub real_dap_port: Mutex<Option<u16>>,
    pub dap_proxy_port: u16,
    pub workspace_uri: String,
    pub workdir: String,
    pub log: Mutex<fs::File>,
    /// jar:// → 本地源码提取缓存(JDK / 第三方库跳转)。
    pub jars: jars::Cache,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: intellij-lsp-bridge <intellij-server> <workspace-uri> <workdir>");
        std::process::exit(2);
    }
    let server_path = args[1].clone();
    let workspace_uri = args[2].clone();
    let workdir = args[3].clone();

    fs::create_dir_all(&workdir).ok();
    let log_path = Path::new(&workdir).join("intellij-lsp-bridge.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|_| fs::File::create(&log_path).expect("open log file"));
    let mut log = log;
    let _ = writeln!(
        log,
        "--- bridge {} starting: server={server_path} uri={workspace_uri} ---",
        env!("CARGO_PKG_VERSION")
    );

    let mut child = match Command::new(&server_path)
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(log, "failed to spawn language server: {e}");
            std::process::exit(1);
        }
    };
    let server_stdin = child.stdin.take().expect("server stdin");
    let mut server_stdout = child.stdout.take().expect("server stdout");
    let mut server_stderr = child.stderr.take().expect("server stderr");

    // DAP proxy port is fixed up-front so it can be handed to Zed inside the
    // `start_debug_server` response.
    let dap_listener = TcpListener::bind("127.0.0.1:0").expect("bind DAP proxy");
    let dap_proxy_port = dap_listener.local_addr().expect("DAP addr").port();

    let shared = Arc::new(Shared {
        next_id: AtomicU64::new(ID_BASE),
        pending: Mutex::new(HashMap::new()),
        server_stdin: Mutex::new(server_stdin),
        child: Mutex::new(child),
        real_dap_port: Mutex::new(None),
        dap_proxy_port,
        workspace_uri: workspace_uri.clone(),
        workdir: workdir.clone(),
        log: Mutex::new(log),
        jars: jars::Cache::new(&workdir),
    });

    // Server stderr → log file (how we diagnose server crashes).
    {
        let shared = shared.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match server_stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut log) = shared.log.lock() {
                            let _ = log.write_all(&buf[..n]);
                            let _ = log.flush();
                        }
                    }
                }
            }
        });
    }

    // Zed's LSP stdin → server (transparent forwarding). When Zed closes the
    // pipe (stopping/reloading the language server), EOF here is the signal to
    // terminate the server child — otherwise the JVM leaks and keeps the
    // workspace/port locked for the next session.
    //
    // `textDocument/inlayHint` requests are answered locally (empty result):
    // the IntelliJ server always fails them with "InlayOptions$Companion
    // .create, parameter raw", so forwarding is a doomed round-trip that
    // spams error logs and slows the server down.
    {
        let shared = shared.clone();
        thread::spawn(move || {
            let mut frame = framing::FrameReader::new();
            let mut buf = [0u8; 8192];
            loop {
                match std::io::stdin().read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = shared.child.lock().unwrap().kill();
                        break;
                    }
                    Ok(n) => {
                        frame.push(&buf[..n]);
                        while let Some(body) = frame.next_frame() {
                            if let Some(reply) = intercept_inlay_hint(&body) {
                                let mut stdout = std::io::stdout().lock();
                                let _ =
                                    stdout.write_all(&framing::encode_frame(reply.to_string().as_bytes()));
                                let _ = stdout.flush();
                                continue;
                            }
                            let mut stdin = shared.server_stdin.lock().unwrap();
                            if stdin.write_all(&framing::encode_frame(&body)).is_err() {
                                return;
                            }
                            let _ = stdin.flush();
                        }
                    }
                }
            }
        });
    }

    // HTTP control endpoint (for the extension) and the DAP TCP proxy run on
    // their own threads — both must accept connections concurrently, and the
    // main thread below stays on server stdout routing.
    {
        let shared = shared.clone();
        thread::spawn(move || http::serve(shared));
    }
    {
        let shared = shared.clone();
        thread::spawn(move || dap::serve(dap_listener, shared));
    }

    // Main loop: server stdout → route.
    let mut frame = framing::FrameReader::new();
    let mut buf = [0u8; 8192];
    loop {
        match server_stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                frame.push(&buf[..n]);
                while let Some(body) = frame.next_frame() {
                    lsp::handle_server_message(&shared, &body);
                }
            }
        }
    }

    let _ = writeln!(
        shared.log.lock().unwrap(),
        "--- server stdout closed; bridge exiting ---"
    );
    cleanup_port_file(&workdir, &workspace_uri);
    let _ = shared.child.lock().unwrap().wait();
    std::process::exit(0);
}

fn cleanup_port_file(workdir: &str, workspace_uri: &str) {
    let hex: String = workspace_uri
        .bytes()
        .map(|b| format!("{:02x}", b))
        .collect();
    let port_file = Path::new(workdir).join("proxy").join(hex);
    let _ = fs::remove_file(port_file);
}

/// 若 Zed→服务器 的请求是 `textDocument/inlayHint`,返回要直接回给 Zed 的
/// 空响应(服务器对该请求必然报错,拦截可消除报错刷屏并减轻服务器负担);
/// 其他请求返回 `None`,照常转发。
fn intercept_inlay_hint(body: &[u8]) -> Option<serde_json::Value> {
    let msg: serde_json::Value = serde_json::from_slice(body).ok()?;
    if msg.get("method").and_then(|m| m.as_str()) != Some("textDocument/inlayHint") {
        return None;
    }
    let id = msg.get("id")?.clone();
    Some(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        // LSP 3.17: textDocument/inlayHint 的 result 是 InlayHint[] 数组,
        // 不是 {items: []} 对象 — Zed 按数组反序列化。
        "result": []
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercepts_inlay_hint_with_empty_result() {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 14,
            "method": "textDocument/inlayHint",
            "params": { "textDocument": { "uri": "file:///x.java" }, "range": {} }
        });
        let reply = intercept_inlay_hint(req.to_string().as_bytes()).expect("intercepted");
        assert_eq!(reply["id"], 14);
        // LSP 规范:result 是数组(不是 {items: []} 对象)。
        assert_eq!(reply["result"], serde_json::json!([]));
        assert!(reply.get("error").is_none());
    }

    #[test]
    fn forwards_other_requests() {
        let hover = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 13,
            "method": "textDocument/hover",
            "params": {}
        });
        assert!(intercept_inlay_hint(hover.to_string().as_bytes()).is_none());

        let definition = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 15,
            "method": "textDocument/definition",
            "params": {}
        });
        assert!(intercept_inlay_hint(definition.to_string().as_bytes()).is_none());
    }

    #[test]
    fn forwards_non_json() {
        assert!(intercept_inlay_hint(b"not json at all").is_none());
    }

    #[test]
    fn inlay_hint_without_id_is_forwarded() {
        let notify = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/inlayHint",
            "params": {}
        });
        assert!(intercept_inlay_hint(notify.to_string().as_bytes()).is_none());
    }
}
