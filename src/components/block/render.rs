//! 经 GPUI 高层 [`Render`] 特征渲染 [`Block`]。
//!
//! 每种块类型产生不同视觉样式：列表项渲染标记列（项目符号/序号），纯 Markdown 回退渲染为纯文本。

use rgpui::*;

const EDITOR_KEY_CONTEXT: &str = "BlockEditor";

use super::element::{CodeLanguageBarElement, TextBlockElement};
use super::table_axis_menu::{open_table_axis_menu, render_table_axis_menu};
use super::{Block, BlockEvent, BlockKind, ImageRuntime};
use crate::components::{
    AxisMarker, ColumnLayout, Editor, HtmlCssColor, HtmlDocument, HtmlNode, HtmlNodeKind,
    InlineScript, TableAxisHighlight, TableAxisKind, TableCellInlineImageSegment, attr_value,
    display_math_font_size, img_for_resolved_source, inline_math_font_size,
    parse_display_math_source, parse_html_image_block, parse_mermaid_fence_source,
    parse_table_cell_inline_images, render_display_math_svg, render_inline_math_svg,
    render_mermaid_svg_for_display, resolve_image_source, style_for_node,
};
use crate::i18n::{I18nManager, I18nSnapshot};
use crate::theme::{ThemeDimensions, ThemeTypography};

const TASK_CHECK: &str = "\u{2713}";

/// 按嵌套层级取项目符号几何图形（不依赖字体字形）。
///
/// 0 → 实心圆，1 → 空心圆，更深 → 实心方。
fn bulleted_list_marker(depth: usize, theme: &Theme, text_size: f32) -> Div {
    let diameter = (text_size * 0.32).clamp(4.0, 8.0);
    let marker = div().size(px(diameter));
    match depth {
        0 => marker.rounded(px(diameter / 2.0)).bg(theme.foreground),
        1 => marker
            .rounded(px(diameter / 2.0))
            .border(px(1.0))
            .border_color(theme.foreground),
        _ => marker.bg(theme.foreground),
    }
}

/// 判断列轴槽是否可见。
fn column_axis_gutter_visible(
    preview_marker: Option<AxisMarker>,
    selected_marker: Option<AxisMarker>,
) -> bool {
    matches!(
        preview_marker,
        Some(AxisMarker {
            kind: TableAxisKind::Column,
            ..
        })
    ) || matches!(
        selected_marker,
        Some(AxisMarker {
            kind: TableAxisKind::Column,
            ..
        })
    )
}

/// 让行轴高亮色更不透明（更实但仍半透明），供表头行使用，保持主题色相，使表头手柄呈现为正文行手柄的加强版。
fn header_axis_emphasis(color: Hsla) -> Hsla {
    Hsla {
        a: color.a + (1.0 - color.a) * 0.5,
        ..color
    }
}

/// 取图片回退标签。
fn fallback_image_label(alt: &str, i18n: &I18nSnapshot) -> SharedString {
    if alt.trim().is_empty() {
        SharedString::from(i18n.t("image_placeholder", &[]))
    } else {
        SharedString::from(alt.to_string())
    }
}

/// 取图片加载标签（无描述时用通用文案，有描述时填入模板）。
fn loading_image_label(alt: &str, i18n: &I18nSnapshot) -> SharedString {
    if alt.trim().is_empty() {
        SharedString::from(i18n.t("image_loading", &[]))
    } else {
        SharedString::from(i18n.t("image_loading_alt", &[("alt", alt)]))
    }
}

/// 渲染图片占位符。
fn render_image_placeholder(
    width: Length,
    height: Pixels,
    theme: &Theme,
    label: SharedString,
) -> AnyElement {
    let d = &ThemeDimensions::DEFAULT;
    let t = &ThemeTypography::DEFAULT;
    div()
        .w(width)
        .h(height)
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(d.image_radius))
        .border(px(1.0))
        .border_color(theme.border)
        .bg(theme.muted)
        .px(px(d.block_padding_x))
        .text_center()
        .text_size(px(t.text_size))
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
}

/// 渲染加载占位符。
fn render_loading_placeholder(
    width: Length,
    height: Pixels,
    theme: &Theme,
    label: SharedString,
) -> AnyElement {
    let d = &ThemeDimensions::DEFAULT;
    let t = &ThemeTypography::DEFAULT;
    div()
        .w(width)
        .h(height)
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(d.image_radius))
        .border(px(1.0))
        .border_color(theme.border)
        .bg(theme.muted)
        .px(px(d.block_padding_x))
        .text_center()
        .text_size(px(t.code_size))
        .text_color(theme.muted_foreground)
        .child(label)
        .into_any_element()
}

/// 包裹引用引导线。
fn wrap_with_quote_guides(content: AnyElement, quote_depth: usize, theme: &Theme) -> AnyElement {
    if quote_depth == 0 {
        return content;
    }

    let d = &ThemeDimensions::DEFAULT;
    let guide_offset = d.quote_padding_left;
    let total_padding = guide_offset * quote_depth as f32;

    div()
        .w_full()
        .relative()
        .pl(px(total_padding))
        .child(content)
        .children((0..quote_depth).map(|level| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(guide_offset * level as f32))
                .w(px(d.quote_border_width))
                .bg(theme.border)
        }))
        .into_any_element()
}

/// 取可见引用层级。
fn visible_quote_guides(block: &Block) -> usize {
    block.visible_quote_depth
}

/// 取有效表格宽度。
fn effective_table_width(block: &Block, viewport_width: f32, d: &ThemeDimensions) -> f32 {
    let centered_width = Editor::centered_column_width(viewport_width, d);
    let visible_quote_guides = visible_quote_guides(block);
    let quote_inset = d.quote_padding_left * visible_quote_guides as f32;
    let callout_inset = if block.callout_depth > 0 {
        d.callout_padding_x * 2.0 + d.callout_border_width
    } else {
        0.0
    };

    (centered_width - quote_inset - callout_inset)
        .max((d.table_cell_padding_x * 2.0 + 80.0).max(120.0))
}

/// 取容器图片宽度预算。
fn container_image_width_budget(block: &Block, viewport_width: f32, d: &ThemeDimensions) -> f32 {
    let centered_width = Editor::centered_column_width(viewport_width, d);
    let visible_quote_guides = visible_quote_guides(block);
    let quote_inset = d.quote_padding_left * visible_quote_guides as f32;
    let callout_inset = if block.callout_depth > 0 {
        d.callout_padding_x * 2.0 + d.callout_border_width
    } else {
        0.0
    };

    centered_width - quote_inset - callout_inset
}

/// 取有效图片宽度。
fn effective_image_width(block: &Block, viewport_width: f32, d: &ThemeDimensions) -> f32 {
    let list_inset = d.nested_block_indent * block.render_depth as f32;
    (container_image_width_budget(block, viewport_width, d) - d.block_padding_x * 2.0 - list_inset)
        .max(160.0)
}

/// 取列表项有效图片宽度。
fn effective_list_item_image_width(block: &Block, viewport_width: f32, d: &ThemeDimensions) -> f32 {
    let marker_width = match block.kind() {
        BlockKind::BulletedListItem => d.list_marker_width,
        BlockKind::TaskListItem { .. } => d.list_marker_width.max(d.task_checkbox_size),
        BlockKind::NumberedListItem => d.ordered_list_marker_width,
        _ => 0.0,
    };
    let list_inset = d.nested_block_indent * block.render_depth as f32;

    (container_image_width_budget(block, viewport_width, d)
        - d.block_padding_x * 2.0
        - list_inset
        - marker_width
        - d.list_marker_gap)
        .max(160.0)
}

/// 取人类可读的列表序号：零层为数字，一层为小写字母，二层以上为罗马数字。
fn numbered_list_marker(depth: usize, ordinal: usize) -> String {
    match depth {
        0 => format!("{ordinal}."),
        1 => format!("{}.", alphabetic_list_marker(ordinal)),
        _ => format!("{}.", roman_list_marker(ordinal)),
    }
}

/// 超过 26 按回绕展开：a…z、a1…z1、a2…z2……
fn alphabetic_list_marker(ordinal: usize) -> String {
    const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";

    let ordinal = ordinal.max(1);
    if ordinal <= ALPHABET.len() {
        return char::from(ALPHABET[ordinal - 1]).to_string();
    }

    let wrapped = ordinal - (ALPHABET.len() + 1);
    let letter = char::from(ALPHABET[wrapped % ALPHABET.len()]);
    let suffix = wrapped + 1;
    format!("{letter}{suffix}")
}

/// 把 ASCII 罗马数字转为可用的 Unicode 连字（如把 "III" 转为单个罗马数字形）。
fn roman_list_marker(ordinal: usize) -> String {
    let ascii = ascii_roman_numeral(ordinal.max(1));
    let mut index = 0;
    let mut marker = String::new();

    while index < ascii.len() {
        let remaining = &ascii[index..];
        if let Some((token_len, token)) = roman_unicode_token(remaining) {
            marker.push_str(token);
            index += token_len;
        } else {
            break;
        }
    }

    marker
}

