//! 导出嵌图的来源解析与字节加载（HTML/PDF/DOCX 共用）。
//!
//! 图片来源与编辑器渲染一致：`data:` 内嵌 URI、相对/绝对本地路径、
//! 远程 HTTP(S) 地址。加载失败一律返回 `None`，由各格式回退替代文本占位，
//! 不让单张坏图中断整份导出。

use std::path::{Path, PathBuf};

/// 单张图片字节上限（与远程图片加载同限，防止异常文件撑爆内存）。
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// 把图片目标解析为本地文件路径（相对路径按文档目录拼接）。
pub(crate) fn resolve_local_path(url: &str, base_dir: Option<&Path>) -> PathBuf {
    let path = Path::new(url);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    base_dir
        .map(|dir| dir.join(path))
        .unwrap_or_else(|| path.to_path_buf())
}

/// 读取本地图片字节（远程与 `data:` 来源返回 `None`，超限同样拒绝）。
pub(crate) fn load_local_image_bytes(url: &str, base_dir: Option<&Path>) -> Option<Vec<u8>> {
    if url.starts_with("data:") || crate::net::is_remote_image_source(url) {
        return None;
    }
    let path = resolve_local_path(url, base_dir);
    let bytes = std::fs::read(&path).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    Some(bytes)
}

/// 按任意来源加载图片字节（内嵌 URI 解码 / 本地读盘 / 远程同步抓取）。
///
/// 远程抓取是阻塞调用，仅供后台导出线程使用。
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
pub(crate) fn load_image_bytes(url: &str, base_dir: Option<&Path>) -> Option<Vec<u8>> {
    if url.starts_with("data:") {
        let (_, bytes) = crate::components::markdown::image::parse_embedded_data_uri(url)?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return None;
        }
        return Some(bytes);
    }
    if crate::net::is_remote_image_source(url) {
        return crate::net::fetch_image_bytes_blocking(url).ok();
    }
    load_local_image_bytes(url, base_dir)
}

/// 解码图片并在长边超限时等比缩小（控制导出产物体积；失败返回 `None`）。
#[cfg(any(feature = "export-pdf", feature = "export-docx"))]
pub(crate) fn decode_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    /// 解码后长边上限：约 240dpi 打印质量足够，再大只会膨胀产物体积。
    const MAX_DIMENSION: u32 = 2000;

    let decoded = image::load_from_memory(bytes).ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= MAX_DIMENSION {
        return Some(decoded);
    }
    let scale = MAX_DIMENSION as f64 / longest as f64;
    let target_w = ((width as f64 * scale) as u32).max(1);
    let target_h = ((height as f64 * scale) as u32).max(1);
    Some(decoded.resize(target_w, target_h, image::imageops::FilterType::Triangle))
}

#[cfg(test)]
mod tests {
    use super::{load_local_image_bytes, resolve_local_path};

    #[test]
    fn resolve_local_path_joins_relative_and_keeps_absolute() {
        let base = std::path::Path::new("D:/docs");
        assert_eq!(
            resolve_local_path("assets/pic.png", Some(base)),
            std::path::Path::new("D:/docs/assets/pic.png")
        );
        let absolute = resolve_local_path("D:/other/pic.png", Some(base));
        assert_eq!(absolute, std::path::Path::new("D:/other/pic.png"));
    }

    #[test]
    fn local_loader_skips_remote_and_data_sources() {
        assert!(load_local_image_bytes("https://example.com/a.png", None).is_none());
        assert!(load_local_image_bytes("data:image/png;base64,AAAA", None).is_none());
        assert!(load_local_image_bytes("Z:/definitely-missing.png", None).is_none());
    }

    #[test]
    fn local_loader_reads_relative_file_against_base_dir() {
        let root = std::env::temp_dir().join(format!("rumark-export-img-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets")).expect("temp dir");
        std::fs::write(root.join("assets/pic.png"), b"fake-png-bytes").expect("write image");
        let bytes =
            load_local_image_bytes("./assets/pic.png", Some(&root)).expect("should load image");
        assert_eq!(bytes, b"fake-png-bytes");
        let _ = std::fs::remove_dir_all(root);
    }
}
