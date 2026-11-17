# AGENTS.md — rumark（拾墨）

> **致人类贡献者**:本文档是面向 AI 编码助手的项目约定速查表,内容涵盖构建命令、架构约束、内部标识等。人类贡献者请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md),再按需查阅本文档获取细节。文中"不要改回 path 依赖""不要在本仓 patch 框架代码"等表述是对 AI 助手的约束(防止其绕过框架版本管理),人类贡献者按实际场景判断即可。

二进制 crate：基于 `rgpui` 的块式 Markdown 编辑器。

## 构建 / 运行 / 验证

- 构建：`cargo build -p rumark`；运行：`cargo run -p rumark -- [FILES...]`（无参数打开空文档）
- 发布构建用默认 `release`（thin LTO）；打安装包用 `cargo build -p rumark --profile size`（体积最小，构建慢）
- 常用参数：`--help/-h`、`--version/-v`、`--detach/-d`（仅 macOS 生效，其他平台忽略）
- 测试：`cargo test -p rumark`（单元测试全在源码内 `#[cfg(test)]` 模块，无独立 `tests/` 目录）
- 单测聚焦：`cargo test -p rumark <模块路径::测试名>`，如 `cargo test -p rumark editor::document`
- 校验顺序：`cargo fmt --check` → `cargo clippy -p rumark --all-targets` → `cargo test -p rumark`
- 手动渲染回归：用 `docs/render-sample.md` 打开验证（覆盖 CommonMark + GFM 扩展、嵌套、边界用例）
- 待办与进度交接：`docs/TODO.md`（未提交状态、待决事项、待办任务、验证基线、已知陷阱）

## 依赖与 Feature 约定

- `rgpui` / `rgpui-platform` 为 crates.io 版本依赖（当前 `1.7`，`Cargo.lock` 锁定精确版本）。框架升级时同步改 `Cargo.toml` 两处版本号 + `dev-dependencies` 的 `test-support` 项（三处必须同版本，不要留旧号靠 `^` 兜底），跑通后提交 `Cargo.lock`。不要改回 path 依赖，不要在本仓 patch 框架代码。
- 代码高亮是可选裁剪的：`code-highlight-core/official/config`、`html-native` 控制 `tree-sitter*` 依赖。默认全开。各 `build_*_config` 的 `#[cfg]` 必须与注册表里的调用点门控一致（yaml 归 `official`、toml 归 `config`），否则最小构建会引用未链接的 crate。
  - 最小编译验证：`cargo check -p rumark --no-default-features`
  - tree-sitter 版本全部 pin 死（`=x.y.z`），升级需整组对齐，不要单独 bump。
- 构建档位：`[profile.dev]` 本 crate `opt-level = 1`，`[profile.dev.package."*"]` 依赖 `opt-level = 2`，`rgpui`/`rgpui-macros` 再提到 `3`。debug 慢先怀疑自家代码，不要随手调低这些档位（`opt-level = 0` 下块布局与逐帧重绘会慢一个数量级）。
- 现存重型依赖：`printpdf` / `docx-rs`（PDF、DOCX 导出）、`mermaid-rs-renderer`（Mermaid）、`ratex-*`（LaTeX）、`reqwest`（远程图片与网络请求）。加新重型依赖前先确认必要性。

## 结构与入口

