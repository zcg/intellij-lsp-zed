#!/usr/bin/env node
"use strict";
// IntelliJ LSP proxy — wraps the IntelliJ language server.
//
// Responsibilities:
//   1. Spawns `intellij-server --stdio` and transparently forwards stdio
//      between Zed and the server (so normal LSP works unchanged).
//   2. Exposes an HTTP endpoint on 127.0.0.1 that accepts
//      `{ "method": "<lsp-method>", "params": {...} }` and forwards it to the
//      server as a JSON-RPC request. This is how the Zed extension asks the
//      server for a debug port (`start_debug_server`) — zed_extension_api has
//      no way to send LSP requests directly yet.
//   3. Proxies the DAP TCP channel. When the extension asks for a debug port,
//      we hand it OUR port instead, forward the connection to the server's real
//      DAP port, and rewrite `stackTrace` source paths from `file:///...` URIs
//      to absolute paths (Zed's Variables pane requires an absolute path).
//   4. Writes its HTTP port to `<workdir>/proxy/<hex(workspaceUri)>` so the
//      extension (whose working directory is the extension workdir) can find
//      it. The proxy's own working directory is the *project* root, so the
//      workdir is passed explicitly.
//
// Usage: node proxy.cjs <intellij-server-path> <workspace-uri> <workdir>

const { spawn } = require("node:child_process");
const http = require("node:http");
const net = require("node:net");
const { Transform } = require("node:stream");
const fs = require("node:fs");
const path = require("node:path");

const serverPath = process.argv[2];
const workspaceUri = process.argv[3];
const workdir = process.argv[4];
const proxyDir = path.join(workdir, "proxy");
// HTTP request IDs start high so they never collide with Zed's own LSP IDs.
const ID_BASE = 1_000_000;
const REQUEST_TIMEOUT_MS = 60_000;

const child = spawn(serverPath, ["--stdio"], {
  cwd: process.cwd(),
  env: { ...process.env },
  stdio: ["pipe", "pipe", "pipe"],
});

// ---- LSP framing ----
let buffer = Buffer.alloc(0);
let nextId = ID_BASE;
const pending = new Map(); // id -> { method, resolve }

function pump() {
  let headerEnd = buffer.indexOf("\r\n\r\n");
  if (headerEnd === -1) return;
  const header = buffer.subarray(0, headerEnd).toString();
  const m = /Content-Length:\s*(\d+)/i.exec(header);
  if (!m) return;
  const len = parseInt(m[1], 10);
  const bodyStart = headerEnd + 4;
  if (buffer.length < bodyStart + len) return;
  const body = buffer.subarray(bodyStart, bodyStart + len);
  buffer = buffer.subarray(bodyStart + len);
  let msg;
  try {
    msg = JSON.parse(body.toString());
  } catch {
    pump();
    return;
  }
  handleServerMessage(msg);
  pump();
}

child.stdout.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  pump();
});

