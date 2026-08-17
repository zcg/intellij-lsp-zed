# IntelliJ LSP for Zed(中文版)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

🌐 [**English**](README.md) | **简体中文**

非官方的 Zed 扩展,将 [IntelliJ IDEA 的 LSP 服务器][1] 的 Java 与 Kotlin 支持带入
[Zed 编辑器][2] —— 包括代码补全、导航、重构、代码检查、快速修复,**以及完整的
IntelliJ 调试器**,支持 Maven、Gradle 与 Bazel 项目。

> **重要 —— 许可声明。** IntelliJ LSP 服务器是 JetBrains 的**专有软件**(非开源)。
> 在扩展下载或运行它之前,你必须阅读并接受 [JetBrains EULA][3]。本扩展**绝不**
> 从第三方注册表(如 Open VSX API)获取服务器:它要么直接从 JetBrains CDN 下载
> 固定版本,要么使用你自己下载的服务器。参见 [License](#license-许可)。

[1]: https://blog.jetbrains.com/idea/2026/08/intellij-idea-goes-lsp/
[2]: https://zed.dev
[3]: https://www.jetbrains.com/legal/docs/toolbox/user/

## 亮点 —— 相比原版的增强

本仓库在[上游 intellij-lsp-zed][6](v0.2.0)的基础上,新增了完整的 IntelliJ
调试体验与原生 Rust bridge:

- **完整 IntelliJ 调试器** —— 为 Java 与 Kotlin 提供 `intellij_debugger`
  DAP 适配器:F5 启动、附加到运行中的 JVM、gutter 调试(主类自动从
  `build.gradle.kts` / `pom.xml` 推断)。
- **原生 Rust bridge**(`bridge/`)— 取代旧的 Node.js 代理:LSP stdio 转发、
  `start_debug_server` HTTP 端点、DAP TCP 代理(把 IntelliJ 的 `file://`
  源 URI 重写为 Zed Variables 面板所需的路径)。已为全部 6 个平台发布
  (Windows/macOS/Linux × x64/arm64)。
- **第三方库与 JDK 源码跳转** —— 在库类上 `Cmd+Click` 即可打开真实源码:
  bridge 从本地 `<artifact>-<version>-sources.jar` / JDK `src.zip` 提取到
  缓存,解决上游返回 `jar://` URI 导致 Zed 无法打开的问题。
- **Java 与 Kotlin 一等支持** —— 自带完整语言定义(语法、高亮、runnables、
  tasks),覆盖 Java、Kotlin、Gradle、Gradle-KTS 与 Properties。
- **健壮性修复** —— 项目模型未导入时调试启动不再中止;`javaExec` 优先由
  服务器解析(绝不注入裸 `java`);工作目录回退到工作区根;文件型 worktree
  根被归一化;过期 bridge/server 版本自动清理;bridge 退出时终止孤儿 JVM。
- **inlayHint 拦截** —— bridge 本地应答 `textDocument/inlayHint`(该请求在
  IntelliJ 服务器上必定失败),消除报错刷屏并减轻服务器负担。
- **跨平台任务** —— gutter run/test 任务为朴素的 `./gradlew` 命令,任何
  shell 都能执行(Windows 差异见下文说明)。
- **双语文档** —— 英文与简体中文 README,支持语言切换。

[6]: https://github.com/hlucas13/intellij-lsp-zed

## 安装(Install)

发布到 Zed 扩展注册表后:

1. 打开 Zed → `Cmd+Shift+P` → `zed: extensions` → 搜索 **IntelliJ LSP**
2. **接受 JetBrains EULA** —— 在你的 Zed `settings.json`(Linux/macOS 为
   `~/.config/zed/settings.json`)中添加:

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

3. 打开一个 Java 或 Kotlin 项目。服务器(约 368 MB)会从 JetBrains CDN 下载一次,
   之后启动复用缓存。

### 从仓库安装(开发扩展)

1. 克隆仓库:
   ```sh
   git clone https://github.com/zcg/intellij-lsp-zed.git
   ```
2. 在 Zed 中:`Cmd+Shift+P` → `zed: install dev extension`,选择克隆的文件夹。
3. 无需 Rust 工具链 —— 仓库中已提交预构建的 `extension.wasm`。更新时重新
   `git pull` 并重装即可。

### 使用手动下载的服务器

如果你更喜欢完全掌控,可以从 [JetBrains 公告][1] 下载服务器、解压,然后把扩展
指向 `intellij-server` 可执行文件:

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

`server_path` 必须**直接指向 `intellij-server` 可执行文件**(Windows 上为
`intellij-server.exe`)。扩展运行在沙箱中,无法解压沙箱外的压缩包。`server_path`
优先级高于沙箱缓存与固定版本自动下载。

## 调试(Debugging)

扩展自带 IntelliJ 调试引擎,调试适配器名为 `intellij_debugger`(同时绑定
Java 和 Kotlin)。

### 0. 先构建(重要)

IntelliJ 调试器从项目模型解析 classpath,但**不会**帮你编译代码。请先构建:

```sh
./gradlew build      # Gradle
mvn compile          # Maven
bazel build //...    # Bazel
```

没有最近的构建直接调试会报 `java.lang.ClassNotFoundException`。

### 1. 启动(F5)

1. 打开 Java/Kotlin 项目,等待语言服务器完成导入(首次导入大项目可能需要一两分钟)。
2. 设置断点,按 **F5**(`debug: start`)。
3. 选择 **IntelliJ LSP** → **Launch**。`mainClass` 会自动从项目模型解析,
   `javaExec`/`classPaths`/`cwd` 也会自动解析——无需配置任何东西。

需要按项目定制(例如程序参数)时,创建 `.zed/debug.json`:

```jsonc
[
  {
    "adapter": "intellij_debugger",
    "request": "launch",
    "label": "Debug MainKt",
    "mainClass": "org.example.MainKt",
    "args": ["side-effect"], // 可选:程序参数
    "vmArgs": ["-Xmx2G"], // 可选:JVM 参数
    "cwd": "$ZED_WORKTREE_ROOT",
  },
]
```

### 2. 从 gutter 调试

扩展为 `main` runnable 注册了调试定位器——**Kotlin**(`run main`)和
**Java**(`Run MyClass`)都适用。将鼠标悬停在 `fun main` / `main` 旁的 gutter
上选择 **Debug** 即可——无需配置;主类从 `build.gradle.kts` / `pom.xml` 推断。

> **Java 支持**:本扩展自带 Java 语言定义(语法、高亮、runnables、tasks)——
> Java 与 Kotlin 是平级的一等公民。**请卸载 Zed 的 `java` 扩展**,避免两者
> 争夺 Java 语言。两种语言都自动使用 IntelliJ 调试器(`intellij_debugger`)。

### 3. 附加到运行中的 JVM

以 JDWP 方式启动你的 JVM:

```sh
java -agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005 \
     -cp <your-classpath> com.example.Main
```

然后在 `.zed/debug.json` 中:

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

或者先启动应用,再 F5 → **IntelliJ LSP** → **Attach**。

### 调试器工作原理

底层上,扩展通过一个小的 Rust bridge(`bridge/`)来启动语言服务器:它应答
`start_debug_server` LSP 请求、返回 Zed 要连接的 DAP TCP 端口,并代理 DAP
通道(把 IntelliJ 的 `file://` 源 URI 重写为 Zed 的 Variables 面板所需的绝对
路径)。bridge 是原生二进制,首次启动时从本扩展的 GitHub Release 下载——
**不涉及 Node.js**。

> **Run/test 任务与 Windows 和 Linux/macOS 的差异** —— gutter 的
> run/test 按钮(`languages/kotlin/tasks.json`、`languages/java/tasks.json`)
> 以朴素的 `./gradlew ...` 命令形式发布,使用系统默认 shell。这种形式正是
> **Linux 与 macOS** 使用的——它们直接运行仓库内的 Gradle wrapper。
>
> **Windows 不同**:Gradle wrapper 是 `gradlew.bat` 而非 `./gradlew`,且默认
> shell 是 PowerShell。如果任务在 Windows 上失败,请把这两个 `tasks.json` 的
> `command` 字段改成 PowerShell 形式。每个命令需要改两处:
>
> 1. `./gradlew` → `gradlew.bat`(不要 `./` 前缀 —— PowerShell 会从项目目录
>    解析 `gradlew.bat`,但不会解析 `./gradlew`);
> 2. `--tests` 参数内的 Zed `$ZED_CUSTOM_*` 变量必须用双引号包裹,写法严格
>    为 `"$ZED_CUSTOM_kotlin_package_name.$ZED_CUSTOM_kotlin_class_name"`
>    —— PowerShell 会在引号内展开变量,引号也保证 Gradle 的 `--tests`
>    pattern 不被拆分。
>
> 示例,Kotlin `test` 任务在 Windows 上:
>
> ```json
> "command": "gradlew.bat test --tests \"$ZED_CUSTOM_kotlin_package_name.$ZED_CUSTOM_kotlin_class_name\""
> ```

## 设置(Settings)

所有设置都位于 Zed `settings.json` 的 `lsp.intellij-server.settings` 下。

| 键                                | 类型    | 必填 | 说明                                                                                                    |
| --------------------------------- | ------- | ---- | ------------------------------------------------------------------------------------------------------- |
| `accept_jetbrains_eula`           | boolean | 是   | 显式接受 JetBrains EULA。非 `true` 不下载、不执行任何内容。                                             |
| `server_path`                     | string  | 否   | 已解压的 `intellij-server` 可执行文件路径(覆盖自动下载)。                                               |
| `server_version`                  | string  | 否   | 覆盖固定服务器版本(自动模式)。                                                                          |
| `server_download_url`             | string  | 否   | 覆盖固定的 JetBrains 下载 URL(自动模式)。                                                               |
| `eula_hash`                       | string  | 否   | EULA 接受哈希覆盖(高级——见 Troubleshooting)。                                                           |
| `intellij.additionalJvmArgs`      | array   | 否   | 服务器进程的 JVM 参数(如 `["-Xmx4g"]` 提高默认 2 GB 堆)。                                               |
| `intellij.dataSharing`            | string  | 否   | `"full"` / `"anonymous"` / `"none"`。**默认 `none`** —— 独立同意项,绝不继承自 `accept_jetbrains_eula`。 |
| `intellij.region`                 | string  | 否   | JetBrains 产品条款/数据处理地区。                                                                       |
| `intellij.projects`               | array   | 否   | 多仓库项目条目(`[{ "type": "gradle", "path": "file:///..." }]`)。                                       |
| `intellij.buildTool`              | string  | 否   | 全局构建工具覆盖(`"gradle"`、`"maven"`、`"bazel"`,或 `""` 禁用全部)。`buildTool` 作为别名同样接受。     |
| `intellij.jdkForSymbolResolution` | string  | 否   | 用于符号解析的 JDK home 路径。                                                                          |

这些键由扩展消费,并通过**初始化选项**与**环境变量**传递给服务器(与官方
JetBrains VS Code 扩展完全一致)。

## 高级:JetBrains 服务器设置

JetBrains 官方 VS Code 扩展通过**初始化选项**(`eulaHash`、`projects`、
`buildTools`、`defaultSdk`)与**环境变量**(`IJ_JAVA_OPTIONS`、
`INTELLIJ_DATA_SHARING`、`INTELLIJ_REGION`)向语言服务器传递设置。本扩展使用
相同的键(含点号)1:1 复刻该行为。

### 完整示例

一个实际的 `~/.config/zed/settings.json`:为 Java 和 Kotlin 使用 IntelliJ
服务器,接受 EULA、堆提升到 4 GB、保持数据共享关闭、设置地区,并限定两个
多仓库子项目导入:

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

### 各设置说明

其他有用的项:

- `intellij.additionalJvmArgs` —— `["-Xmx4g"]`(→ `IJ_JAVA_OPTIONS`;默认堆 2 GB)
- `intellij.dataSharing` —— `"full"` / `"anonymous"` / `"none"`(可选,默认为 `none`;见 [数据共享](#数据共享))
- `intellij.region` —— 你的地区,用于 JetBrains 产品条款与数据处理
- `intellij.buildTool` —— `"gradle"` / `"maven"` / `"bazel"`(→ 初始化选项中的 `buildTools`)
- `intellij.jdkForSymbolResolution` —— JDK home 路径(→ 初始化选项中的 `defaultSdk`)

完整的官方 IntelliJ LSP 设置列表见[官方文档][4]。

### 选择构建工具

默认让服务器自动检测构建工具。当项目混合多种格式(例如 `build.gradle.kts`
**和** `.idea/` JPS 目录同时存在)时,服务器会询问使用哪个,Zed 会显示选项
——选择 `Use Gradle`、`Use Maven` 等即可。不会替你决定。

要固定选择(跳过提示)或禁用导入,在 `~/.config/zed/settings.json` 中设置:

```json
"lsp": {
  "intellij-server": {
    "settings": {
      "intellij.buildTool": "gradle"
    }
  }
}
```

有效值:`gradle`、`maven`、`bazel`、`jps`。省略该设置(或设为 `null`)表示
自动检测 + 冲突时提示;设为 `""` 则禁用项目导入。`buildTool` 键作为别名同样
接受。

## 已知限制(Zed)

- **Live templates 与 file templates** 是 VS Code 侧的编辑器功能;不属于
  LSP 协议。
- **每个工作区一个后端。** 同一时间只能有一个 IntelliJ 服务器访问某个工作区
  —— 不要同时在 VS Code 和 Zed 里打开同一个文件夹。
- **运行时未实现压缩包完整性校验。** 每个平台服务器压缩包的官方 sha256
  (来自 `server-bundle.json`)保存在 `server-artifacts.json` 中,并记录在
  `src/lib.rs` 注释里,但扩展下载后并不校验,因为 `zed_extension_api` 0.7.0
  的 `download_file` 会原地解压,不向 WASM 沙箱暴露原始字节做哈希。压缩包走
  HTTPS 传输,`download_file` 会报告 HTTP 错误;固定 URL 位于 JetBrains 自家
  CDN。未来支持"先下载后解压"的 `zed_extension_api` 版本会让扩展在校验 sha256
  后再信任内容。

[4]: https://www.jetbrains.com/help/intellij-vscode/IntelliJ-lsp-settings.html
[5]: https://www.jetbrains.com/help/intellij-vscode/Project-import.html

## 禁用 Zed 内置的 Java/Kotlin 服务器(可选)

扩展会自动为 Java 与 Kotlin 注册 IntelliJ 服务器。Zed 也自带自己的服务器
(`jdtls`、`kotlin-language-server`),不禁用的话会出现重复诊断:

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

`!` 前缀表示禁用该语言的对应内置服务器。

## 工作原理(How It Works)

1. 每次启动,扩展都会检查你是否接受了 JetBrains EULA
   (`accept_jetbrains_eula`)。未接受则拒绝启动,并明确打印需要在
   `settings.json` 中添加的内容——不下载、不执行。

2. 检查是否设置了 `server_path`;若设置了,立即使用该二进制(显式覆盖优先于
   一切)。

3. 检查沙箱缓存中是否已有安装过的服务器,有则复用。

4. 若未安装,直接从 JetBrains CDN 下载固定版本——版本与各平台 URL 来自
   `server-artifacts.json`,它在编译时嵌入扩展,并由每两周一次的 CI 工作流
   保持更新(见 [自动更新](#自动更新))。

5. 从捆绑的 `EULA.txt` 计算 EULA 接受哈希,通过初始化选项在启动时传给服务器。
   JetBrains 设置(`intellij.projects`、`intellij.buildTool` 等)同样在启动时
   转发——与官方 VS Code 扩展行为一致。

6. 语言服务器通过 Rust bridge(`bridge/`)启动,它在 Zed 与服务器之间透明转发
   LSP stdio。开始调试会话时,bridge 把 `start_debug_server` 请求转发给服务器,
   返回 Zed 要连接的 DAP TCP 端口。bridge 从本扩展的 GitHub Release 下载一次。

7. 你的项目导入(Maven/Gradle/Bazel)与语言功能随即生效。

缓存版本在后续启动中复用。

## 自动更新(Auto-update)

固定服务器版本与下载 URL 位于 [`server-artifacts.json`](server-artifacts.json)
—— 一个 JSON 文件,包含当前版本与全部 6 个支持平台(macOS x86_64/ARM64、
Linux x86_64/ARM64、Windows x86_64/ARM64)各自的条目(URL + sha256 +
压缩包类型)。该文件在编译时嵌入扩展——运行时零查询。

两个 CI 工作流让 pin 保持最新,都在维护者仓库运行,绝不在最终用户机器上运行:

- **上游构建检测 + 注册表传播**(`auto-update.yml`)—— 每月 1 日与 15 日运行
  (约 13–17 天的稳定间隔)。查询一次 Open VSX API 检查 JetBrains 是否发布了
  新的 vsix。若发现新版本,从 `openvsx.eclipsecontent.org` 下载全部 6 个平台的
  vsix 包,解出各自的 `extension/server-bundle.json`,重建
  `server-artifacts.json`,重建 WASM,提升版本,提交并推送。随后通过提升扩展
  git submodule 并更新 `extensions.toml` 的 `version` 字段,把更新传播到
  `zed-industries/extensions` 注册表——遵循 Zed 官方文档的扩展更新流程。
  构建检测步骤对 Open VSX 基础设施是真实流量(尽管低频、单维护者、不可扩展)
  ——它运行在 GitHub CI,绝不在最终用户机器上,绝不因安装而触发,流量不随
  扩展采用量增长。注册表传播步骤**完全零 Open VSX 流量**——纯粹是对扩展
  仓库的 Git 操作。
- **CDN 健康检查**(`monitor.yml`)—— 同样在每月 1 日与 15 日运行。验证固定
  JetBrains CDN URL 是否仍可访问。失败则在扩展仓库打开 `extension-broken`
  issue。

JetBrains 大约每两周发布一次 preview 构建,每个构建在过期前有效 30 天——
所以 1 日/15 日的计划总是能在上一个构建过期前至少 13 天的余量内抓到新构建。

扩展本身从不触碰任何注册表 API:pin 是静态且预提交的。

## 评估与许可(Evaluation & License)

- 预览期间服务器**免费**——每个构建自发布日起**30 天**有效
- 预览结束后需要 IntelliJ IDEA Ultimate 订阅
- 若约 30 天后服务器停止工作,请安装更新的构建(清除扩展缓存;见 Troubleshooting)

### 数据共享

JetBrains 自己的客户端(VS Code、Cursor)在安装扩展后还会要求用户接受
**数据共享政策**并选择地区。本扩展默认**禁用数据共享**:服务器以
`dataSharing=NONE` 运行,不向 JetBrains 发送遥测。如果你想启用遥测,把
`intellij.dataSharing` 设为 `"full"` 或 `"anonymous"`——这与 EULA 接受是
完全独立的决定。

## 故障排查(Troubleshooting)

| 问题                                          | 解决                                                                                                                                                                                                        |
| --------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "you must read and accept the JetBrains EULA" | 在 `lsp.intellij-server.settings` 下添加 `"accept_jetbrains_eula": true`(见 [安装](#安装install))并重载窗口。                                                                                               |
| "Bundled license agreement is not accepted"   | 服务器会报告它期望的哈希(如 `expected hash 34d850193ee04897`)。如果使用手动 `server_path` 运行服务器,把该哈希复制到 `eula_hash` 设置。自动下载会自动计算。                                                  |
| "Cannot derive JDK home from javaExec"        | 调试启动收到了裸的 `java` 而不是真实的 `<home>/bin/java` 路径。重载扩展后再次启动调试会话——服务器现在会自动解析项目 SDK 的 JDK 路径。                                                                       |
| 服务器无法启动 / 评估过期                     | 清除服务器缓存:`rm -rf ~/Library/Application\ Support/Zed/extensions/work/intellij-lsp`(Linux:`~/.local/share/zed/extensions/work/`,Windows:`%LOCALAPPDATA%\Zed\extensions\work\intellij-lsp`),然后重载 Zed |
| 下载失败                                      | 检查网络连接后重试——扩展可干净续传                                                                                                                                                                          |
| 重复诊断                                      | 添加上面的 `language_servers` 配置禁用 Zed 内置服务器                                                                                                                                                       |

## 开发(Development)

需要 **Rust**(`wasm32-wasip2` target)与 **git**(安装 dev 扩展时 Zed 会从
声明的仓库编译 Kotlin tree-sitter grammar)。

### 安装 dev 扩展(推荐)

```sh
# 构建扩展(需要 Rust + wasm32-wasip2 target)
cargo build --release --target wasm32-wasip2

# 保持提交的 wasm 同步(CI 会校验)
cp target/wasm32-wasip2/release/intellij_lsp_zed.wasm extension.wasm
```

然后在 Zed 中:

1. `Cmd+Shift+P` → `zed: extensions`
2. 右上角齿轮图标 → **Install Dev Extension...**
3. 选择本仓库文件夹(`intellij-lsp-zed`)

Zed 会编译 Rust 扩展与 Kotlin grammar,并以 **Dev** 徽标加载。修改
`src/*.rs`、`extension.toml` 或 `languages/kotlin/*` 后重载扩展(或重启 Zed)。

> Windows 上请从 `rustc` 在 `PATH` 中的终端启动 Zed——Zed 的 GUI 进程在
> 重新登录前可能不会继承更新后的 PATH。

### 手动复制到 `extensions/installed`

把 `extension.toml` + `extension.wasm` 复制到 `extensions/installed/<id>/`
**不推荐**:Zed 只在扩展面板中显示来自注册表或 dev 扩展的扩展。请使用上面的
dev-extension 流程。

### 检查

```sh
cargo test
cargo clippy --target wasm32-wasip2 -- -D warnings
cargo fmt -- --check
```

### 项目结构

| 路径                                | 用途                                                                                       |
| ----------------------------------- | ------------------------------------------------------------------------------------------ |
| `src/lib.rs`                        | 扩展入口 —— EULA 门控、二进制解析、下载/启动、初始化选项、调试适配器                       |
| `bridge/`                           | Rust bridge —— LSP stdio 转发 + `start_debug_server` + DAP 端口代理与 `file://` → 路径重写 |
| `server-artifacts.json`             | 固定服务器版本 + 各平台下载 URL(事实来源,由 CI 更新)                                       |
| `languages/`                        | Java/Kotlin/Gradle/Gradle-KTS/Properties 语言定义(语法、高亮、runnables、tasks)            |
| `extension.toml`                    | Zed 扩展清单                                                                               |
| `extension.wasm`                    | 预构建 WASM 二进制(用户无需 Rust)                                                          |
| `scripts/update-artifacts.py`       | CI 辅助 —— 下载全部平台 vsix、解出 `server-bundle.json`、重建 `server-artifacts.json`      |
| `scripts/bump-version.py`           | CI 辅助 —— 提升 `extension.toml`、`Cargo.toml`、`package.json` 的补丁版本                  |
| `.github/workflows/auto-update.yml` | 每两周一次的 CI,检测新 JetBrains 构建并自动更新 pin + 发布                                 |
| `.github/workflows/monitor.yml`     | 每两周一次的 CI 健康检查 —— 验证固定 CDN URL 可访问                                        |
| `.github/workflows/ci.yml`          | push/PR CI —— fmt、clippy、测试、wasm 构建                                                 |

### 手动更新固定服务器(CI 无法处理时)

auto-update 工作流 99% 的情况下会处理。手动时需要:

1. 在浏览器中从 [Open VSX](https://open-vsx.org/extension/JetBrains/intellij-server)
   下载每个平台的新 `JetBrains.intellij-server` vsix(每平台一次手动下载——正常
   的终端用户用法)。

2. 解压每个 vsix 并读取 `extension/server-bundle.json`:里面是该平台真实的
   JetBrains CDN `url`、`version` 与 `sha256`。

3. 运行 `python3 scripts/update-artifacts.py <vsix-version>`,从下载的 vsixes
   重建 `server-artifacts.json`。

4. 验证新服务器包内的 `EULA.txt` 与 vsix 包内的 `LICENSE.txt` 逐字节一致
   (v263.2689.0 时如此——每次升级都需复查,避免哈希漂移)。

5. 重建 WASM:`cargo build --release --target wasm32-wasip2`,复制
   `extension.wasm`,提升版本并发布。

## 环境要求(Requirements)

- **macOS**、**Linux** 或 **Windows**(x64 或 arm64)
- **Zed** 编辑器(任意近期版本)
- 首次启动需要联网(仅自动模式)

## 注意事项(Caveats)

- **第三方库与 JDK 源码**:在 JDK 或第三方库类上 `Cmd+Click` / 转到定义**可以
  打开源码** —— Rust bridge 会拦截服务器的 `jar://` / `jrt://` URI,向
  IntelliJ 服务器本身请求源码文本(`workspace/textDocumentContent`,与官方
  VS Code 扩展同一机制),写入本地缓存(`<workdir>/sources/`)供 Zed 打开,并
  记住映射。服务器拿不到文本时,bridge 回退为从 jar 内打包源码、同目录
  `-sources.jar`、或 JDK 的 `src.zip` 提取。
- **首次启动**:大项目的首次导入可能需要一两分钟。
- **Java 与 `java` 扩展**:本扩展自带 Java 语言定义,请卸载 Zed 的 `java`
  扩展,避免 Java 语言及其调试器冲突。

## 许可(License)

扩展代码为 [MIT](LICENSE)。

IntelliJ LSP 服务器是 JetBrains 的专有软件,受其自有 [EULA][3] 约束。它
**不**随本扩展捆绑或再分发——在你显式接受 EULA 后从 JetBrains 下载,或使用
你提供的路径。参见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