/// 取 ASCII 罗马数字。
fn ascii_roman_numeral(mut ordinal: usize) -> String {
    const MAP: &[(usize, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];

    let mut result = String::new();
    for (value, symbol) in MAP {
        while ordinal >= *value {
            result.push_str(symbol);
            ordinal -= *value;
        }
    }
    result
}

/// 取罗马数字 Unicode 词元。
fn roman_unicode_token(remaining: &str) -> Option<(usize, &'static str)> {
    const TOKENS: &[(&str, &str)] = &[
        ("XII", "\u{216B}"),
        ("XI", "\u{216A}"),
        ("IX", "\u{2168}"),
        ("VIII", "\u{2167}"),
        ("VII", "\u{2166}"),
        ("VI", "\u{2165}"),
        ("IV", "\u{2163}"),
        ("III", "\u{2162}"),
        ("II", "\u{2161}"),
        ("I", "\u{2160}"),
        ("V", "\u{2164}"),
        ("X", "\u{2169}"),
        ("L", "\u{216C}"),
        ("C", "\u{216D}"),
        ("D", "\u{216E}"),
        ("M", "\u{216F}"),
    ];

    TOKENS.iter().find_map(|(ascii, unicode)| {
        remaining
            .starts_with(ascii)
            .then_some((ascii.len(), *unicode))
    })
}

/// 取 HTML 子节点文本。
fn html_children_text(node: &HtmlNode) -> String {
    if node.children.is_empty() {
        return node.raw_source.clone();
    }

    let mut text = String::new();
    for child in &node.children {
        if child.tag_name == "br" {
            text.push('\n');
        } else {
            text.push_str(&html_children_text(child));
        }
    }
    text
}

#[derive(Clone, Copy, Debug)]
struct HtmlStyle {
    color: Hsla,
    font_size: f32,
    root_font_size: f32,
}

#[derive(Clone, Copy, Debug)]
struct HtmlVisualStyle {
    computed: HtmlStyle,
    background: Option<Hsla>,
}

impl HtmlStyle {
    /// 取根样式。
    fn root(theme: &Theme) -> Self {
        Self {
            color: theme.foreground,
            font_size: ThemeTypography::DEFAULT.text_size,
            root_font_size: ThemeTypography::DEFAULT.text_size,
        }
    }
}

/// 把 HTML CSS 颜色转为显示色。
fn html_css_color_to_hsla(color: HtmlCssColor, current_color: Hsla) -> Hsla {
    match color {
        HtmlCssColor::CurrentColor => current_color,
        HtmlCssColor::Rgba(color) => Hsla::from(Rgba {
            r: color.red as f32 / 255.0,
            g: color.green as f32 / 255.0,
            b: color.blue as f32 / 255.0,
            a: color.alpha.clamp(0.0, 1.0),
        }),
    }
}

/// 计算 HTML 节点视觉样式。
fn html_node_visual_style(node: &HtmlNode, parent: HtmlStyle, theme: &Theme) -> HtmlVisualStyle {
    let t = &ThemeTypography::DEFAULT;
    let mut computed = parent;
    let mut background = None;

    match node.tag_name.as_str() {
        "a" => computed.color = theme.link,
        "blockquote" => computed.color = theme.muted_foreground,
        "code" | "kbd" | "pre" => {
            computed.color = theme.foreground;
            computed.font_size = t.code_size;
            background = Some(theme.muted);
        }
        "mark" => background = Some(theme.muted),
        "figcaption" => {
            computed.color = theme.muted_foreground;
            computed.font_size = t.code_size;
        }
        "small" | "sup" | "sub" => computed.font_size = (computed.font_size * 0.8).max(6.0),
        "th" => background = Some(theme.table_head),
        "td" => background = Some(theme.table_even),
        _ => {}
    }

    let inline_style = style_for_node(node);
    if let Some(color) = inline_style.color {
        computed.color = html_css_color_to_hsla(color, computed.color);
    }
    if let Some(font_size) = inline_style.font_size {
        computed.font_size = font_size.resolve(computed.font_size, computed.root_font_size);
    }
    if let Some(color) = inline_style.background_color {
        background = Some(html_css_color_to_hsla(color, computed.color));
    }

    HtmlVisualStyle {
        computed,
        background,
    }
}

/// 判断 HTML 节点是否按块级（独占一行）排布。
///
/// 行内标签与文本节点归入行内流；`br`/`img` 虽属块级标签，仍在行内流中渲染；
/// 原始回退内容独占整块。
fn html_node_is_block_level(node: &HtmlNode) -> bool {
    match node.kind {
        HtmlNodeKind::InlineSemantic => false,
        HtmlNodeKind::RawTextBlock => true,
        HtmlNodeKind::BlockSemantic => !matches!(node.tag_name.as_str(), "br" | "img"),
    }
}

/// 纯空白文本节点：HTML 折叠空白，结构性换行/缩进本身不产生内容。
fn html_is_whitespace_text(node: &HtmlNode) -> bool {
    node.tag_name == "#text" && node.raw_source.chars().all(char::is_whitespace)
}

/// 把一个行内游程收束为可换行的行内容器；空游程跳过。
fn push_html_inline_run(run: &mut Vec<AnyElement>, out: &mut Vec<AnyElement>) {
    if run.is_empty() {
        return;
    }
    let items = std::mem::take(run);
    out.push(
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .min_w(px(0.0))
            .children(items)
            .into_any_element(),
    );
}

impl Block {
    /// 处理 HTML 折叠切换鼠标按下。
    fn on_html_details_toggle_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.html_details_open = !self.html_details_open;
        cx.stop_propagation();
        cx.notify();
    }

    /// 渲染图片内容。
    fn render_image_content(
        &self,
        runtime: &ImageRuntime,
        max_width: Length,
        max_height: Pixels,
        placeholder_height: Pixels,
        theme: &Theme,
        i18n: &I18nSnapshot,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let source = runtime.resolved_source.clone();
        let placeholder_theme = theme.clone();
        let loading_theme = theme.clone();
        // 所需文案在闭包外解析为自有字符串，再移入回退/加载回调。
        let placeholder_label = fallback_image_label(&runtime.alt, i18n);
        let loading_label = loading_image_label(&runtime.alt, i18n);

        let image = img_for_resolved_source(source)
            .max_w(max_width)
            .max_h(max_height)
            .object_fit(ObjectFit::Contain)
            .with_fallback(move || {
                render_image_placeholder(
                    max_width,
                    placeholder_height,
                    &placeholder_theme,
                    placeholder_label.clone(),
                )
            })
            .with_loading(move || {
                render_loading_placeholder(
                    max_width,
                    placeholder_height,
                    &loading_theme,
                    loading_label.clone(),
                )
            });

        let mut container = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(d.image_caption_gap))
            .child(image);

        if let Some(title) = runtime
            .title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
        {
            container = container.child(
                div()
                    .w_full()
                    .text_center()
                    .text_size(px(t.code_size))
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(title.clone())),
            );
        }

        container.into_any_element()
    }

    /// 渲染列表项中的独立图片内容。
    ///
    /// 编辑态下让「源码行 + 渲染图」同屏，行为与独立图片段落一致（对齐 Typora）。
    fn render_list_item_image(
        &self,
        focused: bool,
        is_placeholder: bool,
        theme: &Theme,
        i18n: &I18nSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let viewport_width = f32::from(window.viewport_size().width.max(px(1.0)));
        let max_width = px(effective_list_item_image_width(self, viewport_width, d));
        let Some(runtime) = self.image_runtime() else {
            return div().min_w(px(0.0)).flex_grow(1.).child(
                self.render_text_or_mixed_inline_visuals(
                    theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.text_size,
                    FontWeight::NORMAL,
                    cx,
                ),
            );
        };
        let image = self.render_image_content(
            runtime,
            max_width.into(),
            px(d.image_root_max_height),
            px(d.image_root_placeholder_height),
            theme,
            i18n,
        );
        if self.showing_image_source_with_render() && focused {
            div()
                .min_w(px(0.0))
                .flex_grow(1.)
                .flex()
                .flex_col()
                .gap(px(d.block_padding_y))
                .child(self.render_text_or_mixed_inline_visuals(
                    theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.text_size,
                    FontWeight::NORMAL,
                    cx,
                ))
                .child(div().flex_grow(1.).child(image))
        } else {
            div().flex_grow(1.).child(image)
        }
    }

    /// 渲染数学公式内容。
    fn render_math_content(&self, theme: &Theme) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let raw = self
            .record
            .raw_fallback
            .as_deref()
            .unwrap_or_else(|| self.rendered_text());

        let Some(source) = parse_display_math_source(raw) else {
            return div()
                .w_full()
                .text_size(px(t.text_size))
                .line_height(rems(t.text_line_height))
                .text_color(theme.foreground)
                .child(SharedString::from(raw.to_string()))
                .into_any_element();
        };

        match render_display_math_svg(
            &source,
            theme.foreground,
            display_math_font_size(t.text_size),
        ) {
            Ok(rendered) => div()
                .w_full()
                .flex()
                .justify_center()
                .py(px(d.block_padding_y.max(6.0)))
                .child(
                    img(rendered.path)
                        .max_w(Length::Definite(relative(1.0)))
                        .max_h(px(d.image_root_max_height))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Err(err) => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .rounded_sm()
                .bg(theme.list_even)
                .px(px(d.block_padding_x))
                .py(px(d.block_padding_y))
                .text_size(px(t.text_size))
                .line_height(rems(t.text_line_height))
                .text_color(theme.foreground)
                .child(SharedString::from(raw.to_string()))
                .child(
                    div()
                        .text_size(px(t.code_size))
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(format!("LaTeX render error: {err}"))),
                )
                .into_any_element(),
        }
    }

    /// 渲染 Mermaid 内容。
    fn render_mermaid_content(
        &self,
        theme: &Theme,
        i18n: &I18nSnapshot,
        window: &Window,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let raw = self
            .record
            .raw_fallback
            .as_deref()
            .unwrap_or_else(|| self.rendered_text());

        let Some(source) = parse_mermaid_fence_source(raw) else {
            return div()
                .w_full()
                .text_size(px(t.text_size))
                .line_height(rems(t.text_line_height))
                .text_color(theme.foreground)
                .child(SharedString::from(raw.to_string()))
                .into_any_element();
        };

        let viewport_width = f32::from(window.viewport_size().width.max(px(1.0)));
        let available_width = effective_image_width(self, viewport_width, d);

        match render_mermaid_svg_for_display(&source, available_width, viewport_width) {
            Ok(rendered) => {
                let display_width = rendered.display_width.max(1.0);
                let display_height = rendered.display_height.max(1.0);
                let image_path = rendered.path.clone();
                let image = move || {
                    img(image_path.clone())
                        .w(px(display_width))
                        .h(px(display_height))
                };
                let content = if display_width <= available_width + 0.5 {
                    div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(image())
                        .into_any_element()
                } else {
                    div()
                        .id(ElementId::Name(
                            format!("mermaid-scroll-{}", self.record.id).into(),
                        ))
                        .w_full()
                        .overflow_x_scroll()
                        .scrollbar_width(px(0.0))
                        .child(div().w(px(display_width)).child(image()))
                        .into_any_element()
                };

                div()
                    .w_full()
                    .py(px(d.block_padding_y.max(6.0)))
                    .child(content)
                    .into_any_element()
            }
            Err(err) => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .rounded_sm()
                .bg(theme.list_even)
                .px(px(d.block_padding_x))
                .py(px(d.block_padding_y))
                .text_size(px(t.text_size))
                .line_height(rems(t.text_line_height))
                .text_color(theme.foreground)
                .child(SharedString::from(raw.to_string()))
                .child(
                    div()
                        .text_size(px(t.code_size))
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(i18n.t(
                            "code_mermaid_render_error",
                            &[("error", &err.to_string())],
                        ))),
                )
                .into_any_element(),
        }
    }

    /// 渲染文本或混合行内视觉。
    // 渲染管线按扁平传参约定逐层透传主题与上下文，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    fn render_text_or_mixed_inline_visuals(
        &self,
        theme: &Theme,
        focused: bool,
        is_placeholder: bool,
        placeholder_text: Option<SharedString>,
        placeholder_color: Option<Hsla>,
        text_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 混合行内视觉仅展示。聚焦后由文本元素接管，使光标移动、投影标记与输入法范围锚定在可编辑文本而非渲染 SVG/上下标偏移上。
        if focused || is_placeholder || !self.has_mixed_inline_visuals() {
            return match placeholder_text {
                Some(placeholder) => TextBlockElement::with_placeholder(
                    cx.entity(),
                    is_placeholder,
                    placeholder,
                    placeholder_color,
                )
                .into_any_element(),
                None => TextBlockElement::new(cx.entity(), is_placeholder).into_any_element(),
            };
        }

        self.render_mixed_inline_visual_runs(theme, text_color, font_size, font_weight, cx)
    }

    /// 渲染混合行内视觉游程。
    fn render_mixed_inline_visual_runs(
        &self,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_inline_tree_runs(
            &self.record.title,
            theme,
            base_color,
            font_size,
            font_weight,
            cx,
        )
    }

    /// 渲染行内树游程。
    fn render_inline_tree_runs(
        &self,
        tree: &crate::components::InlineTextTree,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(0.0))
            .text_size(px(font_size))
            .line_height(rems(ThemeTypography::DEFAULT.text_line_height))
            .children(self.render_inline_tree_children(
                tree,
                theme,
                base_color,
                font_size,
                font_weight,
                cx,
            ))
            .into_any_element()
    }

    /// 渲染行内树子节点。
    fn render_inline_tree_children(
        &self,
        tree: &crate::components::InlineTextTree,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let cache = tree.render_cache();
        let text = cache.plain_text();
        let mut children = Vec::new();
        let mut cursor = 0usize;

        for span in cache.spans() {
            if cursor < span.range.start {
                let fallback_span = crate::components::InlineSpan {
                    range: cursor..span.range.start,
                    style: crate::components::InlineStyle::default(),
                    html_style: None,
                    link: None,
                    footnote: None,
                    math: None,
                };
                children.extend(self.render_inline_text_word_segments(
                    &text[cursor..span.range.start],
                    &fallback_span,
                    theme,
                    base_color,
                    font_size,
                    font_weight,
                    cx,
                ));
            }

            let span_text = &text[span.range.clone()];
            if let Some(math) = span.math.as_ref() {
                children.push(
                    self.render_inline_math_segment(math, span, theme, base_color, font_size, cx),
                );
            } else {
                children.extend(self.render_inline_text_word_segments(
                    span_text,
                    span,
                    theme,
                    base_color,
                    font_size,
                    font_weight,
                    cx,
                ));
            }
            cursor = span.range.end;
        }

        if cursor < text.len() {
            let fallback_span = crate::components::InlineSpan {
                range: cursor..text.len(),
                style: crate::components::InlineStyle::default(),
                html_style: None,
                link: None,
                footnote: None,
                math: None,
            };
            children.extend(self.render_inline_text_word_segments(
                &text[cursor..],
                &fallback_span,
                theme,
                base_color,
                font_size,
                font_weight,
                cx,
            ));
        }

        children
    }

    /// 把带样式文本游程切为易换行的词片段。混合行内视觉布局为 `flex_wrap` 行，长游程若渲染为单个元素会在内部换行并占满整行宽，把下一项（行内公式、上下标等）挤到独占一行。按空白分隔逐词输出元素，行可在词间断行，相邻视觉保持在同一视觉行。行内代码与背景高亮保持单个元素，使其胶囊/背景连续。
    // 渲染管线按扁平传参约定逐层透传主题与上下文，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    fn render_inline_text_word_segments(
        &self,
        text: &str,
        span: &crate::components::InlineSpan,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let has_background = span
            .html_style
            .is_some_and(|style| style.background_color.is_some());
        let mut segments = Vec::new();
        for word in inline_word_chunks(text, span.style.code, has_background) {
            segments.push(self.render_inline_text_segment(
                word,
                span,
                theme,
                base_color,
                font_size,
                font_weight,
                cx,
            ));
        }
        segments
    }

    /// 渲染行内文本片段。
    // 渲染管线按扁平传参约定逐层透传主题与上下文，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    fn render_inline_text_segment(
        &self,
        text: &str,
        span: &crate::components::InlineSpan,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if text.is_empty() {
            return div().into_any_element();
        }

        let mut color = if span.link.is_some() || span.footnote.is_some() {
            theme.link
        } else {
            base_color
        };
        if let Some(style) = span.html_style
            && let Some(html_color) = style.color
        {
            color = html_css_color_to_hsla(html_color, color);
        }

        let script_offset = match span.style.script {
            InlineScript::Normal => 0.0,
            InlineScript::Superscript => -font_size * 0.28,
            InlineScript::Subscript => font_size * 0.22,
        };
        let display_font_size = if span.style.has_script() {
            (font_size * 0.72).max(6.0)
        } else {
            font_size
        };

        let mut element = div()
            .min_w(px(0.0))
            .text_size(px(display_font_size))
            .line_height(rems(ThemeTypography::DEFAULT.text_line_height))
            .text_color(color)
            .font_weight(if span.style.bold {
                FontWeight::BOLD
            } else {
                font_weight
            })
            .child(SharedString::from(text.to_string()));

        if script_offset != 0.0 {
            element = element.relative().top(px(script_offset));
        }

        if span.style.underline || span.link.is_some() || span.footnote.is_some() {
            element = element.underline();
        }
        if span.style.code {
            element = element
                .rounded(px(ThemeDimensions::DEFAULT.code_bg_radius))
                .px(px(ThemeDimensions::DEFAULT.code_bg_pad_x))
                .py(px(ThemeDimensions::DEFAULT.code_bg_pad_y))
                .bg(theme.muted);
        }
        if let Some(style) = span.html_style
            && let Some(background) = style.background_color
        {
            element = element
                .rounded(px(3.0))
                .px(px(2.0))
                .bg(html_css_color_to_hsla(background, color));
        }

        // 该游程渲染为纯（非交互）文本，混合行内视觉块（伴随公式或上下标）中的链接否则无法跟随。直接在片段上挂打开链接处理，仅响应 Cmd/Ctrl+点击，普通点击仍穿透以聚焦块编辑。包裹元素按同一修饰键控制手形光标，与经 `TextBlockElement` 的普通文本路径一致。
        if let Some(link) = span.link.clone() {
            let element = element
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::on_rendered_link_mouse_down),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |block, event: &MouseUpEvent, _window, cx| {
                        if event.modifiers.secondary() {
                            block.open_rendered_link(&link, cx);
                        }
                    }),
                );
            return LinkCursor {
                child: element.into_any_element(),
            }
            .into_any_element();
        }

        element.into_any_element()
    }

    /// 渲染行内公式片段。
    fn render_inline_math_segment(
        &self,
        math: &crate::components::InlineMath,
        span: &crate::components::InlineSpan,
        theme: &Theme,
        base_color: Hsla,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut color = base_color;
        if let Some(style) = span.html_style
            && let Some(html_color) = style.color
        {
            color = html_css_color_to_hsla(html_color, color);
        }
        let math_size = inline_math_font_size(font_size);
        match render_inline_math_svg(&math.body, color, math_size) {
            Ok(rendered) => div()
                .flex()
                .items_center()
                .h(px(math_size * 1.65))
                .child(
                    img(rendered.path)
                        .max_h(px(math_size * 1.65))
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element(),
            Err(_) => self.render_inline_text_segment(
                &math.source,
                span,
                theme,
                base_color,
                font_size,
                FontWeight::NORMAL,
                cx,
            ),
        }
    }

    /// 渲染行内图片内容。
    fn render_inline_image_content(
        &self,
        runtime: &ImageRuntime,
        theme: &Theme,
        i18n: &I18nSnapshot,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let source = runtime.resolved_source.clone();
        let max_height = px(d.image_cell_placeholder_height);
        let max_width =
            Length::Definite(px((d.image_cell_placeholder_height * 1.6).max(48.0)).into());
        let placeholder_theme = theme.clone();
        let loading_theme = theme.clone();
        // 所需文案在闭包外解析为自有字符串，再移入回退/加载回调。
        let placeholder_label = fallback_image_label(&runtime.alt, i18n);
        let loading_label = loading_image_label(&runtime.alt, i18n);

        let image = img_for_resolved_source(source)
            .max_w(max_width)
            .max_h(max_height)
            .object_fit(ObjectFit::Contain)
            .with_fallback(move || {
                render_image_placeholder(
                    max_width,
                    max_height,
                    &placeholder_theme,
                    placeholder_label.clone(),
                )
            })
            .with_loading(move || {
                render_loading_placeholder(
                    max_width,
                    max_height,
                    &loading_theme,
                    loading_label.clone(),
                )
            });

        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .child(image)
            .into_any_element()
    }

    /// 渲染表格单元格行内图片。
    fn render_table_cell_inline_images(
        &self,
        theme: &Theme,
        i18n: &I18nSnapshot,
        font_weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let segments = parse_table_cell_inline_images(&self.record.title.serialize_markdown());
        if !segments
            .iter()
            .any(|segment| matches!(segment, TableCellInlineImageSegment::Image { .. }))
        {
            return None;
        }

        let mut children = Vec::new();
        for segment in segments {
            match segment {
                TableCellInlineImageSegment::Text(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    let tree = self.inline_tree_from_markdown_with_context(&text);
                    children.extend(self.render_inline_tree_children(
                        &tree,
                        theme,
                        theme.foreground,
                        ThemeTypography::DEFAULT.text_size,
                        font_weight,
                        cx,
                    ));
                }
                TableCellInlineImageSegment::Image { markdown, syntax } => {
                    if let Some(runtime) = self.image_runtime_for_syntax(syntax) {
                        children.push(self.render_inline_image_content(&runtime, theme, i18n));
                    } else {
                        let tree = crate::components::InlineTextTree::plain(markdown);
                        children.extend(self.render_inline_tree_children(
                            &tree,
                            theme,
                            theme.foreground,
                            ThemeTypography::DEFAULT.text_size,
                            font_weight,
                            cx,
                        ));
                    }
                }
            }
        }

        Some(
            div()
                .w_full()
                .min_w(px(0.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.0))
                .text_size(px(ThemeTypography::DEFAULT.text_size))
                .line_height(rems(ThemeTypography::DEFAULT.text_line_height))
                .children(children)
                .into_any_element(),
        )
    }

    /// 渲染 HTML 文档。
    fn render_html_document(
        &self,
        document: &HtmlDocument,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        if !document.is_semantic() {
            return div()
                .w_full()
                .rounded_sm()
                .bg(theme.list_even)
                .px(px(d.block_padding_x))
                .py(px(d.block_padding_y))
                .text_size(px(t.code_size))
                .text_color(theme.foreground)
                .child(SharedString::from(document.raw_source.clone()))
                .into_any_element();
        }

        let children =
            self.render_html_flow_children(&document.nodes, theme, HtmlStyle::root(theme), cx);
        div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(d.block_gap * 0.4))
            .children(children)
            .into_any_element()
    }

    /// 渲染 HTML 子节点列表（先收集成 `Vec` 再挂载）。
    ///
    /// 栈保护：`.children(iter.map(...))` 会让父级半成品 `Div`
    /// 连同巨型嵌套泛型帧在子树深度优先求值期间全程存活，
    /// debug 下每层约 175KB，深树滚动直接耗尽主线程栈；
    /// 先逐个渲染收集，父级 `Div` 在子树返回后才构造，
    /// 递归路径上的帧只剩参数与小局部量。
    fn render_html_children(
        &self,
        children: &[HtmlNode],
        theme: &Theme,
        style: HtmlStyle,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rendered = Vec::with_capacity(children.len());
        for child in children {
            rendered.push(self.render_html_node(child, theme, style, cx));
        }
        rendered
    }

    /// 表格结构容器（`table`/`thead`/`tbody`/`tfoot`/`tr`）的子节点渲染：
    /// 丢弃纯空白文本节点。
    ///
    /// 这些元素的内容模型只允许表格元素，源码里的换行与缩进是结构性空白，
    /// 依 HTML 折叠规则不产生内容；照常渲染会在单元格之间多出空白列/空行。
    fn render_html_structural_children(
        &self,
        children: &[HtmlNode],
        theme: &Theme,
        style: HtmlStyle,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut rendered = Vec::with_capacity(children.len());
        for child in children {
            if html_is_whitespace_text(child) {
                continue;
            }
            rendered.push(self.render_html_node(child, theme, style, cx));
        }
        rendered
    }

    /// 按 HTML 匿名盒模型渲染子节点：连续行内子节点合成一个可换行的行内流，
    /// 块级子节点各自成块。
    ///
    /// rgpui 默认 `display: block`，会把每个行内子节点拆成独立行，段内文本被
    /// 逐段断行、块高异常；此处显式还原行内流。文本节点按空白切成词块，使
    /// 行内流可在词间断行。
    fn render_html_flow_children(
        &self,
        children: &[HtmlNode],
        theme: &Theme,
        style: HtmlStyle,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::with_capacity(children.len());
        let mut run: Vec<AnyElement> = Vec::new();
        for (index, child) in children.iter().enumerate() {
            if html_is_whitespace_text(child) {
                // 结构性空白：HTML 折叠空白，块级元素之间的换行/缩进不渲染。
                // 仅当夹在两个行内节点之间时才折叠成一个空格，否则整段丢弃。
                let prev_inline = index
                    .checked_sub(1)
                    .is_some_and(|prev| !html_node_is_block_level(&children[prev]));
                let next_inline = children
                    .get(index + 1)
                    .is_some_and(|next| !html_node_is_block_level(next));
                if prev_inline && next_inline && !run.is_empty() {
                    run.push(
                        div()
                            .text_size(px(style.font_size))
                            .text_color(style.color)
                            .child(SharedString::from(" "))
                            .into_any_element(),
                    );
                }
                continue;
            }
            if html_node_is_block_level(child) {
                push_html_inline_run(&mut run, &mut out);
                out.push(self.render_html_node(child, theme, style, cx));
            } else if child.tag_name == "#text" {
                match split_html_text_at_blank_line(&child.raw_source) {
                    Some((html_part, markdown_part)) => {
                        // 空行之前的缩进/换行只是 HTML 块内结构空白，折叠后丢弃。
                        let html_part = html_part.trim();
                        if !html_part.is_empty() {
                            for chunk in inline_word_chunks(html_part, false, false) {
                                run.push(
                                    div()
                                        .min_w(px(0.0))
                                        .text_size(px(style.font_size))
                                        .text_color(style.color)
                                        .child(SharedString::from(chunk.to_string()))
                                        .into_any_element(),
                                );
                            }
                        }
                        // 空行之后按 Markdown 解析，使折叠区正文里的行内代码/强调正常渲染。
                        let markdown = markdown_part.trim();
                        if !markdown.is_empty() {
                            let tree = crate::components::InlineTextTree::from_markdown(markdown);
                            run.extend(self.render_inline_tree_children(
                                &tree,
                                theme,
                                style.color,
                                style.font_size,
                                FontWeight::NORMAL,
                                cx,
                            ));
                        }
                    }
                    None => {
                        for chunk in inline_word_chunks(&child.raw_source, false, false) {
                            run.push(
                                div()
                                    .min_w(px(0.0))
                                    .text_size(px(style.font_size))
                                    .text_color(style.color)
                                    .child(SharedString::from(chunk.to_string()))
                                    .into_any_element(),
                            );
                        }
                    }
                }
            } else {
                run.push(self.render_html_node(child, theme, style, cx));
            }
        }
        push_html_inline_run(&mut run, &mut out);
        out
    }

    /// 渲染 HTML 节点。
    fn render_html_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        inherited_style: HtmlStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 帧瘦身：各分支的 Div 构造器搬进独立函数，本函数只做分发。
        // debug 下巨型 match 会把所有分支临时量摊进同一帧（曾测出单帧约 170KB），
        // 深 HTML 树逐层累积直接耗尽主线程栈；拆分后每层只剩分发帧。
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;

        if node.kind == HtmlNodeKind::RawTextBlock {
            return div()
                .w_full()
                .rounded_sm()
                .bg(theme.list_even)
                .px(px(d.block_padding_x * 0.6))
                .py(px(d.block_padding_y * 0.6))
                .text_size(px(t.code_size))
                .text_color(theme.foreground)
                .child(SharedString::from(node.raw_source.clone()))
                .into_any_element();
        }

        if node.tag_name == "#text" {
            return div()
                .min_w(px(0.0))
                .text_size(px(inherited_style.font_size))
                .text_color(inherited_style.color)
                .child(SharedString::from(node.raw_source.clone()))
                .into_any_element();
        }

        let node_style = html_node_visual_style(node, inherited_style, theme);
        match node.tag_name.as_str() {
            "strong" | "b" => {
                self.render_html_inline_container(node, theme, node_style, FontWeight::BOLD, cx)
            }
            "em" | "i" | "span" | "abbr" | "dfn" | "time" | "u" | "ins" | "del" | "small"
            | "sup" | "sub" | "a" => {
                self.render_html_inline_container(node, theme, node_style, FontWeight::NORMAL, cx)
            }
            "mark" => {
                self.render_html_inline_container(node, theme, node_style, FontWeight::NORMAL, cx)
            }
            "code" | "kbd" => self.render_html_code_node(node, theme, node_style, cx),
            "q" => self.render_html_q_node(node, theme, node_style, cx),
            "br" => div().child("\n").into_any_element(),
            "hr" => div()
                .w_full()
                .h(px(d.separator_thickness))
                .my(px(d.separator_margin_y))
                .bg(theme.border)
                .rounded(px(999.0))
                .into_any_element(),
            "blockquote" => self.render_html_blockquote_node(node, theme, node_style, cx),
            "pre" => {
                let mut element = div()
                    .w_full()
                    .rounded_sm()
                    .px(px(d.code_block_padding_x))
                    .py(px(d.code_block_padding_y))
                    .text_size(px(node_style.computed.font_size))
                    .text_color(node_style.computed.color)
                    .child(SharedString::from(html_children_text(node)));
                if let Some(bg) = node_style.background {
                    element = element.bg(bg);
                }
                element.into_any_element()
            }
            "img" => self.render_html_image(node, theme, node_style, cx),
            "table" => self.render_html_table(node, theme, node_style, cx),
            "thead" | "tbody" | "tfoot" => {
                self.render_html_table_section_node(node, theme, node_style, cx)
            }
            "tr" => self.render_html_table_row(node, theme, node_style, cx),
            "th" | "td" => self.render_html_cell_node(node, theme, node_style, cx),
            "details" => self.render_html_details(node, theme, node_style, cx),
            "summary" => self.render_html_summary_node(node, theme, node_style, cx),
            "figure" => self.render_html_figure_node(node, theme, node_style, cx),
            "figcaption" => self.render_html_figcaption_node(node, theme, node_style, cx),
            _ => self.render_html_fallback_node(node, theme, node_style, cx),
        }
    }

    /// 渲染 HTML 代码节点（行内容器统一样式见 `render_html_inline_container`）。
    fn render_html_code_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .flex()
            .rounded(px(4.0))
            .px(px(4.0))
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 引用节点（前后引号为静态文本，中间为子节点）。
    fn render_html_q_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut children = Vec::with_capacity(node.children.len() + 2);
        children.push(div().child("“").into_any_element());
        children.extend(self.render_html_children(&node.children, theme, node_style.computed, cx));
        children.push(div().child("”").into_any_element());
        let mut element = div()
            .flex()
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg).rounded(px(3.0)).px(px(2.0));
        }
        element.into_any_element()
    }

    /// 渲染 HTML 引用块节点（左侧竖线 + 缩进）。
    fn render_html_blockquote_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .pl(px(d.quote_padding_left))
            .border_l(px(d.quote_border_width))
            .border_color(theme.border)
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 表格分组节点（thead/tbody/tfoot 纵向堆叠）。
    fn render_html_table_section_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children =
            self.render_html_structural_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .flex()
            .flex_col()
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 表格单元格节点（表头加粗，单元格描边）。
    fn render_html_cell_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .min_w(px(0.0))
            .flex_grow(1.)
            .border_r(px(1.0))
            .border_b(px(1.0))
            .border_color(theme.table_row_border)
            .px(px(d.table_cell_padding_x))
            .py(px(d.table_cell_padding_y))
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .font_weight(if node.tag_name == "th" {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 摘要节点（折叠区标题行）。
    fn render_html_summary_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 插图节点（居中纵向堆叠）。
    fn render_html_figure_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let d = &ThemeDimensions::DEFAULT;
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(d.image_caption_gap))
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 插图标题节点（居中文本）。
    fn render_html_figcaption_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .text_center()
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染未知 HTML 标签节点（透传子节点，不丢内容）。
    fn render_html_fallback_node(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children =
            self.render_html_flow_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 行内容器。
    fn render_html_inline_container(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        weight: FontWeight,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children = self.render_html_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .flex()
            .min_w(px(0.0))
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .font_weight(weight)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg).rounded(px(3.0)).px(px(2.0));
        }
        match node.tag_name.as_str() {
            "sup" => {
                element = element
                    .relative()
                    .top(px(-node_style.computed.font_size * 0.28))
            }
            "sub" => {
                element = element
                    .relative()
                    .top(px(node_style.computed.font_size * 0.22))
            }
            _ => {}
        }
        element.into_any_element()
    }

    /// 渲染 HTML 图片。
    fn render_html_image(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let parsed_image = parse_html_image_block(&node.raw_source);
        let src = parsed_image
            .as_ref()
            .map(|image| image.src.as_str())
            .or_else(|| attr_value(node, "src"))
            .filter(|src| !src.trim().is_empty());
        let Some(src) = src else {
            let mut element = div()
                .text_size(px(node_style.computed.font_size))
                .text_color(node_style.computed.color)
                .child(SharedString::from(node.raw_source.clone()));
            if let Some(bg) = node_style.background {
                element = element.bg(bg);
            }
            return element.into_any_element();
        };
        let alt = parsed_image
            .as_ref()
            .map(|image| image.alt.clone())
            .unwrap_or_else(|| attr_value(node, "alt").unwrap_or_default().to_string());
        let zoom = parsed_image
            .as_ref()
            .map(|image| image.zoom_factor())
            .unwrap_or(1.0);
        let runtime = ImageRuntime {
            alt,
            src: src.to_string(),
            title: None,
            resolved_source: resolve_image_source(src, self.image_base_dir()),
        };
        let i18n = cx.global::<I18nManager>().snapshot();
        let content = self.render_image_content(
            &runtime,
            Length::Definite(relative(zoom)),
            px(ThemeDimensions::DEFAULT.image_root_max_height * zoom),
            px(ThemeDimensions::DEFAULT.image_root_placeholder_height * zoom),
            theme,
            &i18n,
        );
        if let Some(bg) = node_style.background {
            div().w_full().bg(bg).child(content).into_any_element()
        } else {
            content
        }
    }

    /// 渲染 HTML 表格。
    fn render_html_table(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 外框只画上/左两条边，单元格各自画右/下两条边：相邻格线不叠加成 2px，
        // 整表得到 1px 的规整网格（对齐 GitHub 的 collapse 边框）。
        let children =
            self.render_html_structural_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .rounded_sm()
            .border_t(px(1.0))
            .border_l(px(1.0))
            .border_color(theme.table_row_border)
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 表格行。
    fn render_html_table_row(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let children =
            self.render_html_structural_children(&node.children, theme, node_style.computed, cx);
        let mut element = div()
            .w_full()
            .flex()
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .children(children);
        if let Some(bg) = node_style.background {
            element = element.bg(bg);
        }
        element.into_any_element()
    }

    /// 渲染 HTML 折叠区。
    fn render_html_details(
        &self,
        node: &HtmlNode,
        theme: &Theme,
        node_style: HtmlVisualStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_open = attr_value(node, "open").is_some() || self.html_details_open;
        let summary = node
            .children
            .iter()
            .find(|child| child.tag_name == "summary");
        let body: Vec<HtmlNode> = node
            .children
            .iter()
            .filter(|child| child.tag_name != "summary")
            .cloned()
            .collect();
        let mut summary_children = Vec::with_capacity(1);
        if let Some(summary) = summary {
            summary_children.push(self.render_html_node(summary, theme, node_style.computed, cx));
        }

        let mut container = div()
            .w_full()
            .rounded_sm()
            .border(px(1.0))
            .border_color(theme.table_row_border)
            .px(px(ThemeDimensions::DEFAULT.block_padding_x))
            .py(px(ThemeDimensions::DEFAULT.block_padding_y))
            .text_size(px(node_style.computed.font_size))
            .text_color(node_style.computed.color)
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap(px(ThemeDimensions::DEFAULT.list_marker_gap))
                    .font_weight(FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(Self::on_html_details_toggle_mouse_down),
                    )
                    .child(if is_open { "\u{25BE}" } else { "\u{25B8}" })
                    .children(summary_children),
            );
        if let Some(bg) = node_style.background {
            container = container.bg(bg);
        }

        if is_open {
            // 折叠区正文按 HTML 流渲染：结构空白折叠、空行后的内容按 Markdown 解析，
            // 与 GitHub（HTML 块遇空行结束、其后为 Markdown 段落）保持一致。
            let body_children =
                self.render_html_flow_children(&body, theme, node_style.computed, cx);
            container = container.child(
                div()
                    .w_full()
                    .pt(px(ThemeDimensions::DEFAULT.block_padding_y))
                    .children(body_children),
            );
        }

        container.into_any_element()
    }

    /// 渲染块外壳。
    // 渲染管线按扁平传参约定逐层透传主题与上下文，打包结构体反而引入生命周期复杂度。
    #[allow(clippy::too_many_arguments)]
    fn render_shell(
        &self,
        block_id: ElementId,
        source_mode: bool,
        cursor_style: CursorStyle,
        padding_left: f32,
        padding_right: f32,
        dimensions: &ThemeDimensions,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let base = div()
            .id(block_id)
            .key_context(EDITOR_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_newline))
            .on_action(cx.listener(Self::on_delete_back))
            .on_action(cx.listener(Self::on_delete))
            .on_action(cx.listener(Self::on_word_delete_back))
            .on_action(cx.listener(Self::on_word_delete_forward))
            .on_action(cx.listener(Self::on_focus_prev))
            .on_action(cx.listener(Self::on_focus_next))
            .on_action(cx.listener(Self::on_move_left))
            .on_action(cx.listener(Self::on_move_right))
            .on_action(cx.listener(Self::on_word_move_left))
            .on_action(cx.listener(Self::on_word_move_right))
            .on_action(cx.listener(Self::on_home))
            .on_action(cx.listener(Self::on_end))
            .on_action(cx.listener(Self::on_block_up))
            .on_action(cx.listener(Self::on_block_down))
            .on_action(cx.listener(Self::on_select_left))
            .on_action(cx.listener(Self::on_select_right))
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_word_select_left))
            .on_action(cx.listener(Self::on_word_select_right))
            .on_action(cx.listener(Self::on_select_home))
            .on_action(cx.listener(Self::on_select_end))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_exit_code_block))
            .on_key_down(cx.listener(Self::on_block_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .min_w(px(0.0))
            .flex_shrink_0()
            .min_h(px(dimensions.block_min_height))
            .py(px(dimensions.block_padding_y))
            .pl(px(padding_left))
            .pr(px(padding_right))
            .cursor(cursor_style);

        // 记录整块外壳边界：分隔线没有文本元素，自身不会记录块边界，
        // 这里补一次好让跨块选区按点命中能把它当成整块（点击即可选中）；
        // 其余块也记一份外壳边界，供鼠标按下时挡掉落在本块之外的误投递。
        let base = {
            let entity = cx.entity();
            let is_separator = self.kind().is_separator();
            base.on_prepaint(move |bounds, _window, cx| {
                entity.update(cx, |block, _| {
                    block.last_shell_bounds = Some(bounds);
                    if is_separator {
                        block.last_bounds = Some(bounds);
                    }
                });
            })
        };

        if source_mode {
            base
        } else {
            base.on_action(cx.listener(Self::on_indent_block))
                .on_action(cx.listener(Self::on_outdent_block))
                .on_action(cx.listener(Self::on_bold_selection))
                .on_action(cx.listener(Self::on_italic_selection))
                .on_action(cx.listener(Self::on_underline_selection))
                .on_action(cx.listener(Self::on_code_selection))
        }
    }
}

