//! 编辑器专用的有界图片缓存。
//!
//! 框架默认的 `fetch_asset` 把 `Asset::load` 的 `Shared<Task<...>>` 永久留在
//! `loading_assets` 里，且 `Err` 与 `Ok` 一视同仁：一次瞬时网络失败会让该图片
//! 整个会话都停在占位图，浏览大量图片时内存也没有上限。
//!
//! 本模块通过 `Div::image_cache` 为编辑器元素树提供一个自定义 `ImageCache`：
//! 成功结果按 LRU 限量保留并主动 `drop_image`；失败结果只保留一小段时间，
//! 到期后丢弃条目并请求重绘，让断网恢复后的图片能自动重新加载。
//! 这样无需把 `Cargo.toml` 切回 rgpui 的 path 依赖即可修掉本仓的可观测症状。

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use rgpui::{
    App, Asset, ImageCache, ImageCacheError, ImageCacheItem, ImgResourceLoader, RenderImage,
    Resource, Window,
};

/// 缓存条目上限（成功与失败合计，按资源数计）。
pub(crate) const MAX_CACHED_IMAGES: usize = 256;
/// 失败结果的最短保留时间：期间直接返回缓存错误，到期后重新发起加载。
pub(crate) const FAILURE_RETRY: Duration = Duration::from_secs(5);

type LoadResult = Result<Arc<RenderImage>, ImageCacheError>;

/// 按资源来源索引的缓存项。
struct CacheEntry {
    item: ImageCacheItem,
    /// 首次观察到加载失败的时间；`None` 表示尚未失败或正在加载。
    failed_at: Option<Instant>,
    /// 是否已安排失败到期后的重绘，避免每帧重复排队。
    retry_scheduled: bool,
}

/// 有界 LRU 图片缓存，由每个编辑器窗口各持有一份。
pub(crate) struct BoundedImageCache {
    entries: HashMap<u64, CacheEntry>,
    /// 由冷到热排序的资源键；`touch` 时把命中项移到末尾。
    lru: VecDeque<u64>,
    max_entries: usize,
}

impl BoundedImageCache {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_CACHED_IMAGES)
    }

    fn with_limit(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            max_entries: max_entries.max(1),
        }
    }

    /// 标记资源为最近使用，返回因超出上限而被淘汰的资源键。
    fn touch(&mut self, key: u64) -> Vec<u64> {
        self.lru.retain(|existing| *existing != key);
        self.lru.push_back(key);
        let mut evicted = Vec::new();
        while self.lru.len() > self.max_entries {
            match self.lru.pop_front() {
                Some(oldest) => evicted.push(oldest),
                None => break,
            }
        }
        evicted
    }

    /// 释放被淘汰条目里已成功解码的图像（GPU 侧资源需要显式归还）。
    fn drop_evicted(&mut self, evicted: Vec<u64>, window: &mut Window, cx: &mut App) {
        for key in evicted {
            let Some(mut entry) = self.entries.remove(&key) else {
                continue;
            };
            if let Some(Ok(image)) = entry.item.get() {
                cx.drop_image(image, Some(window));
            }
        }
    }

    /// 安排一次延迟重绘：加载完成或失败重试到期后，让元素树重新读取缓存。
    fn schedule_repaint(
        window: &Window,
        cx: &App,
        delay: Option<Duration>,
        done: Option<LoadTask>,
    ) {
        let entity = window.current_view();
        window
            .spawn(cx, async move |cx| {
                if let Some(done) = done {
                    let _ = done.await;
                } else if let Some(delay) = delay {
                    cx.background_executor().timer(delay).await;
                }
                cx.on_next_frame(move |_, cx| {
                    cx.notify(entity);
                });
            })
            .detach();
    }
}

/// `ImageCacheItem::Loading` 里保存的共享加载任务。
type LoadTask = rgpui::ImageLoadingTask;

/// 失败结果是否已过重试窗口。
fn failure_expired(failed_at: Option<Instant>, now: Instant, retry: Duration) -> bool {
    matches!(failed_at, Some(at) if now.duration_since(at) >= retry)
}

/// 资源键：`Resource` 自带 `Hash`，用默认哈希器即可稳定索引。
fn resource_key(resource: &Resource) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    resource.hash(&mut hasher);
    hasher.finish()
}

impl ImageCache for BoundedImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<LoadResult> {
        let key = resource_key(resource);
        let now = Instant::now();

        // 失败缓存到期：丢弃条目，本帧按未命中重新发起加载。
        if self
            .entries
            .get(&key)
            .is_some_and(|entry| failure_expired(entry.failed_at, now, FAILURE_RETRY))
        {
            self.entries.remove(&key);
            self.lru.retain(|existing| *existing != key);
        }

        if let Some(entry) = self.entries.get_mut(&key) {
            let result = entry.item.get();
            if matches!(&result, Some(Err(_))) {
                if entry.failed_at.is_none() {
                    entry.failed_at = Some(now);
                }
                if !entry.retry_scheduled {
                    entry.retry_scheduled = true;
                    Self::schedule_repaint(window, cx, Some(FAILURE_RETRY), None);
                }
            }
            let evicted = self.touch(key);
            self.drop_evicted(evicted, window, cx);
            return result;
        }

        // 未命中：自行 spawn 加载任务，不经过框架的 `fetch_asset` 全局缓存。
        let future = <ImgResourceLoader as Asset>::load(resource.clone(), cx);
        let task = cx.background_executor().spawn(future).shared();
        Self::schedule_repaint(window, cx, None, Some(task.clone()));

        self.entries.insert(
            key,
            CacheEntry {
                item: ImageCacheItem::Loading(task),
                failed_at: None,
                retry_scheduled: false,
            },
        );
        let evicted = self.touch(key);
        self.drop_evicted(evicted, window, cx);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundedImageCache, FAILURE_RETRY, MAX_CACHED_IMAGES, failure_expired, resource_key,
    };
    use rgpui::Resource;
    use std::time::{Duration, Instant};

    #[test]
    fn lru_evicts_least_recently_used_first() {
        let mut cache = BoundedImageCache::with_limit(2);
        assert!(cache.touch(1).is_empty());
        assert!(cache.touch(2).is_empty());
        assert!(cache.touch(1).is_empty(), "命中 1 应把它移到最热");
        assert_eq!(cache.touch(3), vec![2], "应淘汰最冷的 2");
        assert_eq!(cache.lru.iter().copied().collect::<Vec<_>>(), vec![1, 3]);
    }

    #[test]
    fn entry_count_is_bounded_for_many_distinct_sources() {
        let mut cache = BoundedImageCache::new();
        for key in 0..MAX_CACHED_IMAGES * 3 {
            let _ = cache.touch(key as u64);
        }
        assert_eq!(cache.lru.len(), MAX_CACHED_IMAGES);
    }

    #[test]
    fn failed_load_is_only_retryable_after_ttl() {
        let now = Instant::now();
        assert!(!failure_expired(None, now, FAILURE_RETRY));
        assert!(!failure_expired(
            Some(now - Duration::from_secs(1)),
            now,
            FAILURE_RETRY
        ));
        assert!(failure_expired(
            Some(now - FAILURE_RETRY - Duration::from_millis(1)),
            now,
            FAILURE_RETRY
        ));
    }

    #[test]
    fn resource_key_is_stable_and_distinguishes_sources() {
        let a = Resource::Uri("https://example.com/a.png".to_string().into());
        let b = Resource::Uri("https://example.com/b.png".to_string().into());
        assert_eq!(resource_key(&a), resource_key(&a.clone()));
        assert_ne!(resource_key(&a), resource_key(&b));
    }
}
