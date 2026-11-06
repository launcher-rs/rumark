//! 基于 rgpui 通用国际化管理器的界面语言层。
//!
//! 文案以扁平键值对存放（键即旧强类型字段名，如 `menu_file`），
//! 中英文内置包为 `zh-CN.json` / `en-US.json`，经 `include_str!` 内嵌。
//! 自定义语言包为带 `id`/`name` 信封的 JSON，`strings` 为自由键值表，
//! 缺键自动回退英文。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use rgpui::{App, Global};
use serde_json::{Map, Value};

use crate::atomic_file::write_text;
use crate::config::{RumarkConfigDirs, read_json_or_jsonc, sanitize_config_stem};

/// 内置简体中文语言标识。
const LANG_ZH_CN_ID: &str = "zh-CN";
/// 内置简体中文语言名称。
const LANG_ZH_CN_NAME: &str = "简体中文";
/// 内置英文语言标识（同时为回退语言）。
const LANG_EN_US_ID: &str = "en-US";
/// 内置英文语言名称。
const LANG_EN_US_NAME: &str = "English";

/// 可选界面语言的元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageEntry {
    /// 语言标识，如 `zh-CN`。
    pub id: String,
    /// 展示名称。
    pub name: String,
}

/// 返回内置语言目录。
fn builtin_language_catalog() -> Vec<LanguageEntry> {
    vec![
        LanguageEntry {
            id: LANG_ZH_CN_ID.into(),
            name: LANG_ZH_CN_NAME.into(),
        },
        LanguageEntry {
            id: LANG_EN_US_ID.into(),
            name: LANG_EN_US_NAME.into(),
        },
    ]
}

/// 持有当前界面语言的全局单例（内层为 rgpui 通用管理器）。
pub struct I18nManager {
    /// 通用键值翻译器（含中英内置包与自定义覆盖）。
    inner: rgpui::i18n::I18nManager,
    /// 各语言的合并键值表（自定义语言已叠加英文兜底），快照用。
    ///
    /// 用 `Arc` 共享：快照只需递增引用计数，避免每帧深拷贝整张键值表。
    maps: HashMap<String, Arc<HashMap<String, String>>>,
    /// 当前语言标识。
    active_language_id: String,
    /// 自定义语言的展示名（键为语言标识）。
    custom_names: HashMap<String, String>,
    /// 菜单展示用的语言目录。
    language_catalog: Vec<LanguageEntry>,
}

impl Global for I18nManager {}

impl Default for I18nManager {
    /// 返回默认英文语言管理器。
    fn default() -> Self {
        Self::new_with_language_id(LANG_EN_US_ID)
    }
}

impl I18nManager {
    /// 把指定界面语言安装到全局状态。
    pub fn init_with_language_id(cx: &mut App, language_id: &str) {
        let mut manager = Self::new_with_language_id(LANG_EN_US_ID);
        if let Ok(dirs) = RumarkConfigDirs::from_system()
            && let Err(err) = manager.load_custom_languages_from_dirs(&dirs)
        {
            eprintln!("failed to load custom languages: {err}");
        }
        let _ = manager.set_language_by_id(language_id);
        cx.set_global(manager);
    }

    /// 用已知语言标识创建管理器，未知时回退到英文。
    pub fn new_with_language_id(language_id: &str) -> Self {
        let mut inner = rgpui::i18n::I18nManager::new(LANG_EN_US_ID).with_fallback("en-US");
        let en_map = builtin_map(LANG_EN_US_ID);
        let zh_map = builtin_map(LANG_ZH_CN_ID);
        inner.load_translations_map(LANG_EN_US_ID, en_map.clone());
        inner.load_translations_map(LANG_ZH_CN_ID, zh_map.clone());
        let active_language_id = if is_builtin_language_id(language_id) {
            language_id
        } else {
            LANG_EN_US_ID
        };
        inner.set_locale(active_language_id);
        let mut maps = HashMap::new();
        maps.insert(LANG_EN_US_ID.into(), Arc::new(en_map));
        maps.insert(LANG_ZH_CN_ID.into(), Arc::new(zh_map));
        Self {
            inner,
            maps,
            active_language_id: active_language_id.into(),
            custom_names: HashMap::new(),
            language_catalog: builtin_language_catalog(),
        }
    }

    /// 返回当前生效界面语言的标识。
    pub fn active_language_id(&self) -> &str {
        &self.active_language_id
    }