impl Focusable for Block {
    /// 取焦点句柄。
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// 渲染方法构建块的完整元素树：
/// - 通用包裹：按键上下文、焦点跟踪、动作处理、鼠标事件。
/// - 按类型样式：标题取尺寸/字重/边框，列表项取标记加内容的弹性行，其余渲染为纯文本。
/// - [`TextBlockElement`] 负责文本布局、选区与光标。
impl Render for Block {
    /// 渲染块。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let code_language_focused = self.code_language_focus_handle.is_focused(window);
        let input_active = focused || code_language_focused;
        if self.sync_image_focus_state(focused) {
            cx.notify();
        }

        let showing_rendered_image = self.showing_rendered_image();
        // 行内公式在聚焦时保持投影视图（其 `$...$` 源码显示为可编辑文本），同块内的链接与其他样式因而保留属性而不坍缩为纯 Markdown，与上下标已有行为一致。
        self.refresh_inline_projection_for_focus(focused && !showing_rendered_image);

        if input_active && self.cursor_blink_task.is_none() {
            self.start_cursor_blink(cx);
        } else if !input_active && self.cursor_blink_task.is_some() {
            self.cursor_blink_task = None;
        }
        if !input_active {
            self.reset_code_language_input_layout();
        }

        let block_id = ElementId::Name(format!("block-{}", self.record.id).into());
        let is_placeholder =
            focused && self.rendered_text().is_empty() && self.marked_range.is_none();

