//! 应用偏好与导入配置包共用的用户配置工具。

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use directories::ProjectDirs;
use serde_json::{Map, Value};

pub(crate) mod preferences;

pub(crate) use preferences::{
    EditorSettings, ExportThemeMode, ImagePasteBehavior, StartupOpenPreference,
    apply_configured_language, apply_configured_theme, first_existing_recent_markdown_file,
    import_language_config_and_select, import_theme_config_and_select,
    load_or_create_app_preferences, open_preferences_window, read_app_preferences,
};

pub(crate) const RECENT_LIMIT: usize = 20;

/// 拾墨拥有的跨平台配置目录。
#[derive(Debug, Clone)]
pub(crate) struct RumarkConfigDirs {
    root: PathBuf,
}

impl RumarkConfigDirs {
    /// 解析平台相关的应用配置目录。
    ///
    /// rgpui 目前不提供应用配置路径，因此用户导入的语言包与主题包
    /// 存放在 `directories::ProjectDirs` 返回的系统位置下。
    pub(crate) fn from_system() -> anyhow::Result<Self> {
        let dirs = ProjectDirs::from("rs", "launcher", "Rumark")
            .context("failed to resolve the Rumark config directory")?;
        let root = dirs.config_dir().to_path_buf();
        migrate_legacy_config_dir(&root);
        Ok(Self { root })
    }

    /// 从调用方传入的根目录构造配置目录集（仅测试用）。
    #[cfg(test)]
    pub(crate) fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 返回语言包目录。
    pub(crate) fn languages_dir(&self) -> PathBuf {
        self.root.join("languages")
    }

    /// 返回主题包目录。
    pub(crate) fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }

    /// 返回最近文件历史记录文件路径。
    pub(crate) fn history_file(&self) -> PathBuf {
        self.root.join(".history")
    }

    /// 返回应用配置文件路径。
    pub(crate) fn app_config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
}

/// 若新配置目录不存在而旧 `Mojian` 目录存在，则递归拷贝旧目录内容到新目录。
fn migrate_legacy_config_dir(new_root: &Path) {
    if new_root.exists() {
        return;
    }
    let Some(legacy) = ProjectDirs::from("rs", "launcher", "Mojian") else {
        return;
    };
    let legacy_root = legacy.config_dir();
    if !legacy_root.exists() {
        return;
    }
    if let Err(err) = copy_dir_recursive(legacy_root, new_root) {
        eprintln!(
            "failed to migrate legacy Mojian config '{}' to '{}': {err}",
            legacy_root.display(),
            new_root.display()
        );
    }
}

/// 递归拷贝目录内容。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            // 逐个文件原子写：迁移途中中断不会留下「截断的配置文件」被后续读取。
            crate::atomic_file::copy_file(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// 读取最近文件历史记录。
pub(crate) fn read_recent_files() -> anyhow::Result<Vec<PathBuf>> {
    read_recent_files_with_dirs(&RumarkConfigDirs::from_system()?)
}

/// 记录最近打开的文件并刷新历史。
pub(crate) fn record_recent_file(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    record_recent_file_with_dirs(path, &RumarkConfigDirs::from_system()?)
}

/// 从历史记录中移除指定文件。
pub(crate) fn remove_recent_file(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    remove_recent_file_with_dirs(path, &RumarkConfigDirs::from_system()?)
}

/// 用指定目录集读取最近文件历史。
pub(crate) fn read_recent_files_with_dirs(dirs: &RumarkConfigDirs) -> anyhow::Result<Vec<PathBuf>> {
    let path = dirs.history_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read '{}'", path.display()));
        }
    };

    Ok(normalize_recent_files(text.lines().map(PathBuf::from)))
}

/// 用指定目录集记录最近文件。
pub(crate) fn record_recent_file_with_dirs(
    path: &Path,
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<Vec<PathBuf>> {
    if path.to_string_lossy().trim().is_empty() {
        bail!("recent file path cannot be empty");
    }
    if !is_recordable_recent_file_path(path) {
        return read_recent_files_with_dirs(dirs);
    }

    let mut paths = read_recent_files_with_dirs(dirs)?;
    let path = path.to_path_buf();
    paths.retain(|existing| !same_recent_path(existing, &path));
    paths.insert(0, path);
    paths.truncate(RECENT_LIMIT);
    write_recent_files_with_dirs(&paths, dirs)?;
    Ok(paths)
}

/// 用指定目录集移除最近文件。
pub(crate) fn remove_recent_file_with_dirs(
    path: &Path,
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = read_recent_files_with_dirs(dirs)?;
    paths.retain(|existing| !same_recent_path(existing, path));
    write_recent_files_with_dirs(&paths, dirs)?;
    Ok(paths)
}

/// 将最近文件列表写回历史文件。
fn write_recent_files_with_dirs(paths: &[PathBuf], dirs: &RumarkConfigDirs) -> anyhow::Result<()> {
    let history_file = dirs.history_file();
    let normalized = normalize_recent_files(paths.iter().cloned());
    if normalized.is_empty() {
        match std::fs::remove_file(&history_file) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to remove '{}'", history_file.display()));
            }
        }
        return Ok(());
    }

    let mut content = String::new();
    for path in normalized {
        content.push_str(&path.to_string_lossy());
        content.push('\n');
    }
    crate::atomic_file::write_text(&history_file, &content)
        .with_context(|| format!("failed to write '{}'", history_file.display()))
}

