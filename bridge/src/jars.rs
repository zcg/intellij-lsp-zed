//! 虚拟源 URI(`jar://` / `jrt://` / `file://...zip!/...`)→ 本地文件
//!
//! IntelliJ 服务器对 JDK 与第三方库的源码引用返回虚拟 URI,例如
//! `jar:///D:/libs/guava-33.0.0-jre.jar!/com/google/common/collect/Lists.java`、
//! `jrt:/java.base/java/lang/String.java`,或(实测到的形式)
//! `file:///d:/jdk/lib/src.zip!/java.base/java/lang/RuntimeException.java`、
//! `file:///d:/mavenrepo/io/agentscope/...-sources.jar!/io/.../HarnessAgent.java`。
//! Zed 打不开这些虚拟路径,定义跳转会直接失败。
//!
//! 方案:从 jar/zip 内直接提取源码条目到
//! `<workdir>/sources/<hex(jar-path)>/<entry>` 缓存文件,再把消息里的 URI 改写
//! 成 Zed 能打开的 `file://` 路径。jar 在本地磁盘(Maven/Gradle 缓存、JDK
//! src.zip),直接读取即可,不依赖服务器。提取结果按 uri 记忆,后续跳转命中。

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// 解析虚拟源 URI → (压缩包路径, zip 内条目路径)。支持
/// `jar:///...jar!/entry` 与 `file:///...jar!/entry` 两种前缀。
///
/// 路径中的 `%XX` 编码会被解码(实测:Windows 盘符是 `d%3A`,即 `d:`),
/// entry 去掉前导 `/`。
pub fn parse_jar_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri
        .strip_prefix("jar:///")
        .or_else(|| uri.strip_prefix("file:///"))?;
    let (jar, entry) = rest.split_once("!/")?;
    let entry = entry.trim_start_matches('/');
    if jar.is_empty() || entry.is_empty() {
        return None;
    }
    Some((percent_decode(jar), percent_decode(entry)))
}

/// 解码 URI 中的 `%XX` 转义(`%3A` → `:`, `%2F` → `/` 等)。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_val(bytes[i + 1]);
            let lo = hex_val(bytes[i + 2]);
            if let (Some(hi), Some(lo)) = (hi, lo) {
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

/// 缓存的"虚拟 URI → 本地文件"映射。线程安全:多线程跳转互不干扰。
pub struct Cache {
    sources_dir: PathBuf,
    mem: Mutex<HashMap<String, String>>,
}

impl Cache {
    pub fn new(workdir: &str) -> Self {
        Self {
            sources_dir: Path::new(workdir).join("sources"),
            mem: Mutex::new(HashMap::new()),
        }
    }

    /// 是否已缓存过该虚拟 URI(返回本地 file:// 路径)。
    pub fn cached(&self, uri: &str) -> Option<String> {
        self.mem.lock().ok()?.get(uri).cloned()
    }

    /// 记录 uri → 本地 file:// 映射(服务器取文本成功后写入,下次直接命中)。
    pub fn remember(&self, uri: &str, file_uri: String) {
        if let Ok(mut mem) = self.mem.lock() {
            mem.insert(uri.to_string(), file_uri);
        }
    }

    /// 尝试把 `jar://` URI 改写成本地 `file://` 路径(从 jar 内直接提取源码;
    /// 用于服务器取文本失败的兜底)。`jrt://` 与提取失败返回 `None`。
    pub fn rewrite_local(&self, uri: &str) -> Option<String> {
        if let Some(cached) = self.cached(uri) {
            return Some(cached);
        }
        let (jar, entry) = parse_jar_uri(uri)?;
        let target = self.extract(&jar, &entry)?;
        let file_uri = path_to_file_uri(&target);
        self.remember(uri, file_uri.clone());
        Some(file_uri)
    }

    fn extract(&self, jar: &str, entry: &str) -> Option<PathBuf> {
        // 防路径穿越:entry 只允许普通路径段。
        let mut target = self.sources_dir.join(hex_string(jar));
        for part in Path::new(entry).components() {
            match part {
                Component::Normal(seg) => target.push(seg),
                _ => return None,
            }
        }
        if target.is_file() {
            return Some(target);
        }
        let content = read_from_zip(jar, entry)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).ok()?;
        }
        fs::write(&target, content).ok()?;
        Some(target)
    }
}

/// 读取 zip/jar 内某个条目;精确匹配优先,大小写不敏感与尾段匹配兜底。
fn read_from_zip(zip_path: &str, entry: &str) -> Option<Vec<u8>> {
    let file = fs::File::open(zip_path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    if let Ok(mut f) = archive.by_name(entry) {
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        return Some(buf);
    }
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).ok()?;
        let name = f.name().to_string();
        if name.eq_ignore_ascii_case(entry) || name.ends_with(entry) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).ok()?;
            return Some(buf);
        }
    }
    None
}

fn hex_string(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}