        let theme = rgpui::Theme::global(cx).clone();
        let i18n = cx.global::<I18nManager>().snapshot();
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let depth_padding = d.block_padding_x + d.nested_block_indent * self.render_depth as f32;

        if self.is_table_cell() {
            let is_header = self
                .table_cell_position()
                .map(|position| position.is_header())
                .unwrap_or(false);
            // 仅在开启表头展示偏好时，表头行才呈现独特样式（底纹背景、中字重）。
            let style_as_header =
                is_header && crate::config::EditorSettings::table_headers_visible(cx);
            let highlight = self.table_axis_highlight;
            let base_bg = if style_as_header {
                theme.table_head
            } else {
                theme.table_even
            };
            let bg = match highlight {
                TableAxisHighlight::None => base_bg,
                TableAxisHighlight::Preview => theme.table_hover,
                TableAxisHighlight::Selected => theme.table_active,
            };
            let border_color = if focused {
                theme.table_active_border
            } else {
                match highlight {
                    TableAxisHighlight::None => theme.table_row_border,
                    TableAxisHighlight::Preview => theme.table_hover,
                    TableAxisHighlight::Selected => theme.table_active,
                }
            };
            let cell_base = self
                .render_shell(
                    block_id,
                    false,
                    if showing_rendered_image {
                        CursorStyle::PointingHand
                    } else {
                        CursorStyle::IBeam
                    },
                    0.0,
                    0.0,
                    d,
                    cx,
                )
                .w_full()
                .h_full()
                .min_h(px(d.table_cell_min_height))
                .px(px(d.table_cell_padding_x))
                .py(px(d.table_cell_padding_y))
                .rounded(px(2.0))
                .border(px(1.0))
                .border_color(border_color)
                .bg(bg)
                .text_size(px(t.text_size))
                .text_color(theme.foreground)
                .line_height(rems(t.text_line_height));

            let cell_base = if style_as_header {
                cell_base.font_weight(FontWeight::MEDIUM)
            } else {
                cell_base
            };

            if showing_rendered_image && let Some(runtime) = self.image_runtime() {
                return cell_base
                    .child(self.render_image_content(
                        runtime,
                        Length::Definite(relative(1.0)),
                        px(d.image_cell_max_height),
                        px(d.image_cell_placeholder_height),
                        &theme,
                        &i18n,
                    ))
                    .into_any_element();
            }

            if !focused
                && let Some(inline_images) = self.render_table_cell_inline_images(
                    &theme,
                    &i18n,
                    if style_as_header {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    },
                    cx,
                )
            {
                return cell_base.child(inline_images).into_any_element();
            }

            return cell_base
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.text_size,
                    if style_as_header {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    },
                    cx,
                ))
                .into_any_element();
        }

        // HTML 注释与引用定义在渲染视图的非聚焦态完全隐藏（对齐 GitHub/Typora）；
        // 聚焦或源码文档视图时回退到源码显示，以便继续编辑。
        if (self.kind() == BlockKind::Comment || self.is_reference_definition())
            && !focused
            && !self.show_source_line_numbers()
        {
            return div()
                .h(px(0.0))
                .min_h(px(0.0))
                .py(px(0.0))
                .overflow_hidden()
                .into_any_element();
        }

        // 源码模式渲染：无格式的纯文本。数学公式与 Mermaid 块例外——聚焦时仍保留
        // 渲染结果，由下方分支叠加源码行（与图片编辑态一致）。
        // HTML 块在正常渲染模式下同理：始终渲染预览，聚焦时由其分支叠加源码行；
        // 源码文档模式下仍与其它源码块一样按纯源码渲染（含行号）。
        let html_preview_mode =
            self.kind() == BlockKind::HtmlBlock && !self.show_source_line_numbers();
        if self.is_source_raw_mode()
            && !html_preview_mode
            && !matches!(self.kind(), BlockKind::MathBlock | BlockKind::MermaidBlock)
            && (focused
                || !matches!(
                    self.kind(),
                    BlockKind::HtmlBlock | BlockKind::MathBlock | BlockKind::MermaidBlock
                ))
        {
            if focused && self.cursor_blink_task.is_none() {
                self.start_cursor_blink(cx);
            } else if !focused && self.cursor_blink_task.is_some() {
                self.cursor_blink_task = None;
            }
            let source_base = self
                .render_shell(
                    block_id.clone(),
                    true,
                    CursorStyle::IBeam,
                    d.block_padding_x,
                    d.block_padding_x,
                    d,
                    cx,
                )
                .font(crate::theme::code_text_font())
                .text_size(px(t.text_size))
                .text_color(theme.foreground)
                .line_height(rems(t.text_line_height));

            let source_base = if self.kind() == BlockKind::Comment {
                source_base.bg(theme.muted).rounded_sm()
            } else if focused {
                source_base.bg(theme.list_even).rounded_sm()
            } else {
                source_base
            };

            return source_base
                .child(TextBlockElement::new(cx.entity(), is_placeholder))
                .into_any_element();
        }

        let focused_base = self.render_shell(
            block_id.clone(),
            false,
            if showing_rendered_image {
                CursorStyle::PointingHand
            } else {
                CursorStyle::IBeam
            },
            if self.kind().is_separator() {
                depth_padding + d.separator_inset_x
            } else {
                depth_padding
            },
            if self.kind().is_separator() {
                d.block_padding_x + d.separator_inset_x
            } else {
                d.block_padding_x
            },
            d,
            cx,
        );

        // 独立图片段落：编辑态下让「源码行 + 渲染图」同屏，对齐 Typora
        // （点击图片后源码显示在图片上方，图片仍然可见，而不是源码替换图片）。
        if self.kind() == BlockKind::Paragraph
            && !self.is_source_raw_mode()
            && let Some(runtime) = self.image_runtime()
        {
            let viewport_width = f32::from(window.viewport_size().width.max(px(1.0)));
            let max_width = px(effective_image_width(self, viewport_width, d));
            let image = self.render_image_content(
                runtime,
                max_width.into(),
                px(d.image_root_max_height),
                px(d.image_root_placeholder_height),
                &theme,
                &i18n,
            );
            if self.showing_image_source_with_render() && focused {
                let source = self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.text_size,
                    FontWeight::NORMAL,
                    cx,
                );
                return focused_base
                    .flex()
                    .flex_col()
                    .gap(px(d.block_padding_y))
                    .child(source)
                    .child(image)
                    .into_any_element();
            }
            if self.showing_rendered_image() {
                return focused_base.child(image).into_any_element();
            }
        }

        // 分隔线等整块可选中项的空区间高亮：让「已选中」有可见反馈。
        let whole_block_selected = self
            .editor_selection_range
            .as_ref()
            .is_some_and(|range| range.is_empty());

        let content = match self.kind() {
            BlockKind::Separator => {
                let base = focused_base.py(px(d.separator_margin_y));
                let base = if whole_block_selected {
                    base.bg(theme.selection).rounded(px(2.0))
                } else {
                    base
                };
                base.child(
                    div()
                        .w_full()
                        .h(px(d.separator_thickness))
                        .bg(theme.border)
                        .rounded(px(999.0)),
                )
                .into_any_element()
            }
            BlockKind::Heading { level: 1 } => focused_base
                .text_size(px(t.h1_size))
                .font_weight(t.h1_weight)
                .text_color(theme.foreground)
                .pb(px(d.h1_padding_bottom))
                .mb(px(d.h1_margin_bottom))
                .border_b(px(d.h1_border_width))
                .border_color(theme.border)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.h1_size,
                    t.h1_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Heading { level: 2 } => focused_base
                .text_size(px(t.h2_size))
                .font_weight(t.h2_weight)
                .text_color(theme.foreground)
                .pb(px(d.h1_padding_bottom))
                .mb(px(d.h1_margin_bottom))
                .border_b(px(d.h1_border_width))
                .border_color(theme.border)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.h2_size,
                    t.h2_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Heading { level: 3 } => focused_base
                .text_size(px(t.h3_size))
                .font_weight(t.h3_weight)
                .text_color(theme.foreground)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.h3_size,
                    t.h3_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Heading { level: 4 } => focused_base
                .text_size(px(t.h4_size))
                .font_weight(t.h4_weight)
                .text_color(theme.foreground)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.h4_size,
                    t.h4_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Heading { level: 5 } => focused_base
                .text_size(px(t.h5_size))
                .font_weight(t.h5_weight)
                .text_color(theme.foreground)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.h5_size,
                    t.h5_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Heading { level: 6 } => focused_base
                .text_size(px(t.h6_size))
                .font_weight(t.h6_weight)
                .text_color(theme.muted_foreground)
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.muted_foreground,
                    t.h6_size,
                    t.h6_weight,
                    cx,
                ))
                .into_any_element(),
            BlockKind::BulletedListItem => focused_base
                .text_size(px(t.text_size))
                .text_color(theme.foreground)
                .line_height(rems(t.text_line_height))
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(d.list_marker_gap))
                .children([
                    div()
                        .min_w(px(d.list_marker_width))
                        .h(px(t.text_size * t.text_line_height))
                        .flex()
                        .items_center()
                        .child(bulleted_list_marker(self.render_depth, &theme, t.text_size)),
                    if showing_rendered_image || self.showing_image_source_with_render() {
                        self.render_list_item_image(
                            focused,
                            is_placeholder,
                            &theme,
                            &i18n,
                            window,
                            cx,
                        )
                    } else {
                        div().min_w(px(0.0)).flex_grow(1.).child(
                            self.render_text_or_mixed_inline_visuals(
                                &theme,
                                focused,
                                is_placeholder,
                                None,
                                None,
                                theme.foreground,
                                t.text_size,
                                FontWeight::NORMAL,
                                cx,
                            ),
                        )
                    },
                ])
                .into_any_element(),
            BlockKind::TaskListItem { checked } => {
                let marker_width = d.list_marker_width.max(d.task_checkbox_size);
                let first_line_height = t.text_size * t.text_line_height;
                focused_base
                    .text_size(px(t.text_size))
                    .text_color(theme.foreground)
                    .line_height(rems(t.text_line_height))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(d.list_marker_gap))
                    .children([
                        div()
                            .min_w(px(marker_width))
                            .h(px(first_line_height))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .size(px(d.task_checkbox_size))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(d.task_checkbox_radius))
                                    .border(px(d.task_checkbox_border_width))
                                    .border_color(theme.border)
                                    .bg(if checked { theme.primary } else { theme.input })
                                    .text_size(px(d.task_checkbox_check_size))
                                    .text_color(theme.primary_foreground)
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::on_task_checkbox_mouse_down),
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::on_task_checkbox_mouse_up),
                                    )
                                    .child(if checked {
                                        SharedString::new(TASK_CHECK)
                                    } else {
                                        SharedString::new("")
                                    }),
                            ),
                        if showing_rendered_image || self.showing_image_source_with_render() {
                            self.render_list_item_image(
                                focused,
                                is_placeholder,
                                &theme,
                                &i18n,
                                window,
                                cx,
                            )
                        } else {
                            div().min_w(px(0.0)).flex_grow(1.).child(
                                self.render_text_or_mixed_inline_visuals(
                                    &theme,
                                    focused,
                                    is_placeholder,
                                    None,
                                    None,
                                    theme.foreground,
                                    t.text_size,
                                    FontWeight::NORMAL,
                                    cx,
                                ),
                            )
                        },
                    ])
                    .into_any_element()
            }
            BlockKind::NumberedListItem => focused_base
                .text_size(px(t.text_size))
                .text_color(theme.foreground)
                .line_height(rems(t.text_line_height))
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(d.list_marker_gap))
                .children([
                    div()
                        .min_w(px(d.ordered_list_marker_width))
                        .child(SharedString::from(numbered_list_marker(
                            self.render_depth,
                            self.list_ordinal.unwrap_or(1),
                        ))),
                    if showing_rendered_image || self.showing_image_source_with_render() {
                        self.render_list_item_image(
                            focused,
                            is_placeholder,
                            &theme,
                            &i18n,
                            window,
                            cx,
                        )
                    } else {
                        div().min_w(px(0.0)).flex_grow(1.).child(
                            self.render_text_or_mixed_inline_visuals(
                                &theme,
                                focused,
                                is_placeholder,
                                None,
                                None,
                                theme.foreground,
                                t.text_size,
                                FontWeight::NORMAL,
                                cx,
                            ),
                        )
                    },
                ])
                .into_any_element(),
            BlockKind::Quote => focused_base
                .text_size(px(t.text_size))
                .text_color(theme.muted_foreground)
                .line_height(rems(t.text_line_height))
                .child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.muted_foreground,
                    t.text_size,
                    FontWeight::NORMAL,
                    cx,
                ))
                .into_any_element(),
            BlockKind::Callout(variant) => {
                let (accent, _) = super::callout_accent_and_background(variant, &theme);
                let title_is_empty = self.record.title.plain_text().is_empty();
                let show_static_default_label = title_is_empty && !focused;
                let header_label = SharedString::from(variant.label());
                let header_text = if show_static_default_label {
                    div()
                        .text_size(px(t.text_size))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child(header_label.clone())
                        .into_any_element()
                } else {
                    div()
                        .min_w(px(0.0))
                        .flex_grow(1.)
                        .text_size(px(t.text_size))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent)
                        .child(self.render_text_or_mixed_inline_visuals(
                            &theme,
                            focused,
                            is_placeholder,
                            Some(header_label),
                            Some(accent),
                            accent,
                            t.text_size,
                            FontWeight::SEMIBOLD,
                            cx,
                        ))
                        .into_any_element()
                };

                focused_base
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(d.callout_header_gap))
                    .child(
                        div()
                            .text_size(px(t.text_size))
                            .font_weight(FontWeight::BOLD)
                            .text_color(accent)
                            .child(variant.icon()),
                    )
                    .child(header_text)
                    .into_any_element()
            }
            BlockKind::FootnoteDefinition => {
                let ordinal = self.footnote_definition_ordinal();
                let badge = ordinal
                    .map(|ordinal| ordinal.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let badge_text_size = px((t.code_size - 1.0).max(10.0));
                let header = focused_base
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(d.list_marker_gap))
                    .text_size(px(t.code_size))
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .px(px(d.footnote_badge_padding_x))
                            .py(px(d.footnote_badge_padding_y))
                            .rounded(px(999.0))
                            .bg(theme.primary)
                            .text_size(badge_text_size)
                            .text_color(theme.primary_foreground)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(badge)),
                    )
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex_grow(1.)
                            .text_color(theme.muted_foreground)
                            .child(self.render_text_or_mixed_inline_visuals(
                                &theme,
                                focused,
                                is_placeholder,
                                None,
                                None,
                                theme.muted_foreground,
                                t.code_size,
                                FontWeight::NORMAL,
                                cx,
                            )),
                    );

                if self.footnote_definition_has_backref() {
                    header
                        .child(
                            div()
                                .text_color(theme.link)
                                .hover(|this| this.text_color(theme.link))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::on_footnote_backref_mouse_down),
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::on_footnote_backref_mouse_up),
                                )
                                .child("\u{21A9}"),
                        )
                        .into_any_element()
                } else {
                    header.into_any_element()
                }
            }
            BlockKind::CodeBlock { .. } => {
                let show_language_input = focused || code_language_focused;
                let language_placeholder =
                    SharedString::from(i18n.t("code_language_placeholder", &[]));
                let code_panel = focused_base
                    .bg(theme.muted)
                    .rounded_sm()
                    .pl(px(d.code_block_padding_x))
                    .pr(px(d.code_block_padding_x))
                    .py(px(d.code_block_padding_y))
                    .font(crate::theme::code_text_font())
                    .text_size(px(t.code_size))
                    .text_color(theme.foreground)
                    .line_height(rems(t.text_line_height))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .w_full()
                            .child(TextBlockElement::new(cx.entity(), is_placeholder)),
                    );

                if show_language_input {
                    let input_height = d.code_language_input_height
                        + d.code_language_input_padding_y * 2.0
                        + d.code_language_input_border_width * 2.0;
                    div()
                        .w_full()
                        .relative()
                        .pb(px(input_height + d.code_language_input_gap))
                        .child(code_panel)
                        .child(
                            div()
                                .absolute()
                                .right(px(d.code_language_input_gap))
                                .bottom(px(0.0))
                                .occlude()
                                .key_context(EDITOR_KEY_CONTEXT)
                                .track_focus(&self.code_language_focus_handle)
                                .on_action(cx.listener(Self::on_code_language_newline))
                                .on_action(cx.listener(Self::on_code_language_dismiss))
                                .on_action(cx.listener(Self::on_code_language_delete_back))
                                .on_action(cx.listener(Self::on_code_language_delete))
                                .on_action(cx.listener(Self::on_code_language_focus_content))
                                .on_action(cx.listener(Self::on_code_language_focus_next))
                                .on_action(cx.listener(Self::on_code_language_move_left))
                                .on_action(cx.listener(Self::on_code_language_move_right))
                                .on_action(cx.listener(Self::on_code_language_home))
                                .on_action(cx.listener(Self::on_code_language_end))
                                .on_action(cx.listener(Self::on_code_language_select_left))
                                .on_action(cx.listener(Self::on_code_language_select_right))
                                .on_action(cx.listener(Self::on_code_language_select_all))
                                .on_action(cx.listener(Self::on_code_language_copy))
                                .on_action(cx.listener(Self::on_code_language_cut))
                                .on_action(cx.listener(Self::on_code_language_paste))
                                .on_action(cx.listener(Self::on_code_language_indent))
                                .on_action(cx.listener(Self::on_code_language_outdent))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::on_code_language_mouse_down),
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::on_code_language_mouse_up),
                                )
                                .on_mouse_up_out(
                                    MouseButton::Left,
                                    cx.listener(Self::on_code_language_mouse_up_out),
                                )
                                .on_mouse_move(cx.listener(Self::on_code_language_mouse_move))
                                .w(px(d.code_language_input_width))
                                .px(px(d.code_language_input_padding_x))
                                .py(px(d.code_language_input_padding_y))
                                .rounded(px(d.code_language_input_radius))
                                .border(px(d.code_language_input_border_width))
                                .border_color(theme.border)
                                .bg(theme.input)
                                .text_size(px((t.code_size - 1.0).max(10.0)))
                                .text_color(theme.foreground)
                                .cursor(CursorStyle::IBeam)
                                .child(CodeLanguageBarElement::new(
                                    cx.entity(),
                                    language_placeholder,
                                )),
                        )
                        .into_any_element()
                } else {
                    code_panel.into_any_element()
                }
            }
            BlockKind::Table => {
                let Some(runtime) = self.table_runtime.clone() else {
                    return focused_base
                        .text_size(px(t.text_size))
                        .text_color(theme.foreground)
                        .line_height(rems(t.text_line_height))
                        .child(self.render_text_or_mixed_inline_visuals(
                            &theme,
                            focused,
                            is_placeholder,
                            None,
                            None,
                            theme.foreground,
                            t.text_size,
                            FontWeight::NORMAL,
                            cx,
                        ))
                        .into_any_element();
                };

                let viewport_width = f32::from(window.viewport_size().width.max(px(1.0)));
                let table_width = effective_table_width(self, viewport_width, d);
                let column_layout = self
                    .record
                    .table
                    .as_ref()
                    .map(|table| ColumnLayout::measure(table, table_width, window, &theme))
                    .unwrap_or_else(|| ColumnLayout::equal(runtime.header.len()));
                let preview_marker = self.table_axis_preview;
                let selected_marker = self.table_axis_selection;
                let body_row_count = runtime.rows.len();
                let append_extent = px(d.table_append_button_extent);
                let append_inset = px(d.table_append_button_inset);
                let activation_band = px(d.table_append_activation_band);
                let top_gutter = if column_axis_gutter_visible(preview_marker, selected_marker) {
                    activation_band
                } else {
                    px(0.0)
                };
                let column_append_top = top_gutter + activation_band;
                let column_control_visible = self.table_append_column_hovered;
                let row_control_visible = self.table_append_row_hovered;
                let right_gutter = if column_control_visible {
                    append_extent + append_inset
                } else {
                    px(0.0)
                };
                let bottom_gutter = if row_control_visible {
                    append_extent + append_inset
                } else {
                    px(0.0)
                };
                let weak_table_block = cx.entity().downgrade();

                let header_cells = runtime.header;
                let column_axis_row = (top_gutter > px(0.0)).then(|| {
                    div().w_full().h(top_gutter).flex().gap(px(0.0)).children(
                        header_cells.iter().enumerate().map(|(column, _cell)| {
                            let hover_block = weak_table_block.clone();
                            let select_block = weak_table_block.clone();
                            let menu_block = weak_table_block.clone();
                            let marker = crate::components::AxisMarker {
                                kind: TableAxisKind::Column,
                                index: column,
                            };
                            let band_bg = if selected_marker == Some(marker) {
                                theme.table_active
                            } else if preview_marker == Some(marker) {
                                theme.table_hover
                            } else {
                                hsla(0.0, 0.0, 0.0, 0.0)
                            };
                            div()
                                .relative()
                                .flex_none()
                                .flex_basis(relative(column_layout.fraction(column)))
                                .w(relative(column_layout.fraction(column)))
                                .h_full()
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .id(ElementId::Name(
                                            format!(
                                                "table-column-axis-band-{}-{}",
                                                self.record.id, column
                                            )
                                            .into(),
                                        ))
                                        .w_full()
                                        .h_full()
                                        .rounded(px(6.0))
                                        .bg(band_bg)
                                        .cursor_pointer()
                                        .on_hover(move |hovered, _window, cx| {
                                            let _ = hover_block.update(cx, |_block, cx| {
                                                cx.emit(BlockEvent::RequestTableAxisPreview {
                                                    kind: TableAxisKind::Column,
                                                    index: column,
                                                    hovered: *hovered,
                                                });
                                            });
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                let _ = select_block.update(cx, |_block, cx| {
                                                    cx.stop_propagation();
                                                    cx.emit(BlockEvent::RequestSelectTableAxis {
                                                        kind: TableAxisKind::Column,
                                                        index: column,
                                                    });
                                                });
                                            },
                                        )
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            move |event, window, cx| {
                                                // 右键选中该轴（旧链路行为保留），菜单由块自持的框架弹出菜单宿主打开。
                                                if let Some(block) = menu_block.upgrade() {
                                                    block.update(cx, |_block, cx| {
                                                        cx.stop_propagation();
                                                        cx.emit(
                                                            BlockEvent::RequestSelectTableAxis {
                                                                kind: TableAxisKind::Column,
                                                                index: column,
                                                            },
                                                        );
                                                    });
                                                    open_table_axis_menu(
                                                        &block,
                                                        TableAxisKind::Column,
                                                        column,
                                                        event.position,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        )
                                        .block_mouse_except_scroll(),
                                )
                        }),
                    )
                });

                let header_hover_block = weak_table_block.clone();
                let header_select_block = weak_table_block.clone();
                let header_menu_block = weak_table_block.clone();
                // 表头为可视行 0，其手柄用更不透明的正文行颜色以示区别。
                let header_marker = crate::components::AxisMarker {
                    kind: TableAxisKind::Row,
                    index: 0,
                };
                let header_band_bg = if selected_marker == Some(header_marker) {
                    header_axis_emphasis(theme.table_active)
                } else if preview_marker == Some(header_marker) {
                    header_axis_emphasis(theme.table_hover)
                } else {
                    hsla(0.0, 0.0, 0.0, 0.0)
                };
                let header_row = div()
                    .relative()
                    .w_full()
                    .flex()
                    .gap(px(0.0))
                    .child(
                        // 左缘条与正文行一致，使表头行同样可悬停、选中与右键，并在其菜单追加表头行开关。
                        div()
                            .id(ElementId::Name(
                                format!("table-header-axis-band-{}", self.record.id).into(),
                            ))
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(-activation_band)
                            .w(activation_band)
                            .rounded(px(6.0))
                            .bg(header_band_bg)
                            .cursor_pointer()
                            .on_hover(move |hovered, _window, cx| {
                                let _ = header_hover_block.update(cx, |_block, cx| {
                                    cx.emit(BlockEvent::RequestTableAxisPreview {
                                        kind: TableAxisKind::Row,
                                        index: 0,
                                        hovered: *hovered,
                                    });
                                });
                            })
                            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                let _ = header_select_block.update(cx, |_block, cx| {
                                    cx.stop_propagation();
                                    cx.emit(BlockEvent::RequestSelectTableAxis {
                                        kind: TableAxisKind::Row,
                                        index: 0,
                                    });
                                });
                            })
                            .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                                // 右键选中该轴（旧链路行为保留），菜单由块自持的框架弹出菜单宿主打开。
                                if let Some(block) = header_menu_block.upgrade() {
                                    block.update(cx, |_block, cx| {
                                        cx.stop_propagation();
                                        cx.emit(BlockEvent::RequestSelectTableAxis {
                                            kind: TableAxisKind::Row,
                                            index: 0,
                                        });
                                    });
                                    open_table_axis_menu(
                                        &block,
                                        TableAxisKind::Row,
                                        0,
                                        event.position,
                                        window,
                                        cx,
                                    );
                                }
                            })
                            .block_mouse_except_scroll(),
                    )
                    .children(header_cells.into_iter().enumerate().map(|(column, cell)| {
                        let hover_block = weak_table_block.clone();
                        let select_block = weak_table_block.clone();
                        let menu_block = weak_table_block.clone();
                        div()
                            .relative()
                            .flex_none()
                            .flex_basis(relative(column_layout.fraction(column)))
                            .w(relative(column_layout.fraction(column)))
                            .h_full()
                            .min_w(px(0.0))
                            .child(
                                div()
                                    .id(ElementId::Name(
                                        format!(
                                            "table-column-axis-activation-{}-{}",
                                            self.record.id, column
                                        )
                                        .into(),
                                    ))
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .right_0()
                                    .h(activation_band)
                                    .cursor_pointer()
                                    .on_hover(move |hovered, _window, cx| {
                                        let _ = hover_block.update(cx, |_block, cx| {
                                            cx.emit(BlockEvent::RequestTableAxisPreview {
                                                kind: TableAxisKind::Column,
                                                index: column,
                                                hovered: *hovered,
                                            });
                                        });
                                    })
                                    .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                                        let _ = select_block.update(cx, |_block, cx| {
                                            cx.stop_propagation();
                                            cx.emit(BlockEvent::RequestSelectTableAxis {
                                                kind: TableAxisKind::Column,
                                                index: column,
                                            });
                                        });
                                    })
                                    .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                                        // 右键选中该轴（旧链路行为保留），菜单由块自持的框架弹出菜单宿主打开。
                                        if let Some(block) = menu_block.upgrade() {
                                            block.update(cx, |_block, cx| {
                                                cx.stop_propagation();
                                                cx.emit(BlockEvent::RequestSelectTableAxis {
                                                    kind: TableAxisKind::Column,
                                                    index: column,
                                                });
                                            });
                                            open_table_axis_menu(
                                                &block,
                                                TableAxisKind::Column,
                                                column,
                                                event.position,
                                                window,
                                                cx,
                                            );
                                        }
                                    })
                                    .block_mouse_except_scroll(),
                            )
                            .child(cell)
                    }));

                let body_rows =
                    runtime
                        .rows
                        .into_iter()
                        .enumerate()
                        .map(|(body_row_index, row)| {
                            let hover_block = weak_table_block.clone();
                            let select_block = weak_table_block.clone();
                            let menu_block = weak_table_block.clone();
                            // 行选区按可视索引寻址，表头为 `0`，正文行依次为 `1..`。
                            let visual_row = body_row_index + 1;
                            let marker = crate::components::AxisMarker {
                                kind: TableAxisKind::Row,
                                index: visual_row,
                            };
                            let band_bg = if selected_marker == Some(marker) {
                                theme.table_active
                            } else if preview_marker == Some(marker) {
                                theme.table_hover
                            } else {
                                hsla(0.0, 0.0, 0.0, 0.0)
                            };
                            div()
                                .relative()
                                .w_full()
                                .flex()
                                .gap(px(0.0))
                                .child(
                                    div()
                                        .id(ElementId::Name(
                                            format!(
                                                "table-row-axis-band-{}-{}",
                                                self.record.id, body_row_index
                                            )
                                            .into(),
                                        ))
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left(-activation_band)
                                        .w(activation_band)
                                        .rounded(px(6.0))
                                        .bg(band_bg)
                                        .cursor_pointer()
                                        .on_hover(move |hovered, _window, cx| {
                                            let _ = hover_block.update(cx, |_block, cx| {
                                                cx.emit(BlockEvent::RequestTableAxisPreview {
                                                    kind: TableAxisKind::Row,
                                                    index: visual_row,
                                                    hovered: *hovered,
                                                });
                                            });
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |_event, _window, cx| {
                                                let _ = select_block.update(cx, |_block, cx| {
                                                    cx.stop_propagation();
                                                    cx.emit(BlockEvent::RequestSelectTableAxis {
                                                        kind: TableAxisKind::Row,
                                                        index: visual_row,
                                                    });
                                                });
                                            },
                                        )
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            move |event, window, cx| {
                                                // 右键选中该轴（旧链路行为保留），菜单由块自持的框架弹出菜单宿主打开。
                                                if let Some(block) = menu_block.upgrade() {
                                                    block.update(cx, |_block, cx| {
                                                        cx.stop_propagation();
                                                        cx.emit(
                                                            BlockEvent::RequestSelectTableAxis {
                                                                kind: TableAxisKind::Row,
                                                                index: visual_row,
                                                            },
                                                        );
                                                    });
                                                    open_table_axis_menu(
                                                        &block,
                                                        TableAxisKind::Row,
                                                        visual_row,
                                                        event.position,
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            },
                                        )
                                        .block_mouse_except_scroll(),
                                )
                                .children(row.into_iter().enumerate().map(|(column, cell)| {
                                    div()
                                        .flex_none()
                                        .flex_basis(relative(column_layout.fraction(column)))
                                        .w(relative(column_layout.fraction(column)))
                                        .h_full()
                                        .min_w(px(0.0))
                                        .child(cell)
                                }))
                        });

                {
                    let mut rows = Vec::with_capacity(2 + body_row_count);
                    if let Some(column_axis_row) = column_axis_row {
                        rows.push(column_axis_row.into_any_element());
                    }
                    rows.push(header_row.into_any_element());
                    rows.extend(body_rows.map(|row| row.into_any_element()));

                    let column_edge_band = div()
                        .id(ElementId::Name(
                            format!("table-append-column-edge-{}", self.record.id).into(),
                        ))
                        .absolute()
                        .top(column_append_top)
                        .bottom(bottom_gutter)
                        .right(right_gutter)
                        .w(activation_band)
                        .on_hover(cx.listener(Self::on_table_append_column_edge_hover));

                    let row_edge_band = div()
                        .id(ElementId::Name(
                            format!("table-append-row-edge-{}", self.record.id).into(),
                        ))
                        .absolute()
                        .left_0()
                        .right(right_gutter)
                        .bottom(bottom_gutter)
                        .h(activation_band)
                        .on_hover(cx.listener(Self::on_table_append_row_edge_hover));

                    let column_control = {
                        let base = div()
                            .id(ElementId::Name(
                                format!("table-append-column-zone-{}", self.record.id).into(),
                            ))
                            .absolute()
                            .top(column_append_top)
                            .bottom(bottom_gutter)
                            .right_0()
                            .w(right_gutter)
                            .on_hover(cx.listener(Self::on_table_append_column_zone_hover));

                        if column_control_visible {
                            base.child(
                                div()
                                    .id(ElementId::Name(
                                        format!("table-append-column-button-{}", self.record.id)
                                            .into(),
                                    ))
                                    .absolute()
                                    .top(append_inset)
                                    .bottom_0()
                                    .right_0()
                                    .w(append_extent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(999.0))
                                    .bg(theme.button_secondary)
                                    .hover(|this| this.bg(theme.button_secondary_hover))
                                    .cursor_pointer()
                                    .text_size(px(t.text_size))
                                    .text_color(theme.button_secondary_foreground)
                                    .block_mouse_except_scroll()
                                    .on_hover(
                                        cx.listener(Self::on_table_append_column_button_hover),
                                    )
                                    .on_click(cx.listener(Self::on_append_table_column))
                                    .child("+"),
                            )
                        } else {
                            base
                        }
                    };

                    let row_control = {
                        let base = div()
                            .id(ElementId::Name(
                                format!("table-append-row-zone-{}", self.record.id).into(),
                            ))
                            .absolute()
                            .left_0()
                            .right(right_gutter)
                            .bottom_0()
                            .h(bottom_gutter)
                            .on_hover(cx.listener(Self::on_table_append_row_zone_hover));

                        if row_control_visible {
                            base.child(
                                div()
                                    .id(ElementId::Name(
                                        format!("table-append-row-button-{}", self.record.id)
                                            .into(),
                                    ))
                                    .absolute()
                                    .left(append_inset)
                                    .right(append_inset)
                                    .bottom_0()
                                    .h(append_extent)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(999.0))
                                    .bg(theme.button_secondary)
                                    .hover(|this| this.bg(theme.button_secondary_hover))
                                    .cursor_pointer()
                                    .text_size(px(t.text_size))
                                    .text_color(theme.button_secondary_foreground)
                                    .block_mouse_except_scroll()
                                    .on_hover(cx.listener(Self::on_table_append_row_button_hover))
                                    .on_click(cx.listener(Self::on_append_table_row))
                                    .child("+"),
                            )
                        } else {
                            base
                        }
                    };

                    let mut table_root = div()
                        .id(block_id)
                        .w_full()
                        .relative()
                        .flex()
                        .flex_col()
                        .pr(right_gutter)
                        .pb(bottom_gutter)
                        .gap(px(0.0))
                        .children(rows)
                        .child(column_edge_band)
                        .child(row_edge_band)
                        .child(column_control)
                        .child(row_control);
                    // 表格整体被跨块选中时给出底色反馈。
                    if whole_block_selected {
                        table_root = table_root.bg(theme.selection).rounded(px(4.0));
                    }
                    // 表格没有文本元素，自身不会记录块边界；这里补一次，
                    // 好让跨块选区按点命中能把表格当成整块（点表格框即可选中）。
                    {
                        let entity = cx.entity();
                        table_root = table_root.on_prepaint(move |bounds, _window, cx| {
                            entity.update(cx, |block, _| block.last_bounds = Some(bounds));
                        });
                    }
                    // 表格轴右键菜单宿主（框架弹出菜单，无菜单时为空）。
                    if let Some(axis_menu) = render_table_axis_menu(self, window, cx) {
                        table_root = table_root.child(axis_menu);
                    }
                    table_root.into_any_element()
                }
            }
            BlockKind::HtmlBlock => {
                if !focused {
                    self.cursor_blink_task = None;
                    self.last_layout = None;
                    self.last_bounds = None;
                } else if self.cursor_blink_task.is_none() {
                    self.start_cursor_blink(cx);
                }
                let html = self.record.html.as_ref().cloned().unwrap_or_else(|| {
                    crate::components::parse_html_document(
                        self.record
                            .raw_fallback
                            .as_deref()
                            .unwrap_or_else(|| self.rendered_text()),
                    )
                });
                let rendered = self.render_html_document(&html, &theme, cx);
                // 聚焦时源码行叠加在预览上方，预览保持可见、可直接编辑源码（对齐图片/公式/图表）。
                let child = if focused {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(d.block_padding_y))
                        .child(TextBlockElement::new(cx.entity(), is_placeholder))
                        .child(rendered)
                        .into_any_element()
                } else {
                    rendered.into_any_element()
                };
                // 未聚焦时没有文本元素写入 `last_bounds`，这里补整块边界，供跨块选区按点命中。
                let entity = cx.entity();
                focused_base
                    .text_size(px(t.text_size))
                    .text_color(theme.foreground)
                    .line_height(rems(t.text_line_height))
                    .w_full()
                    .child(child)
                    .on_prepaint(move |bounds, _window, cx| {
                        entity.update(cx, |block, _| {
                            if block.last_layout.is_none() {
                                block.last_bounds = Some(bounds);
                            }
                        });
                    })
                    .into_any_element()
            }
            BlockKind::MathBlock => {
                if !focused {
                    self.cursor_blink_task = None;
                    self.last_layout = None;
                    self.last_bounds = None;
                } else if self.cursor_blink_task.is_none() {
                    self.start_cursor_blink(cx);
                }
                let rendered = self.render_math_content(&theme);
                // 聚焦时源码行叠加在渲染公式上方，公式保持可见（对齐 Typora）。
                let child = if focused {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(d.block_padding_y))
                        .child(TextBlockElement::new(cx.entity(), is_placeholder))
                        .child(rendered)
                        .into_any_element()
                } else {
                    rendered
                };
                focused_base.w_full().child(child).into_any_element()
            }
            BlockKind::MermaidBlock => {
                if !focused {
                    self.cursor_blink_task = None;
                    self.last_layout = None;
                    self.last_bounds = None;
                } else if self.cursor_blink_task.is_none() {
                    self.start_cursor_blink(cx);
                }
                let rendered = self.render_mermaid_content(&theme, &i18n, window);
                // 聚焦时源码行叠加在渲染图上方，图保持可见（对齐 Typora）。
                let child = if focused {
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(d.block_padding_y))
                        .child(TextBlockElement::new(cx.entity(), is_placeholder))
                        .child(rendered)
                        .into_any_element()
                } else {
                    rendered
                };
                focused_base.w_full().child(child).into_any_element()
            }
            BlockKind::Paragraph
            | BlockKind::Comment
            | BlockKind::RawMarkdown
            | BlockKind::Heading { .. } => {
                let base = focused_base
                    .text_size(px(t.text_size))
                    .text_color(theme.foreground)
                    .line_height(rems(t.text_line_height));
                // 相邻列表组之间的空段落只承担源码里的空行，渲染时折叠为零高，
                // 对齐 Typora：列表间不显示多余空行。聚焦时恢复常规高度以显示光标。
                let base = if self.collapses_as_list_group_separator(focused) {
                    base.h(px(0.0)).min_h(px(0.0)).py(px(0.0)).overflow_hidden()
                } else {
                    base
                };
                base.child(self.render_text_or_mixed_inline_visuals(
                    &theme,
                    focused,
                    is_placeholder,
                    None,
                    None,
                    theme.foreground,
                    t.text_size,
                    FontWeight::NORMAL,
                    cx,
                ))
                .into_any_element()
            }
        };

        wrap_with_quote_guides(content, visible_quote_guides(self), &theme)
    }
}

