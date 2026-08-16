use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::{cmp::Ordering, fs};
use zed_extension_api::{
    self as zed, download_file, make_file_executable, resolve_tcp_template,
    set_language_server_installation_status, Command, DebugAdapterBinary, DebugConfig,
    DebugRequest, DebugScenario, DebugTaskDefinition, DownloadedFileType, Extension,
    LanguageServerId, LanguageServerInstallationStatus, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, TcpArgumentsTemplate, Worktree,
};

// ---------------------------------------------------------------------------
// Pinned server metadata — v263.2689.0
// ---------------------------------------------------------------------------
//
// The IntelliJ LSP server is proprietary software distributed by JetBrains
// under their EULA. The extension NEVER queries third-party registries (such
// as the Open VSX API) at runtime: that pattern was rejected by the Zed
// extension registry. Instead, the server is downloaded directly from
// JetBrains' own CDN, pinned to a verified version, and only after the user has
// explicitly accepted the EULA (see the `accept_jetbrains_eula` setting).
//
// The pinned version and platform URLs live in `server-artifacts.json`,
// which was built from the official `extension/server-bundle.json` inside
// each platform's `.vsix` wrapper.  The file is embedded at compile time so
// the WASM binary carries the pin — no runtime queries needed.  A CI workflow
// (`auto-update.yml`) updates the JSON whenever JetBrains publishes a new
// build, so the pin stays current without manual maintenance.
//
// See the README section "Updating the pinned server" for how the JSON
// was originally captured and how to verify it.

#[derive(Debug, Deserialize)]
struct ServerArtifactsFile {
    version: String,
    platforms: HashMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
struct PlatformEntry {
    url: String,
    #[serde(default)]
    #[allow(dead_code)]
    sha256: Option<String>,
    file_type: String,
}

fn server_artifacts() -> Result<ServerArtifactsFile, String> {
    serde_json::from_str(include_str!("../server-artifacts.json"))
        .map_err(|e| format!("failed to parse server-artifacts.json: {e}"))
}

/// Maps `zed::current_platform()` to the key used in `server-artifacts.json`.
fn platform_key(os: &zed::Os, arch: &zed::Architecture) -> &'static str {
    match (os, arch) {
        (zed::Os::Mac, zed::Architecture::Aarch64) => "mac-aarch64",
        (zed::Os::Mac, _) => "mac-x86_64",
        (zed::Os::Linux, zed::Architecture::Aarch64) => "linux-aarch64",
        (zed::Os::Linux, _) => "linux-x86_64",
        (zed::Os::Windows, zed::Architecture::Aarch64) => "windows-aarch64",
        (zed::Os::Windows, _) => "windows-x86_64",
    }
}

/// Returns `(version, download_url, file_type)` for the current platform.
fn artifact_for_platform() -> Result<(String, String, DownloadedFileType), String> {
    let data = server_artifacts()?;
    let platform = zed::current_platform();
    let key = platform_key(&platform.0, &platform.1);
    let entry = data
        .platforms
        .get(key)
        .ok_or_else(|| {
            format!(
                "IntelliJ LSP server build not available for your platform ({:?}-{:?}). \
                 You can still use the extension by downloading the server manually and \
                 setting \"server_path\". See https://blog.jetbrains.com/idea/2026/08/intellij-idea-goes-lsp/",
                platform.0, platform.1,
            )
        })?;
    let file_type = match entry.file_type.as_str() {
        "zip" => DownloadedFileType::Zip,
        "gzip-tar" => DownloadedFileType::GzipTar,
        other => {
            return Err(format!(
                "unknown file type in server-artifacts.json: {other}"
            ))
        }
    };
    Ok((data.version.clone(), entry.url.clone(), file_type))
}

// The EULA hash must match the one the real extension computes from the
// `LICENSE.txt` inside the vsix wrapper.  That file and the `EULA.txt` shipped
// inside the server archive are byte-for-byte identical for v263.2689.0
// (verified with `diff` + shasum).  If a future build ever diverges, the
// server startup will report the expected hash and the user can set the
// `eula_hash` setting to the correct value — the README documents this
// bootstrap path.  Re-verify identity on each version bump.
#[allow(dead_code)]
const SERVER_EULA_HASH: &str = "34d850193ee04897";

/// Executable names the server ships under, per platform.
const SERVER_BINARIES: [&str; 2] = ["intellij-server", "intellij-server.exe"];

/// Shown when the user has not accepted the JetBrains EULA.
const EULA_GATE_MESSAGE: &str = concat!(
    "The IntelliJ LSP server is proprietary software by JetBrains. Before it can be\n",
    "downloaded and run you must read and accept the JetBrains EULA:\n",
    "https://www.jetbrains.com/legal/docs/toolbox/user/\n",
    "(the exact license also ships as EULA.txt inside the server bundle).\n",
    "\n",
    "To accept, add this to your Zed settings.json and reload the window:\n",
    "\n",
    "{\n",
    "  \"lsp\": {\n",
    "    \"intellij-server\": {\n",
    "      \"settings\": {\n",
    "        \"accept_jetbrains_eula\": true\n",
    "      }\n",
    "    }\n",
    "  }\n",
    "}",
);

/// Shown when neither automatic nor manual mode is configured.
#[allow(dead_code)]
const MANUAL_MODE_MESSAGE: &str = concat!(
    "The IntelliJ LSP server is not installed, and this build of the extension has\n",
    "no pinned automatic download configured (it deliberately does not fetch the\n",
    "server from third-party registries such as the Open VSX API). Either:\n",
    "\n",
    "1. Download the server once (see https://blog.jetbrains.com/idea/2026/08/\n",
    "   intellij-idea-goes-lsp/) and point the extension at the extracted\n",
    "   `intellij-server` executable via the \"server_path\" setting, or\n",
    "2. Configure automatic download by setting \"server_version\" and\n",
    "   \"server_download_url\" in \"lsp\".\"intellij-server\".\"settings\" to a\n",
    "   version and JetBrains CDN URL you trust.\n",
    "\n",
    "Both options also require \"accept_jetbrains_eula\": true.",
);

/// Settings the user can configure under `lsp.intellij-server.settings`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
struct IntellijServerSettings {
    /// Explicit consent to the JetBrains EULA. No download or execution
    /// happens unless this is `true`.
    accept_jetbrains_eula: bool,
    /// Path to an already-extracted `intellij-server` executable (manual mode).
    server_path: Option<String>,
    /// Override the pinned server version (automatic mode).
    server_version: Option<String>,
    /// Override the pinned JetBrains download URL (automatic mode).
    server_download_url: Option<String>,
    /// EULA acceptance hash override (advanced — see README).
    eula_hash: Option<String>,

    // --- JetBrains server settings (mapped 1:1 from the real extension) ---
    /// `intellij.additionalJvmArgs` — JVM options for the language server
    /// process (e.g. `["-Xmx4g"]`). Passed via the `IJ_JAVA_OPTIONS`
    /// environment variable, which the JetBrains launcher reads on startup.
    #[serde(rename = "intellij.additionalJvmArgs")]
    additional_jvm_args: Option<Vec<String>>,

    /// `intellij.dataSharing` — independent consent axis for telemetry.
    /// Valid values: `"full"`, `"anonymous"`, `"none"`.
    /// Defaults to `none` (no telemetry) when absent.  This is deliberately
    /// **not** coupled to `accept_jetbrains_eula`; data sharing requires its
    /// own explicit opt-in, exactly as in JetBrains' own client.
    #[serde(rename = "intellij.dataSharing")]
    data_sharing: Option<String>,

