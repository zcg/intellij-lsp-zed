//! 虚拟源 URI(`jar://` / `jrt://` / `file://...zip!/...`)→ 本地文件
//!
//! IntelliJ 服务器对 JDK 与第三方库的源码引用返回虚拟 URI,例如
//! `jar:///D:/libs/guava-33.0.0-jre.jar!/com/google/common/collect/Lists.java`、
//! `jrt:/java.base/java/lang/String.java`,或(实测到的形式)
//! `file:///d:/jdk/lib/src.zip!/java.base/java/lang/RuntimeException.java`、
//! `file:///d:/mavenrepo/io/agentscope/...-sources.jar!/io/.../HarnessAgent.java`。
//! Zed 打不开这些虚拟路径,定义跳转会直接失败。
//!
//! 方案(与官方 VS Code 插件一致):向 IntelliJ 服务器发
//! `workspace/textDocumentContent` 请求拿源码文本,写入
//! `<workdir>/sources/<hex(uri)>/<basename>` 缓存文件,再把消息里的 URI 改写
//! 成 Zed 能打开的 `file://` 路径。提取结果按 uri 记忆,后续跳转直接命中。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 缓存的"虚拟 URI → 本地文件"映射。线程安全:多线程跳转互不干扰。
pub struct Cache {
    mem: Mutex<HashMap<String, String>>,
}

impl Cache {
    pub fn new(_workdir: &str) -> Self {
        Self {
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
}

/// 是否是需要在 Zed 侧展开的虚拟源 URI:
/// - `jar://` / `jrt://`(IntelliJ 经典库 URI);
/// - `file://` 且路径中含 `!`(实测:JDK `src.zip!` 与 Maven `-sources.jar!`)。
pub fn is_virtual_uri(uri: &str) -> bool {
    uri.starts_with("jar://")
        || uri.starts_with("jrt:")
        || (uri.starts_with("file://") && uri.contains('!'))
}

/// 按原始 URI 计算落盘缓存路径:`sources/<hex(uri)>/<basename>`。
/// 服务器取文本成功后写入,之后直接打开本地文件。
pub fn cache_target_for(uri: &str, workdir: &str) -> Option<PathBuf> {
    let base = uri
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())?
        .split('!')
        .next()
        .unwrap_or_default();
    if base.is_empty() {
        return None;
    }
    Some(
        Path::new(workdir)
            .join("sources")
            .join(hex_string(uri))
            .join(base),
    )
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

fn hex_string(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
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
    fn cache_target_uses_uri_hash_and_basename() {
        let target = cache_target_for(
            "file:///d:/jdk/lib/src.zip!/java.base/java/lang/String.java",
            r"C:\workdir",
        )
        .unwrap();
        let s = target.to_string_lossy();
        assert!(s.starts_with(r"C:\workdir\sources\"));
        assert!(s.ends_with("String.java"));
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
}