    /// 取翻译文案（`{name}` 占位符经 `args` 替换，缺键返回键名）。
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.inner.t(key, args)
    }

    /// 对当前语言拍 owned 快照（渲染热路径用：快照不借用 `cx`，
    /// 可与 `&mut Context` 同存，替代旧 `strings_arc` 模式）。
    pub fn snapshot(&self) -> I18nSnapshot {
        // 这里只克隆 `Arc`（引用计数递增），不深拷贝键值表。
        let fallback = self.maps.get(LANG_EN_US_ID).cloned().unwrap_or_default();
        let strings = self
            .maps
            .get(&self.active_language_id)
            .cloned()
            .unwrap_or_default();
        I18nSnapshot { strings, fallback }
    }

    /// 返回菜单中展示的全部内置与导入语言。
    pub fn listed_languages(&self) -> &[LanguageEntry] {
        &self.language_catalog
    }

    /// 按标识激活界面语言，未知标识返回 `false`。
    pub fn set_language_by_id(&mut self, language_id: &str) -> bool {
        if !is_builtin_language_id(language_id) && !self.custom_names.contains_key(language_id) {
            return false;
        }
        let changed = self.active_language_id != language_id;
        self.active_language_id = language_id.into();
        self.inner.set_locale(language_id);
        changed
    }

    /// 导入用户语言包，落盘规范化副本并激活。
    pub fn load_language_config(&mut self, path: impl AsRef<Path>) -> anyhow::Result<String> {
        let dirs = RumarkConfigDirs::from_system()?;
        self.import_language_config_with_dirs(path, &dirs)
    }

    /// 用指定目录集导入用户语言包。
    fn import_language_config_with_dirs(
        &mut self,
        path: impl AsRef<Path>,
        dirs: &RumarkConfigDirs,
    ) -> anyhow::Result<String> {
        let raw = read_json_or_jsonc(path.as_ref())?;
        let (id, name, strings, normalized) = custom_language_pack_from_value(raw)?;
        let file_name = format!("{}.json", sanitize_config_stem(&id));
        let languages_dir = dirs.languages_dir();
        write_text(
            &languages_dir.join(file_name),
            &serde_json::to_string_pretty(&normalized)?,
        )?;
        self.apply_custom_map(&id, &name, strings);
        self.set_language_by_id(&id);
        Ok(id)
    }

    /// 从指定目录集加载自定义语言。
    fn load_custom_languages_from_dirs(&mut self, dirs: &RumarkConfigDirs) -> anyhow::Result<()> {
        let languages_dir = dirs.languages_dir();
        if !languages_dir.exists() {
            return Ok(());
        }

        let mut loaded = Vec::new();
        for entry in std::fs::read_dir(&languages_dir)? {
            let path = entry?.path();
            if path.is_file() {
                match read_json_or_jsonc(&path).and_then(custom_language_pack_from_value) {
                    Ok((id, name, strings, _)) => loaded.push((id, name, strings)),
                    Err(err) => eprintln!(
                        "skipping custom language config '{}': {err}",
                        path.display()
                    ),
                }
            }
        }
        loaded.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
        for (id, name, strings) in loaded {
            self.apply_custom_map(&id, &name, strings);
        }
        Ok(())
    }

    /// 登记自定义语言（英文兜底叠加覆盖）并重建目录。
    fn apply_custom_map(&mut self, id: &str, name: &str, strings: HashMap<String, String>) {
        let mut merged = builtin_map(LANG_EN_US_ID);
        merged.extend(strings);
        self.inner.load_translations_map(id, merged.clone());
        self.maps.insert(id.into(), Arc::new(merged));
        self.custom_names.insert(id.into(), name.into());
        self.rebuild_language_catalog();
    }

    /// 按内置与自定义语言重建语言目录。
    fn rebuild_language_catalog(&mut self) {
        let mut catalog = builtin_language_catalog();
        let mut customs: Vec<(&String, &String)> = self.custom_names.iter().collect();
        customs.sort_by(|left, right| left.1.cmp(right.1).then(left.0.cmp(right.0)));
        catalog.extend(customs.into_iter().map(|(id, name)| LanguageEntry {
            id: (*id).clone(),
            name: (*name).clone(),
        }));
        self.language_catalog = catalog;
    }
}

