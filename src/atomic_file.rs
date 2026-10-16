//! 原子文件写入：先写同目录临时文件并落盘，再重命名覆盖目标。
//!
//! 直接 `fs::write` / `fs::copy` 在写到一半时崩溃或断电会留下截断文件；
//! 用户配置、文档、粘贴图片与各类缓存都由此受损。同目录重命名在同一文件
//! 系统上是原子的，最坏也只是保留旧内容。

use std::path::Path;

/// 原子写文本文件。
///
/// 代价是目标被整块替换成新文件对象：原文件的权限位与硬链接不会保留，
/// 新文件按目录默认权限创建。
pub(crate) fn write_text(path: &Path, contents: &str) -> std::io::Result<()> {
    write_bytes(path, contents.as_bytes())
}

/// 原子写二进制文件，供图片、导出产物等非文本落盘使用。
pub(crate) fn write_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    write_with(path, |file| file.write_all(contents))
}

/// 原子复制文件：源文件内容流式写入目标同目录临时文件后重命名，不整份读入内存。
///
/// 供「把外部图片复制进文档目录 / 附件目录」这类用户数据复制使用：中断只会留下
/// 临时文件，不会留下半截图片被文档引用。
pub(crate) fn copy_file(source: &Path, target: &Path) -> std::io::Result<()> {
    write_with(target, |file| {
        let mut source_file = std::fs::File::open(source)?;
        std::io::copy(&mut source_file, file).map(|_| ())
    })
}

/// 在 `path` 同目录的临时文件上执行 `write` 并落盘，成功后重命名覆盖 `path`。
///
/// 任一步失败都会清理临时文件并原样返回错误，目标文件因此要么是旧内容、
/// 要么是完整新内容。
fn write_with(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "路径缺少文件名"))?;
    let mut temp_name = file_name.to_os_string();
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp_path = parent.join(temp_name);

    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        write(&mut file)?;
        // 先落盘再改名：否则断电后可能留下「已改名但内容仍在页缓存」的空文件。
        file.sync_all()
    })();

    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{copy_file, write_bytes, write_text};

    /// 目录里除 `expected` 外不应有任何文件（尤其是临时文件）。
    fn assert_only_files(root: &Path, expected: &[&str]) {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut expected: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        expected.sort();
        assert_eq!(names, expected, "unexpected leftovers in {root:?}");
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rumark-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn write_text_replaces_contents_and_leaves_no_temp_file() {
        let root = temp_root("atomic-text");
        let target = root.join("config.toml");

        write_text(&target, "first").expect("first write");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");

        write_text(&target, "second").expect("overwrite");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        assert_only_files(&root, &["config.toml"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn write_bytes_stores_binary_contents_and_replaces_previous_file() {
        let root = temp_root("atomic-bytes");
        let target = root.join("image.png");
        let png_bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];

        write_bytes(&target, &png_bytes).expect("first write");
        assert_eq!(std::fs::read(&target).unwrap(), png_bytes);

        write_bytes(&target, b"second").expect("overwrite");
        assert_eq!(std::fs::read(&target).unwrap(), b"second");
        assert_only_files(&root, &["image.png"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn copy_file_streams_source_bytes_and_leaves_no_temp_file() {
        let root = temp_root("atomic-copy");
        let source = root.join("source.png");
        let target = root.join("target.png");
        let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x00, 0xff, 0xfe];
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&source, bytes).unwrap();

        copy_file(&source, &target).expect("copy");
        assert_eq!(std::fs::read(&target).unwrap(), bytes);

        // 覆盖已有目标同样成功，且源文件与目标之外不留临时文件。
        copy_file(&source, &target).expect("overwrite");
        assert_only_files(&root, &["source.png", "target.png"]);

        // 源文件缺失时报错，且不会留下半个目标文件。
        std::fs::remove_file(&source).unwrap();
        assert!(copy_file(&source, &target).is_err());
        assert_only_files(&root, &["target.png"]);

        let _ = std::fs::remove_dir_all(root);
    }
}
