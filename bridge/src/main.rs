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
mod lsp;

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
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
    pub tx: Sender<serde_json::Value>,
}

/// Everything the worker threads share.
pub struct Shared {
    pub next_id: AtomicU64,
    pub pending: Mutex<HashMap<u64, PendingRequest>>,
    pub server_stdin: Mutex<ChildStdin>,
    pub real_dap_port: Mutex<Option<u16>>,
    pub dap_proxy_port: u16,
    pub workspace_uri: String,
    pub workdir: String,
    pub log: Mutex<fs::File>,
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
        real_dap_port: Mutex::new(None),
        dap_proxy_port,
        workspace_uri: workspace_uri.clone(),
        workdir: workdir.clone(),
        log: Mutex::new(log),
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

    // Zed's LSP stdin → server (transparent forwarding).
    {
        let shared = shared.clone();
        thread::spawn(move || {
            let mut frame = framing::FrameReader::new();
            let mut buf = [0u8; 8192];
            loop {
                match std::io::stdin().read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        frame.push(&buf[..n]);
                        while let Some(body) = frame.next_frame() {
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

    http::serve(shared.clone());
    dap::serve(dap_listener, shared.clone());

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
    let _ = child.wait();
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