function encodeLsp(msg) {
  const body = JSON.stringify(msg);
  return `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
}

/// Converts a `file:///d%3A/Projects/...` URI (as returned by the IntelliJ
/// server in `stackTrace` source paths) into an absolute path like
/// `D:/Projects/...`. Zed's stack-frame handling requires an absolute path
/// (Windows `D:\...` or Unix `/...`) — a URI makes it bail out and the
/// Variables pane stays empty.
function uriToAbsPath(uri) {
  if (typeof uri !== "string") return uri;
  // file:///d%3A/... or file:///d:/... or file:///C:/...
  const m = /^file:\/\/\/([a-zA-Z])(?::|%3A)(.*)$/.exec(uri);
  if (m) {
    let rest = m[2];
    try {
      rest = decodeURIComponent(rest);
    } catch {}
    return m[1].toUpperCase() + ":" + rest;
  }
  // Unix: file:///home/user/... → /home/user/...
  if (uri.startsWith("file:///")) {
    let rest = uri.slice("file:///".length);
    try {
      rest = decodeURIComponent(rest);
    } catch {}
    return "/" + rest;
  }
  return uri;
}

/// Rewrites DAP stackTrace responses: source paths are reported as
/// `file:///...` URIs, but Zed needs absolute paths for the Variables pane.
class DapRewrite extends Transform {
  constructor() {
    super();
    this.buf = Buffer.alloc(0);
  }
  _transform(chunk, _enc, cb) {
    this.buf = Buffer.concat([this.buf, chunk]);
    while (true) {
      const idx = this.buf.indexOf("\r\n\r\n");
      if (idx === -1) break;
      const m = /Content-Length:\s*(\d+)/i.exec(this.buf.subarray(0, idx).toString());
      if (!m) break;
      const len = parseInt(m[1], 10);
      const bs = idx + 4;
      if (this.buf.length < bs + len) break;
      const raw = this.buf.subarray(bs, bs + len).toString();
      this.buf = this.buf.subarray(bs + len);
      let msg;
      try {
        msg = JSON.parse(raw);
      } catch {
        continue;
      }
      if (
        msg &&
        msg.type === "response" &&
        msg.command === "stackTrace" &&
        msg.body &&
        Array.isArray(msg.body.stackFrames)
      ) {
        for (const frame of msg.body.stackFrames) {
          if (frame && frame.source && frame.source.path) {
            frame.source.path = uriToAbsPath(frame.source.path);
          }
        }
      }
      this.push(Buffer.from(encodeLsp(msg)));
    }
    cb();
  }
}

// ---- DAP TCP proxy ----
// The real DAP port the server last reported (set on `start_debug_server`).
let realDapPort = null;
// Zed connects to OUR port; we forward to the real port.
const dapProxy = net.createServer((clientSock) => {
  if (realDapPort !== null) {
    forwardDap(clientSock, realDapPort);
    return;
  }
  // Wait briefly for a debug port to be reported before giving up.
  const timer = setInterval(() => {
    if (realDapPort !== null) {
      clearInterval(timer);
      forwardDap(clientSock, realDapPort);
    }
  }, 200);
  setTimeout(() => {
    clearInterval(timer);
    if (!clientSock.destroyed) clientSock.destroy();
  }, 60_000);
});
let dapProxyPort = null;
dapProxy.listen(0, "127.0.0.1", () => {
  dapProxyPort = dapProxy.address().port;
});

function forwardDap(clientSock, realPort) {
  const serverSock = net.connect(realPort, "127.0.0.1");
  clientSock.pipe(serverSock);
  serverSock.pipe(new DapRewrite()).pipe(clientSock);
  serverSock.on("error", () => clientSock.destroy());
  clientSock.on("error", () => serverSock.destroy());
}

function handleServerMessage(msg) {
  if (msg.id !== undefined && pending.has(msg.id)) {
    // Response to a request the proxy itself issued over HTTP — resolve it,
    // and do NOT forward to Zed (it wasn't from Zed).
    const entry = pending.get(msg.id);
    pending.delete(msg.id);
    // If this is the `start_debug_server` response, hand Zed OUR DAP proxy
    // port and remember where to forward connections.
    if (
      entry.method === "workspace/executeCommand" &&
      typeof msg.result === "number" &&
      dapProxyPort !== null
    ) {
      realDapPort = msg.result;
      msg.result = dapProxyPort;
    }
    entry.resolve(msg);
  } else if (msg.id !== undefined && msg.method) {
    if (msg.method === "window/showMessageRequest") {
      // The server wants to ask the user something (e.g. which build tool to
      // use). Forward it to Zed, which renders a prompt and replies with the
      // user's choice. Intercepting it (as we do for other server-initiated
      // requests) would swallow the prompt and make the import silently skip.
      process.stdout.write(encodeLsp(msg));
    } else {
      // Server-initiated request (e.g. client/registerCapability) — ack it.
      const reply = JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: null });
      child.stdin.write(`Content-Length: ${Buffer.byteLength(reply)}\r\n\r\n${reply}`);
    }
  } else {
    // Response or notification meant for Zed — forward unchanged.
    process.stdout.write(encodeLsp(msg));
  }
}

// Forward Zed's stdin to the server.
process.stdin.pipe(child.stdin);

function lspRequest(method, params) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    let settled = false;
    const timer = setTimeout(() => {
      if (!settled) {
        settled = true;
        pending.delete(id);
        reject(new Error(`timeout waiting for ${method}`));
      }
    }, REQUEST_TIMEOUT_MS);
    pending.set(id, { method, resolve: (m) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(m);
    }});
    const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params });
    child.stdin.write(`Content-Length: ${Buffer.byteLength(msg)}\r\n\r\n${msg}`);
  });
}

// ---- HTTP interface for the extension ----
const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    res.writeHead(200, { "Content-Type": "application/json" });
    try {
      const parsed = JSON.parse(body || "{}");
      const r = await lspRequest(parsed.method, parsed.params || {});
      res.end(JSON.stringify(r));
    } catch (e) {
      res.end(JSON.stringify({ error: { code: -1, message: String((e && e.message) || e), data: null } }));
    }
  });
});

server.listen(0, "127.0.0.1", () => {
  const port = server.address().port;
  try {
    const hex = Buffer.from(workspaceUri, "utf8").toString("hex");
    fs.mkdirSync(proxyDir, { recursive: true });
    fs.writeFileSync(path.join(proxyDir, hex), String(port), "utf8");
  } catch (e) {
    process.stderr.write(`[intellij-lsp-proxy] failed to write port file: ${e}\n`);
  }
});

function cleanupPortFile() {
  try {
    const hex = Buffer.from(workspaceUri, "utf8").toString("hex");
    const pf = path.join(proxyDir, hex);
    if (fs.existsSync(pf)) fs.unlinkSync(pf);
  } catch {}
}

child.on("exit", (code) => {
  cleanupPortFile();
  try {
    server.close();
    dapProxy.close();
  } catch {}
  process.exit(code || 0);
});

// Clean up the port file on normal shutdown too (not just child exit).
process.on("SIGTERM", () => {
  cleanupPortFile();
  child.kill();
});
process.on("SIGINT", () => {
  cleanupPortFile();
  child.kill();
});
process.on("exit", cleanupPortFile);
