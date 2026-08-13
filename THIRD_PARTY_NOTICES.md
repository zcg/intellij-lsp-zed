# Third-party notices

## IntelliJ LSP server (JetBrains)

- **Copyright**: © JetBrains s.r.o.
- **License**: Proprietary — JetBrains EULA
  (https://www.jetbrains.com/legal/docs/toolbox/user/). The exact license
  also ships as `EULA.txt` inside the server bundle.
- **Distribution**: The server is **not** bundled with, vendored in, or
  redistributed by this extension. It is either downloaded at runtime from
  JetBrains' own CDN or used from a path the user provides via the
  `server_path` setting — and only **after the user explicitly accepts the
  JetBrains EULA** through the `accept_jetbrains_eula` setting.
- **Data sharing**: JetBrains' own clients also prompt users to accept a
  data-sharing policy and choose a region. This extension does **not** enable
  data sharing: the server runs with data sharing disabled
  (`dataSharing=NONE`) and no telemetry is configured.
- The server may include third-party components subject to their own licenses.

## IntelliJ LSP server download

- **Source**: JetBrains' CDN, pinned to a verified version in
  [`server-artifacts.json`](server-artifacts.json) and embedded in the
  extension at compile time via `platform_artifact()`.
- The extension does not query third-party registry APIs (such as the Open VSX
  API) at runtime.
- **Integrity**: the official sha256 hash from `extension/server-bundle.json`
  inside the JetBrains-published `.vsix` is recorded in code comments for
  each platform. Runtime verification of the downloaded archive is not
  currently implemented — `zed_extension_api` 0.7.0's `download_file` does
  not expose raw bytes to the WASM sandbox after extraction. See the README
  "Known limitations" section for details.

## This extension (intellij-lsp)

- **License**: MIT — see [LICENSE](LICENSE).

## Open-source dependencies

- `zed_extension_api` — MIT OR Apache-2.0
- `serde`, `serde_json` — MIT OR Apache-2.0
- `sha2` — MIT OR Apache-2.0
