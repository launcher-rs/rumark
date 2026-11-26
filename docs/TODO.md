# TODO — 拾墨（rumark）进度交接单

> 用途：把当前进度、待决事项与验收方式落成文件，**下一次照此直接执行**，不必翻聊天记录。
> 约定：完成一项就删掉该条目；每条按「背景 / 做法 / 验收」写清。
> 已随提交落地的内容不再留档，只保留当前未提交事项与长期有效的坑位知识。

## 1. 工作区状态与改动分组

2026-11-26 **表格边框悬停抖动修复（未提交）**：对应 GitHub issue #2。根因是悬停反馈改动了表格自身布局——列轴预览使 `top_gutter` 从 0 变 18px、追加行列悬停使 `right_gutter` / `bottom_gutter` 从 0 变 24px（经 `.pr()` / `.pb()` 挤压），而高亮带与追加热区正贴在表格边界，布局位移反过来触发反向悬停事件，形成「进入→位移→离开→弹回」自持环。修复：`render.rs` 抽出 `table_hover_gutters()` 让三种占位常驻、列轴行无条件渲染，悬停只切换高亮色与绝对定位按钮显隐；原 `top_gutter_only_appears_for_column_axis_state` 单测改为穷举所有状态组合断言占位不变。验收：`cargo fmt --check` 通过、clippy 0 警告、定向单测通过；手动回归待用 `docs/render-sample.md` 实机核对顶部 / 右侧 / 底部边框悬停是否稳定。

2026-11-26 **rgpui 切 git 源码（未提交）**：上游 Linux 后端有大量尚未发版的更新，`Cargo.toml` 四处 `rgpui` / `rgpui-platform` 依赖从 crates.io `1.8.4` 改为 `git = "https://github.com/launcher-rs/rgpui.git"`（不写 version，避免上游 git 内 bump 版本后构建失败）；另加 `[patch.crates-io]` 把 rgpui-extra 传递依赖的 crates.io 版也指向同一 git 源，杜绝双版本类型冲突。`Cargo.lock` 锁定上游 `main` 的 commit。验收：切换后的完整校验与最小构建见第 5 节。

2026-11-26 **测试缺失 global 导致的两个失败已修（未提交）**：`unsaved_changes_dialog_tracks_framework_layer` 与 `dialog_button_click_routes_through_captured_entity` 原先 panic 在 rgpui 的 `crates/rgpui/src/app.rs:22`「global type must match」（文案有误导性，实为 `lease_global` 解包 `Option` 时 global 不存在）。调用链为取消未保存对话框 → `cancel_pending_application_quit` → `update_global::<PendingApplicationQuit>`；该 global 生产由 `app_menu::init` 注册，但测试初始化漏了。修复：`app_menu.rs` 加 `#[cfg(test)]` 的窄 helper `init_pending_application_quit_for_test`（不装窗口关闭订阅，避免测试退出应用），`editor/tests.rs` 的 `init_editor_test_app` 调用它。两个测试单独跑均通过。

## 2. 待决事项（需要拍板）

暂无。

## 3. 待办任务（可直接执行）

### 3.1 性能 backlog 剩余（低优先，需单独回归）

- 保存 / HTML 导出移后台 + 进度提示：涉及 `atomic_file` 的 `sync_all` / rename 重试与对话框 / 关闭保护状态机，改动面大且难用单测覆盖，暂留 UI 线程同步写；数据丢失风险已由「写盘前重取 + 写后比对」消除。
- 粘贴图片后台编码/落盘、内嵌图改引用、启动先出窗再并行加载、最近文件写入防抖合并：属启动 / 事件时序重构，建议单独排期，避免重复改动 `app_menu.rs` / `main.rs`。

### 3.2 本次改动待验

- 用 `docs/render-sample.md` 实机核对表格边框悬停稳定性（顶部 / 右侧 / 底部）。
- rgpui git 源切换后跑完整校验与最小构建。

**别误改（已确认无问题）**：光标闪烁已睡到边界、滚动条淡出是一次性定时器、拖拽自动滚动仅拖拽期 16ms 运行、右键 / 图片菜单轻量、PDF `wrap_text` 为 O(n)、DOCX 不加载字体文件；渲染「先分组后建元素」两趟 + 只为视口内行建元素的优化已在位。

## 5. 验证流程（每次改完照跑）

```pwsh
cargo fmt --check
cargo clippy -p rumark --all-targets          # 期望 0 warning
cargo test -p rumark                          # 当前基线 1049 passed / 0 failed / 10 ignored
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
