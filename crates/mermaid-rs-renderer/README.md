# mermaid-rs-renderer（vendored）

本目录是 [mermaid-rs-renderer](https://github.com/1jehuang/mermaid-rs-renderer) 0.3.1 的 vendor 副本，由拾墨（rumark）自行维护，原协议 MIT（见 [LICENSE](LICENSE)），上游变更历史见 [CHANGELOG.md](CHANGELOG.md)。

## 相对上游的改动

- 裁剪 `cli` / `png` feature 与 `mmdr` CLI 目标（连同 clap、resvg、usvg 依赖），只保留 lib 渲染 SVG 能力。
- 裁剪 criterion 基准与对应 `[[bench]]`；`benches/` 下只保留 `fixtures/`、`typical/` 语料（多个测试 suite 用它们做回归输入）。
- 删除 `tests/cli_suite.rs`（依赖被裁掉的 mmdr bin）。
- `layout/mod.rs` 的 `cycle_fixture_subgraph_entry_aligns_with_spine`：文本度量依赖本机字体，放宽为允许一次 16px（一个网格单元）小折角。
- 状态图（stateDiagram）布局修复：见 `src/render.rs` / `src/layout/flowchart/objectives.rs` / `src/layout/flowchart/route_labels.rs` 内 `rumark` 注释标注的位置。

本仓约定：修改此 crate 时在代码中以 `rumark` 字样注明改动点，便于将来对照上游升级；依赖关系为 rumark 根 Cargo.toml 的 path 依赖，属于自维护 vendor，不受「不要改回 path 依赖」红线约束（该红线仅针对 rgpui）。
