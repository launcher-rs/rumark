# 检查更新功能设计

## 目标与约束

拾墨以**单执行文件**分发：GitHub Release 资产是压缩包（Windows `rumark-windows-x86_64.zip`，
Linux `rumark-linux-x86_64.tar.gz`，macOS `rumark-macos-aarch64.tar.gz`），解压后是单个可执行文件。

需求约束（来自用户）：

1. **不自动检查**：只有用户主动点击「检查更新」才发起网络请求与下载，启动/定时均不触发。
2. **手动下载安装**：检查到新版本后显示版本号、发布日期、更新说明与下载按钮，用户确认才下载。
3. **国内加速**：GitHub 在国内访问不稳定，默认走一组可用镜像；镜像可能失效，支持用户自定义镜像地址。
4. **失败可诊断**：镜像逐个尝试、自动回退，全部失败时给出清晰提示并列出已尝试的地址。

## 版本与资产约定

- 当前版本来自 `env!("CARGO_PKG_VERSION")`（当前 `0.2.4`）。
- 最新版本来自 GitHub API：`GET https://api.github.com/repos/launcher-rs/rumark/releases/latest`，
  字段 `tag_name`（形如 `v0.2.5`）、`published_at`、`body`、`assets[].name`、`browser_download_url`。
- 平台资产名按 `OS + ARCH` 拼出，与 Release 命名一致：
  - Windows x86_64 → `rumark-windows-x86_64.zip`
  - Linux x86_64 → `rumark-linux-x86_64.tar.gz`
  - macOS aarch64 → `rumark-macos-aarch64.tar.gz`
- 版本比较：去掉前导 `v`，按 `major.minor.patch` 数值比较，忽略预发布后缀；远端更高才提示更新。

## 镜像策略

实测（2026-11-21）：`api.github.com` 直连可用；`ghfast.top`、`ghproxy.net` 返回 206 可用；
`github.moeyy.xyz` 已失效（000）。这印证了镜像会失效，必须可配置。

分层设计：

1. **版本查询（API）**：先直连 `api.github.com`；失败则依次尝试镜像化的 API 前缀
   （如 `https://ghfast.top/https://api.github.com/...`）。API 响应小，失败快速跳过。
2. **资产下载**：对选中的资产 URL，依次尝试「直连 + 内置镜像 + 用户自定义镜像」。
   镜像以 URL 前缀形式套在原始 GitHub 下载链接外：`<mirror>/<original-url>`。
3. **用户自定义**：偏好设置里可填一行/多行自定义镜像前缀（留空则用内置默认）。
   每个镜像独立尝试，超时（默认 15s）或非 2xx 即换下一个；全部失败汇总已尝试地址。
4. **内置默认列表**（按可用性排序，随版本可更新）：
   - `https://ghfast.top`
   - `https://ghproxy.net`
   - 直连（放最后，作为有代理/海外环境的兜底）

自定义镜像格式约定：只填域名前缀，如 `https://mirror.example.com`；
程序自动拼成 `https://mirror.example.com/https://github.com/.../asset.zip`。
这样用户无需关心拼接细节，也便于将来替换失效镜像。

## 下载与安装流程

1. 用户点「检查更新」→ 异步查询版本（UI 显示「正在检查…」，不阻塞编辑）。
2. 发现新版本 → 弹出结果对话框：当前版本 → 新版本、发布日期、更新说明、[下载更新] [稍后] [打开发布页]。
3. 用户点「下载更新」→ 用镜像列表下载资产到临时目录，显示进度；校验文件非空（可选校验大小）。
4. 下载完成 → 解压出可执行文件：
   - zip（Windows）：解析中央目录，取出 `rumark.exe`。
   - tar.gz（Linux/macOS）：解压后取出 `rumark`。
5. **替换策略**（运行中程序不能直接覆盖自身，尤其 Windows）：
   - 不自动覆盖当前 exe；把新 exe 落到应用目录下的临时文件 `rumark.new.exe`（或 `.new`），
   - 弹出提示：「新版本已下载，是否现在重启完成安装？」确认则：
     启动一个极小的替换脚本/命令，等待本进程退出后用新文件覆盖旧 exe 再重启；
     若平台/权限不允许自动替换，则打开新 exe 所在目录，指导用户手动替换并重启。
   - 取消则保留临时文件，下次启动可提示「已下载更新待安装」。
6. 失败时清理临时文件，对话框显示错误原因与已尝试镜像。

**已实现（一期）**：检查 + 下载 + zip 解压 + 落到 `配置目录/update/<exe>.new` +
弹窗给出路径与手动替换说明。跨进程自替换脚本（关窗后自动覆盖旧 exe 并重启）
作为后续增强：Windows 下运行中进程无法覆盖自身，需额外的守护脚本，暂不纳入。

**未实现**：Linux/macOS 的 `.tar.gz` 解包（当前下载到该格式会给出明确提示，
引导用户打开发布页手动下载）。如需支持，可新增 `flate2`+`tar` 依赖并扩展
`download_and_stage`。

## 偏好设置

- 新增偏好页「更新」：
  - 自定义镜像前缀输入框（多行，每行一个；留空用内置默认）。
  - 「检查更新」按钮（与菜单入口等效，均为手动触发）。
  - 说明文字：更新需手动检查；镜像失效可自行更换。
- 偏好文件新增 `update` section：`custom_mirrors: Vec<String>`。
- 不做「启动自动检查」开关（需求明确不要自动检查）。

## 菜单与动作

- Help 菜单在「关于」前新增「检查更新」→ `CheckForUpdates` 动作。
- 全局注册动作，绑定 `dispatch_menu_action`。
- 同步更新菜单测试（Help 菜单项数量从 1 变为 2）。

## i18n 键

新增（en-US / zh-CN 各一份）：
`menu_help_check_updates`、`pref_nav_update`、`pref_update_title`、
`pref_update_mirrors_label`、`pref_update_mirrors_placeholder`、`pref_update_mirrors_hint`、
`pref_update_check_button`、`update_checking`、`update_available_title`、
`update_available_message`、`update_none_title`、`update_none_message`、
`update_download`、`update_later`、`update_open_release`、`update_downloading`、
`update_download_failed`、`update_downloaded_restart`、`update_restart_now`、
`update_manual_replace`、`update_error`、`update_current_version`、`update_new_version`、
`update_published_at`、`update_notes`。

## 测试

- 版本比较纯函数单测（v 前缀、补丁号、相等、预发布）。
- 资产名映射单测（各平台）。
- 镜像 URL 拼接单测（含自定义、失效回退顺序）。
- Release JSON 解析单测（用固定样例，不发真实网络）。
- 偏好读写单测（自定义镜像持久化）。

## 手动验收

- 本机点「检查更新」：应显示 0.2.4 已是最新（当前 release 就是 0.2.4）。
- 断网/坏镜像：应逐个回退并给出清晰错误，不崩溃、不长时间卡 UI。