/// 规范化最近文件列表（去空去重并截断）。
fn normalize_recent_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut normalized: Vec<PathBuf> = Vec::new();
    for path in paths {
        let text = path.to_string_lossy();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = PathBuf::from(trimmed);
        if !is_recordable_recent_file_path(&path) {
            continue;
        }
        if normalized
            .iter()
            .any(|existing| same_recent_path(existing, &path))
        {
            continue;
        }
        normalized.push(path);
        if normalized.len() == RECENT_LIMIT {
            break;
        }
    }
    normalized
}

/// 判断路径是否值得记入最近文件。
fn is_recordable_recent_file_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.trim().is_empty() {
        return false;
    }

    !(is_inside_system_temp_dir(path) && has_rumark_temp_fixture_name(path))
}

/// 判断路径是否位于系统临时目录内。
fn is_inside_system_temp_dir(path: &Path) -> bool {
    let temp_dir = std::env::temp_dir();
    if cfg!(windows) {
        let path_text = normalize_windows_path_text(path);
        let mut temp_text = normalize_windows_path_text(&temp_dir);
        if !temp_text.ends_with('\\') {
            temp_text.push('\\');
        }
        return path_text.starts_with(&temp_text);
    }

    path.starts_with(temp_dir)
}

/// 规范化 Windows 路径文本以便比较。
fn normalize_windows_path_text(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// 判断是否为测试用的临时文件名称。
fn has_rumark_temp_fixture_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let name = name.to_ascii_lowercase();
            name.starts_with("rumark-drop-")
        })
        .unwrap_or(false)
}

/// 比较两条最近文件路径是否相同。
fn same_recent_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

/// 判断是否为受支持的配置文件扩展名。
pub(crate) fn is_supported_config_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("jsonc")
        })
        .unwrap_or(false)
}

/// 读取 JSON 或 JSONC 配置文件。
pub(crate) fn read_json_or_jsonc(path: &Path) -> anyhow::Result<Value> {
    if !is_supported_config_file(path) {
        bail!("configuration files must use the .json or .jsonc extension");
    }

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read '{}'", path.display()))?;
    let parsed = if path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("jsonc"))
        .unwrap_or(false)
    {
        parse_jsonc(&text)?
    } else {
        serde_json::from_str(&text)?
    };
    Ok(parsed)
}

/// 解析 JSONC 文本为 JSON 值。
pub(crate) fn parse_jsonc(text: &str) -> anyhow::Result<Value> {
    let stripped = remove_jsonc_comments(text)?;
    Ok(serde_json::from_str(&stripped)?)
}

/// 剥离 JSONC 文本中的注释。
pub(crate) fn remove_jsonc_comments(input: &str) -> anyhow::Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch);
            continue;
        }

        if ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            output.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut closed = false;
                    let mut previous = '\0';
                    for next in chars.by_ref() {
                        if next == '\n' {
                            output.push('\n');
                        }
                        if previous == '*' && next == '/' {
                            closed = true;
                            break;
                        }
                        previous = next;
                    }
                    if !closed {
                        bail!("unterminated block comment in JSONC file");
                    }
                    continue;
                }
                _ => {}
            }
        }

        output.push(ch);
    }

    Ok(output)
}

/// 清洗配置文件名中的非法字符。
pub(crate) fn sanitize_config_stem(value: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;
    for ch in value.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_separator && !output.is_empty() {
                output.push('_');
                last_was_separator = true;
            }
        } else if ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
            last_was_separator = false;
        }
    }

    let output = output.trim_matches(['_', '.']).to_string();
    if output.is_empty() {
        "custom".into()
    } else {
        output
    }
}

