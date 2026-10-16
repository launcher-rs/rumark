//! 原生输入处理器共用的 Unicode 坐标转换。
//!
//! 内部选区用 UTF-8 字节偏移，框架的 IME 接口用 UTF-16 码元。这里的辅助函数
//! 只按标量（code point）裁剪，不按字形（grapheme）裁剪：输入法必须能替换一个
//! 组合音标符号而不删掉它的基础字符。

use std::ops::Range;

/// 把字节偏移向前夹到 UTF-8 标量边界（含文末）。
pub(crate) fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// 把字节偏移向后夹到 UTF-8 标量边界（含文末）。
fn ceil_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// 排定并夹取替换范围，且不切开 UTF-8 标量。
///
/// 非空范围向两侧扩张到完整标量；重合的光标保持在边界的同一侧。选区方向由
/// 调用方另行保存，这里不还原。
pub(crate) fn normalize_utf8_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = floor_char_boundary(text, range.start.min(range.end));
    if range.start == range.end {
        start..start
    } else {
        start..ceil_char_boundary(text, range.start.max(range.end))
    }
}

/// UTF-16 码元偏移转字节偏移，落在代理对内部时向前取整。
pub(crate) fn utf16_to_utf8(text: &str, offset: usize) -> usize {
    utf16_boundary(text, offset, false)
}

/// 按指定取整方向做 UTF-16 码元偏移与字节偏移的互转。
fn utf16_boundary(text: &str, offset: usize, round_up: bool) -> usize {
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        let end = units + ch.len_utf16();
        if offset < end {
            return if round_up && offset > units {
                byte + ch.len_utf8()
            } else {
                byte
            };
        }
        units = end;
    }
    text.len()
}

/// 字节偏移转 UTF-16 码元，先夹到标量边界。
pub(crate) fn utf8_to_utf16(text: &str, offset: usize) -> usize {
    text[..floor_char_boundary(text, offset)]
        .encode_utf16()
        .count()
}

/// 转换 IME 上报的范围：半个代理对扩张成完整标量，光标不扩张。
pub(crate) fn utf16_range_to_utf8(text: &str, range: &Range<usize>) -> Range<usize> {
    let start = utf16_to_utf8(text, range.start.min(range.end));
    if range.start == range.end {
        start..start
    } else {
        start..utf16_boundary(text, range.start.max(range.end), true)
    }
}

/// 内部选区转成排好序的 UTF-16 范围交回框架。
pub(crate) fn utf8_range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    let range = normalize_utf8_range(text, range.clone());
    utf8_to_utf16(text, range.start)..utf8_to_utf16(text, range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_ranges_always_land_on_scalar_boundaries() {
        // ZWJ 家庭 emoji：一个字形内含多个标量，切在中间必须仍然安全。
        const FAMILY: &str = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        for text in ["*测试", "中文😀かな", "e\u{301}", FAMILY, "བོད་"] {
            for start in 0..=text.len() + 1 {
                for end in 0..=text.len() + 1 {
                    let range = normalize_utf8_range(text, start..end);
                    assert!(text.get(range.clone()).is_some(), "{text:?}: {range:?}");
                    if start == end {
                        assert!(range.is_empty());
                    }
                }
            }
            for (byte, _) in text
                .char_indices()
                .chain(std::iter::once((text.len(), '\0')))
            {
                assert_eq!(utf16_to_utf8(text, utf8_to_utf16(text, byte)), byte);
            }
        }
    }

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn unicode_partial_surrogate_ranges_expand_but_carets_do_not() {
        assert_eq!(utf16_range_to_utf8("a😀b", &(2..2)), 1..1);
        assert_eq!(utf16_range_to_utf8("a😀b", &(1..2)), 1..5);
        assert_eq!(utf16_range_to_utf8("a😀b", &(2..3)), 1..5);
        // Windows 的 IME 会直接上报倒序范围。
        assert_eq!(utf16_range_to_utf8("a😀b", &(3..1)), 1..5);
        assert_eq!(utf16_range_to_utf8("a😀b", &(99..99)), 6..6);
    }

    #[test]
    fn unicode_combining_mark_replacement_does_not_expand_to_grapheme() {
        let text = "e\u{301}";
        assert_eq!(utf16_range_to_utf8(text, &(1..2)), 1..3);
        assert_eq!(normalize_utf8_range(text, 1..3), 1..3);
    }
}