    /// `intellij.region` — region for JetBrains product terms / data
    /// processing.  Passed via `INTELLIJ_REGION` env var when set.
    #[serde(rename = "intellij.region")]
    region: Option<String>,

    /// `intellij.projects` — monorepo project entries (array of `{ type, path }`
    /// objects).  Forwarded to the server via initialization options.
    #[serde(rename = "intellij.projects")]
    projects: Option<serde_json::Value>,

    /// `intellij.buildTool` — global build tool override (e.g. `"gradle"`,
    /// `"maven"`, `"bazel"`, or `""` to disable all).  Forwarded to the server
    /// via initialization options, mapped per worktree folder.  The plain
    /// `buildTool` key is accepted as an alias for backwards compatibility.
    #[serde(rename = "intellij.buildTool", alias = "buildTool")]
    build_tool: Option<String>,

    /// `intellij.jdkForSymbolResolution` — path to a JDK home for symbol
    /// resolution.  Sent as `defaultSdk` in initialization options.
    #[serde(rename = "intellij.jdkForSymbolResolution")]
    jdk_for_symbol_resolution: Option<String>,
}

fn read_settings(server_name: &str, worktree: &Worktree) -> IntellijServerSettings {
    let settings = zed::settings::LspSettings::for_worktree(server_name, worktree)
        .ok()
        .and_then(|settings| settings.settings)
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(settings).unwrap_or_default()
}

/// Normalise data-sharing value to the casing the server expects
/// (`full`, `anonymous`, `none`).  Returns `None` for `none` (which means
/// "don't set the env var at all" — the server defaults to no telemetry).
fn normalised_data_sharing(raw: Option<&str>) -> Option<&str> {
    match raw.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("full") => Some("full"),
        Some("anonymous") => Some("anonymous"),
        _ => None, // `none` or anything else → omit env var → server defaults to none
    }
}

/// Returns the env-vars block for the server process, mirroring the real
/// JetBrains VSCode extension's `buildLaunchEnvironment` logic.
fn server_launch_env(settings: &IntellijServerSettings) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();

    if let Some(args) = &settings.additional_jvm_args {
        if !args.is_empty() {
            env.push(("IJ_JAVA_OPTIONS".to_string(), args.join(" ")));
        }
    }

    if let Some(level) = normalised_data_sharing(settings.data_sharing.as_deref()) {
        env.push(("INTELLIJ_DATA_SHARING".to_string(), level.to_string()));
    }
    // When `none`, omitting the env var is *by design*: the server's default
    // is `dataSharing=NONE` (seen in launch logs), and the real extension
    // explicitly deletes the var when "none" was chosen.  No telemetry is sent
    // unless the user explicitly opts in to "full" or "anonymous".

    if let Some(region) = settings.region.as_deref().filter(|r| !r.is_empty()) {
        env.push(("INTELLIJ_REGION".to_string(), region.to_string()));
    }

    env
}

/// The debug adapter name exposed to Zed. Declared in `extension.toml` under
/// `[debug_adapters.*]`.
///
/// Must be `intellij_debugger` — the IntelliJ DAP server rejects any other
/// `adapterID` in the DAP `initialize` request ("No debugger adapter found
/// for given adapter id: ..."). Zed forwards this name verbatim as the DAP
/// `adapterID`, so it has to match what the server expects.
const DEBUG_ADAPTER_NAME: &str = "intellij_debugger";

/// Rust bridge binary that wraps `intellij-server` so the extension can issue
/// LSP requests (e.g. `start_debug_server`) through a local HTTP endpoint, and
/// proxies the DAP TCP channel (rewriting IntelliJ's `file://` source URIs
/// into the absolute paths Zed needs for the Variables pane). Downloaded on
/// first launch from this extension's GitHub Release, exactly like the server.
const BRIDGE_NAME: &str = "intellij-lsp-bridge";

struct IntelliJLspExtension {
    cached_binary_path: Option<String>,
    cached_bridge_path: Option<String>,
    cached_workspace: Option<String>,
}

/// Returns `true` if `name` is the IntelliJ LSP server launcher.
///
/// The launcher is named `intellij-server` on Unix and `intellij-server.exe`
/// on Windows. Match those exactly so we never pick up auxiliary files like
/// `intellij-server.log` or `intellij-server.vmoptions`.
fn is_server_binary(name: &str) -> bool {
    if SERVER_BINARIES.contains(&name) {
        return true;
    }
    [".exe", ".bat", ".cmd"].iter().any(|ext| {
        name.strip_suffix(ext)
            .is_some_and(|stem| stem == "intellij-server")
    })
}

fn server_version_dir(version: &str) -> String {
    format!("intellij-server-{}", version)
}

/// Finds the server executable below `dir`, bounded to 4 levels of nesting.
fn find_binary_in(dir: &str, depth: u32) -> Option<String> {
    if depth > 4 {
        return None;
    }
    for entry in fs::read_dir(dir).ok()?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_binary_in(&path.to_string_lossy(), depth + 1) {
                return Some(found);
            }
        } else if is_server_binary(&entry.file_name().to_string_lossy()) {
            return Some(path.to_string_lossy().to_string());
        }
    }
    None
}

/// Returns the highest version already installed in the extension sandbox,
/// together with the path to its binary, if any.
fn find_installed_server() -> Option<(String, String)> {
    let entries = fs::read_dir(".").ok()?;
    let mut latest: Option<(String, String)> = None;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(version) = name.strip_prefix("intellij-server-") else {
            continue;
        };
        for candidate in [
            format!("{name}/bin/intellij-server"),
            format!("{name}/bin/intellij-server.exe"),
        ] {
            if fs::metadata(&candidate).is_ok_and(|stat| stat.is_file())
                && latest.as_ref().is_none_or(|(current, _)| {
                    compare_versions(version, current.as_str()) == Ordering::Greater
                })
            {
                latest = Some((version.to_string(), candidate));
            }
        }
    }
    latest
}

/// Returns the path of a previously downloaded Rust bridge binary in the
/// extension sandbox, if one exists (name: `intellij-lsp-bridge-<...>.exe`).
fn find_bridge_installed() -> Option<String> {
    let entries = fs::read_dir(".").ok()?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("intellij-lsp-bridge-") {
            return Some(name);
        }
    }
    None
}

/// Numeric dot-segment comparison, so `263.2689.10` sorts after `263.2689.9`.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let a_parts: Vec<&str> = a.split('.').collect();
    let b_parts: Vec<&str> = b.split('.').collect();
    for (a_part, b_part) in a_parts.iter().zip(b_parts.iter()) {
        match (a_part.parse::<u64>(), b_part.parse::<u64>()) {
            (Ok(a_num), Ok(b_num)) => match a_num.cmp(&b_num) {
                Ordering::Equal => continue,
                other => return other,
            },
            _ => match a_part.cmp(b_part) {
                Ordering::Equal => continue,
                other => return other,
            },
        }
    }
    a_parts.len().cmp(&b_parts.len())
}

/// First 16 hex chars (64 bits) of the SHA-256 digest — the EULA acceptance
/// hash the IntelliJ server expects.
fn sha256_prefix_16(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    format!("{:016x}", u64::from_be_bytes(prefix))
}

