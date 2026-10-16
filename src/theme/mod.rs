//! 编辑器布局与主题选择（基于 rgpui 通用主题）。
//!
//! 颜色全部直接使用 rgpui `Theme` 调色板（`theme.colors.X`
//! 经 `Deref` 可直接写作 `theme.X`）；本模块只保留组件
//! 没有对应项的布局尺寸与字排（转为常量），
//! 以及明/暗两套内置主题的选择与切换。

use std::path::Path;

use anyhow::bail;
use rgpui::{App, Font, FontFallbacks, FontWeight, Theme, ThemeMode, ThemeRegistry, font};

use crate::config::{RumarkConfigDirs, sanitize_config_stem};

/// 深色内置主题标识（兼容旧存用户配置）。
pub const THEME_ID_DARK: &str = "rumark";
/// 浅色内置主题标识（兼容旧存用户配置）。
pub const THEME_ID_LIGHT: &str = "rumark-light";

/// 可选界面主题的元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeCatalogEntry {
    /// 主题标识。
    pub id: String,
    /// 展示名称。
    pub name: String,
}

/// 返回内置主题目录（深/浅各一）。
pub fn bundled_theme_catalog() -> Vec<ThemeCatalogEntry> {
    vec![
        ThemeCatalogEntry {
            id: THEME_ID_DARK.into(),
            name: "Rumark".into(),
        },
        ThemeCatalogEntry {
            id: THEME_ID_LIGHT.into(),
            name: "Rumark Light".into(),
        },
    ]
}

/// 主题标识对应的明暗模式，未知标识回退到浅色。
pub fn theme_mode_for_id(theme_id: &str) -> ThemeMode {
    if theme_id == THEME_ID_DARK {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    }
}

/// 是否为内置主题标识（与注册表自定义主题重名时内置优先）。
fn is_builtin_theme_id(theme_id: &str) -> bool {
    theme_id == THEME_ID_DARK || theme_id == THEME_ID_LIGHT
}

/// 当前生效主题的标识：自定义主题返回注册表中的主题名。
pub fn active_theme_id(cx: &App) -> String {
    let theme = Theme::global(cx);
    let active = if theme.is_dark() {
        &theme.dark_theme
    } else {
        &theme.light_theme
    };
    if active.is_default {
        (if theme.is_dark() {
            THEME_ID_DARK
        } else {
            THEME_ID_LIGHT
        })
        .to_string()
    } else {
        active.name.to_string()
    }
}

/// 返回主题目录：内置两套 + 已导入的自定义主题（按名称排序）。
pub fn available_theme_catalog(cx: &App) -> Vec<ThemeCatalogEntry> {
    let mut catalog = bundled_theme_catalog();
    let mut custom: Vec<ThemeCatalogEntry> = ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|config| !config.is_default && !is_builtin_theme_id(config.name.as_str()))
        .map(|config| ThemeCatalogEntry {
            id: config.name.to_string(),
            name: config.name.to_string(),
        })
        .collect();
    custom.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    catalog.extend(custom);
    catalog
}

/// 把配置中的主题安装到全局状态（先恢复已导入的主题包，再应用保存的标识）。
pub fn init(cx: &mut App, theme_id: &str) {
    rgpui::theme::init(cx);
    load_persisted_theme_configs(cx);
    set_theme_by_id(cx, theme_id);
}

/// 按标识切换主题：内置走明暗模式，自定义包走注册表按名称应用；未知标识返回 `false`。
pub fn set_theme_by_id(cx: &mut App, theme_id: &str) -> bool {
    if is_builtin_theme_id(theme_id) {
        Theme::change(theme_mode_for_id(theme_id), None, cx);
    } else if !Theme::apply_named(theme_id, None, cx) {
        return false;
    }
    // 同步滚动条外观，与组件初始化保持一致。
    Theme::sync_scrollbar_appearance(cx);
    true
}

/// 从配置目录恢复已导入的主题包（坏包跳过，与语言包行为一致）。
fn load_persisted_theme_configs(cx: &mut App) {
    let Ok(dirs) = RumarkConfigDirs::from_system() else {
        return;
    };
    load_persisted_theme_configs_from_dirs(cx, &dirs);
}

