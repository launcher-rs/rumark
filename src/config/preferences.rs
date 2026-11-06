//! 持久化应用偏好与偏好设置窗口。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use rgpui::components::Select;
use rgpui::components::{HotkeyListInput, HotkeyListInputState, HotkeyValue};
use rgpui::title_bar::TitleBar;
use rgpui::*;
use serde::{Deserialize, Serialize};

use super::{RumarkConfigDirs, read_recent_files};
use crate::app_menu::rumark_window_options;
use crate::atomic_file::write_text;
use crate::components::{
    ShortcutCategory, ShortcutCommand, ShortcutDefinition, install_keybindings,
    normalize_shortcut_config, normalize_shortcut_keys, resolved_shortcut_keys,
    shortcut_conflict_for, shortcut_definitions,
};
use crate::i18n::{I18nManager, language_id_from_preferences};
use crate::theme::{ThemeCatalogEntry, ThemeDimensions, ThemeTypography};

const FALLBACK_THEME_ID: &str = "rumark-light";
const FALLBACK_LANGUAGE_ID: &str = "en-US";

/// 配置文件格式版本。
///
/// 缺失该字段的旧配置按版本 1 处理；高于当前版本说明配置由更新的版本写入，
/// 此时不得回写默认值，否则新版本写入的字段会被抹掉。
const PREFERENCES_FILE_VERSION: u32 = 1;

/// 配置损坏时的恢复信息。
#[derive(Debug)]
pub(crate) struct PreferencesRecovery {
    /// 损坏配置的原路径。
    pub damaged_path: PathBuf,
    /// 原文件的备份路径。
    pub backup_path: PathBuf,
}

/// 待展示的配置恢复提示。
///
/// 启动时发现配置文件损坏、已备份并重建默认配置后登记在此，由首个窗口消费并弹窗告知。
#[derive(Default)]
pub struct PendingConfigRecovery {
    recovery: Option<PreferencesRecovery>,
}

impl Global for PendingConfigRecovery {}

impl PendingConfigRecovery {
    /// 登记一次配置恢复。
    pub fn set(cx: &mut App, recovery: PreferencesRecovery) {
        cx.set_global(Self {
            recovery: Some(recovery),
        });
    }

    /// 取出并清空待展示的恢复提示。
    ///
    /// 未登记过恢复提示时全局不存在，直接返回 `None`。
    pub fn take(cx: &mut App) -> Option<PreferencesRecovery> {
        if !cx.has_global::<Self>() {
            return None;
        }
        let mut recovery = None;
        cx.update_global::<Self, _>(|global, _cx| recovery = global.recovery.take());
        recovery
    }
}

/// 显示在状态栏的用户可配置按钮。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StatusBarButton {
    pub id: String,
    pub label: String,
    pub action_id: String,
}

/// 状态栏可见性与组件开关。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatusBarPreferences {
    pub enabled: bool,
    pub show_word_count: bool,
    pub show_cursor_position: bool,
    pub show_sidebar_toggle: bool,
    pub show_mode_switch: bool,
    pub custom_buttons: Vec<StatusBarButton>,
}

impl Default for StatusBarPreferences {
    /// 返回默认全开的状态栏偏好。
    fn default() -> Self {
        Self {
            enabled: true,
            show_word_count: true,
            show_cursor_position: true,
            show_sidebar_toggle: true,
            show_mode_switch: true,
            custom_buttons: Vec::new(),
        }
    }
}

/// 存于 `config.toml` 的启动文档选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupOpenPreference {
    NewFile,
    LastOpenedFile,
}

impl StartupOpenPreference {
    /// 转为配置文件中的字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NewFile => "new_file",
            Self::LastOpenedFile => "last_opened_file",
        }
    }

    /// 从配置文件字符串解析启动选项。
    fn from_str(value: &str) -> Self {
        match value {
            "last_opened_file" => Self::LastOpenedFile,
            _ => Self::NewFile,
        }
    }
}

/// 粘贴剪贴板图片时应先存到何处。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImagePasteBehavior {
    None,
    CopyToDocumentFolder,
    CopyToAssetsFolder,
    CopyToNamedAssetsFolder,
    EmbedAsDataUri,
}

impl ImagePasteBehavior {
    /// 转为配置文件中的字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CopyToDocumentFolder => "copy_to_document_folder",
            Self::CopyToAssetsFolder => "copy_to_assets_folder",
            Self::CopyToNamedAssetsFolder => "copy_to_named_assets_folder",
            Self::EmbedAsDataUri => "embed_as_data_uri",
        }
    }

    /// 从配置文件字符串解析图片粘贴行为。
    fn from_str(value: &str) -> Self {
        match value {
            "copy_to_document_folder" => Self::CopyToDocumentFolder,
            "copy_to_assets_folder" => Self::CopyToAssetsFolder,
            "copy_to_named_assets_folder" => Self::CopyToNamedAssetsFolder,
            "embed_as_data_uri" => Self::EmbedAsDataUri,
            _ => Self::None,
        }
    }
}

/// HTML 导出时使用的配色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExportThemeMode {
    /// 跟随编辑器当前主题。
    FollowEditor,
    /// 固定浅色。
    Light,
    /// 固定深色。
    Dark,
}

impl ExportThemeMode {
    /// 转为配置文件中的字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::FollowEditor => "follow_editor",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// 从配置文件字符串解析导出配色。
    fn from_str(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::FollowEditor,
        }
    }
}

/// 保存在应用配置目录下的用户偏好。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppPreferences {
    pub(crate) startup_open: StartupOpenPreference,
    pub(crate) default_language_id: String,
    pub(crate) default_theme_id: String,
    pub(crate) table_headers_visible: bool,
    pub(crate) image_paste_behavior: ImagePasteBehavior,
    pub(crate) export_include_toc: bool,
    pub(crate) export_theme: ExportThemeMode,
    pub(crate) keybindings: BTreeMap<String, Vec<String>>,
    pub(crate) status_bar: StatusBarPreferences,
}

impl Default for AppPreferences {
    /// 返回默认应用偏好。
    fn default() -> Self {
        Self {
            startup_open: StartupOpenPreference::NewFile,
            default_language_id: FALLBACK_LANGUAGE_ID.into(),
            default_theme_id: FALLBACK_THEME_ID.into(),
            table_headers_visible: true,
            image_paste_behavior: ImagePasteBehavior::None,
            export_include_toc: true,
            export_theme: ExportThemeMode::FollowEditor,
            keybindings: BTreeMap::new(),
            status_bar: StatusBarPreferences::default(),
        }
    }
}

/// 状态栏运行时设置快照。
struct StatusBarSettings {
    status_bar_enabled: bool,
    status_bar_show_word_count: bool,
    status_bar_show_cursor_position: bool,
    status_bar_show_sidebar_toggle: bool,
    status_bar_show_mode_switch: bool,
    custom_buttons: Vec<StatusBarButton>,
}

/// 与 [`AppPreferences`] 同步的运行时编辑器设置，供渲染路径免磁盘读取。
pub struct EditorSettings {
    table_headers_visible: bool,
    status_bar_settings: StatusBarSettings,
}

impl Global for EditorSettings {}

impl EditorSettings {
    /// 初始化全局编辑器设置。
    pub fn init(cx: &mut App, table_headers_visible: bool) {
        let status_bar = read_app_preferences()
            .ok()
            .map(|p| p.status_bar)
            .unwrap_or_default();
        Self::set_global(cx, table_headers_visible, &status_bar);
    }

    /// 写入全局编辑器设置。
    fn set_global(cx: &mut App, table_headers_visible: bool, status_bar: &StatusBarPreferences) {
        cx.set_global(Self {
            table_headers_visible,
            status_bar_settings: StatusBarSettings {
                status_bar_enabled: status_bar.enabled,
                status_bar_show_word_count: status_bar.show_word_count,
                status_bar_show_cursor_position: status_bar.show_cursor_position,
                status_bar_show_sidebar_toggle: status_bar.show_sidebar_toggle,
                status_bar_show_mode_switch: status_bar.show_mode_switch,
                custom_buttons: status_bar.custom_buttons.clone(),
            },
        });
    }

    /// 表格首行是否按表头样式渲染，未安装全局时默认开启。
    pub fn table_headers_visible(cx: &App) -> bool {
        cx.try_global::<Self>()
            .map(|settings| settings.table_headers_visible)
            .unwrap_or(true)
    }

    /// 设置表头显示并持久化到偏好文件。
    pub fn set_table_headers_visible(cx: &mut App, table_headers_visible: bool) {
        let status_bar = cx
            .try_global::<Self>()
            .map(|s| StatusBarPreferences {
                enabled: s.status_bar_settings.status_bar_enabled,
                show_word_count: s.status_bar_settings.status_bar_show_word_count,
                show_cursor_position: s.status_bar_settings.status_bar_show_cursor_position,
                show_sidebar_toggle: s.status_bar_settings.status_bar_show_sidebar_toggle,
                show_mode_switch: s.status_bar_settings.status_bar_show_mode_switch,
                custom_buttons: s.status_bar_settings.custom_buttons.clone(),
            })
            .unwrap_or_default();
        Self::set_global(cx, table_headers_visible, &status_bar);
        match read_app_preferences() {
            Ok(mut preferences) => {
                preferences.table_headers_visible = table_headers_visible;
                if let Err(err) = save_app_preferences(&preferences) {
                    eprintln!("failed to save table header preference: {err}");
                }
            }
            Err(err) => eprintln!("failed to read table header preference: {err}"),
        }
    }

    /// 读取当前状态栏偏好快照。
    pub fn status_bar_preferences(cx: &App) -> StatusBarPreferences {
        cx.try_global::<Self>()
            .map(|s| StatusBarPreferences {
                enabled: s.status_bar_settings.status_bar_enabled,
                show_word_count: s.status_bar_settings.status_bar_show_word_count,
                show_cursor_position: s.status_bar_settings.status_bar_show_cursor_position,
                show_sidebar_toggle: s.status_bar_settings.status_bar_show_sidebar_toggle,
                show_mode_switch: s.status_bar_settings.status_bar_show_mode_switch,
                custom_buttons: s.status_bar_settings.custom_buttons.clone(),
            })
            .unwrap_or_default()
    }
}