/// Full SHA-256 hex digest (64 chars) — only used by tests.
#[cfg(test)]
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Locates the `EULA.txt` bundled with an installed server, if any.
fn find_bundled_eula() -> Option<std::path::PathBuf> {
    let entries = fs::read_dir(".").ok()?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            let eula = path.join("EULA.txt");
            if eula.is_file() {
                return Some(eula);
            }
        }
    }
    None
}

/// Converts an absolute filesystem path into an LSP `file://` URI.
///
/// - Windows: `C:\Users\zcg\proj` → `file:///C:/Users/zcg/proj`
/// - Unix:    `/home/user/proj`  → `file:///home/user/proj` (NOT `file:////...`)
fn workspace_uri(root: &str) -> String {
    let p = root.replace('\\', "/");
    if p.starts_with("file://") {
        p
    } else if p.starts_with('/') {
        // Unix absolute path already starts with `/`, so `file://` + path is
        // the correct three-slash form.
        format!("file://{p}")
    } else {
        // Windows drive path (`C:/...`): prepend a slash after `file://`.
        format!("file:///{p}")
    }
}

/// Hex-encodes a string the same way the Rust bridge does (it uses the hex
/// encoding of the workspace URI to name its port file).
fn string_to_hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}

/// Reads the HTTP port of the bridge for the given workspace from
/// `proxy/<hex(workspace_uri)>` (written by the bridge on startup).
fn proxy_port(workspace_uri: &str) -> Result<u16> {
    let port_file = format!("proxy/{}", string_to_hex(workspace_uri));
    let contents = fs::read_to_string(&port_file).map_err(|e| {
        format!(
            "failed to read bridge port file ({port_file}); is the language server running? {e}"
        )
    })?;
    contents
        .trim()
        .parse::<u16>()
        .map_err(|e| format!("failed to parse bridge port from '{contents}' (corrupted file): {e}"))
}

/// Sends an LSP request to the running IntelliJ server through the bridge's
/// HTTP endpoint, returning the request's `result` as JSON.
fn lsp_request_via_proxy(
    workspace_uri: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let port = proxy_port(workspace_uri)?;
    let body = serde_json::json!({ "method": method, "params": params });
    let response = zed::http_client::fetch(
        &zed::http_client::HttpRequest::builder()
            .method(zed::http_client::HttpMethod::Post)
            .url(format!("http://127.0.0.1:{port}"))
            .body(body.to_string().into_bytes())
            .build()?,
    )?;

    let data: serde_json::Value = serde_json::from_slice(&response.body)
        .map_err(|e| format!("invalid proxy response: {e}"))?;
    if let Some(err) = data.get("error") {
        return Err(format!("LSP request {method} failed: {err}"));
    }
    data.get("result")
        .cloned()
        .ok_or_else(|| format!("LSP request {method} returned no result"))
}

/// Asks the IntelliJ server to start its debug server, returning the TCP port
/// it listens on for the Debug Adapter Protocol.
fn start_debug_server(workspace_uri: &str) -> Result<u16> {
    let result = lsp_request_via_proxy(
        workspace_uri,
        "workspace/executeCommand",
        serde_json::json!({
            "command": "start_debug_server",
            "arguments": [workspace_uri]
        }),
    )?;
    result
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| format!("invalid debug server port from IntelliJ server: {result}"))
}

/// True if `path` looks like a real JDK launcher: `<home>/bin/java` or
/// `<home>/bin/java.exe`. The IntelliJ debug server derives the JDK home from
/// this exact shape and rejects anything else (e.g. a bare `java` on `$PATH`).
fn looks_like_jdk_java(path: &str) -> bool {
    let norm = path.replace('\\', "/");
    let stem = norm.rsplit('/').next().unwrap_or("");
    (stem == "java" || stem == "java.exe") && norm.contains("/bin/")
}

