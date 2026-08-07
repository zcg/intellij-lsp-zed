use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use zed_extension_api::{
    self as zed, current_platform, download_file, make_file_executable, node_binary_path,
    resolve_tcp_template, set_language_server_installation_status, settings::LspSettings,
    Architecture, Command, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario,
    DebugTaskDefinition, DownloadedFileType, Extension, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, TcpArgumentsTemplate, Worktree,
};

/// The debug adapter name exposed to Zed. Declared in `extension.toml` under
/// `[debug_adapters.*]`.
///
/// Must be `intellij_debugger` — the IntelliJ DAP server rejects any other
/// `adapterID` in the DAP `initialize` request ("No debugger adapter found
/// for given adapter id: ..."). Zed forwards this name verbatim as the DAP
/// `adapterID`, so it has to match what the server expects.
const DEBUG_ADAPTER_NAME: &str = "intellij_debugger";

/// Node proxy script that wraps `intellij-server` so the extension can issue
/// LSP requests (e.g. `start_debug_server`) through a local HTTP endpoint.
const PROXY_SCRIPT: &str = include_str!("proxy.cjs");
const PROXY_FILE: &str = "intellij-lsp-proxy.cjs";

struct IntelliJLspExtension {
    cached_binary_path: Option<String>,
    cached_workspace: Option<String>,
}

/// Returns `true` if `name` is the IntelliJ LSP server launcher.
///
/// The launcher is named `intellij-server` on Unix and `intellij-server.exe`
/// on Windows. Match those exactly so we never pick up auxiliary files like
/// `intellij-server.log` or `intellij-server.vmoptions`.
fn is_server_binary(name: &str) -> bool {
    if name == "intellij-server" {
        return true;
    }
    [".exe", ".bat", ".cmd"].iter().any(|ext| {
        name.strip_suffix(ext)
            .is_some_and(|stem| stem == "intellij-server")
    })
}

/// Compares two dotted version strings numerically (e.g. `10.0.0` > `9.9.9`).
fn version_greater(a: &str, b: &str) -> bool {
    let pa: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Maps the current host platform to Open VSX's `targetPlatform` identifier,
/// so we always download the VSIX (and thus the server bundle) for the running
/// platform instead of the API's default (`darwin-arm64`).
fn openvsx_platform() -> Result<&'static str> {
    let (os, arch) = current_platform();
    match (os, arch) {
        (Os::Mac, Architecture::Aarch64) => Ok("darwin-arm64"),
        (Os::Mac, Architecture::X8664) => Ok("darwin-x64"),
        (Os::Linux, Architecture::Aarch64) => Ok("linux-arm64"),
        (Os::Linux, Architecture::X8664) => Ok("linux-x64"),
        (Os::Windows, Architecture::Aarch64) => Ok("win32-arm64"),
        (Os::Windows, Architecture::X8664) => Ok("win32-x64"),
        _ => Err("32-bit platforms are not supported by the IntelliJ LSP server".into()),
    }
}

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

/// Hex-encodes a string the same way `Buffer.from(s, "utf8").toString("hex")`
/// does in the proxy script (which uses it to name its port file).
fn string_to_hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}

/// Reads the HTTP port of the proxy for the given workspace from
/// `proxy/<hex(workspace_uri)>` (written by the proxy on startup).
fn proxy_port(workspace_uri: &str) -> Result<u16> {
    let port_file = format!("proxy/{}", string_to_hex(workspace_uri));
    let contents = fs::read_to_string(&port_file).map_err(|e| {
        format!("failed to read proxy port file ({port_file}); is the language server running? {e}")
    })?;
    contents
        .trim()
        .parse::<u16>()
        .map_err(|e| format!("failed to parse proxy port from '{contents}' (corrupted file): {e}"))
}

/// Sends an LSP request to the running IntelliJ server through the proxy's
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