#[derive(Serialize)]
struct PreferencesFile {
    version: u32,
    startup: StartupPreferencesFile,
    language: LanguagePreferencesFile,
    theme: ThemePreferencesFile,
    editor: EditorPreferencesFile,
    export: ExportPreferencesFile,
    status_bar: StatusBarPreferencesFile,
    keybindings: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
struct StartupPreferencesFile {
    open: String,
}

#[derive(Serialize)]
struct EditorPreferencesFile {
    table_headers_visible: bool,
    image_paste_behavior: String,
}

#[derive(Serialize)]
struct ExportPreferencesFile {
    include_toc: bool,
    theme: String,
}

#[derive(Serialize)]
struct LanguagePreferencesFile {
    default_language_id: String,
}

#[derive(Serialize)]
struct ThemePreferencesFile {
    default_theme_id: String,
}

#[derive(Serialize)]
struct StatusBarPreferencesFile {
    enabled: bool,
    show_word_count: bool,
    show_cursor_position: bool,
    show_sidebar_toggle: bool,
    show_mode_switch: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    custom_buttons: Vec<StatusBarButton>,
}

impl From<&StatusBarPreferences> for StatusBarPreferencesFile {
    /// 由运行时偏好构造文件存储形态。
    fn from(value: &StatusBarPreferences) -> Self {
        Self {
            enabled: value.enabled,
            show_word_count: value.show_word_count,
            show_cursor_position: value.show_cursor_position,
            show_sidebar_toggle: value.show_sidebar_toggle,
            show_mode_switch: value.show_mode_switch,
            custom_buttons: value.custom_buttons.clone(),
        }
    }
}

impl From<&AppPreferences> for PreferencesFile {
    /// 由应用偏好构造文件存储形态。
    fn from(value: &AppPreferences) -> Self {
        Self {
            version: PREFERENCES_FILE_VERSION,
            startup: StartupPreferencesFile {
                open: value.startup_open.as_str().into(),
            },
            language: LanguagePreferencesFile {
                default_language_id: value.default_language_id.clone(),
            },
            theme: ThemePreferencesFile {
                default_theme_id: value.default_theme_id.clone(),
            },
            editor: EditorPreferencesFile {
                table_headers_visible: value.table_headers_visible,
                image_paste_behavior: value.image_paste_behavior.as_str().into(),
            },
            export: ExportPreferencesFile {
                include_toc: value.export_include_toc,
                theme: value.export_theme.as_str().into(),
            },
            status_bar: StatusBarPreferencesFile::from(&value.status_bar),
            keybindings: normalize_shortcut_config(&value.keybindings),
        }
    }
}

/// 读取应用偏好。
pub(crate) fn read_app_preferences() -> anyhow::Result<AppPreferences> {
    read_app_preferences_with_dirs(&RumarkConfigDirs::from_system()?)
}

/// 用指定目录集读取应用偏好。
///
/// 纯读取：配置文件损坏时返回错误而不改写文件，由调用方决定如何处理。
pub(crate) fn read_app_preferences_with_dirs(
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<AppPreferences> {
    let path = dirs.app_config_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppPreferences::default());
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read '{}'", path.display()));
        }
    };
    let value = toml::from_str::<toml::Value>(&text)
        .with_context(|| format!("failed to parse '{}'", path.display()))?;
    ensure_supported_version(&path, &value)?;
    Ok(app_preferences_from_toml_value(
        &value,
        FALLBACK_LANGUAGE_ID,
    ))
}

/// 校验配置文件版本高于当前程序时立即报错，避免回写默认值抹掉新版本字段。
fn ensure_supported_version(path: &std::path::Path, value: &toml::Value) -> anyhow::Result<()> {
    let version = preferences_file_version(value);
    if version > PREFERENCES_FILE_VERSION {
        anyhow::bail!(
            "configuration '{}' has version {} but this build only supports up to {}",
            path.display(),
            version,
            PREFERENCES_FILE_VERSION
        );
    }
    Ok(())
}

/// 从配置值读取格式版本，缺失时按版本 1 处理。
fn preferences_file_version(value: &toml::Value) -> u32 {
    value
        .get("version")
        .and_then(|version| version.as_integer())
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(1)
}

/// 把损坏的配置文件备份为 `.bak`，返回备份路径。
///
/// 直接丢弃损坏配置会让用户的键位、主题与语言设置被随后写回的默认值覆盖，
/// 且原文件不可恢复；备份让用户能自行找回内容。
fn backup_damaged_preferences(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let backup_path = path.with_file_name(format!("{file_name}.bak"));
    crate::atomic_file::copy_file(path, &backup_path)
        .with_context(|| format!("failed to back up '{}'", path.display()))?;
    Ok(backup_path)
}

/// 加载偏好，不存在时按系统语言创建。
///
/// 返回 `(偏好, 配置恢复信息)`：配置损坏并已备份重建时恢复信息为 `Some`。
pub(crate) fn load_or_create_app_preferences()
-> anyhow::Result<(AppPreferences, Option<PreferencesRecovery>)> {
    let dirs = RumarkConfigDirs::from_system()?;
    load_or_create_app_preferences_with_dirs_and_locales(&dirs, sys_locale::get_locales())
}

