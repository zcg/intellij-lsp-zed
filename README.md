# IntelliJ LSP for Zed

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

🌐 **English** | [**简体中文**](README.zh-CN.md)

Unofficial Zed extension that brings [IntelliJ IDEA's LSP server][1] for Java &
Kotlin to the [Zed editor][2] — code completion, navigation, refactorings,
inspections, quick-fixes, **and a full IntelliJ debugger** for Maven, Gradle,
and Bazel projects.

> **Important — licensing.** The IntelliJ LSP server is **proprietary software
> by JetBrains** (not open source). Before the extension downloads or runs it,
> you must read and accept the [JetBrains EULA][3]. This extension never
> fetches the server from third-party registries (such as the Open VSX API):
> it either downloads the pinned build directly from JetBrains' CDN, or uses a
> server you downloaded yourself. See [License](#license).

[1]: https://blog.jetbrains.com/idea/2026/08/intellij-idea-goes-lsp/
[2]: https://zed.dev
[3]: https://www.jetbrains.com/legal/docs/toolbox/user/

## Install

Once published to the Zed extension registry:

1. Open Zed → `Cmd+Shift+P` → `zed: extensions` → search **IntelliJ LSP**
2. **Accept the JetBrains EULA** — add this to your Zed `settings.json`
   (`~/.config/zed/settings.json` on Linux/macOS):

   ```json
   {
     "lsp": {
       "intellij-server": {
         "settings": {
           "accept_jetbrains_eula": true
         }
       }
     }
   }
   ```

3. Open a Java or Kotlin project. The server (~368 MB) is downloaded once from
   JetBrains' CDN and reused on subsequent launches.

### Install from the repository (dev extension)

1. Clone the repository:
   ```sh
   git clone https://github.com/zcg/intellij-lsp-zed.git
   ```
2. In Zed: `Cmd+Shift+P` → `zed: install dev extension`, select the cloned
   folder.
3. No Rust toolchain needed — the pre-built `extension.wasm` is committed to
   the repo. Re-run `git pull` and reinstall to update.

### Using a manually downloaded server

If you prefer full control, download the server from the [JetBrains
announcement][1], extract it, and point the extension at the `intellij-server`
executable:

```json
{
  "lsp": {
    "intellij-server": {
      "settings": {
        "accept_jetbrains_eula": true,
        "server_path": "/absolute/path/to/intellij-server/bin/intellij-server"
      }
    }
  }
}
```

`server_path` must point **directly at the `intellij-server` executable** (or
`intellij-server.exe` on Windows). The extension runs in a sandbox and cannot
extract archives outside of it. `server_path` takes priority over both the
sandbox cache and the pinned auto-download.

## Debugging

The extension ships the IntelliJ debugger engine as a debug adapter named
`intellij_debugger` (bound to both Java and Kotlin).

### 0. Build first (important)

The IntelliJ debugger resolves the classpath from the project model, but it
does **not** compile your code for you. Build your project first:

```sh
./gradlew build      # Gradle
mvn compile          # Maven
bazel build //...    # Bazel
```

Debugging without a recent build fails with
`java.lang.ClassNotFoundException`.

### 1. Launch (F5)

1. Open a Java/Kotlin project and wait for the language server to finish
   importing (first import can take a minute or two).
2. Set a breakpoint, then press **F5** (`debug: start`).
3. Pick **IntelliJ LSP** → **Launch**. `mainClass` is auto-resolved from the
   project model, and `javaExec`/`classPaths`/`cwd` are resolved
   automatically — nothing to configure.

For per-project scenarios (e.g. program args), create `.zed/debug.json`:

```jsonc
[
  {
    "adapter": "intellij_debugger",
    "request": "launch",
    "label": "Debug MainKt",
    "mainClass": "org.example.MainKt",
    "args": ["side-effect"], // optional program arguments
    "vmArgs": ["-Xmx2G"], // optional JVM arguments
    "cwd": "$ZED_WORKTREE_ROOT",
  },
]
```

### 2. Debug from the gutter

The extension registers a debug locator for the `main` runnable — works for
both **Kotlin** (`run main`) and **Java** (`Run MyClass`). Hover the gutter
next to `fun main` / `main` and pick **Debug** — no configuration needed; the
main class is inferred from `build.gradle.kts` / `pom.xml`.

> **Java support**: this extension provides the Java language definition
> itself (grammar, highlighting, runnables, tasks) — Java and Kotlin are
> first-class equals. **Uninstall the Zed `java` extension** so the two don't
> fight over the Java language. Both languages use the IntelliJ debugger
> (`intellij_debugger`) automatically.

### 3. Attach to a running JVM

Start your JVM with JDWP enabled:

```sh
java -agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005 \
     -cp <your-classpath> com.example.Main
```

Then in `.zed/debug.json`:

```jsonc
[
  {
    "adapter": "intellij_debugger",
    "request": "attach",
    "label": "Attach to JVM",
    "hostName": "localhost",
    "port": 5005,
  },
]
```

Or launch the app, then F5 → **IntelliJ LSP** → **Attach**.

### How the debugger works

Under the hood the extension spawns the language server through a small Rust
bridge (`bridge/`) that answers `start_debug_server` LSP requests, returns the
DAP TCP port Zed connects to, and proxies the DAP channel (rewriting IntelliJ's
`file://` source URIs into the absolute paths Zed needs to populate the
Variables pane). The bridge is a native binary downloaded on first launch from
this extension's GitHub Release — no Node.js involved.

> **Run/test tasks and Windows vs Linux/macOS** — the gutter run/test buttons
> (`languages/kotlin/tasks.json`, `languages/java/tasks.json`) ship as plain
> `./gradlew ...` commands using the system's default shell. That form is what
> **Linux and macOS** use — they run the checked-in Gradle wrapper directly.
>
> **Windows differs**: the Gradle wrapper is `gradlew.bat`, not `./gradlew`,
> and the default shell is PowerShell. If the tasks fail on Windows, edit the
> `command` fields in those two `tasks.json` files to PowerShell form. Two
> things change per command:
>
> 1. `./gradlew` → `gradlew.bat` (no `./` prefix — PowerShell resolves
>    `gradlew.bat` from the project directory, but not `./gradlew`);
> 2. Zed's `$ZED_CUSTOM_*` variables inside the `--tests` argument must be
>    wrapped in double quotes exactly like `"$ZED_CUSTOM_kotlin_package_name.
$ZED_CUSTOM_kotlin_class_name"` — PowerShell expands them inside the
>    quotes, and the quotes keep Gradle's `--tests` pattern intact.
>
> Example, Kotlin `test` task on Windows:
>
> ```json
> "command": "gradlew.bat test --tests \"$ZED_CUSTOM_kotlin_package_name.$ZED_CUSTOM_kotlin_class_name\""
> ```

## Settings

All settings live under `lsp.intellij-server.settings` in your Zed
`settings.json`.

| Key                               | Type    | Required | Description                                                                                                                      |
| --------------------------------- | ------- | -------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `accept_jetbrains_eula`           | boolean | yes      | Explicitly accept the JetBrains EULA. No download or execution happens unless this is `true`.                                    |
| `server_path`                     | string  | no       | Path to an already-extracted `intellij-server` executable (overrides auto-download).                                             |
| `server_version`                  | string  | no       | Override the pinned server version (automatic mode).                                                                             |
| `server_download_url`             | string  | no       | Override the pinned JetBrains download URL (automatic mode).                                                                     |
| `eula_hash`                       | string  | no       | EULA acceptance hash override (advanced — see Troubleshooting).                                                                  |
| `intellij.additionalJvmArgs`      | array   | no       | JVM options for the server process (e.g. `["-Xmx4g"]` to raise the 2 GB default heap).                                           |
| `intellij.dataSharing`            | string  | no       | `"full"` / `"anonymous"` / `"none"`. **Defaults to `none`** — independent consent, never inherited from `accept_jetbrains_eula`. |
| `intellij.region`                 | string  | no       | Region for JetBrains product terms / data processing.                                                                            |
| `intellij.projects`               | array   | no       | Monorepo project entries (`[{ "type": "gradle", "path": "file:///..." }]`).                                                      |
| `intellij.buildTool`              | string  | no       | Global build tool override (`"gradle"`, `"maven"`, `"bazel"`, or `""` to disable all). `buildTool` is accepted as an alias.      |
| `intellij.jdkForSymbolResolution` | string  | no       | Path to a JDK home for symbol resolution.                                                                                        |

These keys are consumed by the extension and delivered to the server via
**initialization options** and **environment variables** (exactly as the real
JetBrains VS Code extension does).

## Advanced: JetBrains server settings

JetBrains' own VS Code extension delivers settings to the language server via
**initialization options** (`eulaHash`, `projects`, `buildTools`, `defaultSdk`)
and **environment variables** (`IJ_JAVA_OPTIONS`, `INTELLIJ_DATA_SHARING`,
`INTELLIJ_REGION`). This extension mirrors that behaviour 1:1 using the same
setting keys (dots included).

### Full example

A realistic `~/.config/zed/settings.json` using the IntelliJ server for Java
and Kotlin, with the EULA accepted, heap raised to 4 GB, data sharing kept
off, region set, and two monorepo sub-projects scoped for import:

```json
{
  "lsp": {
    "intellij-server": {
      "settings": {
        "accept_jetbrains_eula": true,
        "intellij.additionalJvmArgs": ["-Xmx4g", "-XX:+UseG1GC"],
        "intellij.dataSharing": "none",
        "intellij.region": "EU",
        "intellij.buildTool": "gradle",
        "intellij.projects": [
          { "type": "gradle", "path": "file:///Users/me/work/monorepo/module-a/build.gradle.kts" },
          { "type": "maven", "path": "file:///Users/me/work/monorepo/module-b/pom.xml" }
        ],
        "intellij.jdkForSymbolResolution": "/opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk/Contents/Home"
      }
    }
  },
  "languages": {
    "Java": {
      "language_servers": ["intellij-server", "!jdtls"]
    },
    "Kotlin": {
      "language_servers": ["intellij-server", "!kotlin-language-server"]
    }
  }
}
```

### Individual settings explained

Other useful ones:

- `intellij.additionalJvmArgs` — `["-Xmx4g"]` (→ `IJ_JAVA_OPTIONS`; default heap is 2 GB)
- `intellij.dataSharing` — `"full"` / `"anonymous"` / `"none"` (opt-in, **defaults to `none`**; see [Data sharing](#data-sharing))
- `intellij.region` — your region for JetBrains' product terms and data processing
- `intellij.buildTool` — `"gradle"` / `"maven"` / `"bazel"` (→ `buildTools` in init options)
- `intellij.jdkForSymbolResolution` — path to a JDK home (→ `defaultSdk` in init options)

See the [official IntelliJ LSP settings documentation][4] for the full list.

### Choosing a build tool

By default the extension lets the server auto-detect the build tool. When a
project mixes formats (e.g. `build.gradle.kts` **and** a `.idea/` JPS folder),
the server asks which one to use and Zed shows the choice — pick `Use Gradle`,
`Use Maven`, etc. Nothing is decided for you.

To pin the choice (skip the prompt) or disable import, set it in
`~/.config/zed/settings.json`:

```json
"lsp": {
  "intellij-server": {
    "settings": {
      "intellij.buildTool": "gradle"
    }
  }
}
```

Valid values: `gradle`, `maven`, `bazel`, `jps`. Omit the setting (or set it
to `null`) to auto-detect + prompt on conflict, or `""` to disable project
import. The plain `buildTool` key is accepted as an alias.

## Known limitations (Zed)

- **Live templates and file templates** are editor-side features in VS Code;
  they are not part of the LSP protocol.
- **One backend per workspace.** Only one IntelliJ server can access a
  workspace at a time — don't use VS Code and Zed on the same folder
  simultaneously.
- **Archive integrity verification not implemented at runtime.** The
  official sha256 hash for each platform's server archive (from
  `server-bundle.json`) is stored in `server-artifacts.json` and recorded in
  `src/lib.rs` comments, but the extension does not verify it after
  downloading because `zed_extension_api` 0.7.0's `download_file` extracts
  the archive in-place and does not expose the raw bytes to the WASM sandbox
  for hashing. The archive is transported over HTTPS, and `download_file`
  reports HTTP errors; the pinned URL lives on JetBrains' own CDN. A future
  `zed_extension_api` release that supports raw download-then-extract would
  allow the extension to verify the sha256 before trusting the contents.

[4]: https://www.jetbrains.com/help/intellij-vscode/IntelliJ-lsp-settings.html
[5]: https://www.jetbrains.com/help/intellij-vscode/Project-import.html

## Disable Zed's built-in Java/Kotlin servers (optional)

The extension registers the IntelliJ server for Java and Kotlin automatically.
Zed also ships its own servers (`jdtls`, `kotlin-language-server`), so you'll
get duplicate diagnostics unless you disable them:

```json
"languages": {
  "Java": {
    "language_servers": ["intellij-server", "!jdtls"]
  },
  "Kotlin": {
    "language_servers": ["intellij-server", "!kotlin-language-server"]
  }
}
```

The `!` prefix disables the built-in server for that language.

## How It Works

1. On every launch the extension checks that you accepted the JetBrains EULA
   (`accept_jetbrains_eula`). If not, it refuses to start and prints exactly
   what to add to `settings.json` — no download, no execution.

2. It checks whether `server_path` is set; if so, it uses that binary
   immediately (explicit override wins over everything else).

3. It checks its sandbox cache for an already-installed server and reuses it.

4. If none is installed, it downloads the pinned build directly from
   JetBrains' CDN — the version and per-platform URLs come from
   `server-artifacts.json`, which is embedded in the extension at compile
   time and kept up-to-date by a biweekly CI workflow (see [Auto-update](#auto-update)).

5. The EULA acceptance hash is computed from the bundled `EULA.txt` and passed
   to the server on startup via initialization options. JetBrains settings
   (`intellij.projects`, `intellij.buildTool`, ...) are also forwarded to the
   server at startup — matching the real VS Code extension's behaviour.

6. The language server is spawned through the Rust bridge (`bridge/`) that
   transparently forwards LSP stdio between Zed and the server. When you start
   a debug session, the bridge forwards the `start_debug_server` request to
   the server and returns the DAP TCP port that Zed connects to. The bridge is
   downloaded once from this extension's GitHub Release.

7. Your project imports (Maven/Gradle/Bazel) and language features activate.

Cached versions are reused on subsequent launches.

## Auto-update

The pinned server version and download URLs live in
[`server-artifacts.json`](server-artifacts.json) — one JSON file with the
current version and a per-platform entry (URL + sha256 + archive type) for
all 6 supported platforms (macOS x86_64/ARM64, Linux x86_64/ARM64, Windows
x86_64/ARM64). This file is embedded in the extension at compile time — no
runtime queries.

Two CI workflow files keep the pin current, both running on the maintainer's
repository, never from an end-user's machine:

- **Upstream build detection + registry propagation** (`auto-update.yml`) —
  runs on the 1st and 15th of each month (a stable ~13–17 day interval). It
  queries the Open VSX API once to check whether JetBrains has published a new
  vsix. If a new version is found, it downloads the vsix package for all 6
  supported platforms from `openvsx.eclipsecontent.org`, extracts
  `extension/server-bundle.json` from each, rebuilds `server-artifacts.json`,
  rebuilds the WASM, bumps the version, commits, and pushes. It then
  propagates the update to the `zed-industries/extensions` registry by bumping
  the extension's git submodule and updating the `version` field in
  `extensions.toml` — following Zed's own documented extension-update process.
  The build-detection step is real (if infrequent, single-maintainer and
  non-scalable) traffic against Open VSX's infrastructure — it runs from
  GitHub's CI, never from an end user's machine, never triggered by an
  install, and its volume does not grow with adoption of the extension. The
  registry-propagation step involves **no Open VSX traffic at all** — it is
  pure Git operations against the extensions repository.
- **CDN health check** (`monitor.yml`) — also runs on the 1st and 15th of each
  month. It verifies the pinned JetBrains CDN URLs are still reachable. If
  the check fails, it opens an `extension-broken` issue on the extension's own
  repository.

JetBrains ships preview builds roughly every 2 weeks, and each build stays
valid for 30 days before it expires — so the 1st/15th schedule always catches
new builds with at least 13 days of margin before the previous build could
expire.

The extension itself never touches any registry API: the pin is static and
pre-committed.

## Evaluation & License

- During the preview the server is **free** — each build is valid for
  **30 days** from its release date
- After the preview, an IntelliJ IDEA Ultimate subscription will be required
- If the server stops working after ~30 days, install a newer build (clear the
  extension's cache; see Troubleshooting)

### Data sharing

JetBrains' own clients (VS Code, Cursor) additionally ask users to accept a
**data-sharing policy** and choose a region after installing the extension.
This extension keeps data sharing **disabled by default**: the server runs with
`dataSharing=NONE` and no telemetry is sent to JetBrains. If you want to opt
into telemetry, set `intellij.dataSharing` to `"full"` or `"anonymous"` — this
is a completely separate decision from EULA acceptance.

## Troubleshooting

| Problem                                       | Fix                                                                                                                                                                                                                          |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "you must read and accept the JetBrains EULA" | Add `"accept_jetbrains_eula": true` under `lsp.intellij-server.settings` (see [Install](#install)) and reload the window.                                                                                                    |
| "Bundled license agreement is not accepted"   | The server reports the hash it expects (e.g. `expected hash 34d850193ee04897`). If you run the server from a manual `server_path`, copy that hash into the `eula_hash` setting. Automatic downloads compute it for you.      |
| "Cannot derive JDK home from javaExec"        | The debug launch was sent a bare `java` instead of a real `<home>/bin/java` path. Reload the extension, then start the debug session again — the server now resolves the project SDK's JDK path automatically.               |
| Server won't start / evaluation expired       | Clear the server cache: `rm -rf ~/Library/Application\ Support/Zed/extensions/work/intellij-lsp` (Linux: `~/.local/share/zed/extensions/work/`, Windows: `%LOCALAPPDATA%\Zed\extensions\work\intellij-lsp`), then reload Zed |
| Download fails                                | Check your internet connection, then retry — the extension resumes cleanly                                                                                                                                                   |
| Duplicate diagnostics                         | Add the `language_servers` config above to disable Zed's built-ins                                                                                                                                                           |

## Development

Requires **Rust** (`wasm32-wasip2` target) and **git** (Zed compiles the
Kotlin tree-sitter grammar from the declared repo when you install the dev
extension).

### Install the dev extension (recommended)

```sh
# Build the extension (requires Rust + wasm32-wasip2 target)
cargo build --release --target wasm32-wasip2

# Keep the committed wasm in sync (CI verifies this)
cp target/wasm32-wasip2/release/intellij_lsp_zed.wasm extension.wasm
```

Then, in Zed:

1. `Cmd+Shift+P` → `zed: extensions`
2. Gear icon (top-right) → **Install Dev Extension...**
3. Select this repository's folder (`intellij-lsp-zed`)

Zed compiles the Rust extension and the Kotlin grammar, then loads it with a
**Dev** badge. Reload the extension (or restart Zed) after changing
`src/*.rs`, `extension.toml`, or `languages/kotlin/*`.

> On Windows, launch Zed from a terminal where `rustc` is on `PATH` — Zed's
> GUI process may not inherit an updated PATH until you sign out/in.

### Manually copying into `extensions/installed`

Copying `extension.toml` + `extension.wasm` into
`extensions/installed/<id>/` is **not** recommended: Zed only shows
extensions from the registry or dev extensions in the Extensions panel. Use
the dev-extension flow above.

### Checks

```sh
cargo test
cargo clippy --target wasm32-wasip2 -- -D warnings
cargo fmt -- --check
```

### Project structure

| Path                                | Purpose                                                                                                    |
| ----------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `src/lib.rs`                        | Extension entry point — EULA gate, binary resolution, download/launch, init options, debug adapter         |
| `bridge/`                           | Rust bridge — LSP stdio forwarding + `start_debug_server` + DAP port proxy and `file://` → path rewriting  |
| `server-artifacts.json`             | Pinned server version + per-platform download URLs (source of truth, updated by CI)                        |
| `languages/`                        | Java/Kotlin/Gradle/Gradle-KTS/Properties language definitions (grammar, highlighting, runnables, tasks)    |
| `extension.toml`                    | Zed extension manifest                                                                                     |
| `extension.wasm`                    | Pre-built WASM binary (so users don't need Rust)                                                           |
| `scripts/update-artifacts.py`       | CI helper — downloads all platform vsixes, extracts `server-bundle.json`, rebuilds `server-artifacts.json` |
| `scripts/bump-version.py`           | CI helper — bumps the patch version in `extension.toml`, `Cargo.toml`, and `package.json`                  |
| `.github/workflows/auto-update.yml` | Biweekly CI that detects new JetBrains builds and auto-updates the pin + releases                          |
| `.github/workflows/monitor.yml`     | Biweekly CI health check — verifies the pinned CDN URLs are reachable                                      |
| `.github/workflows/ci.yml`          | Push/PR CI — fmt, clippy, tests, wasm build                                                                |

### Updating the pinned server (manual, if CI can't)

The auto-update workflow handles this 99% of the time. If you need to do it
manually:

1. Download the new `JetBrains.intellij-server` vsix for each platform from
   [Open VSX](https://open-vsx.org/extension/JetBrains/intellij-server) in a
   browser (one manual download per platform — normal end-user usage).

2. Extract each vsix and read `extension/server-bundle.json`: it contains
   the real JetBrains CDN `url`, `version`, and `sha256` for that platform.

3. Run `python3 scripts/update-artifacts.py <vsix-version>` to rebuild
   `server-artifacts.json` from the downloaded vsixes.

4. Verify that the `EULA.txt` inside the new server bundle is byte-for-byte
   identical to the `LICENSE.txt` inside the vsix wrapper (they were for
   v263.2689.0 — re-check on every bump to avoid hash drift).

5. Rebuild the WASM: `cargo build --release --target wasm32-wasip2`, copy
   `extension.wasm`, bump versions, and release.

## Requirements

- **macOS**, **Linux**, or **Windows** (x64 or arm64)
- **Zed** editor (any recent version)
- Internet connection on first launch (automatic mode only)

## Caveats

- **Third-party & JDK sources**: `Cmd+Click` / goto-definition into JDK and
  third-party library classes **works** — the Rust bridge intercepts the
  server's `jar://` / `jrt://` URIs and fetches the source text from the
  IntelliJ server itself (`workspace/textDocumentContent`, the same mechanism
  the official VS Code extension uses), writes it to a local cache
  (`<workdir>/sources/`) that Zed opens, and remembers the mapping. If the
  server can't provide text, the bridge falls back to extracting from the
  jar's bundled sources, a sibling `-sources.jar`, or the JDK's `src.zip`.
- **First launch**: the initial project import can take a minute or two on
  large projects.
- **Java vs `java` extension**: this extension provides its own Java language
  definition, so uninstall the Zed `java` extension to avoid a conflict over
  the Java language and its debugger.

## License

The extension code is [MIT](LICENSE).

The IntelliJ LSP server is proprietary software by JetBrains, subject to its
own [EULA][3]. It is **not** bundled with or redistributed by this extension —
it is downloaded from JetBrains after you explicitly accept the EULA, or used
from a path you provide. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
