//! jar:// → 本地源码文件
//!
//! IntelliJ 服务器对 JDK 与第三方库的源码引用返回 `jar://` URI,例如
//! `jar:///D:/path/to/guava-33.0.0-jre.jar!/com/google/common/collect/Lists.java`
//! 或 JDK 的 `jar:///.../src.zip!/java/lang/String.java`。Zed 不识别
//! `jar://` scheme,定义跳转会直接失败。
//!
//! 这里的做法是:把 jar/zip 内的源码条目提取到
//! `<workdir>/sources/<hex(jar-path)>/<entry>` 缓存文件,然后把 URI 改写成
//! Zed 能打开的 `file://` 路径。提取结果按 (jar, entry) 记忆,后续跳转直接
//! 命中缓存,不再打开压缩包。

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// 解析 `jar:///...jar!/entry` → (jar 路径, zip 内条目路径)。
///
/// 返回的 entry 已去掉前导 `/`(IntelliJ 有时写成 `!/java/lang/String.java`)。
pub fn parse_jar_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("jar:///")?;
    let (jar, entry) = rest.split_once("!/")?;
    let entry = entry.trim_start_matches('/');
    if jar.is_empty() || entry.is_empty() {
        return None;
    }
    Some((jar.to_string(), entry.to_string()))
}