/// 从 TOML 值解析应用偏好。
fn app_preferences_from_toml_value(
    value: &toml::Value,
    fallback_language_id: &str,
) -> AppPreferences {
    let startup_open = value
        .get("startup")
        .and_then(|startup| startup.get("open"))
        .and_then(|open| open.as_str())
        .map(StartupOpenPreference::from_str)
        .unwrap_or(StartupOpenPreference::NewFile);
    let default_language_id = value
        .get("language")
        .and_then(|language| language.get("default_language_id"))
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(fallback_language_id)
        .to_string();
    let default_theme_id = value
        .get("theme")
        .and_then(|theme| theme.get("default_theme_id"))
        .and_then(|id| id.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(FALLBACK_THEME_ID)
        .to_string();
    let keybindings = value
        .get("keybindings")
        .and_then(|keybindings| keybindings.as_table())
        .map(|table| {
            table
                .iter()
                .filter_map(|(key, value)| {
                    let keys = value
                        .as_array()?
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect::<Vec<_>>();
                    Some((key.clone(), keys))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .map(|keybindings| normalize_shortcut_config(&keybindings))
        .unwrap_or_default();

    let table_headers_visible = value
        .get("editor")
        .and_then(|editor| editor.get("table_headers_visible"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let image_paste_behavior = value
        .get("editor")
        .and_then(|editor| editor.get("image_paste_behavior"))
        .and_then(|value| value.as_str())
        .map(ImagePasteBehavior::from_str)
        .unwrap_or(ImagePasteBehavior::None);
    let export_include_toc = value
        .get("export")
        .and_then(|export| export.get("include_toc"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let export_theme = value
        .get("export")
        .and_then(|export| export.get("theme"))
        .and_then(|value| value.as_str())
        .map(ExportThemeMode::from_str)
        .unwrap_or(ExportThemeMode::FollowEditor);

    let status_bar = value
        .get("status_bar")
        .map(|sb| {
            let enabled = sb.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let show_word_count = sb
                .get("show_word_count")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let show_cursor_position = sb
                .get("show_cursor_position")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let show_sidebar_toggle = sb
                .get("show_sidebar_toggle")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let show_mode_switch = sb
                .get("show_mode_switch")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let custom_buttons = sb
                .get("custom_buttons")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let id = item.get("id")?.as_str()?.to_string();
                            let label = item.get("label")?.as_str()?.to_string();
                            Some(StatusBarButton {
                                id,
                                label,
                                action_id: item
                                    .get("action_id")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            StatusBarPreferences {
                enabled,
                show_word_count,
                show_cursor_position,
                show_sidebar_toggle,
                show_mode_switch,
                custom_buttons,
            }
        })
        .unwrap_or_default();

    AppPreferences {
        startup_open,
        default_language_id,
        default_theme_id,
        table_headers_visible,
        image_paste_behavior,
        export_include_toc,
        export_theme,
        keybindings,
        status_bar,
    }
}

/// 按系统语言列表推断默认语言。
fn detected_language_id_from_locales<I, S>(locales: I) -> &'static str
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    language_id_from_preferences(locales)
}

/// 用指定目录与语言列表加载或创建偏好。
///
/// 配置文件损坏时先备份原文件再重建默认配置，并返回恢复信息供上层告知用户；
/// 版本高于当前程序时不改写文件，直接报错。
fn load_or_create_app_preferences_with_dirs_and_locales<I, S>(
    dirs: &RumarkConfigDirs,
    locales: I,
) -> anyhow::Result<(AppPreferences, Option<PreferencesRecovery>)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let detected_language_id = detected_language_id_from_locales(locales);
    let path = dirs.app_config_file();
    let defaults = AppPreferences {
        default_language_id: detected_language_id.into(),
        ..AppPreferences::default()
    };
    let (preferences, recovery) = match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<toml::Value>(&text) {
            Ok(value) => {
                ensure_supported_version(&path, &value)?;
                (
                    app_preferences_from_toml_value(&value, detected_language_id),
                    None,
                )
            }
            Err(_) => {
                let backup_path = backup_damaged_preferences(&path)?;
                (
                    defaults,
                    Some(PreferencesRecovery {
                        damaged_path: path.clone(),
                        backup_path,
                    }),
                )
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (defaults, None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read '{}'", path.display()));
        }
    };
    save_app_preferences_with_dirs(&preferences, dirs)?;
    Ok((preferences, recovery))
}

/// 保存应用偏好。
pub(crate) fn save_app_preferences(preferences: &AppPreferences) -> anyhow::Result<()> {
    save_app_preferences_with_dirs(preferences, &RumarkConfigDirs::from_system()?)
}

/// 用指定目录集保存应用偏好。
pub(crate) fn save_app_preferences_with_dirs(
    preferences: &AppPreferences,
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<()> {
    let path = dirs.app_config_file();
    let text = toml::to_string_pretty(&PreferencesFile::from(preferences))?;
    write_text(&path, &text).with_context(|| format!("failed to write '{}'", path.display()))
}

/// 返回首个仍存在的最近 Markdown 文件。
pub(crate) fn first_existing_recent_markdown_file() -> Option<PathBuf> {
    let recent_files = read_recent_files().ok()?;
    recent_files.into_iter().find(|path| path.is_file())
}

/// 应用配置的语言并持久化。
pub(crate) fn apply_configured_language(cx: &mut App, language_id: &str) -> anyhow::Result<bool> {
    let mut applied = false;
    let changed = cx.update_global::<I18nManager, _>(|i18n_manager, _cx| {
        let changed = i18n_manager.set_language_by_id(language_id);
        applied = changed || i18n_manager.active_language_id() == language_id;
        changed
    });
    if !applied {
        return Ok(false);
    }
    update_app_preferences(|preferences| {
        preferences.default_language_id = language_id.into();
    })?;
    Ok(changed)
}

/// 应用配置的主题并持久化。
pub(crate) fn apply_configured_theme(cx: &mut App, theme_id: &str) -> anyhow::Result<bool> {
    let changed = crate::theme::set_theme_by_id(cx, theme_id);
    let applied = changed || crate::theme::active_theme_id(cx) == theme_id;
    if !applied {
        return Ok(false);
    }
    update_app_preferences(|preferences| {
        preferences.default_theme_id = theme_id.into();
    })?;
    Ok(changed)
}

/// 导入主题包并应用其中首个主题。
pub(crate) fn import_theme_config_and_select(
    cx: &mut App,
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<Vec<String>> {
    let names = crate::theme::load_theme_config(cx, path)?;
    let Some(first) = names.first().cloned() else {
        anyhow::bail!("theme pack contains no themes");
    };
    apply_configured_theme(cx, &first)?;
    Ok(names)
}

/// 导入语言包并设为当前语言。
pub(crate) fn import_language_config_and_select(
    cx: &mut App,
    path: impl AsRef<std::path::Path>,
) -> anyhow::Result<String> {
    let imported_id = cx.update_global::<I18nManager, _>(|i18n_manager, _cx| {
        i18n_manager.load_language_config(path)
    })?;
    update_app_preferences(|preferences| {
        preferences.default_language_id = imported_id.clone();
    })?;
    Ok(imported_id)
}

/// 保存偏好设置窗口的修改。
pub(crate) fn save_preferences_from_window(
    startup_open: StartupOpenPreference,
    default_theme_id: &str,
    image_paste_behavior: ImagePasteBehavior,
    export_include_toc: bool,
    export_theme: ExportThemeMode,
    keybindings: BTreeMap<String, Vec<String>>,
    status_bar: &StatusBarPreferences,
) -> anyhow::Result<AppPreferences> {
    let dirs = RumarkConfigDirs::from_system()?;
    save_preferences_from_window_with_dirs(
        startup_open,
        default_theme_id,
        image_paste_behavior,
        export_include_toc,
        export_theme,
        keybindings,
        status_bar,
        &dirs,
    )
}

/// 用指定目录集保存偏好设置窗口的修改。
#[allow(clippy::too_many_arguments)]
fn save_preferences_from_window_with_dirs(
    startup_open: StartupOpenPreference,
    default_theme_id: &str,
    image_paste_behavior: ImagePasteBehavior,
    export_include_toc: bool,
    export_theme: ExportThemeMode,
    keybindings: BTreeMap<String, Vec<String>>,
    status_bar: &StatusBarPreferences,
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<AppPreferences> {
    let (mut preferences, _recovery) =
        load_or_create_app_preferences_with_dirs_and_locales(dirs, sys_locale::get_locales())?;
    preferences.startup_open = startup_open;
    preferences.default_theme_id = default_theme_id.into();
    preferences.image_paste_behavior = image_paste_behavior;
    preferences.export_include_toc = export_include_toc;
    preferences.export_theme = export_theme;
    preferences.keybindings = normalize_shortcut_config(&keybindings);
    // 偏好窗口不管理自定义状态栏按钮，保留磁盘上的既有配置，避免保存其他设置时把它们清空。
    let mut status_bar = status_bar.clone();
    status_bar.custom_buttons = preferences.status_bar.custom_buttons.clone();
    preferences.status_bar = status_bar;
    save_app_preferences_with_dirs(&preferences, dirs)?;
    Ok(preferences)
}

/// 更新并持久化应用偏好。
fn update_app_preferences(
    update: impl FnOnce(&mut AppPreferences),
) -> anyhow::Result<AppPreferences> {
    let (mut preferences, _recovery) = load_or_create_app_preferences()?;
    update(&mut preferences);
    save_app_preferences(&preferences)?;
    Ok(preferences)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreferencesNav {
    File,
    Theme,
    Image,
    Export,
    Shortcuts,
    StatusBar,
}

/// 独立的偏好设置窗口视图。
pub(crate) struct PreferencesWindow {
    nav: PreferencesNav,
    startup_open: StartupOpenPreference,
    selected_theme_id: String,
    image_paste_behavior: ImagePasteBehavior,
    export_include_toc: bool,
    export_theme: ExportThemeMode,
    keybindings: BTreeMap<String, Vec<String>>,
    saved_startup_open: StartupOpenPreference,
    saved_theme_id: String,
    saved_image_paste_behavior: ImagePasteBehavior,
    saved_export_include_toc: bool,
    saved_export_theme: ExportThemeMode,
    saved_keybindings: BTreeMap<String, Vec<String>>,
    theme_options: Vec<ThemeCatalogEntry>,
    focus_handle: FocusHandle,
    /// 各命令的多值录制输入状态（框架 `HotkeyListInput` 的绑定实体，录制态由组件自持）。
    hotkey_inputs: BTreeMap<ShortcutCommand, Entity<HotkeyListInputState>>,
    shortcut_error: Option<String>,
    status_bar_enabled: bool,
    status_bar_show_word_count: bool,
    status_bar_show_cursor_position: bool,
    status_bar_show_sidebar_toggle: bool,
    status_bar_show_mode_switch: bool,
    saved_status_bar_enabled: bool,
    saved_status_bar_show_word_count: bool,
    saved_status_bar_show_cursor_position: bool,
    saved_status_bar_show_sidebar_toggle: bool,
    saved_status_bar_show_mode_switch: bool,
}

/// 快捷键字符串转框架值（解析失败的条目丢弃，与旧规范化语义一致）。
fn hotkey_values_from_keys(keys: &[String]) -> Vec<HotkeyValue> {
    keys.iter()
        .filter_map(|key| {
            Keystroke::parse(key)
                .ok()
                .and_then(|stroke| HotkeyValue::from_keystroke(&stroke))
        })
        .collect()
}

/// 框架值转快捷键字符串（经 `Keystroke::unparse` 规范形，可直接喂给规范化与冲突检查）。
fn hotkey_keys_from_values(values: &[HotkeyValue]) -> Vec<String> {
    values
        .iter()
        .map(|value| {
            Keystroke {
                modifiers: value.modifiers,
                key: value.key.clone(),
                key_char: None,
            }
            .unparse()
        })
        .collect()
}

impl PreferencesWindow {
    /// 从偏好与主题选项创建窗口状态。
    fn new(
        preferences: AppPreferences,
        theme_options: Vec<ThemeCatalogEntry>,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_theme_id = if theme_options
            .iter()
            .any(|entry| entry.id == preferences.default_theme_id)
        {
            preferences.default_theme_id
        } else {
            FALLBACK_THEME_ID.into()
        };
        let startup_open = preferences.startup_open;
        let image_paste_behavior = preferences.image_paste_behavior;
        let export_include_toc = preferences.export_include_toc;
        let export_theme = preferences.export_theme;
        let keybindings = preferences.keybindings;
        let mut hotkey_inputs = BTreeMap::new();
        for definition in shortcut_definitions() {
            let values =
                hotkey_values_from_keys(&resolved_shortcut_keys(&keybindings, definition.command));
            hotkey_inputs.insert(
                definition.command,
                cx.new(|cx| HotkeyListInputState::with_hotkeys(cx, values)),
            );
        }
        Self {
            nav: PreferencesNav::File,
            startup_open,
            selected_theme_id: selected_theme_id.clone(),
            image_paste_behavior,
            export_include_toc,
            export_theme,
            keybindings: keybindings.clone(),
            saved_startup_open: startup_open,
            saved_theme_id: selected_theme_id,
            saved_image_paste_behavior: image_paste_behavior,
            saved_export_include_toc: export_include_toc,
            saved_export_theme: export_theme,
            saved_keybindings: keybindings,
            theme_options,
            focus_handle: cx.focus_handle(),
            hotkey_inputs,
            shortcut_error: None,
            status_bar_enabled: preferences.status_bar.enabled,
            status_bar_show_word_count: preferences.status_bar.show_word_count,
            status_bar_show_cursor_position: preferences.status_bar.show_cursor_position,
            status_bar_show_sidebar_toggle: preferences.status_bar.show_sidebar_toggle,
            status_bar_show_mode_switch: preferences.status_bar.show_mode_switch,
            saved_status_bar_enabled: preferences.status_bar.enabled,
            saved_status_bar_show_word_count: preferences.status_bar.show_word_count,
            saved_status_bar_show_cursor_position: preferences.status_bar.show_cursor_position,
            saved_status_bar_show_sidebar_toggle: preferences.status_bar.show_sidebar_toggle,
            saved_status_bar_show_mode_switch: preferences.status_bar.show_mode_switch,
        }
    }

    /// 判断草稿相对已保存值是否有未保存修改。
    fn has_unsaved_changes(&self) -> bool {
        self.startup_open != self.saved_startup_open
            || self.selected_theme_id != self.saved_theme_id
            || self.image_paste_behavior != self.saved_image_paste_behavior
            || self.export_include_toc != self.saved_export_include_toc
            || self.export_theme != self.saved_export_theme
            || normalize_shortcut_config(&self.keybindings)
                != normalize_shortcut_config(&self.saved_keybindings)
            || self.status_bar_enabled != self.saved_status_bar_enabled
            || self.status_bar_show_word_count != self.saved_status_bar_show_word_count
            || self.status_bar_show_cursor_position != self.saved_status_bar_show_cursor_position
            || self.status_bar_show_sidebar_toggle != self.saved_status_bar_show_sidebar_toggle
            || self.status_bar_show_mode_switch != self.saved_status_bar_show_mode_switch
    }

    /// 停止全部快捷键录制（切换偏好页时调用，避免后台残留录制态）。
    fn stop_all_hotkey_recordings(&mut self, cx: &mut Context<Self>) {
        for state in self.hotkey_inputs.values() {
            state.update(cx, |state, cx| {
                if state.is_recording() {
                    state.stop_recording(cx);
                }
            });
        }
    }

    /// 切换到文件偏好页。
    fn set_nav_file(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::File;
        self.stop_all_hotkey_recordings(cx);
        cx.notify();
    }

    /// 切换到主题偏好页。
    fn set_nav_theme(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::Theme;
        self.stop_all_hotkey_recordings(cx);
        cx.notify();
    }

    /// 切换到图片偏好页。
    fn set_nav_image(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::Image;
        self.stop_all_hotkey_recordings(cx);
        cx.notify();
    }

    /// 切换到导出偏好页。
    fn set_nav_export(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::Export;
        self.stop_all_hotkey_recordings(cx);
        cx.notify();
    }

    /// 切换到快捷键偏好页。
    fn set_nav_shortcuts(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::Shortcuts;
        self.shortcut_error = None;
        cx.notify();
    }

    /// 切换到状态栏偏好页。
    fn set_nav_status_bar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.nav = PreferencesNav::StatusBar;
        self.stop_all_hotkey_recordings(cx);
        cx.notify();
    }

    /// 取消修改并关闭偏好窗口。
    fn cancel(&mut self, _: &ClickEvent, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    /// 保存草稿偏好并应用到全局。
    fn save(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_unsaved_changes() {
            return;
        }

        let preferences = match save_preferences_from_window(
            self.startup_open,
            &self.selected_theme_id,
            self.image_paste_behavior,
            self.export_include_toc,
            self.export_theme,
            self.keybindings.clone(),
            &StatusBarPreferences {
                enabled: self.status_bar_enabled,
                show_word_count: self.status_bar_show_word_count,
                show_cursor_position: self.status_bar_show_cursor_position,
                show_sidebar_toggle: self.status_bar_show_sidebar_toggle,
                show_mode_switch: self.status_bar_show_mode_switch,
                custom_buttons: Vec::new(),
            },
        ) {
            Ok(preferences) => preferences,
            Err(err) => {
                // 取保存失败弹窗文案（借用及时释放，避免与后续弹窗的可变借用冲突）。
                let (ok, title) = {
                    let i18n = cx.global::<I18nManager>().snapshot();
                    (
                        i18n.t("dialog_ok", &[]),
                        i18n.t("message_save_preferences_failed", &[]),
                    )
                };
                // 对话框 future 必须轮询才会弹出：转交后台任务等待用户响应。
                let title = title.clone();
                let detail = err.to_string();
                let ok = ok.clone();
                let buttons = [ok.as_str()];
                let prompt =
                    window.prompt(PromptLevel::Critical, &title, Some(&detail), &buttons, cx);
                cx.spawn(async move |_, _| {
                    let _ = prompt.await;
                })
                .detach();
                return;
            }
        };

        self.apply_saved_preferences(preferences, window, cx);
    }

    /// 将已保存偏好应用到主题、键位与状态栏。
    fn apply_saved_preferences(
        &mut self,
        preferences: AppPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::theme::set_theme_by_id(cx, &preferences.default_theme_id) {
            crate::theme::set_theme_by_id(cx, FALLBACK_THEME_ID);
        }
        // 清空会连带移除输入框/树等框架绑定（链接弹窗回退键即因此失效），重装时一并恢复。
        cx.clear_key_bindings();
        rgpui::input_ui::init(cx);
        rgpui::tree::init(cx);
        install_keybindings(cx, &preferences.keybindings);
        crate::app_menu::install_menus(cx);
        cx.update_global::<EditorSettings, _>(|settings, _cx| {
            settings.status_bar_settings.status_bar_enabled = preferences.status_bar.enabled;
            settings.status_bar_settings.status_bar_show_word_count =
                preferences.status_bar.show_word_count;
            settings.status_bar_settings.status_bar_show_cursor_position =
                preferences.status_bar.show_cursor_position;
            settings.status_bar_settings.status_bar_show_sidebar_toggle =
                preferences.status_bar.show_sidebar_toggle;
            settings.status_bar_settings.status_bar_show_mode_switch =
                preferences.status_bar.show_mode_switch;
        });
        cx.refresh_windows();
        window.activate_window();
        self.focus_handle.focus(window, cx);
        self.saved_startup_open = self.startup_open;
        self.saved_theme_id = self.selected_theme_id.clone();
        self.saved_image_paste_behavior = self.image_paste_behavior;
        self.saved_export_include_toc = self.export_include_toc;
        self.saved_export_theme = self.export_theme;
        self.saved_keybindings = normalize_shortcut_config(&self.keybindings);
        self.saved_status_bar_enabled = self.status_bar_enabled;
        self.saved_status_bar_show_word_count = self.status_bar_show_word_count;
        self.saved_status_bar_show_cursor_position = self.status_bar_show_cursor_position;
        self.saved_status_bar_show_sidebar_toggle = self.status_bar_show_sidebar_toggle;
        self.saved_status_bar_show_mode_switch = self.status_bar_show_mode_switch;
        cx.notify();
    }

    /// 构造左侧导航按钮（框架 `Button` 承载选中态）。
    fn nav_button(
        &self,
        id: &'static str,
        label: String,
        selected: bool,
        _theme: &Theme,
        on_click: fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let button = Button::new(id)
            .label(label)
            .w(px(156.0))
            .on_click(cx.listener(on_click));
        if selected {
            button.primary()
        } else {
            button.secondary()
        }
    }

    /// 构造带标签的设置行。
    fn labeled_row(&self, label: &str, control: impl IntoElement, theme: &Theme) -> Div {
        let t = &ThemeTypography::DEFAULT;
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .w(px(280.0))
                    .text_size(px(t.dialog_body_size))
                    .font_weight(t.dialog_button_weight)
                    .text_color(theme.popover_foreground)
                    .child(SharedString::from(label.to_string())),
            )
            .child(control)
    }

    /// 渲染启动选项页（下拉框由框架 `Select` 组件承载）。
    fn render_startup_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let options = [
            StartupOpenPreference::NewFile,
            StartupOpenPreference::LastOpenedFile,
        ];
        let labels: Vec<SharedString> = options
            .iter()
            .map(|option| {
                let text = match option {
                    StartupOpenPreference::NewFile => i18n.t("pref_startup_new_file", &[]),
                    StartupOpenPreference::LastOpenedFile => {
                        i18n.t("pref_startup_last_opened", &[])
                    }
                };
                SharedString::from(text)
            })
            .collect();
        let selected = options
            .iter()
            .position(|option| *option == self.startup_open);
        let entity = cx.entity();
        let select = Select::new(labels)
            .id("preferences-startup-dropdown")
            .selected(selected)
            .w(px(280.0))
            .on_change(move |ix, _, _, cx| {
                if let Some(option) = options.get(ix).copied() {
                    entity.update(cx, |this, cx| {
                        this.startup_open = option;
                        cx.notify();
                    });
                }
            });
        self.labeled_row(&i18n.t("pref_startup_label", &[]), select, theme)
    }

    /// 渲染主题选择页（下拉框由框架 `Select` 组件承载）。
    fn render_theme_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let entries = self.theme_options.clone();
        let labels: Vec<SharedString> = entries
            .iter()
            .map(|entry| entry.name.clone().into())
            .collect();
        let selected = entries
            .iter()
            .position(|entry| entry.id == self.selected_theme_id);
        let entity = cx.entity();
        let select = Select::new(labels)
            .id("preferences-theme-dropdown")
            .selected(selected)
            .w(px(280.0))
            .on_change(move |ix, _, _, cx| {
                if let Some(entry) = entries.get(ix) {
                    let theme_id = entry.id.clone();
                    entity.update(cx, |this, cx| {
                        this.selected_theme_id = theme_id;
                        cx.notify();
                    });
                }
            });
        self.labeled_row(&i18n.t("pref_theme_local", &[]), select, theme)
    }

    /// 返回图片粘贴行为的显示文案。
    fn image_paste_behavior_label(
        behavior: ImagePasteBehavior,
        i18n: &crate::i18n::I18nSnapshot,
    ) -> String {
        match behavior {
            ImagePasteBehavior::None => i18n.t("pref_image_paste_none", &[]),
            ImagePasteBehavior::CopyToDocumentFolder => {
                i18n.t("pref_image_paste_document_folder", &[])
            }
            ImagePasteBehavior::CopyToAssetsFolder => i18n.t("pref_image_paste_assets_folder", &[]),
            ImagePasteBehavior::CopyToNamedAssetsFolder => {
                i18n.t("pref_image_paste_named_assets_folder", &[])
            }
            ImagePasteBehavior::EmbedAsDataUri => i18n.t("pref_image_paste_embed_data_uri", &[]),
        }
    }

    /// 渲染图片插入行为页（下拉框由框架 `Select` 组件承载）。
    fn render_image_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let options = [
            ImagePasteBehavior::None,
            ImagePasteBehavior::CopyToDocumentFolder,
            ImagePasteBehavior::CopyToAssetsFolder,
            ImagePasteBehavior::CopyToNamedAssetsFolder,
            ImagePasteBehavior::EmbedAsDataUri,
        ];
        let labels: Vec<SharedString> = options
            .iter()
            .map(|behavior| Self::image_paste_behavior_label(*behavior, i18n).into())
            .collect();
        let selected = options
            .iter()
            .position(|behavior| *behavior == self.image_paste_behavior);
        let entity = cx.entity();
        let select = Select::new(labels)
            .id("preferences-image-dropdown")
            .selected(selected)
            .w(px(280.0))
            .on_change(move |ix, _, _, cx| {
                if let Some(behavior) = options.get(ix).copied() {
                    entity.update(cx, |this, cx| {
                        this.image_paste_behavior = behavior;
                        cx.notify();
                    });
                }
            });
        self.labeled_row(&i18n.t("pref_image_insert_behavior", &[]), select, theme)
    }

    /// 返回导出配色的显示文案。
    fn export_theme_label(mode: ExportThemeMode, i18n: &crate::i18n::I18nSnapshot) -> String {
        match mode {
            ExportThemeMode::FollowEditor => i18n.t("pref_export_theme_follow", &[]),
            ExportThemeMode::Light => i18n.t("pref_export_theme_light", &[]),
            ExportThemeMode::Dark => i18n.t("pref_export_theme_dark", &[]),
        }
    }

    /// 渲染导出选项页（目录开关 + 配色下拉框）。
    fn render_export_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = &ThemeTypography::DEFAULT;
        let accent = theme.button_primary;
        let entity = cx.entity();
        let include_toc = self.export_include_toc;
        let toggle = div()
            .w(px(280.0))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(t.dialog_body_size))
                    .text_color(theme.foreground)
                    .child(SharedString::from(i18n.t("pref_export_include_toc", &[]))),
            )
            .child(
                Switch::new(ElementId::Name("preferences-toggle-export-toc".into()))
                    .checked(include_toc)
                    .color(accent)
                    .on_change(move |new_checked: bool, _, cx| {
                        let value = new_checked;
                        entity.update(cx, |this, cx| {
                            this.export_include_toc = value;
                            cx.notify();
                        });
                    }),
            );
        let options = [
            ExportThemeMode::FollowEditor,
            ExportThemeMode::Light,
            ExportThemeMode::Dark,
        ];
        let labels: Vec<SharedString> = options
            .iter()
            .map(|mode| Self::export_theme_label(*mode, i18n).into())
            .collect();
        let selected = options.iter().position(|mode| *mode == self.export_theme);
        let theme_entity = cx.entity();
        let select = Select::new(labels)
            .id("preferences-export-theme-dropdown")
            .selected(selected)
            .w(px(280.0))
            .on_change(move |ix, _, _, cx| {
                if let Some(mode) = options.get(ix).copied() {
                    theme_entity.update(cx, |this, cx| {
                        this.export_theme = mode;
                        cx.notify();
                    });
                }
            });
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(12.0))
            .child(toggle)
            .child(self.labeled_row(&i18n.t("pref_export_theme", &[]), select, theme))
    }

    /// 返回快捷键分组的显示文案。
    fn shortcut_category_label(
        category: ShortcutCategory,
        i18n: &crate::i18n::I18nSnapshot,
    ) -> String {
        match category {
            ShortcutCategory::File => i18n.t("keys_group_file", &[]),
            ShortcutCategory::Edit => i18n.t("keys_group_edit", &[]),
            ShortcutCategory::Navigation => i18n.t("keys_group_navigation", &[]),
            ShortcutCategory::Formatting => i18n.t("keys_group_formatting", &[]),
            ShortcutCategory::Block => i18n.t("keys_group_block", &[]),
            ShortcutCategory::Other => i18n.t("keys_group_other", &[]),
        }
    }

    /// 返回快捷键命令的显示文案。
    fn shortcut_command_label(
        command: ShortcutCommand,
        i18n: &crate::i18n::I18nSnapshot,
    ) -> String {
        match command {
            ShortcutCommand::Newline => i18n.t("keys_newline", &[]),
            ShortcutCommand::DeleteBack => i18n.t("keys_delete_back", &[]),
            ShortcutCommand::Delete => i18n.t("keys_delete", &[]),
            ShortcutCommand::WordDeleteBack => i18n.t("keys_word_delete_back", &[]),
            ShortcutCommand::WordDeleteForward => i18n.t("keys_word_delete_forward", &[]),
            ShortcutCommand::FocusPrev => i18n.t("keys_focus_prev", &[]),
            ShortcutCommand::FocusNext => i18n.t("keys_focus_next", &[]),
            ShortcutCommand::MoveLeft => i18n.t("keys_move_left", &[]),
            ShortcutCommand::MoveRight => i18n.t("keys_move_right", &[]),
            ShortcutCommand::WordMoveLeft => i18n.t("keys_word_move_left", &[]),
            ShortcutCommand::WordMoveRight => i18n.t("keys_word_move_right", &[]),
            ShortcutCommand::Home => i18n.t("keys_home", &[]),
            ShortcutCommand::End => i18n.t("keys_end", &[]),
            ShortcutCommand::BlockUp => i18n.t("keys_block_up", &[]),
            ShortcutCommand::BlockDown => i18n.t("keys_block_down", &[]),
            ShortcutCommand::PageUp => i18n.t("keys_page_up", &[]),
            ShortcutCommand::PageDown => i18n.t("keys_page_down", &[]),
            ShortcutCommand::JumpToTop => i18n.t("keys_jump_to_top", &[]),
            ShortcutCommand::JumpToBottom => i18n.t("keys_jump_to_bottom", &[]),
            ShortcutCommand::SelectLeft => i18n.t("keys_select_left", &[]),
            ShortcutCommand::SelectRight => i18n.t("keys_select_right", &[]),
            ShortcutCommand::WordSelectLeft => i18n.t("keys_word_select_left", &[]),
            ShortcutCommand::WordSelectRight => i18n.t("keys_word_select_right", &[]),
            ShortcutCommand::SelectHome => i18n.t("keys_select_home", &[]),
            ShortcutCommand::SelectEnd => i18n.t("keys_select_end", &[]),
            ShortcutCommand::SelectAll => i18n.t("keys_select_all", &[]),
            ShortcutCommand::Copy => i18n.t("keys_copy", &[]),
            ShortcutCommand::Cut => i18n.t("keys_cut", &[]),
            ShortcutCommand::Paste => i18n.t("keys_paste", &[]),
            ShortcutCommand::Undo => i18n.t("keys_undo", &[]),
            ShortcutCommand::Redo => i18n.t("keys_redo", &[]),
            ShortcutCommand::BoldSelection => i18n.t("keys_bold_selection", &[]),
            ShortcutCommand::ItalicSelection => i18n.t("keys_italic_selection", &[]),
            ShortcutCommand::UnderlineSelection => i18n.t("keys_underline_selection", &[]),
            ShortcutCommand::CodeSelection => i18n.t("keys_code_selection", &[]),
            ShortcutCommand::IndentBlock => i18n.t("keys_indent_block", &[]),
            ShortcutCommand::OutdentBlock => i18n.t("keys_outdent_block", &[]),
            ShortcutCommand::ExitCodeBlock => i18n.t("keys_exit_code_block", &[]),
            ShortcutCommand::SaveDocument => i18n.t("keys_save_document", &[]),
            ShortcutCommand::SaveDocumentAs => i18n.t("keys_save_document_as", &[]),
            ShortcutCommand::NewWindow => i18n.t("keys_new_window", &[]),
            ShortcutCommand::OpenFile => i18n.t("keys_open_file", &[]),
            ShortcutCommand::QuitApplication => i18n.t("keys_quit_application", &[]),
            ShortcutCommand::CloseWindow => i18n.t("keys_close_window", &[]),
            ShortcutCommand::DismissTransientUi => i18n.t("keys_dismiss_transient_ui", &[]),
            ShortcutCommand::ToggleViewMode => i18n.t("keys_toggle_view_mode", &[]),
            ShortcutCommand::ToggleWorkspace => i18n.t("keys_toggle_workspace", &[]),
            ShortcutCommand::InsertLink => i18n.t("keys_insert_link", &[]),
            ShortcutCommand::InsertImage => i18n.t("keys_insert_image", &[]),
            ShortcutCommand::InsertTable => i18n.t("keys_insert_table", &[]),
            ShortcutCommand::InsertCodeBlock => i18n.t("keys_insert_code_block", &[]),
            ShortcutCommand::InsertMathBlock => i18n.t("keys_insert_math_block", &[]),
            ShortcutCommand::InsertQuote => i18n.t("keys_insert_quote", &[]),
            ShortcutCommand::InsertBulletedList => i18n.t("keys_insert_bulleted_list", &[]),
            ShortcutCommand::InsertNumberedList => i18n.t("keys_insert_numbered_list", &[]),
            ShortcutCommand::InsertTaskList => i18n.t("keys_insert_task_list", &[]),
            ShortcutCommand::InsertSeparator => i18n.t("keys_insert_separator", &[]),
            ShortcutCommand::InsertParagraphAbove => i18n.t("keys_insert_paragraph_above", &[]),
            ShortcutCommand::InsertParagraphBelow => i18n.t("keys_insert_paragraph_below", &[]),
            ShortcutCommand::ConvertHeading1 => i18n.t("keys_convert_heading_1", &[]),
            ShortcutCommand::ConvertHeading2 => i18n.t("keys_convert_heading_2", &[]),
            ShortcutCommand::ConvertHeading3 => i18n.t("keys_convert_heading_3", &[]),
            ShortcutCommand::ConvertHeading4 => i18n.t("keys_convert_heading_4", &[]),
            ShortcutCommand::ConvertHeading5 => i18n.t("keys_convert_heading_5", &[]),
            ShortcutCommand::ConvertHeading6 => i18n.t("keys_convert_heading_6", &[]),
            ShortcutCommand::ConvertParagraph => i18n.t("keys_convert_paragraph", &[]),
            ShortcutCommand::FindInDocument => i18n.t("keys_find_in_document", &[]),
            ShortcutCommand::TableInsertRowAbove => i18n.t("keys_table_insert_row_above", &[]),
            ShortcutCommand::TableInsertRowBelow => i18n.t("keys_table_insert_row_below", &[]),
            ShortcutCommand::TableDeleteRow => i18n.t("keys_table_delete_row", &[]),
            ShortcutCommand::TableInsertColumnLeft => i18n.t("keys_table_insert_column_left", &[]),
            ShortcutCommand::TableInsertColumnRight => {
                i18n.t("keys_table_insert_column_right", &[])
            }
            ShortcutCommand::TableDeleteColumn => i18n.t("keys_table_delete_column", &[]),
            ShortcutCommand::TableMoveRowUp => i18n.t("keys_table_move_row_up", &[]),
            ShortcutCommand::TableMoveRowDown => i18n.t("keys_table_move_row_down", &[]),
            ShortcutCommand::TableMoveColumnLeft => i18n.t("keys_table_move_column_left", &[]),
            ShortcutCommand::TableMoveColumnRight => i18n.t("keys_table_move_column_right", &[]),
            ShortcutCommand::TableAlignColumnLeft => i18n.t("keys_table_align_column_left", &[]),
            ShortcutCommand::TableAlignColumnCenter => {
                i18n.t("keys_table_align_column_center", &[])
            }
            ShortcutCommand::TableAlignColumnRight => i18n.t("keys_table_align_column_right", &[]),
            ShortcutCommand::ToggleTableHeaders => i18n.t("keys_toggle_table_headers", &[]),
        }
    }

    /// 重置指定命令的快捷键为默认值。
    fn reset_shortcut(
        &mut self,
        command: ShortcutCommand,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(definition) = shortcut_definitions()
            .iter()
            .find(|definition| definition.command == command)
        {
            self.keybindings.remove(definition.id);
        }
        // 草稿回到默认值后，录制组件显示同步回退。
        let current = self.effective_hotkey_values(command);
        if let Some(state) = self.hotkey_inputs.get(&command).cloned() {
            state.update(cx, |state, cx| state.set_hotkeys(current, cx));
        }
        self.shortcut_error = None;
        cx.notify();
    }

    /// 当前草稿下指定命令的生效快捷键（框架值形式，用于初始化与回滚）。
    fn effective_hotkey_values(&self, command: ShortcutCommand) -> Vec<HotkeyValue> {
        hotkey_values_from_keys(&resolved_shortcut_keys(&self.keybindings, command))
    }

    /// 提交录制组件变化后的快捷键列表（含规范化、冲突检查与回滚）。
    fn commit_hotkey_list(
        &mut self,
        command: ShortcutCommand,
        values: &[HotkeyValue],
        cx: &mut Context<Self>,
    ) {
        let keys = hotkey_keys_from_values(values);
        // 冲突或无效时把组件显示回滚到草稿值（旧捕获逻辑同语义）。
        let revert = |this: &mut Self, cx: &mut Context<Self>| {
            let current = this.effective_hotkey_values(command);
            if let Some(state) = this.hotkey_inputs.get(&command).cloned() {
                state.update(cx, |state, cx| state.set_hotkeys(current, cx));
            }
        };

        let Some(keys) = normalize_shortcut_keys(&keys) else {
            let offender = hotkey_keys_from_values(values)
                .into_iter()
                .find(|key| normalize_shortcut_keys(std::slice::from_ref(key)).is_none())
                .unwrap_or_default();
            let message = cx
                .global::<I18nManager>()
                .snapshot()
                .t("pref_keys_invalid", &[("shortcut", offender.as_str())]);
            revert(self, cx);
            self.shortcut_error = Some(message);
            cx.notify();
            return;
        };

        if let Some(conflict) = shortcut_conflict_for(command, &keys, &self.keybindings) {
            // 快捷键冲突时按模板填充冲突命令占位符。
            let message = {
                let i18n = cx.global::<I18nManager>().snapshot();
                let label = Self::shortcut_command_label(conflict.command, &i18n);
                i18n.t("pref_keys_conflict", &[("command", label.as_str())])
            };
            revert(self, cx);
            self.shortcut_error = Some(message);
            cx.notify();
            return;
        }

        if let Some(definition) = shortcut_definitions()
            .iter()
            .find(|definition| definition.command == command)
        {
            let defaults = definition
                .default_keys
                .iter()
                .map(|key| key.to_string())
                .collect::<Vec<_>>();
            if keys == defaults {
                self.keybindings.remove(definition.id);
            } else {
                self.keybindings.insert(definition.id.to_string(), keys);
            }
        }
        self.shortcut_error = None;
        cx.notify();
    }

    /// 构造快捷键录制与重置按钮。
    fn shortcut_action_button(
        id: impl Into<ElementId>,
        label: String,
        theme: &Theme,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        div()
            .id(id)
            .h(px(28.0))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px((d.dialog_radius - 5.0).max(0.0)))
            .border(px(d.dialog_border_width))
            .border_color(theme.border)
            .bg(theme.button_secondary)
            .hover(|this| this.bg(theme.button_secondary_hover))
            .cursor_pointer()
            .text_size(px((t.dialog_button_size - 1.0).max(10.0)))
            .font_weight(t.dialog_button_weight)
            .text_color(theme.button_secondary_foreground)
            .child(label)
            .on_click(cx.listener(on_click))
    }

    /// 渲染单条快捷键设置行（多值录制由框架 `HotkeyListInput` 承载）。
    fn render_shortcut_row(
        &self,
        definition: ShortcutDefinition,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let label = Self::shortcut_command_label(definition.command, i18n);
        let command = definition.command;

        // 录制组件按命令持有实体；变化经提交函数走规范化与冲突检查。
        let input = self.hotkey_inputs.get(&command).cloned().map(|state| {
            let window_entity = cx.entity().downgrade();
            HotkeyListInput::new(state)
                .recording_text(i18n.t("pref_keys_recording", &[]))
                .on_change(move |values, _window, cx| {
                    let values = values.to_vec();
                    let _ = window_entity.update(cx, |this, cx| {
                        this.commit_hotkey_list(command, &values, cx);
                    });
                })
        });

        div()
            .w_full()
            .min_h(px(42.0))
            .px(px(10.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .rounded(px(d.menu_item_radius))
            .bg(theme.popover)
            .child(
                div()
                    .min_w(px(144.0))
                    .text_size(px(t.dialog_body_size))
                    .text_color(theme.foreground)
                    .child(label),
            )
            .child(div().flex_1().children(input))
            .child(Self::shortcut_action_button(
                ("preferences-shortcut-reset", definition.command as u32),
                i18n.t("pref_keys_reset", &[]),
                theme,
                move |this, event, window, cx| this.reset_shortcut(command, event, window, cx),
                cx,
            ))
            .into_any_element()
    }

    /// 渲染快捷键设置页。
    fn render_shortcuts_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let mut content = div()
            .id("preferences-shortcuts-scroll")
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .pr(px(4.0));

        let categories = [
            ShortcutCategory::File,
            ShortcutCategory::Edit,
            ShortcutCategory::Navigation,
            ShortcutCategory::Formatting,
            ShortcutCategory::Block,
            ShortcutCategory::Other,
        ];

        for category in categories {
            let mut group = div().w_full().flex().flex_col().gap(px(8.0)).child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(t.dialog_body_size))
                            .font_weight(t.dialog_button_weight)
                            .text_color(theme.popover_foreground)
                            .child(Self::shortcut_category_label(category, i18n)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(d.dialog_border_width.max(1.0)))
                            .bg(theme.border),
                    ),
            );
            for definition in shortcut_definitions()
                .iter()
                .copied()
                .filter(|definition| definition.category == category)
            {
                group = group.child(self.render_shortcut_row(definition, theme, i18n, cx));
            }
            content = content.child(group);
        }

        let mut page = div()
            .w_full()
            .h_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.0));
        if let Some(error) = &self.shortcut_error {
            page = page.child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .text_size(px(t.dialog_body_size))
                    .text_color(theme.button_danger)
                    .child(error.clone()),
            );
        }
        page.child(content)
    }

    /// 渲染状态栏设置页。
    fn render_status_bar_page(
        &self,
        theme: &Theme,
        i18n: &crate::i18n::I18nSnapshot,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = &ThemeTypography::DEFAULT;

        // 开关行：标签 + rgpui 开关组件（选中色沿用拾墨主题强调色）。
        let accent = theme.button_primary;
        let switch_row = |label: &str,
                          checked: bool,
                          on_toggle: fn(&mut Self, bool, &mut Context<Self>),
                          cx: &mut Context<Self>| {
            let entity = cx.entity().clone();
            div()
                .w(px(280.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(t.dialog_body_size))
                        .text_color(theme.foreground)
                        .child(SharedString::from(label.to_string())),
                )
                .child(
                    Switch::new(ElementId::Name(
                        format!("preferences-toggle-{label}").into(),
                    ))
                    .checked(checked)
                    .color(accent)
                    .on_change(move |new_checked: bool, _, cx| {
                        let value = new_checked;
                        entity.update(cx, |this, cx| {
                            on_toggle(this, value, cx);
                        });
                    }),
                )
        };

        let items = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(switch_row(
                &i18n.t("pref_status_bar_enabled", &[]),
                self.status_bar_enabled,
                |this, value, cx| {
                    this.status_bar_enabled = value;
                    cx.notify();
                },
                cx,
            ))
            .child(switch_row(
                &i18n.t("pref_status_bar_word_count", &[]),
                self.status_bar_show_word_count,
                |this, value, cx| {
                    this.status_bar_show_word_count = value;
                    cx.notify();
                },
                cx,
            ))
            .child(switch_row(
                &i18n.t("pref_status_bar_cursor_position", &[]),
                self.status_bar_show_cursor_position,
                |this, value, cx| {
                    this.status_bar_show_cursor_position = value;
                    cx.notify();
                },
                cx,
            ))
            .child(switch_row(
                &i18n.t("pref_status_bar_sidebar_toggle", &[]),
                self.status_bar_show_sidebar_toggle,
                |this, value, cx| {
                    this.status_bar_show_sidebar_toggle = value;
                    cx.notify();
                },
                cx,
            ))
            .child(switch_row(
                &i18n.t("pref_status_bar_mode_switch", &[]),
                self.status_bar_show_mode_switch,
                |this, value, cx| {
                    this.status_bar_show_mode_switch = value;
                    cx.notify();
                },
                cx,
            ));

        div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .child(items)
    }
}

