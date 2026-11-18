# 拾墨 Rumark

[![CI](https://img.shields.io/github/actions/workflow/status/launcher-rs/rumark/ci.yml?branch=main&label=CI)](https://github.com/launcher-rs/rumark/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/launcher-rs/rumark?label=Release)](https://github.com/launcher-rs/rumark/releases)
[![License](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE-APACHE)

基于 `rgpui` 的块式 Markdown 编辑器：所见即所得渲染编辑与 Markdown 源码编辑两种模式，原生桌面 UI。

## 快速开始

前置要求：Windows 环境、Rust 2024 edition 工具链与 Cargo，以及 `rgpui` 所需的平台原生构建依赖。

```sh
cargo run -p rumark -- [FILES...]
```

- 不带参数打开空文档；可一次打开多个 Markdown 文件。
- 常用参数：`--help/-h`、`--version/-v`。
- 渲染回归验证：用 `docs/render-sample.md` 打开（覆盖 CommonMark + GFM 扩展、嵌套与边界用例）。

## 特性

- **块模型**：Markdown 结构表示为可编辑块，结构清晰、可控、可扩展，无预览窗同步循环。
- **原生 UI**：基于 `rgpui` 的桌面原生渲染。
- **双编辑模式**：渲染态所见即所得编辑 + Markdown 源码编辑。
- **主题与语言包**：支持导入自定义主题 / 语言配置（`.json` / `.jsonc`）。缺失字段回退到内置主题（`rumark` / `rumark-light`）与内置英文文案。
- **导出与分享**：导出单文件 HTML / PDF / Word，或把 HTML、Markdown 直接复制到剪贴板。
- **单文件便携**：编译后为单个可执行文件，免安装；当前仅在 Windows 上发布与验证。

## 工作区与默认打开方式

- 点击底部状态栏「工作区」或按 `Ctrl+W`，展开文件树与文档大纲；再次点击或按键收起。标题栏不再单独放置工作区菜单。
- Windows：打开「文件 → 偏好设置 → 文件」，两个按钮都只写当前用户、无需管理员权限：
  - 「添加到右键菜单」：在资源管理器中右键 `.md` / `.markdown` 文件，选择「用 Rumark 打开」。
  - 「在 Windows 中设置…」：登记打开能力后打开系统设置，由你在其中把 `.md` / `.markdown` 关联到 Rumark；程序不会自动抢占默认应用。
  - 移动 EXE 后需要重新点击对应按钮登记。
- Windows 构建会内嵌多尺寸应用图标，无需在 EXE 旁额外放置图标文件；已固定的任务栏快捷方式若显示旧图标，可取消固定后重新固定。

## 导出与分享

窗口左上角品牌名右侧的菜单按钮（或应用菜单「文件」）里，导出项集中在「文件 → 导出」子菜单：

- **导出 → HTML**：生成单文件 HTML（内嵌样式、可选目录，图片按相对路径引用），经另存框落盘。
- **导出 → PDF / Word（DOCX）**：纯 Rust 渲染后另存；PDF 缺中文字体时会弹窗提示。
- **导出 → 复制 HTML**：不落盘，把同一份单文件 HTML 放进剪贴板，便于发博客或邮件。
- **导出 → 复制 Markdown**：复制当前文档全文（渲染模式按规范序列化，源码模式复制原始文本）。

导出产物里的原始 HTML 会经过安全清洗：脚本标签、事件属性（`on*`）与 `javascript:` 一类危险协议会被剔除或转义为文本。

## 校验

```sh
cargo fmt --check
cargo clippy -p rumark --all-targets
cargo test -p rumark
```

- 最小编译验证：`cargo check -p rumark --no-default-features`。
- 单测聚焦：`cargo test -p rumark <模块路径::测试名>`。

## 参考

块式编辑交互与界面设计参考了：

- [Typora](https://typora.io/)
- [velotype](https://github.com/manyougz/velotype)

## 许可

Apache-2.0，见 [`LICENSE-APACHE`](./LICENSE-APACHE)。