/// 用指定目录集恢复已导入的主题包。
fn load_persisted_theme_configs_from_dirs(cx: &mut App, dirs: &RumarkConfigDirs) {
    let themes_dir = dirs.themes_dir();
    if !themes_dir.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&themes_dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    for path in files {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("skipping custom theme config '{}': {err}", path.display());
                continue;
            }
        };
        if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(&content) {
            eprintln!("skipping custom theme config '{}': {err}", path.display());
        }
    }
}

/// 导入用户主题包：落盘规范化副本并注册，返回包内主题名。
pub fn load_theme_config(cx: &mut App, path: impl AsRef<Path>) -> anyhow::Result<Vec<String>> {
    let dirs = RumarkConfigDirs::from_system()?;
    import_theme_config_with_dirs(cx, path, &dirs)
}

/// 用指定目录集导入用户主题包。
fn import_theme_config_with_dirs(
    cx: &mut App,
    path: impl AsRef<Path>,
    dirs: &RumarkConfigDirs,
) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path.as_ref())?;
    let theme_set: rgpui::ThemeSet = serde_json::from_str(&content)?;
    if theme_set.themes.is_empty() {
        bail!(
            "theme pack '{}' contains no themes",
            path.as_ref().display()
        );
    }
    ThemeRegistry::global_mut(cx).load_themes_from_str(&content)?;
    let stem = if theme_set.name.is_empty() {
        theme_set.themes[0].name.to_string()
    } else {
        theme_set.name.to_string()
    };
    let file_name = format!("{}.json", sanitize_config_stem(&stem));
    let themes_dir = dirs.themes_dir();
    crate::atomic_file::write_text(&themes_dir.join(file_name), &content)?;
    Ok(theme_set
        .themes
        .iter()
        .map(|theme| theme.name.to_string())
        .collect())
}

// 以下布局/字排取自旧内置主题默认值（深浅两套共用），转为常量。