/// 递归裁剪 JSON 中的空值。
pub(crate) fn drop_empty_json_values(value: &mut Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => {
            items.retain_mut(|item| !drop_empty_json_values(item));
            items.is_empty()
        }
        Value::Object(object) => {
            object.retain(|_, item| !drop_empty_json_values(item));
            object.is_empty()
        }
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// 返回去掉空值后的 JSON 对象。
pub(crate) fn object_without_empty_values(mut object: Map<String, Value>) -> Map<String, Value> {
    object.retain(|_, value| !drop_empty_json_values(value));
    object
}

#[cfg(test)]
mod tests {
    use super::{
        RECENT_LIMIT, RumarkConfigDirs, drop_empty_json_values, parse_jsonc,
        read_recent_files_with_dirs, record_recent_file_with_dirs, remove_jsonc_comments,
        remove_recent_file_with_dirs, sanitize_config_stem,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};

    #[test]
    fn jsonc_comments_are_stripped_without_touching_strings() {
        let text = r#"
        {
            // line comment
            "url": "https://example.com/a//b",
            "text": "/* not a comment */",
            /* block comment */
            "value": 1
        }
        "#;

        let parsed = parse_jsonc(text).expect("jsonc should parse");
        assert_eq!(parsed["url"], "https://example.com/a//b");
        assert_eq!(parsed["text"], "/* not a comment */");
        assert_eq!(parsed["value"], 1);
        assert!(remove_jsonc_comments(text).is_ok());
    }

    #[test]
    fn empty_values_are_pruned_recursively() {
        let mut value = json!({
            "name": "",
            "colors": {
                "text_default": null,
                "selection": "#fff"
            },
            "items": ["", null]
        });

        assert!(!drop_empty_json_values(&mut value));
        assert_eq!(value, json!({ "colors": { "selection": "#fff" } }));
    }

    #[test]
    fn config_file_stems_are_sanitized() {
        assert_eq!(sanitize_config_stem("My Theme / Blue"), "My_Theme_Blue");
        assert_eq!(sanitize_config_stem("  ...  "), "custom");
    }

    #[test]
    fn missing_recent_history_file_returns_empty_list() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);

        assert!(read_recent_files_with_dirs(&dirs).unwrap().is_empty());
        assert!(!dirs.history_file().exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn empty_recent_history_write_does_not_create_file() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);

        super::write_recent_files_with_dirs(&[], &dirs).unwrap();

        assert!(!dirs.history_file().exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn blank_recent_file_path_is_rejected() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);

        assert!(record_recent_file_with_dirs(Path::new("   "), &dirs).is_err());
        assert!(!dirs.history_file().exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recent_history_filters_empty_lines_and_deduplicates() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            dirs.history_file(),
            "  \nC:\\one.md\n\nC:\\two.md\nC:\\one.md\n",
        )
        .unwrap();

        let paths = read_recent_files_with_dirs(&dirs).unwrap();
        assert_eq!(
            paths,
            vec![PathBuf::from("C:\\one.md"), PathBuf::from("C:\\two.md")]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recent_history_filters_legacy_rumark_temp_fixture_paths() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        let fixture_path = std::env::temp_dir().join(format!(
            "rumark-drop-save-replace-{}-123.md",
            std::process::id()
        ));
        let real_path = PathBuf::from("C:\\notes\\real.md");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            dirs.history_file(),
            format!("{}\n{}\n", fixture_path.display(), real_path.display()),
        )
        .unwrap();

        let paths = read_recent_files_with_dirs(&dirs).unwrap();
        assert_eq!(paths, vec![real_path]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recording_rumark_temp_fixture_path_is_noop() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        let fixture_path = std::env::temp_dir().join(format!(
            "rumark-drop-dirty-discard-{}-123.md",
            std::process::id()
        ));

        assert!(
            record_recent_file_with_dirs(&fixture_path, &dirs)
                .unwrap()
                .is_empty()
        );
        assert!(!dirs.history_file().exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ordinary_temp_markdown_file_can_still_be_recorded() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        let path = std::env::temp_dir().join(format!("manual-note-{}.md", std::process::id()));

        let paths = record_recent_file_with_dirs(&path, &dirs).unwrap();

        assert_eq!(paths, vec![path]);
        assert!(dirs.history_file().exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recording_recent_file_moves_it_to_front_and_truncates() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);

        for index in 0..(RECENT_LIMIT + 2) {
            record_recent_file_with_dirs(&PathBuf::from(format!("file-{index}.md")), &dirs)
                .unwrap();
        }
        record_recent_file_with_dirs(&PathBuf::from("file-3.md"), &dirs).unwrap();

        let paths = read_recent_files_with_dirs(&dirs).unwrap();
        assert_eq!(paths.len(), RECENT_LIMIT);
        assert_eq!(paths[0], PathBuf::from("file-3.md"));
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.as_path() == Path::new("file-3.md"))
                .count(),
            1
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removing_recent_file_persists_history_without_it() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        record_recent_file_with_dirs(&PathBuf::from("one.md"), &dirs).unwrap();
        record_recent_file_with_dirs(&PathBuf::from("two.md"), &dirs).unwrap();

        let paths = remove_recent_file_with_dirs(&PathBuf::from("one.md"), &dirs).unwrap();

        assert_eq!(paths, vec![PathBuf::from("two.md")]);
        assert_eq!(
            read_recent_files_with_dirs(&dirs).unwrap(),
            vec![PathBuf::from("two.md")]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn removing_last_recent_file_deletes_history_file() {
        let root = std::env::temp_dir().join(format!("rumark-config-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        let path = PathBuf::from("only.md");
        record_recent_file_with_dirs(&path, &dirs).unwrap();
        assert!(dirs.history_file().exists());

        let paths = remove_recent_file_with_dirs(&path, &dirs).unwrap();

        assert!(paths.is_empty());
        assert!(!dirs.history_file().exists());
        assert!(read_recent_files_with_dirs(&dirs).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(root);
    }
}