/// 当前语言文案的 owned 快照，与 `cx` 借用无关，可自由传入需要 `&mut` 上下文的渲染辅助函数。
#[derive(Debug, Clone)]
pub struct I18nSnapshot {
    /// 当前语言合并键值表（与其他快照共享同一份，不深拷贝）。
    strings: Arc<HashMap<String, String>>,
    /// 英文兜底键值表（与其他快照共享同一份，不深拷贝）。
    fallback: Arc<HashMap<String, String>>,
}

impl I18nSnapshot {
    /// 取翻译文案（占位符规则与 [`I18nManager::t`] 一致）。
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        let value = self
            .strings
            .get(key)
            .or_else(|| self.fallback.get(key))
            .cloned()
            .unwrap_or_else(|| key.to_string());
        let mut result = value;
        for (name, replacement) in args {
            result = result.replace(&format!("{{{name}}}"), replacement);
        }
        result
    }
}

/// 取内置语言的键值表（缺键在本包内不可能，加载失败即 panic）。
fn builtin_map(language_id: &str) -> HashMap<String, String> {
    let json = match language_id {
        LANG_ZH_CN_ID => include_str!("zh-CN.json"),
        _ => include_str!("en-US.json"),
    };
    serde_json::from_str(json).expect("built-in language pack should parse")
}

/// 判断是否为内置语言标识。
fn is_builtin_language_id(language_id: &str) -> bool {
    matches!(language_id, LANG_ZH_CN_ID | LANG_EN_US_ID)
}

/// 校验自定义语言标识是否合法。
fn is_valid_custom_language_id(language_id: &str) -> bool {
    !language_id.trim().is_empty()
        && language_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && language_id.chars().any(|ch| ch.is_ascii_alphabetic())
}

/// 按系统偏好语言列表选择内置语言标识。
pub fn language_id_from_preferences<I, S>(locales: I) -> &'static str
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    locales
        .into_iter()
        .find_map(|locale| language_id_for_locale(locale.as_ref()))
        .unwrap_or(LANG_EN_US_ID)
}

/// 按单个系统语言标记匹配内置语言标识。
fn language_id_for_locale(locale: &str) -> Option<&'static str> {
    let locale = locale.trim();
    if locale.is_empty() {
        return None;
    }

    let no_encoding = locale
        .split_once('.')
        .map_or(locale, |(locale, _encoding)| locale);
    let no_modifier = no_encoding
        .split_once('@')
        .map_or(no_encoding, |(locale, _modifier)| locale);
    let locale = no_modifier.replace('_', "-");
    let language = locale.split('-').next()?.to_ascii_lowercase();
    if !language.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }

    match language.as_str() {
        "zh" => Some(LANG_ZH_CN_ID),
        "en" => Some(LANG_EN_US_ID),
        _ => None,
    }
}

/// 从 JSON 值解析自定义语言包，返回标识、名称、键值表与规范化副本。
fn custom_language_pack_from_value(
    mut value: Value,
) -> anyhow::Result<(String, String, HashMap<String, String>, Value)> {
    crate::config::drop_empty_json_values(&mut value);
    let Value::Object(object) = value else {
        bail!("language config must be a JSON object");
    };
    let object = crate::config::object_without_empty_values(object);
    let id = required_string(&object, "id")?;
    if is_builtin_language_id(&id) {
        bail!("custom language id '{id}' would override a built-in language");
    }
    if !is_valid_custom_language_id(&id) {
        bail!("custom language id '{id}' contains unsupported characters");
    }
    let name = required_string(&object, "name")?;
    let mut strings = HashMap::new();
    let mut normalized_strings = Map::new();
    if let Some(table) = object.get("strings").and_then(Value::as_object) {
        for (key, text) in table {
            let Some(text) = text.as_str() else {
                bail!("language string '{key}' must be a string");
            };
            strings.insert(key.clone(), text.to_string());
            normalized_strings.insert(key.clone(), Value::String(text.to_string()));
        }
    }
    let mut normalized_object = Map::new();
    normalized_object.insert("id".into(), Value::String(id.clone()));
    normalized_object.insert("name".into(), Value::String(name.clone()));
    for key in ["author", "description", "version", "homepage", "license"] {
        if let Some(value) = object.get(key) {
            normalized_object.insert(key.into(), value.clone());
        }
    }
    if !normalized_strings.is_empty() {
        normalized_object.insert("strings".into(), Value::Object(normalized_strings));
    }
    let normalized = Value::Object(normalized_object);
    Ok((id, name, strings, normalized))
}