impl Render for PreferencesWindow {
    /// 渲染偏好设置窗口。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = rgpui::Theme::global(cx).clone();
        let i18n = cx.global::<I18nManager>().snapshot();
        let d = &ThemeDimensions::DEFAULT;
        let t = &ThemeTypography::DEFAULT;
        let can_save = self.has_unsaved_changes();
        let window_title = SharedString::from(format!("Rumark - {}", i18n.t("pref_title", &[])));
        window.set_window_title(window_title.as_ref());

        let content = div()
            .w_full()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .key_context("Preferences")
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .w(relative(0.3))
                    .h_full()
                    .pr(px(20.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .border_r(px(d.dialog_border_width))
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(self.nav_button(
                                "preferences-nav-file",
                                i18n.t("pref_nav_file", &[]),
                                self.nav == PreferencesNav::File,
                                &theme,
                                Self::set_nav_file,
                                cx,
                            ))
                            .child(self.nav_button(
                                "preferences-nav-theme",
                                i18n.t("pref_nav_theme", &[]),
                                self.nav == PreferencesNav::Theme,
                                &theme,
                                Self::set_nav_theme,
                                cx,
                            ))
                            .child(self.nav_button(
                                "preferences-nav-image",
                                i18n.t("pref_nav_image", &[]),
                                self.nav == PreferencesNav::Image,
                                &theme,
                                Self::set_nav_image,
                                cx,
                            ))
                            .child(self.nav_button(
                                "preferences-nav-export",
                                i18n.t("pref_nav_export", &[]),
                                self.nav == PreferencesNav::Export,
                                &theme,
                                Self::set_nav_export,
                                cx,
                            ))
                            .child(self.nav_button(
                                "preferences-nav-shortcuts",
                                i18n.t("pref_nav_shortcuts", &[]),
                                self.nav == PreferencesNav::Shortcuts,
                                &theme,
                                Self::set_nav_shortcuts,
                                cx,
                            ))
                            .child(self.nav_button(
                                "preferences-nav-status-bar",
                                i18n.t("pref_nav_status_bar", &[]),
                                self.nav == PreferencesNav::StatusBar,
                                &theme,
                                Self::set_nav_status_bar,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .w(relative(0.7))
                    .h_full()
                    .p(px(d.dialog_padding))
                    .flex()
                    .flex_col()
                    .gap(px(d.dialog_gap))
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h(px(0.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(d.dialog_gap * 1.5))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(t.dialog_title_size))
                                    .font_weight(t.dialog_title_weight)
                                    .text_color(theme.popover_foreground)
                                    .child(match self.nav {
                                        PreferencesNav::File => i18n.t("pref_nav_file", &[]),
                                        PreferencesNav::Theme => i18n.t("pref_nav_theme", &[]),
                                        PreferencesNav::Image => i18n.t("pref_nav_image", &[]),
                                        PreferencesNav::Export => i18n.t("pref_nav_export", &[]),
                                        PreferencesNav::Shortcuts => {
                                            i18n.t("pref_nav_shortcuts", &[])
                                        }
                                        PreferencesNav::StatusBar => {
                                            i18n.t("pref_nav_status_bar", &[])
                                        }
                                    }),
                            )
                            .child(match self.nav {
                                PreferencesNav::File => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(self.render_startup_page(&theme, &i18n, cx))
                                    .into_any_element(),
                                PreferencesNav::Theme => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(self.render_theme_page(&theme, &i18n, cx))
                                    .into_any_element(),
                                PreferencesNav::Image => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(self.render_image_page(&theme, &i18n, cx))
                                    .into_any_element(),
                                PreferencesNav::Export => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(self.render_export_page(&theme, &i18n, cx))
                                    .into_any_element(),
                                PreferencesNav::Shortcuts => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .child(self.render_shortcuts_page(&theme, &i18n, cx))
                                    .into_any_element(),
                                PreferencesNav::StatusBar => div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(self.render_status_bar_page(&theme, &i18n, cx))
                                    .into_any_element(),
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .gap(px(d.dialog_button_gap))
                            .child(
                                Button::new("preferences-cancel")
                                    .label(i18n.t("pref_cancel", &[]))
                                    .secondary()
                                    .on_click(cx.listener(Self::cancel)),
                            )
                            .child(
                                Button::new("preferences-save")
                                    .label(i18n.t("pref_save", &[]))
                                    .primary()
                                    .disabled(!can_save)
                                    .on_click(cx.listener(Self::save)),
                            ),
                    ),
            );
        // 组件标题栏走流式布局，占据顶部固定高度。
        let root = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                TitleBar::new().child(
                    div()
                        .min_w(px(0.0))
                        .truncate()
                        // 与主窗口品牌同字号同字重，保持跨窗口标题栏观感一致。
                        .text_size(px(ThemeDimensions::DEFAULT.titlebar_brand_text_size))
                        .line_height(relative(1.0))
                        .font_weight(rgpui::FontWeight::SEMIBOLD)
                        .text_color(theme.button_secondary_foreground)
                        .child(window_title),
                ),
            );
        root.child(content)
    }
}
/// 用给定状态打开偏好设置窗口（任务栏标题由窗口渲染时经 `set_window_title` 设置）。
fn open_preferences_window_with_state(
    cx: &mut App,
    preferences: AppPreferences,
    theme_options: Vec<ThemeCatalogEntry>,
) -> WindowHandle<PreferencesWindow> {
    let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
    let handle = cx
        .open_window(rumark_window_options(bounds), move |_window, cx| {
            cx.new(move |cx| PreferencesWindow::new(preferences, theme_options, cx))
        })
        .expect("preferences window should open");

    handle
        .update(cx, |preferences, window, cx| {
            window.activate_window();
            preferences.focus_handle.focus(window, cx);
        })
        .expect("newly opened preferences window should be updateable");

    handle
}

/// 打开偏好设置窗口。
pub(crate) fn open_preferences_window(cx: &mut App) -> WindowHandle<PreferencesWindow> {
    let preferences = match read_app_preferences() {
        Ok(preferences) => preferences,
        Err(err) => {
            eprintln!("failed to read app preferences: {err}");
            AppPreferences::default()
        }
    };
    let theme_options = crate::theme::available_theme_catalog(cx);
    open_preferences_window_with_state(cx, preferences, theme_options)
}

#[cfg(test)]
mod tests {
    use super::{
        AppPreferences, EditorSettings, ExportThemeMode, ImagePasteBehavior, PendingConfigRecovery,
        PreferencesRecovery, StartupOpenPreference, StatusBarButton, StatusBarPreferences,
        hotkey_keys_from_values, hotkey_values_from_keys,
        load_or_create_app_preferences_with_dirs_and_locales, open_preferences_window_with_state,
        read_app_preferences_with_dirs, save_app_preferences_with_dirs,
        save_preferences_from_window_with_dirs,
    };
    use crate::components::ShortcutCommand;
    use crate::config::RumarkConfigDirs;
    use crate::i18n::I18nManager;
    use crate::theme::ThemeCatalogEntry;
    use rgpui::TestAppContext;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// 初始化偏好设置测试应用（测试辅助）。
    fn init_preferences_test_app(cx: &mut TestAppContext) {
        cx.update(|cx| {
            I18nManager::init_with_language_id(cx, "en-US");
            rgpui::theme::init(cx);
            crate::components::init(cx);
            EditorSettings::init(cx, true);
        });
    }

