# 拾墨 Rumark

基于 `rgpui` 的块式 Markdown 编辑器：所见即所得渲染编辑与 Markdown 源码编辑两种模式，原生桌面 UI。

## 快速开始

前置要求：Rust 2024 edition 工具链、Cargo，及 `rgpui` 所需的平台原生构建依赖。

```sh
cargo run -p rumark -- [FILES...]
```

- 不带参数打开空文档；可一次打开多个 Markdown 文件。
- 常用参数：`--help/-h`、`--version/-v`、`--detach/-d`（仅 macOS 生效）。
- 渲染回归验证：用 `docs/render-sample.md` 打开（覆盖 CommonMark + GFM 扩展、嵌套与边界用例）。

## 特性

- **块模型**：Markdown 结构表示为可编辑块，结构清晰、可控、可扩展，无预览窗同步循环。
- **原生 UI**：基于 `rgpui` 的桌面原生渲染。
- **双编辑模式**：渲染态所见即所得编辑 + Markdown 源码编辑。
- **主题与语言包**：支持导入自定义主题 / 语言配置（`.json` / `.jsonc`）。缺失字段回退到内置主题（`rumark` / `rumark-light`）与内置英文文案。
- **单文件便携**：编译后为单个可执行文件，免安装，目标平台 Windows / Linux / macOS。

## 校验

```sh
cargo fmt --check
cargo clippy -p rumark --all-targets
cargo test -p rumark
```

- 最小编译验证：`cargo check -p rumark --no-default-features`。
- 单测聚焦：`cargo test -p rumark <模块路径::测试名>`。

## 许可

Apache-2.0，见 [`LICENSE-APACHE`](./LICENSE-APACHE)。
