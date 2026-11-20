//! Mermaid 导出的图形生成：HTML 内嵌 SVG，PDF / DOCX 内嵌 SVG 栅格化后的 PNG。
//!
//! 渲染失败（源码不受支持、语法错误、栅格化失败）一律返回 `None`，由调用方
//! 退回等宽源码文本，保证内容不因单张图中断整份导出。

use base64::Engine as _;

/// 把 Mermaid 源码渲染为导出用 SVG（明确像素尺寸）。
pub(crate) fn render_svg(source: &str) -> Option<String> {
    crate::components::mermaid::render_mermaid_svg_for_export(source)
        .ok()
        .filter(|svg| svg.contains("<svg"))
}

/// 把 SVG 编码为可直接放进 `<img src>` 的 data URI。
pub(crate) fn svg_data_uri(svg: &str) -> String {
    format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg.as_bytes())
    )
}

/// 把 SVG 栅格化为 PNG 字节（PDF / DOCX 嵌图用）。
///
/// 字体库按进程缓存一次：usvg 默认不带任何字体，不加载系统字体的话中文标签
/// 会渲染成空白。尺寸设上限，避免异常 SVG 撑爆导出内存。
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
pub(crate) fn svg_to_png_bytes(svg: &str, scale: f32) -> Option<Vec<u8>> {
    use std::sync::{Arc, OnceLock};

    static FONT_DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONT_DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    });

    let options = usvg::Options {
        fontdb: fontdb.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options).ok()?;
    let size = tree.size();
    let scale = scale.max(0.1);
    let width = (size.width() * scale).ceil() as u32;
    let height = (size.height() * scale).ceil() as u32;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::{render_svg, svg_data_uri};

    #[test]
    fn mermaid_source_renders_to_sized_svg() {
        let svg = render_svg("flowchart TD\n  A --> B\n").expect("支持的图应渲染成功");
        assert!(svg.contains("<svg"), "svg: {svg}");
        assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        // 导出用 SVG 必须是明确像素尺寸，不能留 `width="100%"`。
        assert!(!svg.contains("width=\"100%\""), "svg: {svg}");
        assert!(svg.contains("width=\""), "svg: {svg}");
        assert!(svg.contains("height=\""), "svg: {svg}");
    }

    #[test]
    fn unsupported_source_returns_none() {
        assert!(render_svg("这不是图表源码").is_none());
        assert!(render_svg("").is_none());
    }

    #[test]
    fn svg_data_uri_is_base64_encoded() {
        let uri = svg_data_uri("<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
        assert!(uri.starts_with("data:image/svg+xml;base64,"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(uri.strip_prefix("data:image/svg+xml;base64,").unwrap())
            .expect("合法 base64");
        assert!(String::from_utf8_lossy(&decoded).contains("<svg"));
    }

    #[cfg(any(feature = "export-pdf", feature = "export-docx"))]
    #[test]
    fn svg_rasterizes_to_png() {
        let svg = render_svg("flowchart TD\n  A --> B\n").expect("渲染成功");
        let png = super::svg_to_png_bytes(&svg, 2.0).expect("栅格化成功");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "应为 PNG");
        let decoded = image::load_from_memory(&png).expect("PNG 可解码");
        assert!(decoded.width() > 100 && decoded.height() > 50, "尺寸异常");
        let rgba = decoded.to_rgba8();
        let background = rgba.get_pixel(0, 0).0;
        assert!(
            rgba.pixels().any(|pixel| pixel.0 != background),
            "图中应有图形内容"
        );
    }
}