/// 把带样式行内文本游程切为易换行的分块，供混合行内视觉布局使用。自带盒子的游程（行内代码、背景高亮）保持整块以使内边距/背景连续；其余按空白切分且每词保留尾空格，使 `flex_wrap` 行可在词间断行，而不把下一个行内视觉挤到独占一行。
/// 包裹渲染后的行内链接游程，使手形光标仅在按住 Cmd/Ctrl 跟随修饰键时出现。混合行内视觉块（公式、上下标、行内图片）中的链接渲染为普通层，故悬停且修饰键按下时置 `PointingHand`，与 `TextBlockElement` 处理普通文本一致。编辑器根在跟随修饰键切换时重绘，故指针静止时光标也会重算。布局与绘制委托给子元素。
struct LinkCursor {
    child: AnyElement,
}

impl IntoElement for LinkCursor {
    type Element = Self;

    /// 转为元素。
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LinkCursor {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    /// 取元素标识。
    fn id(&self) -> Option<ElementId> {
        None
    }

    /// 取源码位置。
    /// 取源码位置。
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    /// 请求布局。
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    /// 预绘制。
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    /// 绘制。
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if hitbox.is_hovered(window) && window.modifiers().secondary() {
            // 编辑器根在跟随修饰键切换时重绘，故指针静止时手形光标在此重算。
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        self.child.paint(window, cx);
    }
}

/// 按 CommonMark 的 HTML 块规则拆分文本节点：遇到首个空行即结束 HTML 块，
/// 其后的内容按 Markdown 解析（GitHub 同此，例如 `<details>` 折叠区正文）。
/// 返回 `(HTML 块内原文, 空行之后的 Markdown)`；无空行时返回 `None`，整段仍是 HTML 块内文本。
fn split_html_text_at_blank_line(raw: &str) -> Option<(&str, &str)> {
    let bytes = raw.as_bytes();
    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            if raw[line_start..index].trim().is_empty() {
                return Some((&raw[..line_start], &raw[index + 1..]));
            }
            line_start = index + 1;
        }
        index += 1;
    }
    None
}