/// Resolves the path to a JDK `java` executable, returning `None` when no
/// *real* JDK launcher can be found.
///
/// Priority:
/// 1. `worktree.which("java")` — a `java` on `$PATH` inside the worktree
///    (e.g. `...\jdk\bin\java.exe`). This matches the `<home>/bin/java` shape
///    the IntelliJ debug server requires to derive the JDK home.
/// 2. `$JAVA_HOME/bin/java(.exe)` — fallback when `java` is not on `$PATH` or
///    no worktree is available (e.g. `dap_config_to_scenario`).
///
/// A bare `"java"` is never returned: the server rejects it with "Cannot
/// derive JDK home from javaExec 'java' (expected <home>/bin/java)". When
/// nothing valid is found, `None` lets the server fall back to the project
/// SDK from the project model instead.
fn resolve_java_exec(worktree: Option<&Worktree>) -> Option<String> {
    if let Some(worktree) = worktree {
        if let Some(java) = worktree.which("java") {
            if looks_like_jdk_java(&java) {
                return Some(java);
            }
        }
    }
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        let candidate = format!("{}/bin/{}", java_home.trim_end_matches(['/', '\\']), exe);
        if std::path::Path::new(&candidate).is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Injects a `javaExec` into a launch debug configuration if it's missing and
/// a real JDK launcher can be resolved. The IntelliJ debug server rejects
/// launch requests without `javaExec`, but also rejects a bare `java` — so
/// nothing is injected when no `<home>/bin/java` path is found (the server
/// then uses the project SDK).
fn inject_java_exec(config: &mut serde_json::Value, worktree: Option<&Worktree>) {
    if config.get("request").and_then(serde_json::Value::as_str) != Some("launch") {
        return;
    }
    if config.get("javaExec").is_some() {
        return;
    }
    if let Some(java) = resolve_java_exec(worktree) {
        config["javaExec"] = serde_json::Value::String(java);
    }
}

/// True if `path` ends with a common source-file extension.
///
/// The IntelliJ server's `resolveWorkingDirectory` can fall back to the source
/// file's own path (e.g. `.../src/main/kotlin/Main.kt`) when the project model
/// isn't ready — e.g. right after a debug session ended. A working directory
/// must be a directory, never a source file, or the JVM launch fails with
/// "Cannot start a process, the working directory ... does not exist".
fn looks_like_source_file(path: &str) -> bool {
    let lower = path.trim_end_matches(['/', '\\']).to_lowercase();
    [".kt", ".kts", ".java", ".groovy", ".scala", ".class"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Ensures a launch config has a sane working directory: never empty, never a
/// source-file path (which the server may return as a fallback). Falls back to
/// `workspace_root` — a real directory that always exists. Attach configs are
/// left untouched.
fn ensure_launch_cwd(config: &mut serde_json::Value, workspace_root: &str) {
    if config.get("request").and_then(serde_json::Value::as_str) != Some("launch") {
        return;
    }
    let bad_cwd = match config.get("cwd").and_then(serde_json::Value::as_str) {
        Some(cwd) => cwd.is_empty() || looks_like_source_file(cwd),
        None => true,
    };
    if bad_cwd {
        config["cwd"] = serde_json::Value::String(workspace_root.to_string());
    }
}

/// Tries to infer the fully-qualified main class from common build files, so
/// the gutter **Debug** button can start a launch without the user configuring
/// anything.
///
/// * Gradle: `build.gradle.kts` / `build.gradle` — `mainClass.set(...)`
///   (`application` plugin) or `kotlin { jvmToolchain(...) }`.
/// * Maven: `pom.xml` — `<mainClass>` under `maven-jar-plugin` or
///   `exec-maven-plugin`.
fn infer_main_class(root: &str) -> Option<String> {
    // build.gradle.kts / build.gradle: application { mainClass.set("...") }
    for file in ["build.gradle.kts", "build.gradle"] {
        let path = format!("{root}/{file}");
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        // Prefer a quoted string on a line that names the main class
        // (`mainClass.set(...)`, `mainClass = ...`, `getOrElse("...")`).
        // This avoids grabbing `group = "com.example"` / `version = "1.0"`.
        let mut candidates: Vec<String> = Vec::new();
        for line in text.lines() {
            let lower = line.to_lowercase();
            if !lower.contains("mainclass") {
                continue;
            }
            // First quoted string on the line, e.g. `getOrElse("org.example.MainKt")`.
            if let Some(start) = line.find('"') {
                let after = &line[start + 1..];
                if let Some(end) = after.find('"') {
                    let candidate = after[..end].trim();
                    if candidate.contains('.') {
                        return Some(candidate.to_string());
                    }
                }
            }
        }
        // Fallback: first quoted string containing a dot, skipping common
        // non-class values (`group`, `version`, `description`, etc.).
        for line in text.lines() {
            let lower = line.to_lowercase();
            if [
                "group",
                "version",
                "description",
                "repositories",
                "plugins",
                "id(",
            ]
            .iter()
            .any(|kw| lower.contains(kw))
            {
                continue;
            }
            let mut rest = line;
            while let Some(start) = rest.find('"') {
                let after = &rest[start + 1..];
                if let Some(end) = after.find('"') {
                    let candidate = &after[..end];
                    if candidate.contains('.') {
                        candidates.push(candidate.to_string());
                    }
                    rest = &after[end + 1..];
                } else {
                    break;
                }
            }
        }
        if let Some(c) = candidates.into_iter().next() {
            return Some(c);
        }
    }
    // pom.xml: <mainClass>a.b.C</mainClass>
    let pom = format!("{root}/pom.xml");
    if let Ok(text) = fs::read_to_string(&pom) {
        if let Some(start) = text.find("<mainClass>") {
            let rest = &text[start + "<mainClass>".len()..];
            if let Some(end) = rest.find("</mainClass>") {
                let candidate = rest[..end].trim();
                if !candidate.is_empty() {
                    return Some(candidate.to_string());
                }
            }
        }
    }
    None
}

/// True if the task is a "main run" task (as opposed to a test/build task).
///
/// The extension API's TaskTemplate carries no runnable tags, so this matches
/// on label/command shape:
///   Kotlin: "run main"          → gradle run / mvn compile exec:java
///   Java:   "Run MyClass"       → gradle run / mvn exec:java
/// Test tasks (label/command contains "test") are excluded.
fn is_main_run_task(build_task: &zed::TaskTemplate) -> bool {
    let label_lower = build_task.label.to_lowercase();
    let command_lower = build_task.command.to_lowercase();
    if label_lower.contains("test") || command_lower.contains("test") {
        return false;
    }
    label_lower.contains("run")
        || command_lower.contains("run")
        || command_lower.contains("exec:java")
}

impl IntelliJLspExtension {
    fn language_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
        settings: &IntellijServerSettings,
    ) -> Result<String> {
        // Reuse a previously resolved path if it still exists.
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        // Manual mode: the user downloaded the server themselves.
        // Checked before find_installed_server() so an explicit user override
        // always wins over any previously cached auto-download.
        if let Some(path) = settings.server_path.as_deref() {
            let file_name = std::path::Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !is_server_binary(file_name) {
                return Err(concat!(
                    "\"server_path\" must point directly at the extracted\n",
                    "`intellij-server` executable (e.g. \"/path/to/intellij-server/bin/\n",
                    "intellij-server\"). The extension runs in a sandbox and cannot\n",
                    "extract or inspect files outside of it.",
                )
                .to_string());
            }
            self.cached_binary_path = Some(path.to_string());
            return Ok(path.to_string());
        }

        // Reuse an already-installed server from a previous session.
        if let Some((_, path)) = find_installed_server() {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Automatic mode: download the pinned build from JetBrains' CDN.
        let (pinned_version, url, file_type) = artifact_for_platform()?;
        let version = settings.server_version.clone().unwrap_or(pinned_version);
        let url = settings.server_download_url.clone().unwrap_or(url);

        self.download_server(language_server_id, &version, &url, file_type)
    }

    fn download_server(
        &mut self,
        language_server_id: &LanguageServerId,
        version: &str,
        url: &str,
        file_type: DownloadedFileType,
    ) -> Result<String> {
        set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let version_dir = server_version_dir(version);
        if fs::metadata(&version_dir).is_err() {
            set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            download_file(url, &version_dir, file_type).map_err(|e| {
                format!("failed to download the IntelliJ LSP server ({version}): {e}")
            })?;
        }

        let binary = find_binary_in(&version_dir, 0)
            .ok_or_else(|| format!("server binary not found after extracting {version}"))?;
        make_file_executable(&binary)
            .map_err(|e| format!("failed to make the server binary executable: {e}"))?;
        self.cached_binary_path = Some(binary.clone());
        Ok(binary)
    }

    /// Resolves (downloading on first use) the Rust bridge binary that wraps
    /// the language server. The bridge is published as a release asset of this
    /// extension's repository, named `intellij-lsp-bridge-<platform>-<version>`.
    fn bridge_binary_path(&mut self) -> Result<String> {
        if let Some(path) = &self.cached_bridge_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }
        // Reuse a previously downloaded bridge in the sandbox.
        if let Some(path) = find_bridge_installed() {
            self.cached_bridge_path = Some(path.clone());
            return Ok(path);
        }

        let (os, arch) = zed::current_platform();
        let platform_tag = match (os, arch) {
            (zed::Os::Mac, zed::Architecture::Aarch64) => "macos-aarch64",
            (zed::Os::Mac, _) => "macos-x86_64",
            (zed::Os::Linux, zed::Architecture::Aarch64) => "linux-aarch64",
            (zed::Os::Linux, _) => "linux-x86_64",
            (zed::Os::Windows, zed::Architecture::Aarch64) => "windows-aarch64",
            (zed::Os::Windows, _) => "windows-x86_64",
        };
        let exe = if cfg!(windows) { ".exe" } else { "" };
        let file_name = format!(
            "{BRIDGE_NAME}-{platform_tag}-{}{exe}",
            env!("CARGO_PKG_VERSION")
        );
        let url = format!(
            "https://github.com/zcg/intellij-lsp-zed/releases/download/v{}/{}",
            env!("CARGO_PKG_VERSION"),
            file_name
        );

        download_file(&url, &file_name, DownloadedFileType::Uncompressed).map_err(|e| {
            format!("failed to download the IntelliJ LSP bridge ({file_name}): {e}")
        })?;
        make_file_executable(&file_name)
            .map_err(|e| format!("failed to make the bridge executable: {e}"))?;
        self.cached_bridge_path = Some(file_name.clone());
        Ok(file_name)
    }

    /// Resolves the EULA acceptance hash to send to the server: an explicit
    /// user override, or the hash computed from the EULA.txt bundled with the
    /// installed server.
    fn eula_hash_for(&self, settings: &IntellijServerSettings) -> Option<String> {
        if let Some(hash) = &settings.eula_hash {
            return Some(hash.clone());
        }
        // Compute from the EULA.txt shipped with the installed server.
        // This auto-adapts to whatever version was downloaded — no pin drift.
        let eula_path = self
            .cached_binary_path
            .as_deref()
            .and_then(|binary| std::path::Path::new(binary).parent())
            .and_then(|bin_dir| bin_dir.parent())
            .map(|server_root| server_root.join("EULA.txt"))
            .filter(|path| path.is_file())
            .or_else(find_bundled_eula);
        let data = fs::read(eula_path?).ok()?;
        Some(sha256_prefix_16(&data))
    }

    /// Mirrors the official VS Code extension's `resolveLaunchConfig`: asks
    /// the IntelliJ server to resolve the main class's source document, then
    /// its classpath, javaExec and working directory — so the user never has
    /// to configure them. Everything is done through the bridge's HTTP endpoint
    /// (LSP `workspace/executeCommand`).
    fn resolve_launch_config(
        &mut self,
        config: &mut serde_json::Value,
        ws_uri: &str,
        main_class: &str,
    ) -> Result<(), String> {
        // 1. Locate the source file declaring the main class.
        let doc: serde_json::Value = lsp_request_via_proxy(
            ws_uri,
            "workspace/executeCommand",
            serde_json::json!({
                "command": "intellij.java.resolveClassDocument",
                "arguments": [{ "fqn": main_class }]
            }),
        )?;
        let file_uri = doc
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .ok_or("resolveClassDocument returned no uri")?;

        // 2. Resolve the runtime classpath.
        if config.get("classPaths").is_none()
            || config
                .get("classPaths")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|a| a.is_empty())
        {
            let cp: serde_json::Value = lsp_request_via_proxy(
                ws_uri,
                "workspace/executeCommand",
                serde_json::json!({
                    "command": "intellij.java.resolveClasspath",
                    "arguments": [{ "uri": file_uri }]
                }),
            )?;
            if let Some(classpath) = cp.get("classpath").and_then(serde_json::Value::as_array) {
                config["classPaths"] = serde_json::Value::Array(classpath.clone());
            }
        }

        // 3. Resolve javaExec (JDK) — only if the user hasn't set one.
        if config.get("javaExec").is_none() {
            let je: serde_json::Value = lsp_request_via_proxy(
                ws_uri,
                "workspace/executeCommand",
                serde_json::json!({
                    "command": "intellij.java.resolveJavaExecutable",
                    "arguments": [{ "uri": file_uri }]
                }),
            )?;
            if let Some(java_exec) = je.get("javaExec").and_then(serde_json::Value::as_str) {
                config["javaExec"] = serde_json::Value::String(java_exec.to_string());
            }
        }

        // 4. Resolve the working directory if not set.
        if config.get("cwd").is_none() {
            let wd: serde_json::Value = lsp_request_via_proxy(
                ws_uri,
                "workspace/executeCommand",
                serde_json::json!({
                    "command": "intellij.java.resolveWorkingDirectory",
                    "arguments": [{ "uri": file_uri }]
                }),
            )?;
            if let Some(working_directory) = wd
                .get("workingDirectory")
                .and_then(serde_json::Value::as_str)
            {
                // The server can fall back to the source file's own path when
                // the project model isn't ready (e.g. right after a debug
                // session ended). A working directory must be a directory —
                // skip file-shaped paths and let the caller fall back to the
                // workspace root.
                if !looks_like_source_file(working_directory) {
                    config["cwd"] = serde_json::Value::String(working_directory.to_string());
                }
            }
        }

        Ok(())
    }
}

impl Extension for IntelliJLspExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
            cached_bridge_path: None,
            cached_workspace: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let settings = read_settings(language_server_id.as_ref(), worktree);

        // EULA gate: nothing is downloaded or executed without explicit consent.
        if !settings.accept_jetbrains_eula {
            set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Failed(EULA_GATE_MESSAGE.to_string()),
            );
            return Err(EULA_GATE_MESSAGE.to_string());
        }

        let binary_path = self.language_server_binary_path(language_server_id, &settings)?;
        let bridge_path = self.bridge_binary_path()?;
        let root = worktree.root_path();
        let ws_uri = workspace_uri(&root);
        self.cached_workspace = Some(root.clone());

        // The extension's wasm runs with its working directory set to the
        // extension workdir (e.g. .../extensions/work/intellij-lsp), whereas
        // the spawned bridge process runs with the *project* directory as cwd.
        // Resolve absolute paths for the bridge and the server binary so they
        // can be found regardless of the bridge's working directory.
        let workdir =
            std::env::current_dir().map_err(|e| format!("failed to get extension workdir: {e}"))?;
        let bridge_path = {
            let p = std::path::Path::new(&bridge_path);
            if p.is_absolute() {
                bridge_path
            } else {
                workdir.join(p).to_string_lossy().to_string()
            }
        };
        // `binary_path` is relative to the extension workdir; make it absolute.
        let binary_path = {
            let p = std::path::Path::new(&binary_path);
            if p.is_absolute() {
                binary_path
            } else {
                workdir.join(p).to_string_lossy().to_string()
            }
        };

        // The bridge inherits these and forwards them to the server process it
        // spawns (`IJ_JAVA_OPTIONS`, `INTELLIJ_REGION`, ...).
        let env = server_launch_env(&settings);

        Ok(Command {
            command: bridge_path,
            args: vec![
                binary_path,
                ws_uri,
                workdir.to_string_lossy().to_string(), // bridge writes its port file here
            ],
            env,
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings = read_settings(language_server_id.as_ref(), worktree);
        if !settings.accept_jetbrains_eula {
            return Err(EULA_GATE_MESSAGE.to_string());
        }

        self.cached_workspace = Some(worktree.root_path());

        let mut init = serde_json::json!({
            "eulaHash": self.eula_hash_for(&settings),
        });

        // Mirroring the real JetBrains VSCode extension's
        // `buildInitializationOptions`: forward projects, buildTools, and
        // defaultSdk verbatim so the server sees the same shape.
        if let Some(ref projects) = settings.projects {
            init["projects"] = projects.clone();
        }
        if let Some(ref build_tool) = settings.build_tool {
            // The real extension sends a per-worktree-folder URI → buildTool
            // mapping.  We have a single worktree, so we map the root path
            // to a `file://` URI.
            let uri = workspace_uri(&worktree.root_path());
            init["buildTools"] = serde_json::json!({ uri: build_tool });
        }
        if let Some(ref jdk) = settings.jdk_for_symbol_resolution {
            init["defaultSdk"] = serde_json::Value::String(jdk.clone());
        }

        Ok(Some(init))
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        _user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != DEBUG_ADAPTER_NAME {
            return Err(format!(
                "Cannot create binary for adapter \"{adapter_name}\""
            ));
        }

        let workspace = worktree.root_path();
        let ws_uri = workspace_uri(&workspace);

        // Ask the IntelliJ server to start its DAP server; we connect to it
        // over TCP (the server itself is the debug adapter).
        let port = start_debug_server(&ws_uri)?;

        let mut config_json: serde_json::Value = serde_json::from_str(&config.config)
            .map_err(|e| format!("Invalid JSON configuration: {e}"))?;

        // Mirror the official VS Code extension's `resolveLaunchConfig` flow:
        // ask the server to resolve the classpath, javaExec and working dir
        // from the project model, so the user doesn't have to configure them.
        //
        // This must run BEFORE any local javaExec injection: the server
        // resolves the project SDK's real `<home>/bin/java` path, while a
        // locally-injected guess (e.g. the bare name "java" when Zed's process
        // can't see `$PATH`/`$JAVA_HOME`) would shadow it and make the server
        // fail with "Cannot derive JDK home from javaExec".
        let main_class = config_json
            .get("mainClass")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if let Some(main_class) = main_class {
            if let Err(e) = self.resolve_launch_config(&mut config_json, &ws_uri, &main_class) {
                // Resolution may fail if the project import is still in
                // progress. Surface a clear error rather than silently
                // launching with an empty classpath.
                return Err(format!(
                    "failed to resolve IntelliJ debug configuration (is the \
                     project imported yet?): {e}"
                ));
            }
        }

        // Fallback: if the server didn't provide a javaExec, inject one from
        // worktree/PATH/JAVA_HOME so the launch is not rejected with "launch
        // arguments missing 'javaExec'". Only a real `<home>/bin/java` path is
        // injected — never a bare "java".
        inject_java_exec(&mut config_json, Some(worktree));

        // Final safety net: the working directory must be a real directory.
        // The server's resolveWorkingDirectory can return the source file's
        // own path when the project model isn't ready (e.g. on the second
        // launch right after a debug session ended) — fall back to the
        // workspace root, which always exists.
        ensure_launch_cwd(&mut config_json, &workspace);

        Ok(DebugAdapterBinary {
            command: None,
            arguments: vec![],
            cwd: Some(workspace.clone()),
            envs: vec![],
            request_args: StartDebuggingRequestArguments {
                request: self.dap_request_kind(adapter_name, config_json.clone())?,
                configuration: config_json.to_string(),
            },
            connection: Some(resolve_tcp_template(TcpArgumentsTemplate {
                host: None,
                port: Some(port),
                timeout: None,
            })?),
        })
    }

    fn dap_request_kind(
        &mut self,
        adapter_name: String,
        config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        if adapter_name != DEBUG_ADAPTER_NAME {
            return Err(format!(
                "Cannot create binary for adapter \"{adapter_name}\""
            ));
        }

        match config.get("request").and_then(serde_json::Value::as_str) {
            Some("launch") => Ok(StartDebuggingRequestArgumentsRequest::Launch),
            Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
            Some(other) => Err(format!(
                "Unexpected value for `request` key in IntelliJ debug adapter configuration: {other:?}"
            )),
            None => Err("Missing required `request` field in IntelliJ debug adapter configuration".into()),
        }
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        if config.adapter != DEBUG_ADAPTER_NAME {
            return Err(format!("Unsupported debug adapter: {}", config.adapter));
        }

        let workspace = self
            .cached_workspace
            .clone()
            .ok_or("LSP workspace not initialized yet")?;
        let ws_uri = workspace_uri(&workspace);
        let port = start_debug_server(&ws_uri)?;

        let debug_config = match config.request {
            DebugRequest::Launch(launch) => {
                let env: HashMap<String, String> = launch.envs.into_iter().collect();
                let mut launch_config = serde_json::json!({
                    "request": "launch",
                    "args": launch.args,
                    "cwd": launch.cwd,
                    "env": env,
                });
                // The IntelliJ server auto-resolves the main class from the
                // project model when `mainClass` is omitted, so an empty
                // `program` (the F5 modal's required field) should not block
                // the launch. Only set it when the user actually provided one,
                // or we can infer it from the build files (gutter Debug).
                if !launch.program.is_empty() {
                    launch_config["mainClass"] = serde_json::Value::String(launch.program);
                } else if let Some(main_class) = infer_main_class(&workspace) {
                    launch_config["mainClass"] = serde_json::Value::String(main_class);
                }
                // No worktree is available here; fall back to `$JAVA_HOME`.
                inject_java_exec(&mut launch_config, None);
                // Working directory must be a real directory (see
                // `ensure_launch_cwd`) — fall back to the workspace root.
                ensure_launch_cwd(&mut launch_config, &workspace);
                launch_config
            }
            DebugRequest::Attach(attach) => match attach.process_id {
                Some(process_id) => serde_json::json!({
                    "request": "attach",
                    "processId": process_id,
                }),
                None => serde_json::json!({
                    "request": "attach",
                    "hostName": "localhost",
                    "port": 5005,
                }),
            },
        };

        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            tcp_connection: Some(TcpArgumentsTemplate {
                host: None,
                port: Some(port),
                timeout: None,
            }),
            config: debug_config.to_string(),
        })
    }

    /// Gutter **Debug** button: turns a `main` runnable task into an IntelliJ
    /// debug scenario. Works for both Kotlin (`run main`) and Java (`Run
    /// MyClass`) runnables — the main class is inferred from the build files
    /// so the user doesn't have to configure anything.
    fn dap_locator_create_scenario(
        &mut self,
        _locator_name: String,
        build_task: zed::TaskTemplate,
        resolved_label: String,
        debug_adapter_name: String,
    ) -> Option<DebugScenario> {
        if debug_adapter_name != DEBUG_ADAPTER_NAME {
            return None;
        }
        if !is_main_run_task(&build_task) {
            return None;
        }
        let workspace = self.cached_workspace.clone()?;
        let main_class = infer_main_class(&workspace)?;
        Some(DebugScenario {
            adapter: debug_adapter_name,
            label: resolved_label,
            build: None,
            tcp_connection: None,
            config: serde_json::json!({
                "request": "launch",
                "mainClass": main_class,
            })
            .to_string(),
        })
    }

    /// Second phase of locator resolution. Our scenarios don't run a build
    /// step, so this is never invoked; returning an error is safe.
    fn run_dap_locator(
        &mut self,
        _locator_name: String,
        _build_task: zed::TaskTemplate,
    ) -> Result<DebugRequest, String> {
        Err("IntelliJ debug locator does not run build tasks".to_string())
    }
}

