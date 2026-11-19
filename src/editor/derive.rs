//! 文档派生数据缓存。
//!
//! 序列化文本、源码映射与引用定义都由块内容决定，此前每次调用都全量重算，
//! 而状态栏、查找、选区与撤销会在同一帧或同一次交互里反复取用。这里按内容
//! 指纹缓存一份 `Arc` 共享的结果：指纹只做整数哈希与实体读，不分配；任何
//! 结构变更（[`super::tree::DocumentTree::revision`]）或块内容变更
//! （`BlockRecord::content_version`）都会让它失效。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rgpui::*;

use super::{Editor, SourceMapEntry};
use crate::components::{
    ImageReferenceDefinitions, LinkReferenceDefinitions, parse_image_reference_definitions,
    parse_link_reference_definitions,
};

/// 一次算好的文档派生数据。字段全部 `Arc` 共享，命中缓存时只递增引用计数。
#[derive(Clone)]
pub(super) struct DerivedDocument {
    fingerprint: u64,
    pub markdown: Arc<str>,
    pub raw_source: Arc<str>,
    pub mappings: Arc<Vec<SourceMapEntry>>,
    /// 按块实体标识索引 `mappings` 的下标，避免选区换算时线性查找整份映射。
    pub mapping_index_by_entity: Arc<HashMap<EntityId, usize>>,
    pub block_ranges: Arc<HashMap<EntityId, Range<usize>>>,
    pub image_references: Arc<ImageReferenceDefinitions>,
    pub link_references: Arc<LinkReferenceDefinitions>,
}

impl DerivedDocument {
    /// 本份派生数据对应的内容指纹。
    pub(super) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

impl Editor {
    /// 内容指纹：结构修订号折叠全部可见块的标识与内容版本号。
    ///
    /// 覆盖序列化会读到的每一个块（含表格块自身，表格单元格内容经
    /// `set_table` 计入表格块的版本号），因此不会漏掉任何内容变更。
    pub(super) fn document_fingerprint(&self, cx: &App) -> u64 {
        let epoch = crate::components::DOCUMENT_CONTENT_EPOCH.load(Ordering::Relaxed);
        let revision = self.document.revision();
        if let Some((cached_epoch, cached_revision, fingerprint)) = self.fingerprint_cache.get()
            && cached_epoch == epoch
            && cached_revision == revision
        {
            return fingerprint;
        }

        count_fingerprint_full_scan();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        revision.hash(&mut hasher);
        for visible in self.document.visible_blocks() {
            visible.entity.entity_id().hash(&mut hasher);
            visible
                .entity
                .read(cx)
                .record
                .content_version()
                .hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        self.fingerprint_cache
            .set(Some((epoch, revision, fingerprint)));
        fingerprint
    }

    /// 取当前文档的派生数据，指纹未变时直接复用缓存。
    pub(super) fn derived_document(&self, cx: &App) -> DerivedDocument {
        let fingerprint = self.document_fingerprint(cx);
        if let Some(cached) = self.derive_cache.borrow().as_ref()
            && cached.fingerprint == fingerprint
        {
            return cached.clone();
        }

        let derived = self.compute_derived_document(fingerprint, cx);
        *self.derive_cache.borrow_mut() = Some(derived.clone());
        derived
    }

    /// 全量重算派生数据。只在指纹变化时调用，内部不得再取缓存以免递归。
    fn compute_derived_document(&self, fingerprint: u64, cx: &App) -> DerivedDocument {
        let markdown: Arc<str> = Arc::from(self.document.markdown_text(cx));
        let raw_source: Arc<str> = Arc::from(self.document.raw_source_text(cx));
        let (mappings, block_ranges) = self.compute_source_target_mappings_with_block_ranges(cx);
        let mut mapping_index_by_entity = HashMap::with_capacity(mappings.len());
        for (index, mapping) in mappings.iter().enumerate() {
            mapping_index_by_entity
                .entry(mapping.entity.entity_id())
                .or_insert(index);
        }
        let image_references = Arc::new(parse_image_reference_definitions(&markdown));
        let link_references = Arc::new(parse_link_reference_definitions(&markdown));
        // 定义集合与上次派生内容一致时复用旧 Arc：块级运行时同步按指针短路，
        // 指针稳定可让万级块文档的全量同步从 O(块数 × 定义数) 降到 O(块数)。
        // 重新解析会分配新的 HashMap，指针必然不同，不复用就会让每次全量
        // 同步都退化为逐块深度比较（实测 1,505 条定义 × 万级块 ≈ 3.5 秒）。
        let cache = self.derive_cache.borrow();
        let image_references = match cache.as_ref() {
            Some(cached) if *cached.image_references == *image_references => {
                cached.image_references.clone()
            }
            _ => image_references,
        };
        let link_references = match cache.as_ref() {
            Some(cached) if *cached.link_references == *link_references => {
                cached.link_references.clone()
            }
            _ => link_references,
        };
        drop(cache);
        DerivedDocument {
            fingerprint,
            markdown,
            raw_source,
            mappings: Arc::new(mappings),
            mapping_index_by_entity: Arc::new(mapping_index_by_entity),
            block_ranges: Arc::new(block_ranges),
            image_references,
            link_references,
        }
    }
}

// 测试观测点：内容指纹的逐块全扫描次数（生产构建不编译）。
#[cfg(test)]
thread_local! {
    static FINGERPRINT_FULL_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_fingerprint_full_scan() {
    FINGERPRINT_FULL_SCANS.with(|runs| runs.set(runs.get() + 1));
}

#[cfg(not(test))]
fn count_fingerprint_full_scan() {}

/// 读取并清零逐块扫描计数（测试辅助）。
#[cfg(test)]
pub(super) fn take_fingerprint_full_scans() -> usize {
    FINGERPRINT_FULL_SCANS.with(|runs| runs.replace(0))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rgpui::{AppContext, TestAppContext};

    use super::{Editor, take_fingerprint_full_scans};
    use crate::components::{Block, BlockKind, BlockRecord, InlineTextTree};
    use crate::i18n::I18nManager;

    /// 初始化派生缓存测试应用（测试辅助）。
    fn init_editor_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
        });
    }

