//! 原子文件写入：先写同目录临时文件并落盘，再重命名覆盖目标。
//!
//! 直接 `fs::write` / `fs::copy` 在写到一半时崩溃或断电会留下截断文件；
//! 用户配置、文档、粘贴图片与各类缓存都由此受损。同目录重命名在同一文件
//! 系统上是原子的，最坏也只是保留旧内容。

use std::path::Path;

/// 原子写文本文件。
///
/// 代价是目标被整块替换成新文件对象：原文件的硬链接不会保留；Unix 上会沿用
/// 原目标文件的权限位，目标不存在时按目录默认权限创建。
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
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?;
    // 临时名带进程号与 UUID：同一进程内并发写同一目标（自动保存与手动保存同时
    // 触发）不会共用同一个临时文件而互相截断。
    let mut temp_name = file_name.to_os_string();
    temp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let temp_path = parent.join(temp_name);
    // 覆盖已有文件时记下原权限：重命名换的是新文件对象，权限来自目录 umask，
    // 会把用户收紧过的 0600 文档/配置放宽成 0644。
    #[cfg(unix)]
    let existing_permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());

    let write_result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp_path)?;
        write(&mut file)?;
        // 先落盘再改名：否则断电后可能留下「已改名但内容仍在页缓存」的空文件。
        file.sync_all()?;
        #[cfg(unix)]
        if let Some(permissions) = existing_permissions {
            std::fs::set_permissions(&temp_path, permissions)?;
        }
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    match rename_with_retry(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            // 改名失败时保留临时文件反而更糟（用户看不到、下次写入又撞上），
            // 统一清理：目标文件此时仍是完整的旧内容。
            let _ = std::fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

/// 短时重试的重命名次数与间隔。
///
/// Windows 上杀软、索引器或资源管理器预览会短暂占用目标文件，
/// `rename` 报共享冲突（`PermissionDenied`）；这类占用百毫秒内即释放，
/// 重试比直接把「保存失败」推给用户更贴近实际。
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

/// 重命名临时文件到目标，共享冲突时短时重试。
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut err = match std::fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };
    for _ in 1..RENAME_ATTEMPTS {
        if !is_transient_rename_error(&err) {
            break;
        }
        std::thread::sleep(RENAME_BACKOFF);
        err = match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(retry_err) => retry_err,
        };
    }
    Err(err)
}

/// 判断重命名错误是否值得重试：仅共享/权限类冲突。
fn is_transient_rename_error(err: &std::io::Error) -> bool {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    // Windows：ERROR_SHARING_VIOLATION(32) 与 ERROR_ACCESS_DENIED(5)。
    cfg!(windows) && matches!(err.raw_os_error(), Some(5) | Some(32))
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

    #[cfg(unix)]
    #[test]
    fn write_text_preserves_existing_unix_permissions_on_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_root("atomic-mode");
        let target = root.join("secret.md");

        write_text(&target, "first").expect("first write");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        // 重命名换的是新文件对象，不显式沿用就会退回目录默认权限（通常 0644）。
        write_text(&target, "second").expect("overwrite");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "覆盖写应保留原文件权限");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        assert_only_files(&root, &["secret.md"]);

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

    #[test]
    fn concurrent_writes_to_one_target_never_mix_contents_or_leave_temp() {
        let root = temp_root("atomic-concurrent");
        let target = root.join("doc.md");
        write_text(&target, "seed").expect("seed write");

        // 每份负载长度唯一：残留半截或交织内容必然不等于任何一份完整负载。
        let payloads: Vec<String> = (0..8)
            .map(|index| format!("payload-{index}:{}", "x".repeat(2048 * (index + 1))))
            .collect();
        std::thread::scope(|scope| {
            for payload in &payloads {
                let payload = payload.clone();
                let target = target.clone();
                scope.spawn(move || {
                    let _ = write_text(&target, &payload);
                });
            }
        });

        let final_text = std::fs::read_to_string(&target).unwrap();
        assert!(
            payloads.contains(&final_text) || final_text == "seed",
            "target holds interleaved or truncated contents"
        );
        assert_only_files(&root, &["doc.md"]);

        let _ = std::fs::remove_dir_all(root);
    }
}