zed::register_extension!(IntelliJLspExtension);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_binary_in_empty_dir() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-empty");
        let _ = fs::create_dir_all(&dir);
        assert!(find_binary_in(&dir.to_string_lossy(), 0).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_binary_in_nested() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-nested");
        let bin_dir = dir.join("nested").join("bin");
        let _ = fs::create_dir_all(&bin_dir);
        fs::write(bin_dir.join("intellij-server"), b"fake").unwrap();
        let found = find_binary_in(&dir.to_string_lossy(), 0);
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("intellij-server"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_binary_in_finds_exe_too() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-exe");
        let bin_dir = dir.join("bin");
        let _ = fs::create_dir_all(&bin_dir);
        fs::write(bin_dir.join("intellij-server.exe"), b"fake").unwrap();
        let found = find_binary_in(&dir.to_string_lossy(), 0);
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("intellij-server.exe"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_server_binary() {
        assert!(is_server_binary("intellij-server"));
        assert!(is_server_binary("intellij-server.exe"));
        assert!(is_server_binary("intellij-server.bat"));
        assert!(is_server_binary("intellij-server.cmd"));
        assert!(!is_server_binary("intellij-server.log"));
        assert!(!is_server_binary("intellij-server.vmoptions"));
        assert!(!is_server_binary("intellij-server-snapshot"));
        assert!(!is_server_binary("java"));
    }

    #[test]
    fn test_server_version_dir() {
        assert_eq!(server_version_dir("1.2.3"), "intellij-server-1.2.3");
    }

    #[test]
    fn test_compare_versions() {
        use Ordering::*;
        assert_eq!(compare_versions("263.2689.0", "263.2689.0"), Equal);
        assert_eq!(compare_versions("263.2689.1", "263.2689.0"), Greater);
        assert_eq!(compare_versions("263.2689.0", "263.2689.1"), Less);
        assert_eq!(compare_versions("263.2689.10", "263.2689.9"), Greater);
        assert_eq!(compare_versions("263.2689.9", "263.2689.10"), Less);
        assert_eq!(compare_versions("264.0.0", "263.2689.10"), Greater);
        assert_eq!(compare_versions("263.2689.0", "263.2689.0.1"), Less);
    }

    #[test]
    fn test_sha256_prefix_16_known_vector() {
        assert_eq!(sha256_prefix_16(b"ACCEPT_ME"), "c79ea8172fb984df");
    }

    #[test]
    fn test_sha256_prefix_16_is_hex() {
        let hash = sha256_prefix_16(b"anything");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sha256_prefix_16_deterministic() {
        let data = b"same content";
        assert_eq!(sha256_prefix_16(data), sha256_prefix_16(data));
    }

    #[test]
    fn test_sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex(b"ACCEPT_ME"),
            "c79ea8172fb984df90625215a6e79461e0d978040373cd2d264307434b059daf"
        );
    }

    #[test]
    fn test_settings_default_when_missing() {
        let settings: IntellijServerSettings =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!settings.accept_jetbrains_eula);
        assert!(settings.server_path.is_none());
        assert!(settings.server_version.is_none());
        assert!(settings.server_download_url.is_none());
        assert!(settings.eula_hash.is_none());
        assert!(settings.additional_jvm_args.is_none());
        assert!(settings.data_sharing.is_none());
        assert!(settings.region.is_none());
    }

    #[test]
    fn test_settings_accepts_eula() {
        let settings: IntellijServerSettings =
            serde_json::from_value(serde_json::json!({ "accept_jetbrains_eula": true })).unwrap();
        assert!(settings.accept_jetbrains_eula);
    }

    #[test]
    fn test_settings_ignores_unknown_fields() {
        let settings: IntellijServerSettings = serde_json::from_value(serde_json::json!({
            "accept_jetbrains_eula": true,
            "some_future_option": "x",
        }))
        .unwrap();
        assert!(settings.accept_jetbrains_eula);
    }

    #[test]
    fn test_settings_reads_server_fields() {
        let settings: IntellijServerSettings = serde_json::from_value(serde_json::json!({
            "server_path": "/opt/intellij-server/bin/intellij-server",
            "server_version": "263.2689.0",
            "server_download_url": "https://example.com/server.sit",
            "eula_hash": "deadbeefdeadbeef",
        }))
        .unwrap();
        assert_eq!(
            settings.server_path.as_deref(),
            Some("/opt/intellij-server/bin/intellij-server")
        );
        assert_eq!(settings.server_version.as_deref(), Some("263.2689.0"));
        assert_eq!(
            settings.server_download_url.as_deref(),
            Some("https://example.com/server.sit")
        );
        assert_eq!(settings.eula_hash.as_deref(), Some("deadbeefdeadbeef"));
    }

    #[test]
    fn test_settings_reads_build_tool_alias() {
        // The plain `buildTool` key must be accepted alongside the
        // `intellij.buildTool` rename (backwards compatibility).
        let settings: IntellijServerSettings = serde_json::from_value(serde_json::json!({
            "buildTool": "gradle",
        }))
        .unwrap();
        assert_eq!(settings.build_tool.as_deref(), Some("gradle"));
        let settings: IntellijServerSettings = serde_json::from_value(serde_json::json!({
            "intellij.buildTool": "maven",
        }))
        .unwrap();
        assert_eq!(settings.build_tool.as_deref(), Some("maven"));
    }

    #[test]
    fn test_settings_reads_additional_jvm_args() {
        let settings: IntellijServerSettings = serde_json::from_value(serde_json::json!({
            "intellij.additionalJvmArgs": ["-Xmx4g", "-Dfoo=bar"],
        }))
        .unwrap();
        assert_eq!(
            settings.additional_jvm_args,
            Some(vec!["-Xmx4g".to_string(), "-Dfoo=bar".to_string()])
        );
    }

    #[test]
    fn test_normalised_data_sharing_defaults_to_none() {
        assert_eq!(normalised_data_sharing(None), None);
        assert_eq!(normalised_data_sharing(Some("none")), None);
        assert_eq!(normalised_data_sharing(Some("NONE")), None);
        assert_eq!(normalised_data_sharing(Some("")), None);
        assert_eq!(normalised_data_sharing(Some("garbage")), None);
        assert_eq!(normalised_data_sharing(Some("full")), Some("full"));
        assert_eq!(
            normalised_data_sharing(Some("anonymous")),
            Some("anonymous")
        );
    }

    #[test]
    fn test_data_sharing_never_defaults_to_sharing() {
        // None → no env var set → server gets no INTELIJ_DATA_SHARING → defaults to none
        let settings = IntellijServerSettings::default();
        let env = server_launch_env(&settings);
        let has_data_sharing = env.iter().any(|(k, _)| k == "INTELLIJ_DATA_SHARING");
        assert!(
            !has_data_sharing,
            "data sharing env must be absent by default"
        );

        // Explicit "none" → also absent
        let settings = IntellijServerSettings {
            data_sharing: Some("none".into()),
            ..Default::default()
        };
        let env = server_launch_env(&settings);
        let has_data_sharing = env.iter().any(|(k, _)| k == "INTELLIJ_DATA_SHARING");
        assert!(
            !has_data_sharing,
            "explicit none must also omit the env var"
        );
    }

    #[test]
    fn test_env_includes_jvm_args_and_region() {
        let settings = IntellijServerSettings {
            additional_jvm_args: Some(vec!["-Xmx4g".into()]),
            region: Some("EU".into()),
            ..Default::default()
        };
        let env = server_launch_env(&settings);
        assert!(env
            .iter()
            .any(|(k, v)| k == "IJ_JAVA_OPTIONS" && v == "-Xmx4g"));
        assert!(env.iter().any(|(k, v)| k == "INTELLIJ_REGION" && v == "EU"));
        assert!(!env.iter().any(|(k, _)| k == "INTELLIJ_DATA_SHARING"));
    }

    #[test]
    fn test_workspace_uri_windows() {
        assert_eq!(
            workspace_uri(r"C:\Users\zcg\proj"),
            "file:///C:/Users/zcg/proj"
        );
        assert_eq!(
            workspace_uri("D:/Projects/kkkkt"),
            "file:///D:/Projects/kkkkt"
        );
    }

    #[test]
    fn test_workspace_uri_unix() {
        assert_eq!(workspace_uri("/home/user/proj"), "file:///home/user/proj");
        assert_eq!(workspace_uri("/Users/zcg/proj"), "file:///Users/zcg/proj");
        // Already a URI — unchanged.
        assert_eq!(workspace_uri("file:///D:/proj"), "file:///D:/proj");
    }

    #[test]
    fn test_string_to_hex_matches_node() {
        // Matches Buffer.from("file:///D:/proj", "utf8").toString("hex").
        assert_eq!(
            string_to_hex("file:///D:/proj"),
            "66696c653a2f2f2f443a2f70726f6a"
        );
        assert_eq!(string_to_hex("a"), "61");
    }

    #[test]
    fn test_looks_like_jdk_java() {
        assert!(looks_like_jdk_java("D:/jdk/bin/java.exe"));
        assert!(looks_like_jdk_java("D:\\jdk\\bin\\java.exe"));
        assert!(looks_like_jdk_java("/usr/lib/jvm/java-17/bin/java"));
        assert!(!looks_like_jdk_java("java"));
        assert!(!looks_like_jdk_java("C:/apps/java"));
        assert!(!looks_like_jdk_java("C:/apps/bin/javac.exe"));
    }

    #[test]
    fn test_inject_java_exec_launch() {
        // Launch config without javaExec. Either no JDK is discoverable
        // (javaExec stays absent — the server falls back to the project SDK),
        // or a real <home>/bin/java path is injected. Never a bare "java".
        let mut config = serde_json::json!({
            "request": "launch",
            "mainClass": "MainKt",
            "cwd": "/proj"
        });
        inject_java_exec(&mut config, None);
        if let Some(java) = config.get("javaExec").and_then(serde_json::Value::as_str) {
            assert!(looks_like_jdk_java(java), "unexpected javaExec: {java}");
        }
    }

    #[test]
    fn test_inject_java_exec_never_bare_java() {
        // Even with no worktree and no JAVA_HOME, the config must never end up
        // with the bare name "java" — the IntelliJ server rejects it with
        // "Cannot derive JDK home from javaExec".
        let mut config = serde_json::json!({
            "request": "launch",
            "mainClass": "MainKt"
        });
        inject_java_exec(&mut config, None);
        let injected = config.get("javaExec").and_then(serde_json::Value::as_str);
        assert!(
            injected.is_none_or(|j| j != "java"),
            "must not inject bare \"java\""
        );
    }

    #[test]
    fn test_inject_java_exec_preserves_existing() {
        // An explicitly-set javaExec is preserved.
        let mut config = serde_json::json!({
            "request": "launch",
            "mainClass": "MainKt",
            "javaExec": "D:/jdk/bin/java.exe"
        });
        inject_java_exec(&mut config, None);
        assert_eq!(
            config.get("javaExec").and_then(serde_json::Value::as_str),
            Some("D:/jdk/bin/java.exe")
        );
    }

    #[test]
    fn test_inject_java_exec_attach() {
        // Attach configs must NOT get javaExec injected.
        let mut config = serde_json::json!({
            "request": "attach",
            "port": 5005
        });
        inject_java_exec(&mut config, None);
        assert!(config.get("javaExec").is_none());
    }

    #[test]
    fn test_looks_like_source_file() {
        // Source files — the server may return these as a bogus working dir.
        assert!(looks_like_source_file(r"D:\proj\src\main\kotlin\Main.kt"));
        assert!(looks_like_source_file(r"D:\proj\src\main\kotlin\Main.kt/"));
        assert!(looks_like_source_file("D:/proj/src/App.java"));
        assert!(looks_like_source_file("/proj/App.groovy"));
        assert!(looks_like_source_file("Main.scala"));
        assert!(looks_like_source_file("build/classes/Main.class"));
        // Real directories and non-source paths are fine.
        assert!(!looks_like_source_file(r"D:\proj"));
        assert!(!looks_like_source_file(r"D:\proj\build\classes\java\main"));
        assert!(!looks_like_source_file("D:/proj/src"));
        assert!(!looks_like_source_file(""));
    }

    #[test]
    fn test_ensure_launch_cwd_fixes_source_file_cwd() {
        // The exact failure from the field: the server returned the source
        // file's own path as the working directory.
        let mut config = serde_json::json!({
            "request": "launch",
            "mainClass": "org.example.MainKt",
            "cwd": "D:/Projects/javaprojects/kkkkt/src/main/kotlin/Main.kt",
        });
        ensure_launch_cwd(&mut config, "D:/Projects/javaprojects/kkkkt");
        assert_eq!(
            config.get("cwd").and_then(serde_json::Value::as_str),
            Some("D:/Projects/javaprojects/kkkkt")
        );
    }

    #[test]
    fn test_ensure_launch_cwd_fixes_empty_or_missing() {
        // Empty cwd → workspace root.
        let mut config = serde_json::json!({
            "request": "launch",
            "cwd": "",
        });
        ensure_launch_cwd(&mut config, "/proj");
        assert_eq!(
            config.get("cwd").and_then(serde_json::Value::as_str),
            Some("/proj")
        );
        // Missing cwd → workspace root.
        let mut config = serde_json::json!({
            "request": "launch",
        });
        ensure_launch_cwd(&mut config, "/proj");
        assert_eq!(
            config.get("cwd").and_then(serde_json::Value::as_str),
            Some("/proj")
        );
    }

    #[test]
    fn test_ensure_launch_cwd_preserves_valid() {
        // A real directory cwd is kept.
        let mut config = serde_json::json!({
            "request": "launch",
            "cwd": "D:/Projects/javaprojects/kkkkt",
        });
        ensure_launch_cwd(&mut config, "/fallback");
        assert_eq!(
            config.get("cwd").and_then(serde_json::Value::as_str),
            Some("D:/Projects/javaprojects/kkkkt")
        );
        // Attach configs are never touched.
        let mut config = serde_json::json!({
            "request": "attach",
            "port": 5005,
        });
        ensure_launch_cwd(&mut config, "/fallback");
        assert!(config.get("cwd").is_none());
    }

    #[test]
    fn test_resolve_java_exec_from_java_home() {
        // Without a worktree, resolve from JAVA_HOME (or None when no JDK is
        // discoverable). We can't rely on JAVA_HOME being set in CI, so just
        // assert the result is either None or a real <home>/bin/java path.
        let resolved = resolve_java_exec(None);
        if let Some(java) = resolved {
            assert!(looks_like_jdk_java(&java), "unexpected javaExec: {java}");
        }
    }

    #[test]
    fn test_infer_main_class_gradle() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-mainclass");
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("build.gradle.kts"),
            "plugins { application }\napplication {\n    mainClass.set(providers.gradleProperty(\"mainClass\").getOrElse(\"org.example.MainKt\"))\n}",
        )
        .unwrap();
        assert_eq!(
            infer_main_class(&dir.to_string_lossy()),
            Some("org.example.MainKt".to_string())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_infer_main_class_maven() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-mainclass-mvn");
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("pom.xml"),
            "<project><build><plugins><plugin><configuration><mainClass>com.acme.App</mainClass></configuration></plugin></plugins></build></project>",
        )
        .unwrap();
        assert_eq!(
            infer_main_class(&dir.to_string_lossy()),
            Some("com.acme.App".to_string())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_infer_main_class_none() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-mainclass-none");
        let _ = fs::create_dir_all(&dir);
        // Empty dir — no build files.
        assert_eq!(infer_main_class(&dir.to_string_lossy()), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_main_run_task() {
        let task = |label: &str, command: &str| zed::TaskTemplate {
            label: label.to_string(),
            command: command.to_string(),
            args: vec![],
            env: Default::default(),
            cwd: None,
        };

        // Kotlin "run main"
        assert!(is_main_run_task(&task("run main", "./gradlew run")));
        // Java "Run MyClass"
        assert!(is_main_run_task(&task("Run MyClass", "./gradlew run")));
        // Maven exec:java
        assert!(is_main_run_task(&task("Run App", "mvn compile exec:java")));
        // Test tasks are excluded
        assert!(!is_main_run_task(&task("test MyClass", "./gradlew test")));
        assert!(!is_main_run_task(&task("Run tests", "./gradlew test")));
        assert!(!is_main_run_task(&task("Test class MyClass", "mvn test")));
    }
}