/// 是否是需要在 Zed 侧展开的虚拟源 URI:
/// - `jar://` / `jrt://`(IntelliJ 经典库 URI);
/// - `file://` 且路径中含 `!`(实测:JDK `src.zip!` 与 Maven `-sources.jar!`)。
pub fn is_virtual_uri(uri: &str) -> bool {
    uri.starts_with("jar://")
        || uri.starts_with("jrt:")
        || (uri.starts_with("file://") && uri.contains('!'))
}

/// 递归收集消息中所有虚拟源 URI(去重)。
pub fn collect_virtual_uris(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if is_virtual_uri(s) && !out.contains(s) {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_virtual_uris(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_virtual_uris(v, out);
            }
        }
        _ => {}
    }
}

/// 把 value 中所有等于 `from` 的字符串替换为 `to`。
pub fn replace_uri(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s == from {
                *s = to.to_string();
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replace_uri(item, from, to);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                replace_uri(v, from, to);
            }
        }
        _ => {}
    }
}

pub fn path_to_file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_jar_uri() {
        assert!(is_virtual_uri("jar:///D:/libs/guava.jar!/com/x/Lists.java"));
        assert!(is_virtual_uri("jrt:/java.base/java/lang/String.java"));
        assert!(is_virtual_uri(
            "file:///d:/jdk/lib/src.zip!/java.base/java/lang/RuntimeException.java"
        ));
        assert!(is_virtual_uri(
            "file:///d:/mavenrepo/io/x/lib-1.0-sources.jar!/io/x/Agent.java"
        ));
    }

    #[test]
    fn real_files_not_virtual() {
        assert!(!is_virtual_uri("file:///D:/Projects/app/src/Main.java"));
        assert!(!is_virtual_uri("file:///D:/Projects/app"));
        assert!(!is_virtual_uri("untitled:Untitled-1"));
        assert!(!is_virtual_uri(""));
    }

    #[test]
    fn collect_and_replace() {
        let mut msg = serde_json::json!({
            "result": [{
                "uri": "jar:///D:/libs/guava.jar!/com/x/Lists.java",
                "range": { "start": { "line": 0 } }
            }]
        });
        let mut uris = Vec::new();
        collect_virtual_uris(&msg, &mut uris);
        assert_eq!(uris.len(), 1);
        replace_uri(&mut msg, &uris[0], "file:///C:/cache/Lists.java");
        assert_eq!(
            msg["result"][0]["uri"],
            "file:///C:/cache/Lists.java"
        );
    }

    #[test]
    fn cache_remember_and_hit() {
        let cache = Cache::new(r"C:\workdir");
        assert!(cache.cached("jar:///x.jar!/a.java").is_none());
        cache.remember("jar:///x.jar!/a.java", "file:///C:/sources/a.java".into());
        assert_eq!(
            cache.cached("jar:///x.jar!/a.java").as_deref(),
            Some("file:///C:/sources/a.java")
        );
    }

    #[test]
    fn parses_percent_encoded_windows_uri() {
        // 实测格式:盘符是 `d%3A`(即 `d:`),其余路径未编码。
        let (jar, entry) = parse_jar_uri(
            "jar:///d%3A/mavenrepo/io/agentscope/agentscope-harness/2.0.1/agentscope-harness-2.0.1-sources.jar!/io/agentscope/harness/agent/HarnessAgent.java",
        )
        .unwrap();
        assert_eq!(
            jar,
            "d:/mavenrepo/io/agentscope/agentscope-harness/2.0.1/agentscope-harness-2.0.1-sources.jar"
        );
        assert_eq!(entry, "io/agentscope/harness/agent/HarnessAgent.java");
    }

    #[test]
    fn local_extraction_fallback() {
        use std::io::Write;
        let dir = std::env::temp_dir().join("intellij-lsp-test-local-extract");
        let _ = fs::create_dir_all(&dir);
        let jar_path = dir.join("demo-1.0-sources.jar");
        {
            let file = fs::File::create(&jar_path).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("io/agentscope/harness/agent/HarnessAgent.java", opts)
                .unwrap();
            w.write_all(b"package io.agentscope; class HarnessAgent {}")
                .unwrap();
            w.finish().unwrap();
        }

        let cache = Cache::new(&dir.to_string_lossy());
        // 构造带 %3A 编码的 jar:// uri(与服务器返回一致)。
        let jar_uri = jar_path
            .to_string_lossy()
            .replace(':', "%3A")
            .replace('\\', "/");
        let uri = format!("jar:///{jar_uri}!/io/agentscope/harness/agent/HarnessAgent.java");
        let file_uri = cache.rewrite_local(&uri).expect("local extraction");
        assert!(file_uri.starts_with("file:///"));
        let path = file_uri.strip_prefix("file:///").unwrap().replace('/', "\\");
        let content = fs::read_to_string(std::path::PathBuf::from(&path)).unwrap();
        assert!(content.contains("class HarnessAgent"));

        let _ = fs::remove_dir_all(&dir);
    }
}
