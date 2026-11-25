# 检查更新功能设计

## 目标与约束

拾墨以**单执行文件**分发：GitHub Release 资产是压缩包（Windows `rumark-windows-x86_64.zip`，
Linux `rumark-linux-x86_64.tar.gz`，macOS `rumark-macos-aarch64.tar.gz`），解压后是单个可执行文件。

需求约束（来自用户）：

1. **不自动检查**：只有用户主动点击「检查更新」才发起网络请求与下载，启动/定时均不触发。
2. **确认后安装**：检查到新版本后显示版本号、发布日期、更新说明与下载按钮，用户确认才下载；下载完成后再次确认才替换安装，绝不擅自改动正在运行的程序。
3. **按需加速**：默认直连；镜像中转在偏好里显式开启后才生效，可自定义镜像地址。镜像可能失效，需支持逐个回退。
4. **失败可诊断**：镜像逐个尝试、自动回退，全部失败时给出清晰提示并列出已尝试的地址。

## 组件边界

通用逻辑已抽到独立仓 [launcher-rs/rgpui-extra](https://github.com/launcher-rs/rgpui-extra)，
供 launcher-rs 其他程序复用。该仓是单 crate 多 feature 的 rgpui 扩展库（以 git 源依赖）：
`rgpui_extra::updater` 模块（feature `updater`，需显式开启）提供 `Config` / `Provider` /
`Version` / `check` / `download_and_stage` / `load_staged_update` / `spawn_install_helper`
等核心逻辑，不依赖 GUI，应用只注入仓库、发布平台、应用名、资产名、镜像偏好与当前版本；
`ui` feature 可选，提供 `render_update_dialog`（限高可滚动的说明区 + 三个按钮）与
`UpdateStatus`（状态栏进度模型，含下载完成阶段）。
rumark 的 `src/update.rs` 只剩应用配置薄层，`src/editor/update.rs` 负责 i18n、对话框与进度回传。

新程序接入顺序：以 git 源依赖 `rgpui-extra` 并开启 `ui` feature → 实现偏好里的
`use_mirrors`/`custom_mirrors` → 用 `updater_provider()` 选定发布平台 → 调用 `updater::check()`
→ 展示对话框 → `updater::download_and_stage()`（传版本号）并把 `DownloadProgress` 投给自己的 UI
→ 下载完成后用 `load_staged_update()` 判断可否复用，用户确认时用 `spawn_install_helper()` 替换重启。

## 版本与资产约定

- 当前版本来自调用方传入的 `env!("CARGO_PKG_VERSION")`（发布版本与 tag 一致，如 `0.2.5`）。
- 默认值平台为 GitHub：`GET https://api.github.com/repos/launcher-rs/rumark/releases/latest`，
  字段 `tag_name`（形如 `v0.2.5`）、`published_at`、`body`、`assets[].name`、`browser_download_url`。
  `Provider` 还可切到 Gitee（同构）、GitLab（`assets.links` / `released_at` / `direct_asset_url`）
  或 Gitea/Forgejo 等兼容 GitHub API 的自定义地址；rumark 当前默认 GitHub。
- 平台资产名按 `OS + ARCH` 拼出，与 Release 命名一致：
  - Windows x86_64 → `rumark-windows-x86_64.zip`
  - Linux x86_64 → `rumark-linux-x86_64.tar.gz`
  - macOS aarch64 → `rumark-macos-aarch64.tar.gz`
- 版本比较：去掉前导 `v`，按 `major.minor.patch` 数值比较，忽略预发布后缀；远端更高才提示更新。

## 镜像策略

实测（2026-11-21）：`api.github.com` 直连可用；`ghfast.top`、`ghproxy.net` 返回 206 可用；
`github.moeyy.xyz` 已失效（000）。这印证了镜像会失效，必须可配置。

分层设计：

1. **开关**：偏好 `update.use_mirrors` 默认 false。关闭时 `candidate_mirrors()` 只返回直连；
   开启后返回「自定义 → 内置 → 直连」去重列表。查询与下载共用该列表。
2. **版本查询（API）**：按列表逐一尝试镜像化的 API 前缀（直连为 `https://api.github.com/...`，
   镜像为 `https://ghfast.top/https://api.github.com/...`）。响应小，失败快速跳过。
3. **资产下载**：同样按列表逐一尝试，镜像以 URL 前缀形式套在原始下载链接外。
   每个请求 10s 连接超时 / 20s 总超时，非 2xx 即换下一个；全部失败汇总已尝试地址。
   用户自定义：偏好设置里可填一行/多行镜像前缀，或一键填入内置推荐列表。
4. **内置默认列表**（按可用性排序，随版本可更新）：
   - `https://ghfast.top`
   - `https://ghproxy.net`
   - 直连（放最后，作为有代理/海外环境的兜底）

自定义镜像格式约定：只填域名前缀，如 `https://mirror.example.com`；
程序自动拼成 `https://mirror.example.com/https://github.com/.../asset.zip`。
这样用户无需关心拼接细节，也便于将来替换失效镜像。

## 下载与安装流程

1. 用户点「检查更新」→ 状态栏显示「正在检查更新…」，后台线程查询，不阻塞编辑。
2. 发现新版本 → 自绘 `Dialog` 覆盖层：当前版本 → 新版本、发布日期、Markdown 渲染的更新说明
   （限高 220px、超出可滚动）、[稍后] [打开发布页] [下载更新]；
   若本地已有与最新版本匹配的暂存文件，主按钮变为「安装已下载的更新」。
3. 用户点「下载更新」→ 后台按镜像列表下载，状态栏右侧显示百分比与字节数、上方显示细进度条；
   校验文件非空与 200MB 上限。进度泵每次排空消息队列只保留最新一条，避免显示过期进度。
4. 下载完成 → 状态栏立即切到「更新下载完成，准备安装」再弹确认，两者不再矛盾。解压出可执行文件：
   - zip（Windows）：取出 `rumark.exe`。
   - tar.gz（Linux/macOS）：取出 `rumark`。
   落地为 `配置目录/update/<exe>.new`，并同时写 `<exe>.new.version` 标记版本。
5. 弹出安装确认：[稍后] [立即安装]。
   - 立即安装 → 启动独立 helper 脚本 → 走标准退出流程（含未保存文档检查）
     → helper 等待进程退出后用新 exe 覆盖旧 exe，并带原命令行参数重启。
   - 稍后 → 保留 `.new` 与版本标记；下次检查更新时匹配到同一版本即可直接安装，不重复下载。
   - 若退出被用户取消，helper 超时后自行退出，不会误替换。
6. 失败时对话框显示错误原因与已尝试镜像；暂存文件校验不通过则视为无缓存，重新下载。

## 偏好设置

- 新增偏好页「更新」：
  - 「启用镜像中转」开关（默认关闭，关闭时仅直连）。
  - 自定义镜像前缀输入框（多行，每行一个）+「填入推荐镜像」按钮。
  - 「检查更新」按钮（与菜单入口等效，均为手动触发）。
- 偏好文件 `update` section：`custom_mirrors: Vec<String>`、`use_mirrors: bool`（默认 false）。
- 不做「启动自动检查」开关（需求明确不要自动检查）。

## 菜单与动作

- Help 菜单在「关于」前新增「检查更新」→ `CheckForUpdates` 动作。
- 全局注册动作，绑定 `dispatch_menu_action`。
- 同步更新菜单测试（Help 菜单项数量从 1 变为 2）。

## i18n 键

新增（en-US / zh-CN 各一份，完整清单见 `src/i18n/zh-CN.json`）：
`menu_help_check_updates`、`pref_nav_update`、`pref_update_title`、
`pref_update_mirrors_label`、`pref_update_mirrors_placeholder`、`pref_update_mirrors_hint`、
`pref_update_check_button`、`update_checking`、`update_available_title`、
`update_none_message`、`update_download`、`update_install_downloaded`、`update_later`、
`update_open_release`、`update_downloading`、`update_download_complete`、`update_download_failed`、
`update_install_title`、`update_install_confirm`、`update_install_now`、
`update_install_cancel`、`update_install_failed`、`update_error`、
`update_current_version`、`update_new_version`、`update_published_at`、`update_notes`。

## 测试

- 版本比较纯函数单测（v 前缀、补丁号、相等、预发布）。
- 资产名映射单测（各平台）。
- 镜像 URL 拼接单测（含自定义、失效回退顺序）。
- Release JSON 解析单测（用固定样例，不发真实网络）。
- 偏好读写单测（自定义镜像持久化）。

## 手动验收

- 发布后本机点「检查更新」：应显示 0.2.5 已是最新。
- 断网/坏镜像：应逐个回退并给出清晰错误，不崩溃、不长时间卡 UI。