/// 编辑器布局尺寸常量。
#[derive(Debug, Clone)]
pub struct ThemeDimensions {
    /// 布局尺寸 `editor_padding`。
    pub editor_padding: f32,
    /// 布局尺寸 `block_gap`。
    pub block_gap: f32,
    /// 布局尺寸 `block_min_height`。
    pub block_min_height: f32,
    /// 布局尺寸 `block_padding_y`。
    pub block_padding_y: f32,
    /// 布局尺寸 `block_padding_x`。
    pub block_padding_x: f32,
    /// 布局尺寸 `nested_block_indent`。
    pub nested_block_indent: f32,
    /// 布局尺寸 `list_marker_gap`。
    pub list_marker_gap: f32,
    /// 布局尺寸 `list_marker_width`。
    pub list_marker_width: f32,
    /// 布局尺寸 `ordered_list_marker_width`。
    pub ordered_list_marker_width: f32,
    /// 布局尺寸 `task_checkbox_size`。
    pub task_checkbox_size: f32,
    /// 布局尺寸 `task_checkbox_radius`。
    pub task_checkbox_radius: f32,
    /// 布局尺寸 `task_checkbox_border_width`。
    pub task_checkbox_border_width: f32,
    /// 布局尺寸 `task_checkbox_check_size`。
    pub task_checkbox_check_size: f32,
    /// 布局尺寸 `h1_padding_bottom`。
    pub h1_padding_bottom: f32,
    /// 布局尺寸 `h1_margin_bottom`。
    pub h1_margin_bottom: f32,
    /// 布局尺寸 `cursor_width`。
    pub cursor_width: f32,
    /// 布局尺寸 `underline_thickness`。
    pub underline_thickness: f32,
    /// 布局尺寸 `h1_border_width`（h1/h2 标题下边框，对齐 Typora/GitHub 样式）。
    pub h1_border_width: f32,
    /// 布局尺寸 `quote_border_width`。
    pub quote_border_width: f32,
    /// 布局尺寸 `quote_padding_left`。
    pub quote_padding_left: f32,
    /// 布局尺寸 `callout_padding_x`。
    pub callout_padding_x: f32,
    /// 布局尺寸 `callout_padding_y`。
    pub callout_padding_y: f32,
    /// 布局尺寸 `callout_body_gap`。
    pub callout_body_gap: f32,
    /// 布局尺寸 `callout_radius`。
    pub callout_radius: f32,
    /// 布局尺寸 `callout_border_width`。
    pub callout_border_width: f32,
    /// 布局尺寸 `callout_header_gap`。
    pub callout_header_gap: f32,
    /// 布局尺寸 `callout_header_margin_bottom`。
    pub callout_header_margin_bottom: f32,
    /// 布局尺寸 `footnote_padding_x`。
    pub footnote_padding_x: f32,
    /// 布局尺寸 `footnote_padding_y`。
    pub footnote_padding_y: f32,
    /// 布局尺寸 `footnote_radius`。
    pub footnote_radius: f32,
    /// 布局尺寸 `footnote_badge_padding_x`。
    pub footnote_badge_padding_x: f32,
    /// 布局尺寸 `footnote_badge_padding_y`。
    pub footnote_badge_padding_y: f32,
    /// 布局尺寸 `separator_thickness`。
    pub separator_thickness: f32,
    /// 布局尺寸 `separator_inset_x`。
    pub separator_inset_x: f32,
    /// 布局尺寸 `separator_margin_y`。
    pub separator_margin_y: f32,
    /// 布局尺寸 `code_block_padding_y`。
    pub code_block_padding_y: f32,
    /// 布局尺寸 `code_block_padding_x`。
    pub code_block_padding_x: f32,
    /// 布局尺寸 `code_bg_pad_x`。
    pub code_bg_pad_x: f32,
    /// 布局尺寸 `code_bg_pad_y`。
    pub code_bg_pad_y: f32,
    /// 布局尺寸 `code_bg_radius`。
    pub code_bg_radius: f32,
    /// 布局尺寸 `code_language_input_width`。
    pub code_language_input_width: f32,
    /// 布局尺寸 `code_language_input_height`。
    pub code_language_input_height: f32,
    /// 布局尺寸 `code_language_input_padding_x`。
    pub code_language_input_padding_x: f32,
    /// 布局尺寸 `code_language_input_padding_y`。
    pub code_language_input_padding_y: f32,
    /// 布局尺寸 `code_language_input_radius`。
    pub code_language_input_radius: f32,
    /// 布局尺寸 `code_language_input_border_width`。
    pub code_language_input_border_width: f32,
    /// 布局尺寸 `code_language_input_gap`。
    pub code_language_input_gap: f32,
    /// 布局尺寸 `table_cell_padding_x`。
    pub table_cell_padding_x: f32,
    /// 布局尺寸 `table_cell_padding_y`。
    pub table_cell_padding_y: f32,
    /// 布局尺寸 `table_cell_min_height`。
    pub table_cell_min_height: f32,
    /// 布局尺寸 `table_append_button_extent`。
    pub table_append_button_extent: f32,
    /// 布局尺寸 `table_append_button_inset`。
    pub table_append_button_inset: f32,
    /// 布局尺寸 `table_append_activation_band`。
    pub table_append_activation_band: f32,
    /// 布局尺寸 `image_radius`。
    pub image_radius: f32,
    /// 布局尺寸 `image_root_max_height`。
    pub image_root_max_height: f32,
    /// 布局尺寸 `image_cell_max_height`。
    pub image_cell_max_height: f32,
    /// 布局尺寸 `image_root_placeholder_height`。
    pub image_root_placeholder_height: f32,
    /// 布局尺寸 `image_cell_placeholder_height`。
    pub image_cell_placeholder_height: f32,
    /// 布局尺寸 `image_caption_gap`。
    pub image_caption_gap: f32,
    /// 布局尺寸 `scrollbar_width`。
    pub scrollbar_width: f32,
    /// 布局尺寸 `scrollbar_right`。
    pub scrollbar_right: f32,
    /// 布局尺寸 `centered_shrink_start`。
    pub centered_shrink_start: f32,
    /// 布局尺寸 `centered_shrink_end`。
    pub centered_shrink_end: f32,
    /// 布局尺寸 `centered_min_ratio`。
    pub centered_min_ratio: f32,
    /// 布局尺寸 `dialog_width`。
    pub dialog_width: f32,
    /// 布局尺寸 `dialog_padding`。
    pub dialog_padding: f32,
    /// 布局尺寸 `dialog_gap`。
    pub dialog_gap: f32,
    /// 布局尺寸 `dialog_radius`。
    pub dialog_radius: f32,
    /// 布局尺寸 `dialog_border_width`。
    pub dialog_border_width: f32,
    /// 布局尺寸 `dialog_button_gap`。
    pub dialog_button_gap: f32,
    /// 布局尺寸 `menu_bar_gap`。
    pub menu_bar_gap: f32,
    /// 标题栏开头品牌文字大小（目标：比窗内菜单大一档、加粗突出）。
    pub titlebar_brand_text_size: f32,
    /// 标题栏窗内菜单按钮文字大小（目标：比品牌小，比旧默认按钮小）。
    pub titlebar_menu_text_size: f32,
    /// 布局尺寸 `menu_item_radius`。
    pub menu_item_radius: f32,
    /// 布局尺寸 `table_insert_dialog_width`。
    pub table_insert_dialog_width: f32,
    /// 布局尺寸 `table_insert_stepper_gap`。
    pub table_insert_stepper_gap: f32,
    /// 布局尺寸 `table_insert_stepper_button_size`。
    pub table_insert_stepper_button_size: f32,
    /// 布局尺寸 `table_insert_stepper_value_min_width`。
    pub table_insert_stepper_value_min_width: f32,
    /// 布局尺寸 `table_insert_stepper_value_padding_x`。
    pub table_insert_stepper_value_padding_x: f32,
    /// 布局尺寸 `table_insert_stepper_radius`。
    pub table_insert_stepper_radius: f32,
}

