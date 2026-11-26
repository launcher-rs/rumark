# TODO — 拾墨（rumark）进度交接单

> 用途：把当前进度、待决事项与验收方式落成文件，**下一次照此直接执行**，不必翻聊天记录。
> 约定：完成一项就删掉该条目；修复记录一句话说清状态与结论，细节留在代码注释和 CHANGELOG。
> 已修复并验证的内容简明留档，另保留长期有效的坑位知识；状态只标「已修复 / 待处理」，不记提交进度。

## 1. 修复记录

- **已修复** 表格边框悬停抖动（issue #2）：悬停反馈不再改变表格布局，实机确认顶部 / 右侧 / 底部边框悬停稳定。
- **已完成** rgpui 切 git 源码：四处依赖改用上游 `main`，`[patch.crates-io]` 统一 rgpui-extra 的依赖源，完整校验与最小构建通过。
- **已修复** 两个对话框测试失败：测试初始化补上缺失的 `PendingApplicationQuit` global，全量测试通过。
- **已修复** DOCX 导出观感与导出目录：补齐文档级默认字体/字号/行距，并注册具名样式（Heading1-6、Title、CodeBlock、Quote）——WPS/Word 能识别语义而不是当成无样式段落；表格网格线、表头底纹与单元格内边距（上下 4pt、左右 6pt）、代码块与行内代码底纹及内边距、真实横线（只写下边框，避免变成矩形）、公式栅格化为图片、emoji 单独换 Segoe UI Emoji 字体；代码块上下与表格前后各留 6pt 空白；HTML / PDF / DOCX 默认不带目录，旧配置自动迁移为关闭。
- **已处理** 样张开头的 HTML 注释按 Markdown 语义跳过，不进入导出。
- **决定不做** 导出首行缩进：Markdown 正文无首行缩进，导出与预览保持一致。
- **保持现状** 远程图片仍尝试下载并内嵌，失败回退替代文本；样张里 `example.com` 的测试图不可达，显示占位属预期。

## 2. 待决事项（需要拍板）

暂无。

## 3. 待办任务（可直接执行）

### 3.1 性能 backlog 剩余（低优先，需单独回归）

- 保存 / HTML 导出移后台 + 进度提示：涉及 `atomic_file` 的 `sync_all` / rename 重试与对话框 / 关闭保护状态机，改动面大且难用单测覆盖，暂留 UI 线程同步写；数据丢失风险已由「写盘前重取 + 写后比对」消除。
- 粘贴图片后台编码/落盘、内嵌图改引用、启动先出窗再并行加载、最近文件写入防抖合并：属启动 / 事件时序重构，建议单独排期，避免重复改动 `app_menu.rs` / `main.rs`。

### 3.2 待核对

- 用 Word 打开 `docs/render-sample.md` 导出的样张，确认标题层级、表格边框与无目录效果。

**别误改（已确认无问题）**：光标闪烁已睡到边界、滚动条淡出是一次性定时器、拖拽自动滚动仅拖拽期 16ms 运行、右键 / 图片菜单轻量、PDF `wrap_text` 为 O(n)、DOCX 不加载字体文件；渲染「先分组后建元素」两趟 + 只为视口内行建元素的优化已在位。

## 5. 验证流程（每次改完照跑）

```pwsh
cargo fmt --check
cargo clippy -p rumark --all-targets          # 期望 0 warning
cargo test -p rumark                          # 当前基线 1058 passed / 0 failed / 10 ignored
cargo check -p rumark --no-default-features   # 期望少量既有 warning
git diff --check -- 'src/*.rs' 'AGENTS.md'    # 期望无输出
```

- 测试总量会随用例增删变化，比较时以「失败数为 0」为准。
- 所有 `.rs` 必须是 **CRLF 无 BOM**（校验：LF 计数 == CR 计数，首字节不是 `EF BB BF`）。

## 6. 已知陷阱（踩过的坑）

1. **edit 工具偶发把整文件写成 LF**：发现后按字节转回（不要用文本读写）：

   ```pwsh
   $p = 'src/xxx.rs'
   $b = [IO.File]::ReadAllBytes($p)
   $o = New-Object System.Collections.Generic.List[byte]
   foreach($x in $b){ if($x -eq 10){ $o.Add(13); $o.Add(10) } else { $o.Add($x) } }
   [IO.File]::WriteAllBytes($p, $o.ToArray())
   ```

   新文件（`write` 工具创建）默认也是 LF，同样要转。
2. **禁止**用 PowerShell `Get-Content` / `Set-Content` 改源码文件 —— 本会话曾因此毁掉 `src/editor/source_mapping.rs` 的换行，只能去 opencode 快照仓按字节抢救。取 git 内文件请用 `cmd /c "git show HEAD:path > file"`。
3. **`cargo fmt` 会把被改写的文件落成 LF**（`cargo fmt --check` 不会）——跑完 `cargo fmt` 要按第 6.1 条转回 CRLF。
4. 跨块复制/删除的源码映射：任何字符串切片/替换都要经 `normalize_utf8_range` + `get()` 兜底（多字节 UTF-8 文本），不要按 usize 裸切。
5. 表格是**唯一**没有自身 `SourceTargetMapping` 的块（只推单元格映射），整表复制走 `src/editor/selection.rs` 的回退分支，改动表格相关映射时两边都要看。
6. `rgpui` / `rgpui-platform` 现为 git 源码（见第 1 节）：四处依赖要一起改，并保留 `[patch.crates-io]` 指向同一 git 源，否则 rgpui-extra 会拉入 crates.io 版造成双版本类型冲突。
7. 行内强调：`has_closing_delimiter` 预扫描与 `parse_until` 的闭合判定**必须共用 `emphasis_close_at`**，且递归解析前要 `builder.checkpoint`、闭不上时 `builder.rollback`。两者任一失配都会让未闭合区间的正文被外层重放，产出重复文本并且**保存即把重复写进文件**（`__foo__bar` 曾写出三段）。口径对齐只挡住扁平场景，区间内还有更短的嵌套分隔符时预扫描必然偏乐观，回滚才是兜底。已知与规范的偏差逐条写在 `commonmark_emphasis_documented_deviations`，新增偏差要写明原因。
8. 实机脚本合成 Shift：`keybd_event` 送 Shift 时 `GetAsyncKeyState` / `GetKeyState` 偶发拿不到修饰键，Shift+方向键会退化成单键；改用 `System.Windows.Forms.SendKeys` 的 `"+{RIGHT}"`。键盘行为以 `cx.simulate_keystrokes` 的单测为准。

## 7. 参考

- `AGENTS.md`：项目约定、构建/验证顺序、结构与平台注意事项。
- `docs/render-sample.md`：手动渲染回归样本。
- 本轮改动涉及的核心文件：`Cargo.toml`、`Cargo.lock`、`CHANGELOG.md`、`src/components/block/render.rs`。