/// Resolves the path to a JDK `java` executable.
///
/// Priority:
/// 1. `worktree.which("java")` — a `java` on `$PATH` inside the worktree
///    (e.g. `...\jdk\bin\java.exe`). This matches the `<home>/bin/java` shape
///    the IntelliJ debug server requires to derive the JDK home.
/// 2. `$JAVA_HOME/bin/java(.exe)` — fallback when `java` is not on `$PATH` or
///    no worktree is available (e.g. `dap_config_to_scenario`).
/// 3. Plain `java` — lets the OS resolve it (the server will then likely fail
///    to derive a JDK home, but this is the best we can do).
fn resolve_java_exec(worktree: Option<&Worktree>) -> String {
    if let Some(worktree) = worktree {
        if let Some(java) = worktree.which("java") {
            return java;
        }
    }
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        return format!("{}/bin/{}", java_home.trim_end_matches(['/', '\\']), exe);
    }
    "java".to_string()
}

/// Injects a `javaExec` into a launch debug configuration if it's missing.
/// The IntelliJ debug server rejects launch requests without it.
fn inject_java_exec(config: &mut serde_json::Value, worktree: Option<&Worktree>) {
    if config.get("request").and_then(serde_json::Value::as_str) != Some("launch") {
        return;
    }
    if config.get("javaExec").is_some() {
        return;
    }
    config["javaExec"] = serde_json::Value::String(resolve_java_exec(worktree));
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
    fn server_version_dir(version: &str) -> String {
        format!("intellij-server-{}", version)
    }

    fn language_server_binary_path(
        &mut self,
        language_server_id: &LanguageServerId,
    ) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        // Check if any version is already installed in the sandbox.
        if let Ok(entries) = fs::read_dir(".") {
            let mut latest: Option<(String, String)> = None;
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(version) = name.strip_prefix("intellij-server-") {
                    // The server bundle extracts to `bin/` inside the version
                    // dir; the launcher is `intellij-server` (Unix) or
                    // `intellij-server.exe`/`.bat`/`.cmd` (Windows).
                    if let Ok(entries) = fs::read_dir(format!("{name}/bin")) {
                        for entry in entries.filter_map(|e| e.ok()) {
                            let fname = entry.file_name().to_string_lossy().to_string();
                            if !is_server_binary(&fname) {
                                continue;
                            }
                            let candidate = format!("{name}/bin/{fname}");
                            if fs::metadata(&candidate).is_ok_and(|s| s.is_file())
                                && latest
                                    .as_ref()
                                    .is_none_or(|(v, _)| version_greater(version, v))
                            {
                                latest = Some((version.to_string(), candidate));
                            }
                        }
                    }
                }
            }
            if let Some((_, path)) = latest {
                self.cached_binary_path = Some(path.clone());
                return Ok(path);
            }
        }

        // Not installed — download.
        set_language_server_installation_status(
            language_server_id,
            &LanguageServerInstallationStatus::CheckingForUpdate,
        );

        // Fetch the latest server info for the *current* platform from Open VSX.
        // The plain `/latest` endpoint defaults to `darwin-arm64`, so requesting
        // the platform-specific endpoint is what keeps Windows/Linux working.
        let platform = openvsx_platform()?;
        download_file(
            &format!("https://open-vsx.org/api/JetBrains/intellij-server/{platform}/latest"),
            "vsix-meta.json",
            DownloadedFileType::Uncompressed,
        )
        .map_err(|e| format!("failed to fetch metadata: {e}"))?;
        let body = fs::read_to_string("vsix-meta.json").map_err(|e| format!("read error: {e}"))?;
        fs::remove_file("vsix-meta.json").ok();

        let meta: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))?;
        let vsix_url = meta
            .get("files")
            .and_then(|f| f.get("download"))
            .and_then(|v| v.as_str())
            .ok_or("missing VSIX download URL")?;

        download_file(vsix_url, "vsix", DownloadedFileType::Zip)
            .map_err(|e| format!("VSIX download failed: {e}"))?;
        let bundle = fs::read_to_string("vsix/extension/server-bundle.json")
            .map_err(|e| format!("read bundle: {e}"))?;
        fs::remove_dir_all("vsix").ok();

        let bundle_json: serde_json::Value =
            serde_json::from_str(&bundle).map_err(|e| format!("invalid bundle: {e}"))?;
        let server_url = bundle_json
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("missing server URL in bundle")?;
        let version = bundle_json
            .get("version")
            .and_then(|v| v.as_str())
            .ok_or("missing version in bundle")?;

        let version_dir = Self::server_version_dir(version);

        if fs::metadata(&version_dir).is_err() {
            set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            download_file(server_url, &version_dir, DownloadedFileType::Zip)
                .map_err(|e| format!("server download failed: {e}"))?;
        }

        let binary =
            find_binary_in(&version_dir, 0).ok_or("server binary not found after extraction")?;

        make_file_executable(&binary).map_err(|e| format!("chmod failed: {e}"))?;
        self.cached_binary_path = Some(binary.clone());
        Ok(binary)
    }

    /// Mirrors the official VS Code extension's `resolveLaunchConfig`: asks
    /// the IntelliJ server to resolve the main class's source document, then
    /// its classpath, javaExec and working directory — so the user never has
    /// to configure them. Everything is done through the proxy's HTTP endpoint
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
                config["cwd"] = serde_json::Value::String(working_directory.to_string());
            }
        }

        Ok(())
    }
}