impl ThemeDimensions {
    /// 默认布局尺寸。
    pub const DEFAULT: Self = Self {
        editor_padding: 24.0,
        block_gap: 6.0,
        block_min_height: 28.0,
        block_padding_y: 4.0,
        block_padding_x: 12.0,
        nested_block_indent: 20.0,
        list_marker_gap: 8.0,
        list_marker_width: 12.0,
        ordered_list_marker_width: 20.0,
        task_checkbox_size: 14.0,
        task_checkbox_radius: 4.0,
        task_checkbox_border_width: 1.0,
        task_checkbox_check_size: 10.0,
        h1_padding_bottom: 4.0,
        h1_margin_bottom: 4.0,
        cursor_width: 2.0,
        underline_thickness: 1.0,
        h1_border_width: 1.0,
        quote_border_width: 3.0,
        quote_padding_left: 12.0,
        callout_padding_x: 14.0,
        callout_padding_y: 10.0,
        callout_body_gap: 8.0,
        callout_radius: 10.0,
        callout_border_width: 4.0,
        callout_header_gap: 6.0,
        callout_header_margin_bottom: 6.0,
        footnote_padding_x: 10.0,
        footnote_padding_y: 6.0,
        footnote_radius: 6.0,
        footnote_badge_padding_x: 4.0,
        footnote_badge_padding_y: 1.0,
        separator_thickness: 1.0,
        separator_inset_x: 40.0,
        separator_margin_y: 10.0,
        code_block_padding_y: 8.0,
        code_block_padding_x: 12.0,
        code_bg_pad_x: 3.0,
        code_bg_pad_y: 1.0,
        code_bg_radius: 4.0,
        code_language_input_width: 156.0,
        code_language_input_height: 18.0,
        code_language_input_padding_x: 8.0,
        code_language_input_padding_y: 3.0,
        code_language_input_radius: 6.0,
        code_language_input_border_width: 1.0,
        code_language_input_gap: 8.0,
        table_cell_padding_x: 10.0,
        table_cell_padding_y: 8.0,
        table_cell_min_height: 42.0,
        table_append_button_extent: 16.0,
        table_append_button_inset: 8.0,
        table_append_activation_band: 18.0,
        image_radius: 12.0,
        image_root_max_height: 420.0,
        image_cell_max_height: 180.0,
        image_root_placeholder_height: 260.0,
        image_cell_placeholder_height: 120.0,
        image_caption_gap: 8.0,
        scrollbar_width: 6.0,
        scrollbar_right: 6.0,
        centered_shrink_start: 1100.0,
        centered_shrink_end: 2200.0,
        centered_min_ratio: 0.58,
        dialog_width: 460.0,
        dialog_padding: 20.0,
        dialog_gap: 14.0,
        dialog_radius: 14.0,
        dialog_border_width: 1.0,
        dialog_button_gap: 10.0,
        menu_bar_gap: 4.0,
        titlebar_brand_text_size: 14.0,
        titlebar_menu_text_size: 13.0,
        menu_item_radius: 5.0,
        table_insert_dialog_width: 380.0,
        table_insert_stepper_gap: 8.0,
        table_insert_stepper_button_size: 32.0,
        table_insert_stepper_value_min_width: 56.0,
        table_insert_stepper_value_padding_x: 10.0,
        table_insert_stepper_radius: 8.0,
    };
}