/// 切分行内词块以供换行。
fn inline_word_chunks(text: &str, code: bool, has_background: bool) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    if code || has_background {
        return vec![text];
    }
    text.split_inclusive(char::is_whitespace).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        HtmlStyle, column_axis_gutter_visible, html_is_whitespace_text, html_node_is_block_level,
        html_node_visual_style, inline_word_chunks, split_html_text_at_blank_line,
    };
    use crate::components::{AxisMarker, TableAxisKind};
    use crate::components::{Block, BlockKind, BlockRecord, InlineTextTree, parse_html_document};
    use crate::i18n::I18nManager;
    use rgpui::{Hsla, Rgba, TestAppContext, px};

    #[test]
    fn top_gutter_only_appears_for_column_axis_state() {
        assert!(!column_axis_gutter_visible(
            Some(AxisMarker {
                kind: TableAxisKind::Row,
                index: 0,
            }),
            None,
        ));
        assert!(!column_axis_gutter_visible(None, None));
        assert!(column_axis_gutter_visible(
            Some(AxisMarker {
                kind: TableAxisKind::Column,
                index: 0,
            }),
            None,
        ));
        assert!(column_axis_gutter_visible(
            None,
            Some(AxisMarker {
                kind: TableAxisKind::Column,
                index: 0,
            }),
        ));
    }

    /// 断言颜色接近。
    fn assert_color_near(color: Hsla, red: u8, green: u8, blue: u8, alpha: u8) {
        let color = Rgba::from(color);
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as i16;
        assert!((channel(color.r) - red as i16).abs() <= 1);
        assert!((channel(color.g) - green as i16).abs() <= 1);
        assert!((channel(color.b) - blue as i16).abs() <= 1);
        assert!((channel(color.a) - alpha as i16).abs() <= 1);
    }

    #[test]
    fn html_flow_treats_br_and_img_as_inline() {
        // 行内标签、文本节点以及 br/img 都归入行内流；嵌套块级标签各自成块。
        let doc = parse_html_document("<p>a<br>b</p>");
        let paragraph = &doc.nodes[0];
        assert!(html_node_is_block_level(paragraph));
        assert!(
            paragraph
                .children
                .iter()
                .all(|child| !html_node_is_block_level(child))
        );

        let nested = parse_html_document("<div><p>x</p></div>");
        assert!(html_node_is_block_level(&nested.nodes[0]));
        assert!(html_node_is_block_level(&nested.nodes[0].children[0]));
    }

    #[test]
    fn html_table_structural_children_drop_whitespace() {
        // 表格结构容器只保留表格元素，源码缩进/换行属结构性空白。
        let doc = parse_html_document(
            "<table>\n  <thead>\n    <tr><th>a</th></tr>\n  </thead>\n  <tbody></tbody>\n</table>",
        );
        let table = &doc.nodes[0];
        let significant: Vec<&str> = table
            .children
            .iter()
            .filter(|child| !html_is_whitespace_text(child))
            .map(|child| child.tag_name.as_str())
            .collect();
        assert_eq!(significant, vec!["thead", "tbody"]);
        assert!(
            table.children.iter().any(html_is_whitespace_text),
            "样例源码应含结构性空白，否则本测试失去意义"
        );
    }

    #[test]
    fn html_text_after_blank_line_splits_for_markdown() {
        // 空行结束 HTML 块：之前是块内原文，之后按 Markdown 解析。
        assert_eq!(
            split_html_text_at_blank_line("\n\n隐藏内容，含 `code`。\n\n"),
            Some(("", "\n隐藏内容，含 `code`。\n\n")),
        );
        assert_eq!(split_html_text_at_blank_line("a\n\nb"), Some(("a\n", "b")));
        // 无空行：整段仍属 HTML 块内文本，保持原样。
        assert_eq!(split_html_text_at_blank_line("foo\nbar"), None);
        assert_eq!(split_html_text_at_blank_line(""), None);
    }

    #[test]
    fn inline_word_chunks_split_text_runs_for_wrapping() {
        // 纯文本逐词切分，使弹性换行行可在词间断行，相邻行内公式保持在同一视觉行。
        assert_eq!(
            inline_word_chunks("Fusce x malesuada", false, false),
            vec!["Fusce ", "x ", "malesuada"],
        );
        // 尾空白随附保留，使切分后间距不丢失。
        assert_eq!(inline_word_chunks("end ", false, false), vec!["end "]);
        assert!(inline_word_chunks("", false, false).is_empty());
    }

    #[test]
    fn inline_word_chunks_keep_boxed_runs_whole() {
        // 行内代码与背景高亮保持盒子连续。
        assert_eq!(
            inline_word_chunks("let x = 2", true, false),
            vec!["let x = 2"],
        );
        assert_eq!(
            inline_word_chunks("highlighted text", false, true),
            vec!["highlighted text"],
        );
    }

    #[test]
    fn html_render_style_inherits_color_and_font_size() {
        let theme = rgpui::Theme::default();
        let doc = parse_html_document(
            "<div style=\"color:blue; font-size:20px\"><span style=\"font-size:120%\">x</span></div>",
        );
        let root = HtmlStyle::root(&theme);
        let parent = html_node_visual_style(&doc.nodes[0], root, &theme);
        let child = html_node_visual_style(&doc.nodes[0].children[0], parent.computed, &theme);

        assert_color_near(parent.computed.color, 0, 0, 255, 255);
        assert_color_near(child.computed.color, 0, 0, 255, 255);
        assert!((child.computed.font_size - 24.0).abs() < 0.01);
    }

    #[test]
    fn html_render_style_overrides_link_and_mark_defaults() {
        let theme = rgpui::Theme::default();
        let link_doc = parse_html_document("<a style=\"color:red\">x</a>");
        let link_style =
            html_node_visual_style(&link_doc.nodes[0], HtmlStyle::root(&theme), &theme);
        assert_color_near(link_style.computed.color, 255, 0, 0, 255);

        let mark_doc = parse_html_document("<mark style=\"background-color:#123\">x</mark>");
        let mark_style =
            html_node_visual_style(&mark_doc.nodes[0], HtmlStyle::root(&theme), &theme);
        assert_color_near(mark_style.background.unwrap(), 0x11, 0x22, 0x33, 0xff);
    }

    #[test]
    fn html_render_style_does_not_inherit_background_color() {
        let theme = rgpui::Theme::default();
        let doc =
            parse_html_document("<div style=\"background-color:#112233\"><span>child</span></div>");
        let root = HtmlStyle::root(&theme);
        let parent = html_node_visual_style(&doc.nodes[0], root, &theme);
        let child = html_node_visual_style(&doc.nodes[0].children[0], parent.computed, &theme);

        assert_color_near(parent.background.unwrap(), 0x11, 0x22, 0x33, 0xff);
        assert!(child.background.is_none());
    }

    #[rgpui::test]
    async fn code_language_input_docks_to_right_edge(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
        });
        let (block, cx) = cx.add_window_view(|_window, cx| {
            Block::with_record(
                cx,
                BlockRecord::new(
                    BlockKind::CodeBlock {
                        language: Some("rust".into()),
                    },
                    InlineTextTree::plain("fn main() {}\n"),
                ),
            )
        });

        cx.update(|window, cx| {
            block.update(cx, |block, cx| {
                block.focus_handle.focus(window, cx);
            });
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();

        let (text_bounds, language_bounds) = block.read_with(cx, |block, _cx| {
            (
                block.last_bounds.expect("code text should render"),
                block
                    .code_language_last_bounds
                    .expect("language input should render"),
            )
        });
        assert!(language_bounds.left() > text_bounds.left());
        assert!(language_bounds.top() > text_bounds.bottom());
        let right_gap = f32::from(language_bounds.right() - text_bounds.right());
        assert!(
            right_gap.abs() <= 12.0,
            "expected language input to sit near the code block right edge; right_gap={right_gap}, text_bounds={text_bounds:?}, language_bounds={language_bounds:?}"
        );
        assert!(language_bounds.size.width <= px(156.0));
    }
}