- 入口 `src/main.rs`：解析 CLI → `config::load_or_create_app_preferences` → 初始化 `I18nManager` / `theme::init`（rgpui 明暗主题）/ `EditorSettings` / `net::install_http_client` → `components::init_with_keybindings` + `init_app_menu`。改启动流程只动这里。
- `src/editor/`：文档模型与交互核心（`document.rs` 块模型、`tree.rs` 运行时树与可见快照、`derive.rs` 派生数据缓存、`events.rs` 事件分发、`history.rs` 撤销重做、`persistence.rs` 存取与导出、`workspace.rs`、`table_edit.rs`、`render.rs` 渲染、`source_map.rs` 源码映射、`selection.rs`、`find.rs` 查找替换、`context_menu.rs`、`image_menu.rs`、`image_cache.rs` 有界图片缓存、`file_drop.rs`、`status_bar.rs`、`window_layout.rs`、`titlebar_menu.rs` 标题栏菜单展开状态、`close.rs` 关闭保护、`editor_context.rs`）。
- `src/components/{block,markdown,latex,mermaid}/`：各类块渲染；`actions.rs` 是动作定义与按键绑定装配层（`actions!` 宏 + `install_keybindings`），偏好项开关用 rgpui `Switch` 组件（`config/preferences.rs`）。
- `src/{config,i18n,theme,net,export,app_menu.rs,app_identity.rs,file_url.rs,atomic_file.rs,text_offsets.rs,windows_integration.rs}`：设置持久化、多语言、主题、HTTP 客户端、导出渲染、菜单、原子写盘、文本偏移换算、Windows 打开方式与右键菜单登记。`file_url.rs` 仅 macOS（含 test 下）编译，`windows_integration.rs` 仅 Windows 编译；后者只写 HKCU，不碰 `UserChoice`。
- 编辑器根元素挂 `Entity<BoundedImageCache>`（`src/editor/image_cache.rs`）：整棵元素树内的 `img` 共用本窗口的 LRU 缓存（256 条上限、失败 5s 后自动重试）。新增图片渲染路径不要在根元素之外另建缓存，也不要退回框架全局 `fetch_asset`（它会把 `Err` 永久缓存）。
- 所有落盘统一走 `atomic_file`（`write_text`/`write_bytes`/`copy_file`：同目录临时文件 → `sync_all` → 重命名，失败清理临时文件），不要再用裸 `std::fs::write`/`fs::copy` 写配置文件、文档、粘贴图片或缓存。临时名带进程号与 UUID（同进程并发写同一目标不互撞），重命名遇共享冲突只短时重试 3 次。
- 文档读取一律走 `editor::read_markdown_file`（剥 UTF-8 BOM，非 UTF-8 返回 `NonUtf8Document`），提示文案用 `editor::read_error_detail`；不要再用 `std::fs::read_to_string` 直接读 Markdown，也不要在读取失败时只 `eprintln!`（release 的 Windows 没有控制台）。
- 界面文案走 rgpui 通用 `I18nManager` 包装（`src/i18n/mod.rs`，键值包 `zh-CN.json`/`en-US.json` 内嵌；热路径传 owned `I18nSnapshot`，勿持 `cx.global` 借用跨 `&mut` 调用）。键名一律下划线分层 `域[_组]_项`（如 `menu_file_save`、`keys_save`、`table_column_align_left`），域固定为 `win`/`dialog`/`menu`/`message`/`pref`/`keys`/`find`/`workspace`/`context`/`insert`/`table`/`image`/`code`/`status`/`export`；新增文案沿用该规范，两包键集必须一致。
- 颜色直接用 rgpui `Theme` 调色板（`theme.colors.X` 经 `Deref` 可写作 `theme.X`）；布局/字排是 `theme` 模块的 `ThemeDimensions::DEFAULT` / `ThemeTypography::DEFAULT` 常量。内置主题只有明暗两档（id 为 `rumark`(暗)/`rumark-light`(亮)），另有用户主题包导入（`AddThemeConfig` 动作 → `theme::load_theme_config`）。
- 窗口标题栏直接用 rgpui `TitleBar` 组件（`app_menu.rs` 的 `rumark_window_options` 只做选项基座；编辑器渲染单行 `TitleBar`：开头品牌名 + 框架 `Button` 菜单（`dropdown_menu` 下拉，定位/子菜单由组件处理），右侧三键由组件自动追加；有菜单时清零组件左内边距）：高度固定 34px（`TITLE_BAR_HEIGHT`），流式布局占位；窗口关闭一律走平台路径，经窗口级关闭保护拦截。
- 块文本测量走 `Window::request_measured_layout_simple`，不要“顺手”迁到 1.7 带 `fingerprint`/`text_state` 的复用入口：`TextLayout` 只有只读访问器（`line_layouts`/`bounds` 等），外部无法写入自己的换行结果，carry 命中时会交还空状态、绘制阶段拿不到行。框架侧开放写入后再评估。
- 内部标识（改名会破坏已存用户配置）：内置主题 id（`rumark` / `rumark-light`，映射到组件明暗两档）、action 命名空间（`rumark::...`，用户键位配置依赖）、测试临时文件 `rumark-*` 前缀（`config` 的历史记录过滤器依赖它）。应用标识与配置/缓存目录为 `rs.launcher.Rumark`。
- 版权：`LICENSE-APACHE` 正文为 Apache-2.0 标准文本（文件头版权为 `launcher-rs`）；不要改 `LICENSE-APACHE` 正文。

## 注释与语言规范

- 代码注释一律用中文（`//`、`///`、`//!` 全部中文；含测试模块内的注释）。
- 用户可见文案走 `i18n`，不要在代码里硬编码中英文混排的新增字符串。
- 文档（`README.md`、`AGENTS.md`）使用中文。

## 平台与测试注意

- 平台分支多用 `#[cfg(target_os = "macos")]`：`--detach` 重启自身、`on_open_urls` + 150ms 延迟启动窗口、`file_url`。非 macOS 改动后仍要在本机跑通测试，macOS 特有逻辑不要在 Windows/Linux 下强行验证。
- `#[cfg(not(debug_assertions))] windows_subsystem = "windows"`：release 下 Windows 无控制台，`eprintln!` 调试信息看不到。
- 已知被裁功能：版本更新检查（不要“补回”，也不要加回 `semver`）。HTML 单文件导出、PDF/DOCX 导出仍在，导出产物里的原始 HTML 必须经 `components::markdown::html::sanitize_html_fragment` 清洗，别绕过它直接 `push_html`。
- 配置文件 `config.toml` 带 `version` 字段：解析失败时备份为 `config.toml.bak` 并重建默认配置，由 `PendingConfigRecovery` 在首个窗口弹窗告知；版本号高于当前实现时不得回写默认值。
