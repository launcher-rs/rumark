//! 构建脚本：检测 `assets/fonts/` 下用户自备的中文字体。
//!
//! 命中时设置 `rumark_bundled_cjk` cfg 并把首个字体路径经环境变量传给
//! `export::pdf` 做 `include_bytes!` 内嵌；未命中则静默跳过，
//! PDF 导出回退到运行时系统字体查找。字体文件本身由 `.gitignore`
//! 排除，不进仓库（体积大且各机器自备即可）。

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/fonts");
    println!("cargo::rustc-check-cfg=cfg(rumark_bundled_cjk)");

    let dir = Path::new("assets/fonts");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    // 固定优先级：OFL 的 Noto 优先（嵌入无版权顾虑），再到其他静态字体。
    let mut candidates: Vec<(u8, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let is_font = lower.ends_with(".ttf") || lower.ends_with(".otf");
        if !is_font {
            continue;
        }
        let rank = if lower.contains("noto") && lower.contains("sc") {
            0
        } else if lower.contains("noto") && lower.contains("cjk") {
            1
        } else if lower.contains("wqy")
            || lower.contains("sourcehan")
            || lower.contains("source-han")
        {
            2
        } else {
            3
        };
        candidates.push((rank, path.to_string_lossy().replace('\\', "/")));
    }
    candidates.sort();

    if let Some((_, path)) = candidates.into_iter().next() {
        println!("cargo:rustc-cfg=rumark_bundled_cjk");
        println!("cargo:rustc-env=RUMARK_BUNDLED_CJK_PATH={path}");
    }
}