/// 编辑器字排常量（字重直接用组件 `FontWeight`）。
#[derive(Debug, Clone)]
pub struct ThemeTypography {
    /// 字排 `text_size`。
    pub text_size: f32,
    /// 字排 `text_line_height`。
    pub text_line_height: f32,
    /// 字排 `h1_size`。
    pub h1_size: f32,
    /// 字重 `h1_weight`。
    pub h1_weight: FontWeight,
    /// 字排 `h2_size`。
    pub h2_size: f32,
    /// 字重 `h2_weight`。
    pub h2_weight: FontWeight,
    /// 字排 `h3_size`。
    pub h3_size: f32,
    /// 字重 `h3_weight`。
    pub h3_weight: FontWeight,
    /// 字排 `h4_size`。
    pub h4_size: f32,
    /// 字重 `h4_weight`。
    pub h4_weight: FontWeight,
    /// 字排 `h5_size`。
    pub h5_size: f32,
    /// 字重 `h5_weight`。
    pub h5_weight: FontWeight,
    /// 字排 `h6_size`。
    pub h6_size: f32,
    /// 字重 `h6_weight`。
    pub h6_weight: FontWeight,
    /// 字排 `code_size`。
    pub code_size: f32,
    /// 字排 `dialog_title_size`。
    pub dialog_title_size: f32,
    /// 字重 `dialog_title_weight`。
    pub dialog_title_weight: FontWeight,
    /// 字排 `dialog_body_size`。
    pub dialog_body_size: f32,
    /// 字重 `dialog_body_weight`。
    pub dialog_body_weight: FontWeight,
    /// 字排 `dialog_button_size`。
    pub dialog_button_size: f32,
    /// 字重 `dialog_button_weight`。
    pub dialog_button_weight: FontWeight,
}

impl ThemeTypography {
    /// 默认字排。
    pub const DEFAULT: Self = Self {
        text_size: 17.0,
        text_line_height: 1.6,
        h1_size: 32.0,
        h1_weight: FontWeight::BOLD,
        h2_size: 24.0,
        h2_weight: FontWeight::BOLD,
        h3_size: 20.0,
        h3_weight: FontWeight::SEMIBOLD,
        h4_size: 18.0,
        h4_weight: FontWeight::SEMIBOLD,
        h5_size: 16.0,
        h5_weight: FontWeight::SEMIBOLD,
        h6_size: 14.0,
        h6_weight: FontWeight::SEMIBOLD,
        code_size: 15.0,
        dialog_title_size: 20.0,
        dialog_title_weight: FontWeight::SEMIBOLD,
        dialog_body_size: 14.0,
        dialog_body_weight: FontWeight::NORMAL,
        dialog_button_size: 14.0,
        dialog_button_weight: FontWeight::MEDIUM,
    };
}