    /// 构造默认主题选项列表（测试辅助）。
    fn default_theme_options() -> Vec<ThemeCatalogEntry> {
        vec![ThemeCatalogEntry {
            id: "rumark".into(),
            name: "Rumark".into(),
        }]
    }

    #[rgpui::test]
    fn pending_config_recovery_take_is_noop_without_registration(cx: &mut TestAppContext) {
        cx.update(|cx| {
            // 未登记过恢复提示时全局不存在，取值不得 panic。
            assert!(PendingConfigRecovery::take(cx).is_none());
        });
    }

    #[rgpui::test]
    fn pending_config_recovery_is_consumed_once(cx: &mut TestAppContext) {
        let recovery = PreferencesRecovery {
            damaged_path: PathBuf::from("/tmp/config.toml"),
            backup_path: PathBuf::from("/tmp/config.toml.bak"),
        };
        cx.update(|cx| {
            PendingConfigRecovery::set(cx, recovery);
            let taken = PendingConfigRecovery::take(cx).expect("recovery should be pending");
            assert_eq!(taken.damaged_path, PathBuf::from("/tmp/config.toml"));
            assert_eq!(taken.backup_path, PathBuf::from("/tmp/config.toml.bak"));
            assert!(PendingConfigRecovery::take(cx).is_none());
        });
    }

