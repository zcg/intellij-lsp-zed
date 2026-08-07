# IntelliJ LSP for Zed

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Zed extension that brings [IntelliJ IDEA's LSP server][1] for Java & Kotlin to
the [Zed editor][2] — code completion, navigation, refactorings, inspections,
and quick-fixes for Maven, Gradle, and Bazel projects.

**Zero setup.** The extension downloads and installs the server automatically
on first launch.

[1]: https://blog.jetbrains.com/idea/2026/08/intellij-idea-goes-lsp/
[2]: https://zed.dev

## Install

Once published to the Zed extension registry:

1. Open Zed → `Cmd+Shift+P` → `zed: extensions` → search **IntelliJ LSP**
2. Open a Java or Kotlin project
3. The server (~368 MB) downloads and starts automatically — one-time, first
   launch only

**Not published yet?** Run it locally as a dev extension — see
[Development](#development) below.

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

> **Java support**: this extension now provides the Java language definition
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

### How it works

Under the hood the extension spawns the language server through a small Node
proxy that answers `start_debug_server` LSP requests, returns the DAP TCP port
Zed connects to, and proxies the DAP channel (rewriting IntelliJ's `file://`
source URIs into the absolute paths Zed needs to populate the Variables pane).
See [How It Works](#how-it-works).

> **Kotlin run/test tasks**: the gutter run/test buttons (`languages/kotlin/
tasks.json`) run through **PowerShell 7 (`pwsh`)** on every platform. On
> Windows that's the default; on macOS/Linux install it once with
> `brew install powershell` (macOS) or your distro's package manager. This
> keeps one task file working across Windows/macOS/Linux — the commands pick
> `gradlew.bat` on Windows and `./gradlew` elsewhere automatically.

## Disable Zed's Built-in Java/Kotlin Servers (optional)

The extension registers the IntelliJ server for Java and Kotlin automatically.
Zed also ships its own servers (`jdtls`, `kotlin-language-server`), so you'll
get duplicate diagnostics unless you disable them. Add to
`~/.config/zed/settings.json`:

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

1. The extension checks its cache for an existing server
2. If none is found, it fetches the latest version info from
   [Open VSX](https://open-vsx.org/extension/JetBrains/intellij-server) for the
   **current platform** (the plain `/latest` endpoint defaults to macOS arm64)
3. It downloads the server bundle from JetBrains' CDN and extracts it
4. The EULA acceptance hash is computed from the bundled `EULA.txt`
5. Your project imports (Maven/Gradle/Bazel) and language features activate

The language server is spawned through a bundled Node proxy (`src/proxy.cjs`)
that transparently forwards LSP stdio between Zed and the server. When you
start a debug session, the proxy forwards the `start_debug_server` request to
the server and returns the DAP TCP port that Zed connects to. Zed ships a Node
runtime, so no separate download is needed.

Fresh installs always get the latest published build. Cached versions are
reused on subsequent launches.

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
      "buildTool": "gradle"
    }
  }
}
```

Valid values: `gradle`, `maven`, `bazel`, `jps`. Omit the setting (or set it
to `null`) to auto-detect + prompt on conflict, or `""` to disable project
import.

## Evaluation & License

- During the preview the extension is **free** — each build is valid for
  **30 days** from its release date
- After the preview, an IntelliJ IDEA Ultimate subscription will be required
- If the server stops working after ~30 days, clear the extension's cache to
  fetch a newer build (see Troubleshooting)

## Troubleshooting

| Problem                                 | Fix                                                                                                                                                                                                                          |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Server won't start / evaluation expired | Clear the server cache: `rm -rf ~/Library/Application\ Support/Zed/extensions/work/intellij-lsp` (Linux: `~/.local/share/zed/extensions/work/`, Windows: `%LOCALAPPDATA%\Zed\extensions\work\intellij-lsp`), then reload Zed |
| Download fails                          | Check your internet connection, then retry — the extension resumes cleanly                                                                                                                                                   |
| Duplicate diagnostics                   | Add the `language_servers` config above to disable Zed's built-ins                                                                                                                                                           |

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

## Requirements

- **macOS**, **Linux**, or **Windows** (x64 or arm64)
- **Zed** editor (any recent version)
- Internet connection on first launch

## Caveats

- **Library sources**: `Cmd+Click` into JDK/Spring classes won't open their
  source (Zed doesn't support `jar://` URIs yet). Navigation within your own
  code works fine.
- **First launch**: the initial project import can take a minute or two on
  large projects.
- **Java vs `java` extension**: this extension provides its own Java language
  definition, so uninstall the Zed `java` extension to avoid a conflict over
  the Java language and its debugger.

## License

[MIT](LICENSE) — the extension is MIT-licensed.

The IntelliJ LSP server itself is proprietary software by JetBrains, subject
to its own [EULA](https://www.jetbrains.com/legal/).
