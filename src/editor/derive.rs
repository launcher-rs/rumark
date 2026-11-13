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
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.document.revision().hash(&mut hasher);
        for visible in self.document.visible_blocks() {
            visible.entity.entity_id().hash(&mut hasher);
            visible
                .entity
                .read(cx)
                .record
                .content_version()
                .hash(&mut hasher);
        }
        hasher.finish()
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
        let image_references = Arc::new(parse_image_reference_definitions(&markdown));
        let link_references = Arc::new(parse_link_reference_definitions(&markdown));
        DerivedDocument {
            fingerprint,
            markdown,
            raw_source,
            mappings: Arc::new(mappings),
            block_ranges: Arc::new(block_ranges),
            image_references,
            link_references,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rgpui::{AppContext, TestAppContext};

    use super::Editor;
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