/// 读取对象中的必填非空字符串字段。
fn required_string(object: &Map<String, Value>, key: &str) -> anyhow::Result<String> {
    let Some(value) = object.get(key) else {
        bail!("missing required field '{key}'");
    };
    let Some(text) = value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        bail!("field '{key}' must be a non-empty string");
    };
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::{I18nManager, language_id_from_preferences};
    use std::collections::HashMap;

    #[test]
    fn builtin_chinese_values_are_utf8() {
        let manager = I18nManager::new_with_language_id("zh-CN");
        assert_eq!(manager.t("menu_file", &[]), "文件");
        assert_eq!(manager.t("menu_export", &[]), "导出");
        assert_eq!(manager.t("menu_language", &[]), "语言");
        assert_eq!(manager.t("message_save_failed", &[]), "保存失败");
        assert_eq!(manager.t("status_mode_source", &[]), "源码");
        assert_eq!(manager.t("context_insert", &[]), "插入");
        assert_eq!(manager.t("table_insert_title", &[]), "插入表格");
        assert_eq!(manager.t("image_loading", &[]), "正在加载图片...");
        assert_eq!(manager.t("dialog_about_github", &[]), "GitHub");
        assert_eq!(
            manager.t("dialog_about_star", &[]),
            "如果本项目对您有帮助，那不妨给本项目一颗 Star⭐，十分感谢！"
        );
    }

    #[test]
    fn manager_switches_builtin_languages() {
        let mut manager = I18nManager::default();
        assert_eq!(manager.active_language_id(), "en-US");
        assert_eq!(manager.t("menu_file", &[]), "File");

        assert!(manager.set_language_by_id("zh-CN"));
        assert_eq!(manager.active_language_id(), "zh-CN");
        assert_eq!(manager.t("menu_file", &[]), "文件");

        assert!(!manager.set_language_by_id("fr-FR"));
        assert_eq!(manager.active_language_id(), "zh-CN");
    }

    #[test]
    fn missing_key_falls_back_to_key_name() {
        let manager = I18nManager::default();
        assert_eq!(manager.t("not_a_real_key", &[]), "not_a_real_key");
    }

    #[test]
    fn template_args_are_substituted() {
        let manager = I18nManager::new_with_language_id("zh-CN");
        let text = manager.t(
            "message_recent_missing_message",
            &[("path", "/tmp/example.md")],
        );
        assert!(text.contains("/tmp/example.md"), "text: {text}");
        assert!(!text.contains("{path}"), "text: {text}");
    }

    #[test]
    fn custom_map_overlays_english_fallback() {
        let mut manager = I18nManager::default();
        let mut strings = HashMap::new();
        strings.insert("menu_file".into(), "Fichier".into());
        manager.apply_custom_map("fr-FR", "Français", strings);
        assert!(manager.set_language_by_id("fr-FR"));
        assert_eq!(manager.t("menu_file", &[]), "Fichier");
        // 未覆盖的键回退英文。
        assert_eq!(manager.t("menu_file_quit", &[]), "Quit");
        assert_eq!(manager.listed_languages().len(), 3);
    }

    #[test]
    fn snapshot_shares_maps_instead_of_deep_cloning() {
        let mut manager = I18nManager::new_with_language_id("zh-CN");
        let first = manager.snapshot();
        let second = manager.snapshot();
        // 快照只递增引用计数，两张快照指向同一份键值表。
        assert!(std::sync::Arc::ptr_eq(&first.strings, &second.strings));
        assert_eq!(first.t("menu_file", &[]), "文件");

        manager.set_language_by_id("en-US");
        let third = manager.snapshot();
        assert!(!std::sync::Arc::ptr_eq(&first.strings, &third.strings));
        assert_eq!(third.t("menu_file", &[]), "File");
        // 旧快照仍保持切换前的文案。
        assert_eq!(first.t("menu_file", &[]), "文件");
    }

    #[test]
    fn locale_preferences_match_builtin_languages() {
        assert_eq!(language_id_from_preferences(["zh_CN.UTF-8"]), "zh-CN");
        assert_eq!(language_id_from_preferences(["en-US"]), "en-US");
        assert_eq!(language_id_from_preferences(["fr-FR"]), "en-US");
    }
}
