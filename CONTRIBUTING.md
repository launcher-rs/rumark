# Contributing to Rumark

感谢你对 Rumark(拾墨)的关注!本文档面向外部贡献者,说明如何构建、测试和提交代码。

## 开发环境

- **Rust**: 2024 edition,最低 1.85(含 `let` chains 语法)
- **平台**: Windows / macOS / Linux
- **框架**: [rgpui](https://crates.io/crates/rgpui) 1.7(crates.io 依赖,非 path 依赖)

## 构建与运行

```bash
# 克隆仓库
git clone https://github.com/launcher-rs/rumark.git
cd rumark

# 构建(默认全功能)
cargo build -p rumark

# 运行
cargo run -p rumark -- [FILES...]

# 无参数打开空文档
cargo run -p rumark
```

## 验证

提交 PR 前请确保全部通过:

```bash
# 1. 代码格式
cargo fmt --check

# 2. 静态检查(要求 0 警告)
cargo clippy -p rumark --all-targets

# 3. 单元测试(内嵌在源码 #[cfg(test)] 模块中)
cargo test -p rumark

# 4. 最小构建验证(无默认 feature)
cargo check -p rumark --no-default-features

# 5. Mermaid 渲染库的回归测试（独立仓 launcher-rs/mermaid-renderer）
cd ../mermaid-renderer && cargo test
```

## 代码规范

- **注释**:一律使用中文(`//`、`///`、`//!`,含测试模块)
- **用户可见文案**:走 `i18n`(`src/i18n/`),不要硬编码中英文混排字符串
- **文件写入**:统一走 `atomic_file` 模块(`write_text`/`write_bytes`/`copy_file`),不要用裸 `std::fs::write`
- **文档读取**:走 `editor::read_markdown_file` 或 `editor::read_error_detail`
- **HTML 导出**:原始 HTML 必须经 `sanitize_html_fragment` 清洗
- **内部标识**:不要修改 `rumark` / `rumark-light` 主题 id、`rumark::...` action 命名空间、`rs.launcher.Rumark` 应用标识 —— 改名会破坏已存用户配置
- **依赖**:新增重型依赖前先说明理由;tree-sitter 版本全部 pin 死(`=x.y.z`),升级需整组对齐
- **Mermaid 渲染库**:`mermaid-renderer` 已拆到独立仓(上游 0.3.1 分叉,MIT),以 git 依赖引入;改布局代码请在该仓提交、以 `fork:` 注释标注改动点,并跑它的测试套件,再回本仓 `cargo update -p mermaid-renderer`
- **换行符**:所有 `.rs` 文件必须为 CRLF 无 BOM(项目通过 `.gitattributes` 保证)

## 提交规范

- 提交信息使用中文,格式:`feat: / fix: / docs: / chore: / refactor: / test:` + 简短描述
- 一个 PR 只做一件事,避免混入无关改动
- PR 描述中说明改动动机和验证方式
- 面向用户的变化(新功能、修复、行为变更)请同步写入 [CHANGELOG.md](CHANGELOG.md) 的「未发布」段,条目使用中文、面向用户描述;发布说明由工作流从 CHANGELOG 自动提取
- 解释性内容(提交信息、文档、注释、Issue / PR 回复)一律使用中文

## 报告问题

- **Bug**:使用 GitHub Issues,附上复现步骤、系统信息、版本号
- **安全漏洞**:请勿在公开 issue 中报告,参见 [SECURITY.md](SECURITY.md)

## 许可证

贡献代码即表示你同意将贡献以 Apache-2.0 许可证授权给项目。详见 [LICENSE-APACHE](LICENSE-APACHE) 和 [NOTICE](NOTICE)。