/// 代码字体：等宽主字体 + 其他等宽与中日韩回退。
///
/// 编辑器整体沿用系统 UI 字体，只有代码块与行内代码切到等宽。
/// 回退表按平台一次性构建，热路径只克隆引用计数。
pub fn code_text_font() -> Font {
    static CODE_FONT: std::sync::OnceLock<Font> = std::sync::OnceLock::new();
    CODE_FONT
        .get_or_init(|| {
            let (family, fallbacks) = match std::env::consts::OS {
                "windows" => (
                    "Consolas",
                    &["Cascadia Mono", "Courier New", "Microsoft YaHei"][..],
                ),
                "macos" => ("Menlo", &["SF Mono", "Monaco", "PingFang SC"][..]),
                _ => (
                    "DejaVu Sans Mono",
                    &[
                        "Liberation Mono",
                        "Noto Sans Mono",
                        "Noto Sans CJK SC",
                        "WenQuanYi Micro Hei",
                    ][..],
                ),
            };
            let mut font = font(family);
            font.fallbacks = Some(FontFallbacks::from_fonts(
                fallbacks
                    .iter()
                    .map(|family| (*family).to_string())
                    .collect(),
            ));
            font
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用主题包（与 `ThemeSet` 同形）。
    const PACK_JSON: &str = r#"{"name":"pack","themes":[
        {"name":"Pack Dark","mode":"dark"},
        {"name":"Pack Light","mode":"light"}
    ]}"#;

    #[test]
    fn theme_ids_map_to_expected_modes() {
        assert_eq!(theme_mode_for_id(THEME_ID_DARK), ThemeMode::Dark);
        assert_eq!(theme_mode_for_id(THEME_ID_LIGHT), ThemeMode::Light);
        assert_eq!(theme_mode_for_id("unknown"), ThemeMode::Light);
    }

    #[test]
    fn builtin_catalog_lists_both_themes() {
        let catalog = bundled_theme_catalog();
        assert_eq!(catalog.len(), 2);
        assert!(catalog.iter().any(|entry| entry.id == THEME_ID_DARK));
        assert!(catalog.iter().any(|entry| entry.id == THEME_ID_LIGHT));
    }

    /// 内置标识切换明暗并回写标识；未知标识返回 false。
    #[rgpui::test]
    fn builtin_theme_round_trip(cx: &mut rgpui::TestAppContext) {
        cx.update(|cx| {
            init(cx, THEME_ID_LIGHT);
            assert_eq!(active_theme_id(cx), THEME_ID_LIGHT);
            assert!(set_theme_by_id(cx, THEME_ID_DARK));
            assert_eq!(active_theme_id(cx), THEME_ID_DARK);
            assert!(!set_theme_by_id(cx, "unknown"));
        });
    }

    /// 导入主题包 → 按名称应用 → 当前标识透出自定义名 → 目录恢复后仍可用。
    #[rgpui::test]
    fn custom_theme_pack_round_trip(cx: &mut rgpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!(
            "rumark-theme-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should advance")
                .as_nanos()
        ));
        let dirs = crate::config::RumarkConfigDirs::from_root(&root);
        let pack_path = root.join("pack.json");
        std::fs::create_dir_all(&root).expect("temp dir should be creatable");
        std::fs::write(&pack_path, PACK_JSON).expect("fixture should be writable");

        cx.update(|cx| {
            init(cx, THEME_ID_LIGHT);
            let names = import_theme_config_with_dirs(cx, &pack_path, &dirs)
                .expect("fixture pack should import");
            assert_eq!(names, vec!["Pack Dark", "Pack Light"]);

            assert!(set_theme_by_id(cx, "Pack Dark"));
            assert_eq!(active_theme_id(cx), "Pack Dark");

            // 目录恢复：模拟重启后注册表重建。
            load_persisted_theme_configs_from_dirs(cx, &dirs);
            assert!(set_theme_by_id(cx, "Pack Light"));
            assert_eq!(active_theme_id(cx), "Pack Light");

            // 目录包含内置两套 + 导入两套。
            let catalog = available_theme_catalog(cx);
            assert_eq!(catalog.len(), 4);
            assert!(catalog.iter().any(|entry| entry.id == "Pack Dark"));
        });

        std::fs::remove_dir_all(&root).ok();
    }
}
