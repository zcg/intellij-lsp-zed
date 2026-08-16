//! DAP TCP proxy: Zed connects to *our* port; we forward to the server's real
//! DAP port, rewriting `file://` source URIs in `stackTrace` frames to the
//! absolute paths Zed needs to populate the Variables pane.

use crate::framing::{encode_frame, FrameReader};
use crate::Shared;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub fn serve(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        match stream {
            Ok(client) => {
                let shared = shared.clone();
                thread::spawn(move || handle_client(shared, client));
            }
            Err(_) => break,
        }
    }
}

fn handle_client(shared: Arc<Shared>, mut client: TcpStream) {
    // Wait briefly for the server's real DAP port (reported on
    // `start_debug_server`) before giving up.
    let real_port = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(port) = *shared.real_dap_port.lock().unwrap() {
                break port;
            }
            if Instant::now() > deadline {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    };

    let Ok(mut server) = TcpStream::connect(("127.0.0.1", real_port)) else {
        return;
    };

    // Client → server: raw forwarding.
    let mut server_to_client = server.try_clone().expect("dup server");
    let mut client_to_server = client.try_clone().expect("dup client");
    let forwarder = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match client_to_server.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if server_to_client.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = server_to_client.flush();
                }
            }
        }
    });

    // Server → client: rewrite stackTrace source paths.
    let mut frame = FrameReader::new();
    let mut buf = [0u8; 8192];
    loop {
        match server.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                frame.push(&buf[..n]);
                while let Some(body) = frame.next_frame() {
                    let rewritten = rewrite_stack_trace(&body);
                    if client.write_all(&encode_frame(&rewritten)).is_err() {
                        break;
                    }
                    let _ = client.flush();
                }
            }
        }
    }
    let _ = forwarder.join();
}

/// Rewrites `stackTrace` response frames: `source.path` is a `file://` URI,
/// but Zed's stack-frame handling requires an absolute path.
fn rewrite_stack_trace(body: &[u8]) -> Vec<u8> {
    let mut msg: serde_json::Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return body.to_vec(),
    };
    let is_stack_trace = msg.get("type").and_then(|v| v.as_str()) == Some("response")
        && msg.get("command").and_then(|v| v.as_str()) == Some("stackTrace");
    if is_stack_trace {
        if let Some(frames) = msg
            .pointer_mut("/body/stackFrames")
            .and_then(|v| v.as_array_mut())
        {
            for frame in frames {
                if let Some(uri) = frame
                    .pointer("/source/path")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                {
                    let abs = uri_to_abs_path(&uri);
                    if abs != uri {
                        frame["source"]["path"] = serde_json::Value::String(abs);
                    }
                }
            }
        }
    }
    msg.to_string().into_bytes()
}

/// Converts a `file:///d%3A/Projects/...` URI (as returned by the IntelliJ
/// server in `stackTrace` source paths) into an absolute path like
/// `D:/Projects/...`. Zed's stack-frame handling requires an absolute path
/// (Windows `D:\...` or Unix `/...`) — a URI makes it bail out and the
/// Variables pane stays empty.
fn uri_to_abs_path(uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("file:///") {
        let decoded = percent_decode(rest);
        let bytes = decoded.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            // Windows drive: `d:/Projects/...` → `D:/Projects/...`
            let drive = decoded[..1].to_uppercase();
            return format!("{}:{}", drive, &decoded[2..]);
        }
        // Unix: `/home/user/...`
        return format!("/{decoded}");
    }
    uri.to_string()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_uri() {
        assert_eq!(
            uri_to_abs_path("file:///d%3A/Projects/kkkkt/src/Main.kt"),
            "D:/Projects/kkkkt/src/Main.kt"
        );
        assert_eq!(
            uri_to_abs_path("file:///C:/Users/zcg/proj"),
            "C:/Users/zcg/proj"
        );
    }

    #[test]
    fn unix_uri() {
        assert_eq!(
            uri_to_abs_path("file:///home/user/proj/Main.kt"),
            "/home/user/proj/Main.kt"
        );
    }

    #[test]
    fn non_uri_passthrough() {
        assert_eq!(uri_to_abs_path("D:\\proj\\Main.kt"), "D:\\proj\\Main.kt");
        assert_eq!(uri_to_abs_path(""), "");
    }

    #[test]
    fn percent_decode_handles_colon() {
        assert_eq!(percent_decode("d%3A/Projects"), "d:/Projects");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%25"), "100%");
    }

    #[test]
    fn stack_trace_rewritten() {
        let body = serde_json::json!({
            "type": "response",
            "command": "stackTrace",
            "body": {
                "stackFrames": [
                    { "source": { "path": "file:///d%3A/Projects/kkkkt/src/Main.kt" } }
                ]
            }
        })
        .to_string();
        let out = rewrite_stack_trace(body.as_bytes());
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["body"]["stackFrames"][0]["source"]["path"],
            "D:/Projects/kkkkt/src/Main.kt"
        );
    }

    #[test]
    fn non_stack_trace_untouched() {
        let body = serde_json::json!({
            "type": "response",
            "command": "threads",
            "body": {}
        })
        .to_string();
        assert_eq!(rewrite_stack_trace(body.as_bytes()), body.as_bytes());
    }
}