    #[test]
    fn record_setters_bump_content_version() {
        let mut record = BlockRecord::paragraph("alpha");
        let initial = record.content_version();
        record.set_title(InlineTextTree::plain("beta"));
        assert!(record.content_version() > initial, "set_title 应递增版本号");
        let after_title = record.content_version();
        record.set_kind(BlockKind::Heading { level: 2 });
        assert!(
            record.content_version() > after_title,
            "set_kind 应递增版本号"
        );
        let after_kind = record.content_version();
        record.set_table(None);
        assert!(
            record.content_version() > after_kind,
            "set_table 应递增版本号"
        );
    }

    #[test]
    fn derive_cache_reuses_unchanged_document() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let first = editor.derived_document(cx);
            let second = editor.derived_document(cx);
            assert_eq!(first.fingerprint, second.fingerprint);
            assert!(
                Arc::ptr_eq(&first.markdown, &second.markdown),
                "内容未变时应复用同一份序列化文本"
            );
            assert!(
                Arc::ptr_eq(&first.mappings, &second.mappings),
                "内容未变时应复用同一份源码映射"
            );
        });
        cx.quit();
    }

    #[test]
    fn derive_cache_invalidates_on_block_text_edit() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let before = editor.derived_document(cx);
            let first = editor.document.visible_blocks()[0].entity.clone();
            first.update(cx, |block, _cx| {
                block.record.set_title(InlineTextTree::plain("changed"));
            });

            let after = editor.derived_document(cx);
            assert_ne!(before.fingerprint, after.fingerprint, "改一个字指纹即变");
            assert!(
                !Arc::ptr_eq(&before.mappings, &after.mappings),
                "映射缓存应失效"
            );
            assert_eq!(after.markdown.as_ref(), "changed\n\nbeta");
            assert_eq!(editor.serialized_document_text(cx), "changed\n\nbeta");
        });
        cx.quit();
    }

    /// 空闲帧（仅光标闪烁重绘）不该逐块扫描；内容一变必须立即重算。
    #[test]
    fn idle_frames_reuse_fingerprint_without_block_scan() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let first = editor.derived_document(cx);
            take_fingerprint_full_scans();
            for _ in 0..4 {
                assert_eq!(editor.document_fingerprint(cx), first.fingerprint());
            }
            assert_eq!(take_fingerprint_full_scans(), 0, "空闲帧不应逐块扫描");

            let block = editor.document.visible_blocks()[0].entity.clone();
            block.update(cx, |block, _cx| {
                block.record.set_title(InlineTextTree::plain("changed"));
            });
            let after = editor.derived_document(cx);
            assert!(take_fingerprint_full_scans() >= 1, "内容变更应重新扫描指纹");
            assert_eq!(after.markdown.as_ref(), "changed\n\nbeta");
        });
        cx.quit();
    }

    /// 别的窗口改字只让本窗口保守重校验，不能让本窗口缓存陈旧。
    #[test]
    fn global_content_epoch_forces_fingerprint_recheck() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let before = editor.derived_document(cx);
            take_fingerprint_full_scans();
            let mut elsewhere = BlockRecord::paragraph("other window");
            elsewhere.set_title(InlineTextTree::plain("changed elsewhere"));

            let after = editor.derived_document(cx);
            assert!(
                take_fingerprint_full_scans() >= 1,
                "全局纪元变化应触发重校验"
            );
            assert_eq!(after.fingerprint(), before.fingerprint());
            assert_eq!(after.markdown.as_ref(), "alpha");
        });
        cx.quit();
    }

    #[test]
    fn derive_cache_invalidates_on_kind_and_structure_change() {
        let mut cx = TestAppContext::single();
        init_editor_test_app(&mut cx);
        let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

        editor.update(&mut cx, |editor, cx| {
            let before = editor.derived_document(cx);
            let first = editor.document.visible_blocks()[0].entity.clone();
            first.update(cx, |block, _cx| {
                block.record.set_kind(BlockKind::Heading { level: 1 });
            });
            let after_kind = editor.derived_document(cx);
            assert_ne!(before.fingerprint, after_kind.fingerprint);
            assert_eq!(after_kind.markdown.as_ref(), "# alpha\n\nbeta");

            let extra = cx.new(|cx| Block::with_record(cx, BlockRecord::paragraph("gamma")));
            editor.document.insert_blocks_at(None, 2, vec![extra], cx);
            let after_structure = editor.derived_document(cx);
            assert_ne!(
                after_kind.fingerprint, after_structure.fingerprint,
                "结构变更应使缓存失效"
            );
            assert_eq!(
                after_structure.markdown.as_ref(),
                "# alpha\n\nbeta\n\ngamma"
            );
        });
        cx.quit();
    }
}