/// 缓存的源码提取器。线程安全:多线程跳转互不干扰。
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

    /// 把 `jar://` URI 改写成本地 `file://` 路径;无法提取时返回 `None`
    /// (调用方保留原 URI,不阻塞消息)。
    pub fn rewrite(&self, uri: &str) -> Option<String> {
        if let Ok(mem) = self.mem.lock() {
            if let Some(cached) = mem.get(uri) {
                return Some(cached.clone());
            }
        }
        let (jar, entry) = parse_jar_uri(uri)?;
        let target = self.extract(&jar, &entry)?;
        let file_uri = path_to_file_uri(&target);
        if let Ok(mut mem) = self.mem.lock() {
            mem.insert(uri.to_string(), file_uri.clone());
        }
        Some(file_uri)
    }

    fn extract(&self, jar: &str, entry: &str) -> Option<PathBuf> {
        // 防路径穿越:entry 只允许普通路径段,拒绝 `..`、根路径等。
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

        // 1. 从 jar 本身读源码(库 jar 可能打包了 .java;JDK 的 `src.zip`
        //    本身就是 zip,同样适用)。
        if let Some(content) = read_from_zip(jar, entry) {
            write_cache(&target, content)?;
            return Some(target);
        }

        // 2. jar 内没有源码 → 尝试同目录的 `<artifact>-<version>-sources.jar`
        //    (Maven `~/.m2/repository` 与 Gradle 缓存都把 sources jar 和
        //    jar 放在同一目录;JDK 的 `src.zip` 在第一步已命中)。
        if let Some(sources) = sibling_sources_jar(jar) {
            if let Some(content) = read_from_zip(&sources, entry) {
                write_cache(&target, content)?;
                return Some(target);
            }
        }

        None
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

/// 由 jar 路径推导同目录的 sources jar:`<artifact>-<version>.jar` →
/// `<artifact>-<version>-sources.jar`。已是 sources jar 时返回 None。
fn sibling_sources_jar(jar: &str) -> Option<String> {
    let p = Path::new(jar);
    let file_name = p.file_name()?.to_str()?;
    if file_name.ends_with("-sources.jar") || file_name.ends_with("-src.jar") {
        return None;
    }
    let stem = file_name.strip_suffix(".jar")?;
    let candidate = p.with_file_name(format!("{stem}-sources.jar"));
    if candidate.is_file() {
        Some(candidate.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn write_cache(target: &Path, content: Vec<u8>) -> Option<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    fs::write(target, content).ok()?;
    Some(())
}

fn hex_string(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}

fn path_to_file_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// 递归改写一条 JSON 消息:所有以 `jar://` 开头的字符串都尝试替换为本地
/// `file://` 路径(definition/hover/documents 等响应的 uri 字段)。
pub fn rewrite_jar_uris(value: &mut serde_json::Value, cache: &Cache) {
    match value {
        serde_json::Value::String(s) => {
            if s.starts_with("jar://") {
                if let Some(file_uri) = cache.rewrite(s) {
                    *s = file_uri;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rewrite_jar_uris(item, cache);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                rewrite_jar_uris(v, cache);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_jar(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("com/example/Lists.java", opts).unwrap();
        writer.write_all(b"package com.example; class Lists {}").unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn parses_windows_jar_uri() {
        let (jar, entry) = parse_jar_uri(
            "jar:///D:/libs/guava-33.0.0-jre.jar!/com/google/common/collect/Lists.java",
        )
        .unwrap();
        assert_eq!(jar, "D:/libs/guava-33.0.0-jre.jar");
        assert_eq!(entry, "com/google/common/collect/Lists.java");
    }

    #[test]
    fn parses_leading_slash_entry() {
        let (_, entry) = parse_jar_uri("jar:///x.jar!/java/lang/String.java").unwrap();
        assert_eq!(entry, "java/lang/String.java");
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_jar_uri("not-a-jar-uri").is_none());
        assert!(parse_jar_uri("jar:///only-jar.jar").is_none());
        assert!(parse_jar_uri("jar:///a.jar!/").is_none());
    }

    #[test]
    fn extracts_to_cache_and_rewrites() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-jars");
        let _ = fs::create_dir_all(&dir);
        let jar_path = dir.join("demo.jar");
        make_jar(&jar_path);

        let cache = Cache::new(&dir.to_string_lossy());
        let uri = format!("jar:///{}!/com/example/Lists.java", jar_path.to_string_lossy());
        let file_uri = cache.rewrite(&uri).expect("rewrite");
        assert!(file_uri.starts_with("file:///"));
        let path = file_uri.strip_prefix("file:///").unwrap().replace('/', "\\");
        let content = fs::read_to_string(std::path::PathBuf::from(&path)).unwrap();
        assert!(content.contains("class Lists"));

        // 二次命中内存缓存,返回同一路径。
        assert_eq!(cache.rewrite(&uri), Some(file_uri));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_traversal_blocked() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-traversal");
        let _ = fs::create_dir_all(&dir);
        let jar_path = dir.join("evil.jar");
        make_jar(&jar_path);

        let cache = Cache::new(&dir.to_string_lossy());
        let uri = format!("jar:///{}/!../../outside.java", jar_path.to_string_lossy());
        assert!(cache.rewrite(&uri).is_none());
        assert!(!dir.parent().unwrap().join("outside.java").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_sibling_sources_jar() {
        let dir = std::env::temp_dir().join("intellij-lsp-test-sources");
        let _ = fs::create_dir_all(&dir);

        // 只有 .class、没有源码的 jar。
        let class_jar = dir.join("demo-1.0.jar");
        {
            let file = fs::File::create(&class_jar).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("com/example/Lists.class", opts).unwrap();
            w.write_all(b"class bytes").unwrap();
            w.finish().unwrap();
        }
        // 同目录的 -sources.jar。
        let sources_jar = dir.join("demo-1.0-sources.jar");
        {
            let file = fs::File::create(&sources_jar).unwrap();
            let mut w = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("com/example/Lists.java", opts).unwrap();
            w.write_all(b"package com.example; class Lists {}").unwrap();
            w.finish().unwrap();
        }

        let cache = Cache::new(&dir.to_string_lossy());
        let uri = format!("jar:///{}!/com/example/Lists.java", class_jar.to_string_lossy());
        let file_uri = cache.rewrite(&uri).expect("rewrite via sources jar");
        let path = file_uri.strip_prefix("file:///").unwrap().replace('/', "\\");
        let content = fs::read_to_string(std::path::PathBuf::from(&path)).unwrap();
        assert!(content.contains("class Lists"));

        let _ = fs::remove_dir_all(&dir);
    }
}
