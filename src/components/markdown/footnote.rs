//! 行内脚注引用与已解析脚注注册表状态。

use std::collections::HashMap;

use rgpui::EntityId;
use uuid::Uuid;

/// 从 `[^id]` 语法解析出的行内脚注引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineFootnoteReference {
    /// 不含 `[^` 与 `]` 标记的脚注标识。
    pub id: String,
    /// 引用定义存在时解析出的文档序号。
    pub ordinal: Option<usize>,
    /// 块内从零计数的出现次数。
    pub occurrence_index: usize,
}

/// 渲染后脚注引用的命中测试载荷。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineFootnoteHit {
    pub id: String,
    pub ordinal: usize,
    pub occurrence_index: usize,
}

impl InlineFootnoteReference {
    /// 取原始标记。
    pub(crate) fn raw_markdown(&self) -> String {
        format!("[^{}]", self.id)
    }

    /// 取命中测试载荷。
    pub(crate) fn hit(&self) -> Option<InlineFootnoteHit> {
        Some(InlineFootnoteHit {
            id: self.id.clone(),
            ordinal: self.ordinal?,
            occurrence_index: self.occurrence_index,
        })
    }
}

/// 单个脚注标识的首个已解析行内引用位置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FootnoteReferenceLocation {
    pub(crate) entity_id: EntityId,
    pub(crate) occurrence_index: usize,
}

/// 单个脚注标识的定义块与首引用元数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FootnoteDefinitionBinding {
    pub(crate) ordinal: Option<usize>,
    pub(crate) definition_entity_id: EntityId,
    pub(crate) first_reference: Option<FootnoteReferenceLocation>,
}

/// 关联引用与定义块的文档级注册表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FootnoteRegistry {
    pub(crate) bindings: HashMap<String, FootnoteDefinitionBinding>,
    pub(crate) block_occurrences: HashMap<Uuid, Vec<FootnoteResolvedOccurrence>>,
}

impl FootnoteRegistry {
    /// 取脚注绑定。
    pub(crate) fn binding(&self, id: &str) -> Option<&FootnoteDefinitionBinding> {
        self.bindings.get(id)
    }

    /// 取脚注序号。
    pub(crate) fn ordinal(&self, id: &str) -> Option<usize> {
        self.binding(id).and_then(|binding| binding.ordinal)
    }

    /// 取指定块的出现列表。
    pub(crate) fn occurrences_for_block(
        &self,
        block_id: Uuid,
    ) -> Option<&[FootnoteResolvedOccurrence]> {
        self.block_occurrences.get(&block_id).map(Vec::as_slice)
    }
}

/// 按块存储的已解析出现记录，供行内渲染使用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FootnoteResolvedOccurrence {
    pub(crate) id: String,
    pub(crate) ordinal: Option<usize>,
    pub(crate) occurrence_index: usize,
}

/// 校验脚注标识是否合法。
pub(crate) fn is_valid_footnote_id(id: &str) -> bool {
    !id.is_empty()
        && !id
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r' | '^' | '[' | ']'))
}

/// 解析脚注定义头行。
pub(crate) fn parse_footnote_definition_head(line: &str) -> Option<(String, String)> {
    let trimmed_end = line.trim_end();
    let leading_spaces = trimmed_end.bytes().take_while(|b| *b == b' ').count();
    if leading_spaces > 3 {
        return None;
    }

    let rest = &trimmed_end[leading_spaces..];
    let after_open = rest.strip_prefix("[^")?;
    let label_end = after_open.find("]:")?;
    let id = after_open[..label_end].to_string();
    if !is_valid_footnote_id(&id) {
        return None;
    }

    let remainder = after_open[label_end + 2..]
        .strip_prefix(' ')
        .unwrap_or(&after_open[label_end + 2..])
        .to_string();
    Some((id, remainder))
}

/// 解析行内脚注引用。
pub(crate) fn parse_inline_footnote_reference(markdown: &str) -> Option<String> {
    let rest = markdown.strip_prefix("[^")?;
    let id = rest.strip_suffix(']')?;
    is_valid_footnote_id(id).then(|| id.to_string())
}

/// 把序号转为上标数字串。
pub(crate) fn superscript_ordinal(ordinal: usize) -> String {
    const SUPERSCRIPT_DIGITS: [char; 10] = [
        '\u{2070}', '\u{00B9}', '\u{00B2}', '\u{00B3}', '\u{2074}', '\u{2075}', '\u{2076}',
        '\u{2077}', '\u{2078}', '\u{2079}',
    ];

    ordinal
        .to_string()
        .chars()
        .filter_map(|ch| {
            ch.to_digit(10)
                .map(|digit| SUPERSCRIPT_DIGITS[digit as usize])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        is_valid_footnote_id, parse_footnote_definition_head, parse_inline_footnote_reference,
        superscript_ordinal,
    };

    #[test]
    fn validates_and_parses_reference_footnote_syntax() {
        assert!(is_valid_footnote_id("long-note"));
        assert!(!is_valid_footnote_id("bad id"));
        assert_eq!(
            parse_inline_footnote_reference("[^ref-1]"),
            Some("ref-1".to_string())
        );
        assert_eq!(
            parse_footnote_definition_head("[^ref-1]: body"),
            Some(("ref-1".to_string(), "body".to_string()))
        );
    }

    #[test]
    fn formats_superscript_ordinals() {
        assert_eq!(superscript_ordinal(1), "\u{00B9}");
        assert_eq!(superscript_ordinal(12), "\u{00B9}\u{00B2}");
    }
}