    #[test]
    fn missing_preferences_file_returns_defaults() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let dirs = RumarkConfigDirs::from_root(&root);
        let preferences =
            read_app_preferences_with_dirs(&dirs).expect("missing preferences should load");
        assert_eq!(preferences, AppPreferences::default());
        // 新用户默认使用白色主题。
        assert_eq!(preferences.default_theme_id, "rumark-light");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn partial_or_invalid_preferences_fall_back_by_field() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-partial-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root should exist");
        let dirs = RumarkConfigDirs::from_root(&root);
        std::fs::write(
            dirs.app_config_file(),
            r#"
                [startup]
                open = "not-valid"

                [theme]
                default_theme_id = "rumark-light"
            "#,
        )
        .expect("preferences should be written");

        let preferences =
            read_app_preferences_with_dirs(&dirs).expect("partial preferences should load");
        assert_eq!(preferences.startup_open, StartupOpenPreference::NewFile);
        assert_eq!(preferences.default_language_id, "en-US");
        assert_eq!(preferences.default_theme_id, "rumark-light");
        assert_eq!(preferences.image_paste_behavior, ImagePasteBehavior::None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_image_paste_behavior_falls_back_to_none() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-image-invalid-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root should exist");
        let dirs = RumarkConfigDirs::from_root(&root);
        std::fs::write(
            dirs.app_config_file(),
            r#"
                [editor]
                image_paste_behavior = "somewhere-dangerous"
            "#,
        )
        .expect("preferences should be written");

        let preferences = read_app_preferences_with_dirs(&dirs).expect("preferences should load");
        assert_eq!(preferences.image_paste_behavior, ImagePasteBehavior::None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn damaged_preferences_file_is_reported_and_backed_up() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-damaged-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root should exist");
        let dirs = RumarkConfigDirs::from_root(&root);
        let damaged_text = "not = [valid";
        std::fs::write(dirs.app_config_file(), damaged_text)
            .expect("preferences should be written");

        // 纯读取不改写文件，但必须报错：损坏不能再被静默当成默认值。
        let err = read_app_preferences_with_dirs(&dirs)
            .expect_err("damaged preferences should be reported");
        assert!(
            err.to_string().contains("failed to parse"),
            "unexpected error: {err}"
        );

        // 启动路径会先备份原文件再重建默认配置，原内容不得丢失。
        let (preferences, recovery) =
            load_or_create_app_preferences_with_dirs_and_locales(&dirs, ["zh-CN"])
                .expect("damaged preferences should recover");
        let recovery = recovery.expect("damaged config should report recovery");
        assert_eq!(
            std::fs::read_to_string(&recovery.backup_path).expect("backup should exist"),
            damaged_text
        );
        assert_eq!(recovery.damaged_path, dirs.app_config_file());
        assert_eq!(
            preferences.default_language_id, "zh-CN",
            "detected language should still apply"
        );
        assert_eq!(
            preferences.default_theme_id,
            AppPreferences::default().default_theme_id
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn newer_preferences_version_is_rejected_without_touching_the_file() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-future-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root should exist");
        let dirs = RumarkConfigDirs::from_root(&root);
        let future_text = "version = 99\n";
        std::fs::write(dirs.app_config_file(), future_text).expect("preferences should be written");

        let err = load_or_create_app_preferences_with_dirs_and_locales(&dirs, ["zh-CN"])
            .expect_err("newer version should be rejected");
        assert!(
            err.to_string().contains("version 99"),
            "unexpected error: {err}"
        );
        // 不得回写：否则新版本写入的字段会被默认值抹掉。
        assert_eq!(
            std::fs::read_to_string(dirs.app_config_file()).expect("config should exist"),
            future_text
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saves_and_reads_preferences() {
        let root =
            std::env::temp_dir().join(format!("rumark-preferences-save-{}", uuid::Uuid::new_v4()));
        let dirs = RumarkConfigDirs::from_root(&root);
        let preferences = AppPreferences {
            startup_open: StartupOpenPreference::LastOpenedFile,
            default_language_id: "zh-CN".into(),
            default_theme_id: "rumark-light".into(),
            table_headers_visible: false,
            image_paste_behavior: ImagePasteBehavior::CopyToAssetsFolder,
            export_include_toc: false,
            export_theme: ExportThemeMode::Dark,
            keybindings: BTreeMap::new(),
            status_bar: StatusBarPreferences::default(),
        };

        save_app_preferences_with_dirs(&preferences, &dirs)
            .expect("preferences should save to config.toml");
        let loaded = read_app_preferences_with_dirs(&dirs).expect("preferences should read back");
        assert_eq!(loaded, preferences);

        let text =
            std::fs::read_to_string(dirs.app_config_file()).expect("config.toml should exist");
        assert!(text.contains("open = \"last_opened_file\""));
        assert!(text.contains("default_language_id = \"zh-CN\""));
        assert!(text.contains("default_theme_id = \"rumark-light\""));
        assert!(text.contains("table_headers_visible = false"));
        assert!(text.contains("image_paste_behavior = \"copy_to_assets_folder\""));
        assert!(text.contains("include_toc = false"));
        assert!(text.contains("theme = \"dark\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_preferences_file_is_created_with_detected_language() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-create-{}",
            uuid::Uuid::new_v4()
        ));
        let dirs = RumarkConfigDirs::from_root(&root);
        let (preferences, recovery) =
            load_or_create_app_preferences_with_dirs_and_locales(&dirs, ["zh-HK"])
                .expect("preferences should be created");
        assert!(recovery.is_none(), "fresh config needs no recovery");
        assert_eq!(preferences.default_language_id, "zh-CN");
        assert!(dirs.app_config_file().exists());
        let text =
            std::fs::read_to_string(dirs.app_config_file()).expect("config.toml should exist");
        assert!(text.contains("[language]"));
        assert!(text.contains("default_language_id = \"zh-CN\""));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_preferences_are_normalized_with_language() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-legacy-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("temp root should exist");
        let dirs = RumarkConfigDirs::from_root(&root);
        std::fs::write(
            dirs.app_config_file(),
            r#"
                [startup]
                open = "last_opened_file"

                [theme]
                default_theme_id = "rumark-light"
            "#,
        )
        .expect("legacy preferences should be written");

        let (preferences, recovery) =
            load_or_create_app_preferences_with_dirs_and_locales(&dirs, ["en-GB"])
                .expect("legacy preferences should normalize");
        assert!(recovery.is_none(), "legacy config needs no recovery");
        assert_eq!(
            preferences.startup_open,
            StartupOpenPreference::LastOpenedFile
        );
        assert_eq!(preferences.default_language_id, "en-US");
        assert_eq!(preferences.default_theme_id, "rumark-light");
        let text =
            std::fs::read_to_string(dirs.app_config_file()).expect("config.toml should exist");
        assert!(text.contains("[language]"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saving_preferences_window_preserves_language() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-window-{}",
            uuid::Uuid::new_v4()
        ));
        let dirs = RumarkConfigDirs::from_root(&root);
        let preferences = AppPreferences {
            startup_open: StartupOpenPreference::NewFile,
            default_language_id: "zh-CN".into(),
            default_theme_id: "rumark".into(),
            table_headers_visible: true,
            image_paste_behavior: ImagePasteBehavior::None,
            export_include_toc: true,
            export_theme: ExportThemeMode::FollowEditor,
            keybindings: BTreeMap::new(),
            status_bar: StatusBarPreferences::default(),
        };
        save_app_preferences_with_dirs(&preferences, &dirs)
            .expect("preferences should save to config.toml");

        let saved = save_preferences_from_window_with_dirs(
            StartupOpenPreference::LastOpenedFile,
            "rumark-light",
            ImagePasteBehavior::CopyToNamedAssetsFolder,
            false,
            ExportThemeMode::Dark,
            BTreeMap::from([("save_document".to_string(), vec!["ctrl-alt-s".to_string()])]),
            &StatusBarPreferences::default(),
            &dirs,
        )
        .expect("window preferences should save");
        assert_eq!(saved.default_language_id, "zh-CN");
        assert_eq!(saved.startup_open, StartupOpenPreference::LastOpenedFile);
        assert_eq!(saved.default_theme_id, "rumark-light");
        assert_eq!(
            saved.image_paste_behavior,
            ImagePasteBehavior::CopyToNamedAssetsFolder
        );
        assert!(!saved.export_include_toc);
        assert_eq!(saved.export_theme, ExportThemeMode::Dark);
        assert_eq!(
            saved.keybindings.get("save_document"),
            Some(&vec!["ctrl-alt-s".to_string()])
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn saving_preferences_window_preserves_custom_status_bar_buttons() {
        let root = std::env::temp_dir().join(format!(
            "rumark-preferences-window-buttons-{}",
            uuid::Uuid::new_v4()
        ));
        let dirs = RumarkConfigDirs::from_root(&root);
        let custom = vec![StatusBarButton {
            id: "custom-1".into(),
            label: "Do Thing".into(),
            action_id: "rumark::do_thing".into(),
        }];
        let preferences = AppPreferences {
            status_bar: StatusBarPreferences {
                custom_buttons: custom.clone(),
                ..StatusBarPreferences::default()
            },
            ..AppPreferences::default()
        };
        save_app_preferences_with_dirs(&preferences, &dirs)
            .expect("preferences with custom buttons should save");

        // 偏好窗口不管理自定义按钮：传入空列表也不应清空磁盘上的既有配置。
        save_preferences_from_window_with_dirs(
            StartupOpenPreference::LastOpenedFile,
            "rumark-light",
            ImagePasteBehavior::None,
            true,
            ExportThemeMode::FollowEditor,
            BTreeMap::new(),
            &StatusBarPreferences::default(),
            &dirs,
        )
        .expect("window preferences should save");

        let loaded = read_app_preferences_with_dirs(&dirs).expect("preferences should read back");
        assert_eq!(loaded.status_bar.custom_buttons, custom);
        let _ = std::fs::remove_dir_all(root);
    }

    #[rgpui::test]
    async fn editor_settings_global_keeps_custom_status_bar_buttons(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);
        let custom = vec![StatusBarButton {
            id: "custom-1".into(),
            label: "按钮".into(),
            action_id: "rumark::do_thing".into(),
        }];
        let status_bar = StatusBarPreferences {
            custom_buttons: custom.clone(),
            ..StatusBarPreferences::default()
        };
        cx.update(|cx| {
            // 运行时全局快照必须携带自定义按钮，否则状态栏永远拿不到它们。
            EditorSettings::set_global(cx, true, &status_bar);
            let snapshot = EditorSettings::status_bar_preferences(cx);
            assert_eq!(snapshot.custom_buttons, custom);
        });
    }

    #[rgpui::test]
    async fn preferences_window_activates_and_focuses_on_open(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);

        let handle = cx.update(|cx| {
            open_preferences_window_with_state(
                cx,
                AppPreferences::default(),
                default_theme_options(),
            )
        });
        cx.run_until_parked();

        let active_window = cx.update(|cx| cx.active_window().expect("window should be active"));
        assert_eq!(active_window.window_id(), handle.window_id());
        assert!(
            handle
                .update(cx, |preferences, window, _cx| preferences
                    .focus_handle
                    .is_focused(window))
                .expect("preferences window should be updateable")
        );
        assert!(
            !handle
                .update(cx, |preferences, _window, _cx| preferences
                    .has_unsaved_changes())
                .expect("preferences window should be updateable")
        );
    }

    #[rgpui::test]
    async fn preferences_dirty_state_tracks_draft_changes(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);

        let handle = cx.update(|cx| {
            open_preferences_window_with_state(
                cx,
                AppPreferences::default(),
                default_theme_options(),
            )
        });
        cx.run_until_parked();

        handle
            .update(cx, |preferences, _window, _cx| {
                assert!(!preferences.has_unsaved_changes());
                preferences.startup_open = StartupOpenPreference::LastOpenedFile;
                assert!(preferences.has_unsaved_changes());
                preferences.startup_open = StartupOpenPreference::NewFile;
                assert!(!preferences.has_unsaved_changes());

                preferences.image_paste_behavior = ImagePasteBehavior::CopyToAssetsFolder;
                assert!(preferences.has_unsaved_changes());
                preferences.image_paste_behavior = ImagePasteBehavior::None;
                assert!(!preferences.has_unsaved_changes());

                preferences
                    .keybindings
                    .insert("save_document".into(), vec!["ctrl-alt-s".into()]);
                assert!(preferences.has_unsaved_changes());
            })
            .expect("preferences window should be updateable");
    }
    #[test]
    fn hotkey_key_conversion_round_trips() {
        let values = hotkey_values_from_keys(&["ctrl-a".to_string(), "ctrl-alt-s".to_string()]);
        assert_eq!(values.len(), 2);
        assert_eq!(
            hotkey_keys_from_values(&values),
            vec!["ctrl-a".to_string(), "ctrl-alt-s".to_string()]
        );
        // 解析失败的条目丢弃（如多段非修饰键）。
        assert!(hotkey_values_from_keys(&["a-b".to_string()]).is_empty());
    }

    #[rgpui::test]
    async fn hotkey_list_commit_updates_draft(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);

        let handle = cx.update(|cx| {
            open_preferences_window_with_state(
                cx,
                AppPreferences::default(),
                default_theme_options(),
            )
        });
        cx.run_until_parked();

        handle
            .update(cx, |preferences, _window, cx| {
                let values = hotkey_values_from_keys(&["ctrl-alt-s".to_string()]);
                assert_eq!(values.len(), 1);
                preferences.commit_hotkey_list(ShortcutCommand::SaveDocument, &values, cx);
                assert!(preferences.shortcut_error.is_none());
                assert_eq!(
                    preferences.keybindings.get("save_document"),
                    Some(&vec!["ctrl-alt-s".to_string()])
                );
                assert!(preferences.has_unsaved_changes());
            })
            .expect("preferences window should be updateable");
    }

    #[rgpui::test]
    async fn hotkey_list_conflict_reverts_and_errors(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);

        let handle = cx.update(|cx| {
            open_preferences_window_with_state(
                cx,
                AppPreferences::default(),
                default_theme_options(),
            )
        });
        cx.run_until_parked();

        handle
            .update(cx, |preferences, _window, cx| {
                // `ctrl-x` 已被剪切占用，给复制提交应冲突回滚。
                let values = hotkey_values_from_keys(&["ctrl-x".to_string()]);
                preferences.commit_hotkey_list(ShortcutCommand::Copy, &values, cx);
                assert!(preferences.shortcut_error.is_some());
                assert!(!preferences.keybindings.contains_key("copy"));
            })
            .expect("preferences window should be updateable");
    }

    #[rgpui::test]
    async fn applying_saved_preferences_keeps_window_open_and_focused(cx: &mut TestAppContext) {
        init_preferences_test_app(cx);

        let handle = cx.update(|cx| {
            open_preferences_window_with_state(
                cx,
                AppPreferences::default(),
                default_theme_options(),
            )
        });
        cx.run_until_parked();

        handle
            .update(cx, |preferences, window, cx| {
                preferences.startup_open = StartupOpenPreference::LastOpenedFile;
                assert!(preferences.has_unsaved_changes());
                let saved = AppPreferences {
                    startup_open: StartupOpenPreference::LastOpenedFile,
                    ..AppPreferences::default()
                };
                preferences.apply_saved_preferences(saved, window, cx);
            })
            .expect("preferences window should be updateable");
        cx.run_until_parked();

        assert_eq!(cx.update(|cx| cx.windows().len()), 1);
        let active_window = cx.update(|cx| cx.active_window().expect("window should be active"));
        assert_eq!(active_window.window_id(), handle.window_id());
        assert!(
            handle
                .update(cx, |preferences, window, _cx| preferences
                    .focus_handle
                    .is_focused(window))
                .expect("preferences window should remain updateable")
        );
        assert!(
            !handle
                .update(cx, |preferences, _window, _cx| preferences
                    .has_unsaved_changes())
                .expect("preferences window should remain updateable")
        );
    }
}
