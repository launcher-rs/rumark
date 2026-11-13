//! 公式与 Mermaid 共用的 SVG 缓存。
//!
//! 绘制路径每帧都要取一份 SVG：这里先查进程内文本缓存，再查磁盘文件，
//! 两者都 miss 才真正渲染。缓存目录按进程只创建一次，写入新文件前按修改
//! 时间淘汰最旧文件，避免滚动、光标闪烁或连续 resize 让缓存无限增长。

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;

use anyhow::{Context as _, Result};
use directories::ProjectDirs;

/// 内存缓存最多保留的 SVG 份数。
const MEMORY_LIMIT: usize = 256;
/// 单个磁盘缓存目录最多保留的 SVG 文件数。
const DISK_LIMIT: usize = 512;

/// 按路径索引的 SVG 文本缓存，附带简单的先进先出淘汰顺序。
#[derive(Default)]
struct MemoryCache {
    entries: HashMap<PathBuf, Arc<str>>,
    order: VecDeque<PathBuf>,
}

impl MemoryCache {
    /// 取缓存文本。
    fn get(&self, path: &Path) -> Option<Arc<str>> {
        self.entries.get(path).cloned()
    }

    /// 写入缓存文本，超出上限时按插入顺序淘汰。
    fn insert(&mut self, path: &Path, svg: &str) {
        if self
            .entries
            .insert(path.to_path_buf(), Arc::from(svg))
            .is_none()
        {
            self.order.push_back(path.to_path_buf());
        }
        while self.entries.len() > MEMORY_LIMIT {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    /// 移除指定路径的缓存（磁盘淘汰时同步调用）。
    fn remove(&mut self, path: &Path) {
        if self.entries.remove(path).is_some() {
            self.order.retain(|item| item != path);
        }
    }
}

/// 取进程唯一的内存缓存（锁被污染时继续用内部数据，不让绘图路径崩溃）。
fn memory_cache() -> MutexGuard<'static, MemoryCache> {
    static CACHE: OnceLock<Mutex<MemoryCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(MemoryCache::default()))
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

/// 取（或首次创建）一个 SVG 缓存目录，避免每帧都做 `create_dir_all`。
pub(crate) fn svg_cache_dir(name: &str) -> Result<PathBuf> {
    static DIRS: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut dirs = DIRS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if let Some(dir) = dirs.get(name) {
        return Ok(dir.clone());
    }

    let root = ProjectDirs::from("rs", "launcher", "Rumark")
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("Rumark"));
    let dir = root.join(name);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create SVG cache '{}'", dir.display()))?;
    dirs.insert(name.to_string(), dir.clone());
    Ok(dir)
}

/// 取缓存 SVG：内存命中免读盘，磁盘命中免重渲染，全 miss 才调用 `render`。
///
/// 内存命中但磁盘文件被外部清理时，用内存副本补写文件后再返回，保证调用方
/// 拿到的路径始终可用，同时不触发第二次渲染。
pub(crate) fn load_svg(path: &Path, render: impl FnOnce() -> Result<String>) -> Result<String> {
    if let Some(cached) = memory_cache().get(path) {
        if !path.exists() {
            crate::atomic_file::write_text(path, cached.as_ref())
                .with_context(|| format!("failed to restore SVG cache '{}'", path.display()))?;
        }
        return Ok(cached.to_string());
    }

    if path.exists() {
        let svg = fs::read_to_string(path)
            .with_context(|| format!("failed to read SVG cache '{}'", path.display()))?;
        memory_cache().insert(path, &svg);
        return Ok(svg);
    }

    prune_cache_dir(path.parent());
    let svg = render()?;
    crate::atomic_file::write_text(path, &svg)
        .with_context(|| format!("failed to write SVG cache '{}'", path.display()))?;
    memory_cache().insert(path, &svg);
    Ok(svg)
}

/// 写入新文件前按修改时间淘汰最旧的一批 SVG，同时清掉对应内存项。
fn prune_cache_dir(dir: Option<&Path>) {
    let Some(dir) = dir else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("svg"))
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, entry.path())
        })
        .collect();
    if files.len() < DISK_LIMIT {
        return;
    }

    files.sort_by_key(|(modified, _)| *modified);
    let remove_count = files.len().saturating_sub(DISK_LIMIT - 1);
    let mut cache = memory_cache();
    for (_, path) in files.iter().take(remove_count) {
        let _ = fs::remove_file(path);
        cache.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 建一个测试专用的临时目录（测试辅助）。
    fn temp_cache_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rumark-svg-cache-{label}-{}.svg",
            std::process::id()
        ))
    }

    #[test]
    fn second_load_reuses_rendered_svg_without_calling_renderer() {
        static RENDERS: AtomicUsize = AtomicUsize::new(0);
        let path = temp_cache_path("reuse");
        let _ = fs::remove_file(&path);
        memory_cache().remove(&path);

        let first = load_svg(&path, || {
            RENDERS.fetch_add(1, Ordering::Relaxed);
            Ok("<svg>first</svg>".to_string())
        })
        .expect("first load");
        let second = load_svg(&path, || {
            RENDERS.fetch_add(1, Ordering::Relaxed);
            Ok("<svg>second</svg>".to_string())
        })
        .expect("cached load");

        assert_eq!(first, "<svg>first</svg>");
        assert_eq!(second, first, "命中缓存不应重新渲染");
        assert_eq!(RENDERS.load(Ordering::Relaxed), 1);
        let _ = fs::remove_file(&path);
        memory_cache().remove(&path);
    }

    #[test]
    fn memory_hit_restores_deleted_disk_file() {
        let path = temp_cache_path("restore");
        let _ = fs::remove_file(&path);
        memory_cache().remove(&path);

        load_svg(&path, || Ok("<svg>restore</svg>".to_string())).expect("first load");
        fs::remove_file(&path).expect("remove cached file");
        let restored = load_svg(&path, || panic!("磁盘文件缺失时应复用内存副本")).expect("restore");

        assert_eq!(restored, "<svg>restore</svg>");
        assert!(path.exists(), "缓存路径应被补写回来");
        let _ = fs::remove_file(&path);
        memory_cache().remove(&path);
    }
}