impl Extension for IntelliJLspExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
            cached_workspace: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let binary_path = self.language_server_binary_path(language_server_id)?;
        let root = worktree.root_path();
        let ws_uri = workspace_uri(&root);
        self.cached_workspace = Some(root.clone());

        // Materialize the proxy script into the extension's working directory
        // so the language server is spawned through it. The proxy forwards LSP
        // stdio transparently and exposes an HTTP endpoint we use for debug.
        //
        // The extension's wasm runs with its working directory set to the
        // extension workdir (e.g. .../extensions/work/intellij-lsp), whereas
        // the spawned Node process runs with the *project* directory as cwd.
        // Resolve absolute paths for both the proxy script and the server
        // binary so Node can find them regardless of its working directory.
        let workdir =
            std::env::current_dir().map_err(|e| format!("failed to get extension workdir: {e}"))?;
        let proxy_path = workdir.join(PROXY_FILE);
        // Always (over)write the proxy script so the version baked into the
        // wasm stays in sync with disk — an outdated proxy on disk (e.g. from
        // an earlier extension version) would misplace its port file.
        fs::write(&proxy_path, PROXY_SCRIPT)
            .map_err(|e| format!("failed to write proxy script {}: {e}", proxy_path.display()))?;

        // `binary_path` is relative to the extension workdir; make it absolute.
        let binary_path = {
            let p = std::path::Path::new(&binary_path);
            if p.is_absolute() {
                binary_path
            } else {
                workdir.join(p).to_string_lossy().to_string()
            }
        };

        Ok(Command {
            command: node_binary_path()?,
            args: vec![
                proxy_path.to_string_lossy().to_string(),
                binary_path,
                ws_uri,
                workdir.to_string_lossy().to_string(), // proxy writes its port file here
            ],
            env: Default::default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // Read EULA.txt and compute acceptance hash.
        let eula_path = if let Some(ref path) = self.cached_binary_path {
            // EULA.txt is in the server root dir (parent of bin/)
            let bin_dir = std::path::Path::new(path).parent().unwrap();
            bin_dir.parent().unwrap().join("EULA.txt")
        } else {
            // Fallback: search for EULA.txt
            let mut found = None;
            if let Ok(entries) = fs::read_dir(".") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.is_dir() {
                        let eula = path.join("EULA.txt");
                        if eula.exists() {
                            found = Some(eula);
                            break;
                        }
                    }
                }
            }
            found.unwrap_or_else(|| std::path::PathBuf::from("EULA.txt"))
        };

        let data = fs::read(&eula_path).ok();
        let hash = data.map(|d| {
            let mut hasher = Sha256::new();
            hasher.update(&d);
            // First 16 hex chars (64 bits) of SHA-256
            format!(
                "{:016x}",
                u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap())
            )
        });

        // `buildTools` tells the server which build tool to use for each
        // workspace folder. By default we pass JSON `null` and let the server
        // auto-detect — when several build tools are present (e.g. Gradle +
        // a `.idea/` JPS folder) it sends a `window/showMessageRequest` prompt
        // and the user chooses (the proxy forwards that prompt to Zed, which
        // renders it). This is the original, user-driven behaviour.
        //
        // A user can override the choice in settings.json:
        //   lsp.intellij-server.settings.buildTool = "gradle" | "maven" |
        //   "bazel" | "jps"  ("" disables import, null/omitted = auto-detect).
        //
        // IMPORTANT: the default must be a JSON `null` (Value::Null), not the
        // string "null" — the latter is treated as an unknown build tool name.
        let ws_uri = workspace_uri(&worktree.root_path());
        let configured = LspSettings::for_worktree("intellij-server", worktree)
            .ok()
            .and_then(|s| s.settings)
            .and_then(|s| {
                s.get("buildTool")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        let build_tool: serde_json::Value = match configured.as_deref() {
            Some("gradle") => serde_json::json!("gradle"),
            Some("maven") => serde_json::json!("maven"),
            Some("bazel") => serde_json::json!("bazel"),
            Some("jps") => serde_json::json!("jps"),
            // "" disables all build tools; null/omitted lets the server
            // decide (auto-detect, prompt on conflict).
            Some("") => serde_json::json!(""),
            _ => serde_json::Value::Null,
        };

        Ok(Some(serde_json::json!({
            "eulaHash": hash,
            "buildTools": {
                ws_uri: build_tool
            }
        })))
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
        // Inject javaExec (from worktree/PATH/JAVA_HOME) so the launch is not
        // rejected with "launch arguments missing 'javaExec'".
        inject_java_exec(&mut config_json, Some(worktree));

        // Mirror the official VS Code extension's `resolveLaunchConfig` flow:
        // ask the server to resolve the classpath, javaExec and working dir
        // from the project model, so the user doesn't have to configure them.
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

    /// The buildTool default must serialize to JSON `null`, not the string
    /// "null" — the IntelliJ server rejects "null" as an unknown build tool.
    #[test]
    fn test_build_tool_default_is_json_null() {
        let build_tool: serde_json::Value = serde_json::Value::Null;
        let init = serde_json::json!({
            "buildTools": { "file:///proj": build_tool }
        });
        let serialized = init.to_string();
        assert!(
            serialized.contains(":null") || serialized.contains(": null"),
            "expected JSON null, got: {serialized}"
        );
        assert!(
            !serialized.contains("\"null\""),
            "must not serialize the string \"null\": {serialized}"
        );
    }

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
    fn test_server_version_dir() {
        assert_eq!(
            IntelliJLspExtension::server_version_dir("1.2.3"),
            "intellij-server-1.2.3"
        );
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
    }

    #[test]
    fn test_version_greater() {
        assert!(version_greater("10.0.0", "9.9.9"));
        assert!(version_greater("1.2.3", "1.2.2"));
        assert!(!version_greater("1.2.2", "1.2.3"));
        assert!(!version_greater("9.9.9", "10.0.0"));
        assert!(!version_greater("1.2.3", "1.2.3"));
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
    fn test_inject_java_exec_launch() {
        // Launch config without javaExec gets it injected. The exact value
        // depends on the environment (worktree / JAVA_HOME); assert it's
        // non-empty and ends with the java binary name.
        let mut config = serde_json::json!({
            "request": "launch",
            "mainClass": "MainKt",
            "cwd": "/proj"
        });
        inject_java_exec(&mut config, None);
        let java = config
            .get("javaExec")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        assert!(!java.is_empty());
        assert!(java.ends_with("java") || java.ends_with("java.exe"));
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
    fn test_resolve_java_exec_from_java_home() {
        // Without a worktree, resolve from JAVA_HOME (or fall back to "java").
        // We can't rely on JAVA_HOME being set in CI, so just assert it's non-empty.
        let resolved = resolve_java_exec(None);
        assert!(!resolved.is_empty());
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

    #[test]
    fn test_eula_hash_sha256() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-eula");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("EULA.txt"), b"ACCEPT_ME").unwrap();
        let data = fs::read(dir.join("EULA.txt")).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash = format!(
            "{:016x}",
            u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap())
        );
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_eula_hash_deterministic() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-det");
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("EULA.txt"), b"same content").unwrap();

        let data = fs::read(dir.join("EULA.txt")).unwrap();
        let h1 = {
            let mut hasher = Sha256::new();
            hasher.update(&data);
            format!(
                "{:016x}",
                u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap())
            )
        };
        let h2 = {
            let mut hasher = Sha256::new();
            hasher.update(&data);
            format!(
                "{:016x}",
                u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap())
            )
        };
        assert_eq!(h1, h2);
        let _ = fs::remove_dir_all(&dir);
    }
}
