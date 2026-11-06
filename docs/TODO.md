# TODO — 拾墨（rumark）进度交接单

> 用途：把当前进度、待决事项与验收方式落成文件，**下一次照此直接执行**，不必翻聊天记录。
> 约定：完成一项就删掉该条目；每条按「背景 / 做法 / 验收」写清。

## 1. 工作区状态与改动分组

本轮改动**已全部提交，工作区干净**（见 `git log --oneline -6`）：脚注族跨块映射、Typora 对比体验、图片请求与偏好/i18n/PDF 修复、落盘原子写，另有 `docs:`（本文件 + AGENTS.md）与 `test:`（`docs/render-sample.md` 夹具）两个小提交。

`docs/render-sample.md` 被 `include_str!` 进多处测试（`components/markdown/inline.rs`、`editor/document.rs`、`editor/tests.rs`、`export/{pdf,docx}.rs`），**改它就是改测试夹具**，改完必须跑全量 `cargo test -p rumark`。

若后续又有未提交改动，按下面四个主题归组提交（多主题文件按「主主题」归组；要严格按主题落提交就用 `git add -p` 分块暂存）。提交信息风格 `type: 中文描述`（`fix:` / `test:` / `chore:` / `docs:`）：

| 主题 | 文件 |
| --- | --- |
| A 跨块复制/删除与脚注源码映射 | `src/editor/selection.rs`、`src/editor/source_mapping.rs`、`src/editor/tree.rs`、`src/editor/document.rs`（仅新增测试） |
| B Typora 对比体验 | `src/components/block/render.rs`、`src/components/block/runtime/{mod,image,tests}.rs`、`src/components/mod.rs`、`src/components/actions.rs`、`src/editor/{events,tests}.rs` |
| C 其他修复 | `src/net/mod.rs`、`src/config/preferences.rs`、`src/i18n/mod.rs`、`src/export/pdf.rs`、`src/app_menu.rs` |
| D 落盘统一原子写 | `src/atomic_file.rs`（新）、`src/config/{mod,preferences}.rs`、`src/i18n/mod.rs`、`src/theme/mod.rs`、`src/editor/{persistence,file_drop,events}.rs`、`src/components/{latex,mermaid}/mod.rs`、`src/main.rs`、`AGENTS.md` |
| 单独 | `docs/render-sample.md`（回归夹具，已单独提交） |

各文件改动要点（分组内按文件）：

- **A**：`selection.rs` 的 `cross_block_selected_markdown` 重做拼接记账（`cross_block_separator_newlines` 改语义为「换行数」、新增 `container_blank_line_prefix` 补容器内空行前缀、空片段块与 `pending_empty` 处理；回归测试 `cross_block_copy_and_delete_match_source_for_footnote_definitions`，21 个 fixture 断言「复制 == 序列化文本」且「整篇删除后文档为空」）；`source_mapping.rs` 脚注定义头行/首段/子块映射与容器空行记账（`push_footnote_definition_head_mapping` 带缩进、首段不再重复覆盖 `[^id]: `、子块层级 `list_depth + 2`）；`tree.rs` 的 Quote/Callout 子级 `list_depth` 从 0 起算。
- **B**：`render.rs` H1/H2 细灰下划线、H6 发灰（`theme.muted_foreground`）、图片/Mermaid/MathBlock「渲染保留 + 聚焦叠加源码行」、列表组间空段落折叠、表格与分隔线整块选中；`runtime/mod.rs` 的 `collapses_as_list_group_separator(focused)`、`is_source_raw_mode` 同时排除 `MathBlock | MermaidBlock`；`runtime/image.rs` 的 `showing_image_source_with_render()`；`components/mod.rs` 的 `is_refcell_reborrow_panic`（只吞 RefCell 重借冲突）；`actions.rs` 给 `ToggleViewMode` 补默认键 `ctrl-/`；`events.rs` 表格内部交互前清跨块选区。
- **C**：`net/mod.rs` 图片请求连接/请求超时（10s/30s）+ 固定 4 线程池 `ImageWorkerPool`（原来每个请求新建裸线程）；`preferences.rs` 偏好窗口保存时保留磁盘上的自定义状态栏按钮；`i18n/mod.rs` `I18nSnapshot` 改为共享 map 而非深拷贝；`export/pdf.rs` 毫米↔点换算改为 `MM_PER_PT = 0.3528` / `PT_PER_MM = 2.8346`（原先误用屏幕像素的 0.2646，版心宽度偏差约 1.33 倍）；`app_menu.rs` 打开文件优先复用空白窗口（`try_load_into_active_blank_window` / `open_file_preferring_blank_window`）、`show_window_prompt_with_level` 支持指定对话框级别。
- **D**：新增 `src/atomic_file.rs`（`write_text`/`write_bytes`/`copy_file` + 私有 `write_with`：同目录临时文件 → `sync_all` → `rename`，失败清理临时文件），全部落盘点改走它（配置、主题/语言包、文档保存与另存、HTML/PDF-DOCX 导出、粘贴图片、LaTeX/Mermaid SVG 缓存、旧配置目录迁移）；`theme/mod.rs` 另补了 `h1_border_width` 的注释；`AGENTS.md` 模块清单加 `atomic_file.rs` 并新增「不要再用裸 `std::fs::write`/`fs::copy`」红线。

## 2. 待决事项（需要拍板）

### 2.1 `段落` + `---` 是否禁用 Setext

- 背景：`段落\n---` 现在会被解析成 H2（Setext），与 Typora 一致；但「想把 `---` 当分隔线」的用户会意外得到标题。
- 影响测试（改就要一起改）：
  - `editor::events::tests::setext_dash_underline_enter_promotes_previous_paragraph_to_h2`
  - `editor::events::tests::dash_underline_without_heading_target_stays_a_separator`
  - `editor::document::tests::imports_setext_headings_and_grouped_paragraphs`
- 做法（若禁用）：在 `src/editor/document.rs` 的导入解析里限制 Setext 认可条件（例如仅当上一段自身以空行结束/处于块起始位置才算标题），同步更新上面 3 个测试与 `docs/render-sample.md` 对应用例。
- 状态：**待定**。

### 2.2 AGENTS.md 与 Cargo.toml 的导出能力矛盾

- 背景：`AGENTS.md` 写「已知被裁功能：版本更新检查、HTML/PDF 导出已删除」，但 `Cargo.toml` 的默认 feature 仍含 `export-pdf`/`export-docx`（依赖 `printpdf`/`docx-rs`），`src/export/pdf.rs` 与 `src/editor/persistence.rs` 的导出分支都在编译、测试里也有覆盖。
- 二选一：
  - (a) 只改文档：把 AGENTS.md 那句改成「导出保留（feature 可裁剪）」；
  - (b) 真下线：删菜单项/动作/feature/依赖，**注意别删共享的块模型代码**（`src/main.rs` 里 `cfg_attr(not(any(feature = "export-pdf", feature = "export-docx")), allow(dead_code))` 就是为它留的）。
- 状态：**待定**。

### 2.3 原子写的权限代价（可选）

- 背景：重命名替换会带来新文件对象，原文件的权限位/硬链接不保留、新文件按目录默认权限创建（`src/atomic_file.rs` 的 `write_text` 文档已写明）。Unix 上可能出现原先 `0600` 的文档被放宽成 `0644`。
- 做法（若补）：写完临时文件后 `set_permissions(原目标权限)` 再 rename（VS Code 同款）；需要 `#[cfg(unix)]` 的权限断言测试（开发机是 Windows，跑不到）。
- 状态：**未做**，风险低，可按需补。

## 3. 待办任务（可直接执行）

### 3.1 给「粘贴图片落盘」补单测

- 背景：`src/editor/events.rs` 的 `materialize_pasted_image` 没有测试。它读取 `current_image_paste_behavior()`（内部 `read_app_preferences()` 直读磁盘上的真实用户偏好），测试无法稳定注入行为；而且默认偏好 `ImagePasteBehavior::None` + `LocalPath` 会**提前 return、根本不落盘**。
- 做法：
  1. 把行为参数化：新增 `materialize_pasted_image_with(&self, source, behavior)`（原有 `materialize_pasted_image` 取完行为后转调它，行为不变）。
  2. 在 `events.rs` 的 `#[cfg(test)] mod tests` 里用 `std::env::temp_dir().join("rumark-*")` 建「文档目录 + 文档文件」与**另一个目录**下的源图（源不在目标目录内，才会真的走复制）。
  3. 断言：`ImagePasteBehavior::CopyToDocumentFolder` 下目标文件字节 == 源文件字节；目标目录无 `.tmp-<pid>` 残留；源文件缺失时返回 Err 且不留半截目标文件。
- 验收：`cargo test -p rumark materialize_pasted_image`。

### 3.2 手动渲染回归

用根目录 `docs/render-sample.md` 打开逐项目视检查（覆盖 CommonMark + GFM 扩展、嵌套、边界）。本轮尤其看：引用内空行（`> `）、脚注内空行缩进、列表组之间的空段落折叠、H1/H2 细灰下划线、H6 发灰、表格整块选中（点表格框/非单元格处）、图片与 Mermaid/公式的「渲染 + 源码同屏」。

### 3.3 源码映射与表格列宽的缓存（暂缓，缺前置条件）

- 现状：`build_source_target_mappings`（`src/editor/source_map.rs`）每次调用都遍历全部根块重算映射，`selection.rs` 与 `history.rs` 的一次交互里会调用多次；`ColumnLayout::measure`（`src/components/markdown/table.rs`）每帧为表格每个单元格重建 `render_cache` 并 `shape_text`。
- 为什么暂缓：两处都没有可用的失效信号。文档树里**不存在版本号/修订号**（`grep revision|version` 在 `src/editor/*.rs` 无命中），靠「块数 + 末块标识」之类的指纹必然会出现陈旧缓存，代价是全选复制/删除/撤销的越界或错位，比省下的那点 CPU 贵得多。
- 前置做法：先在 `DocumentTree` 的**所有**变更入口（root 增删、块文本改写、表格结构操作、撤销重做、导入替换）统一递增一个 `revision: u64`，再让缓存以 `(revision, table_width)` 为键。测试要覆盖「改一个字即失效」。
- 已完成的同类优化：渲染改为「先分组、后建元素」两趟，元素只为视口内的行构建，且每帧不再克隆与块数等长的实体句柄（`src/editor/render.rs`）；光标闪烁在前半秒直接睡到边界，不再 33ms 空转（`src/components/block/editing/mod.rs`）。

## 4. 计划中的功能：一个窗口内多文档 tab

> 需求（用户提出）：目前**一个文件一个窗口**，希望一个窗口里用 tab 打开多个文件，并且 tab 可以**拖出来成为独立窗口**、也能**拖回窗口合并**。
> 详细方案见 `docs/multi-document-tabs.md`（现状调查、渲染拆分、路由改造清单、关闭状态机、跨窗口拖拽 spike、测试与验收、工作量粗估）；本节只留摘要与待拍板项。

### 4.1 现状（已调查，证据位置）

- 建窗链路：CLI 逐文件 `open_editor_window`（`src/app_menu.rs:52-72`，调用点 `src/main.rs:248-274`），窗口根视图**直接就是 `Editor`**（`cx.open_window` 内部自动包 `Root`）；菜单「打开/最近文件」走 `open_file_preferring_blank_window`（`src/app_menu.rs:120`）→ `open_file_in_new_window`（`:75`）。
- 动作路由建立在「窗口根 == 唯一 Editor」之上：菜单动作统一 `with_active_editor`（`src/app_menu.rs:396-410`）= `cx.active_window()` + `downcast::<Editor>()`；编辑器自身的动作在根元素注册（`src/editor/render.rs:1187-1235`）。**一窗多文档后这里必须改成「先解析窗口 → 再解析活动文档」**，否则保存/查找/视图切换会打到错误文档或静默失败。
- 拖入文件是**替换当前文档**而非新开窗口：`src/editor/render.rs:1185-1186` 的 `can_drop::<ExternalPaths>`/`on_drop` → `src/editor/file_drop.rs:31` → `replace_document_from_path`（`file_drop.rs:97`）；左侧工作区侧栏点文件同理（`src/editor/workspace.rs:378`）。
- rgpui 1.4.1（`Cargo.lock`）**有可复用的 UI 件**：`Tabs`/`TabsItem`（`rgpui-1.4.1/src/tabs/tabs.rs:62/25`，项目已在用，见 `src/editor/workspace.rs:503`）、`TabBar`/`Tab`（`tabs/tab_bar.rs:40`、`tabs/tab.rs:411`）、同窗拖拽重排 `TabDragDrop`/`TabDragData`（`tabs/tab_drag.rs:154/47`，**只支持同窗口内排序**）、`components/split_pane.rs`、`components/drag_drop.rs`（`Draggable`/`DropZone`）。
- **跨窗口拖拽没有原生支持**：拖拽态是 App 级共享（`rgpui app.rs:685` 的 `active_drag`），但鼠标事件与放置监听按窗口各自派发（`rgpui window.rs:5400` 的 `dispatch_mouse_event`），未发现跨窗口 drop 路由。**这是本需求风险最高、最可能需要平台层补齐的点**。
- `src/editor/workspace.rs` 是**左侧工作区侧边栏**（文件树 + 大纲），与多文档管理无关，别混。

### 4.2 需要先拍板的问题

1. tab 拖出/拖回**是否属于第一期**。若要多花钱：先做跨窗口拖拽 spike，决定走「平台层补齐」还是「先只给等效命令」（`Move Tab to New Window` / `Merge Window into Active Window`）。
2. 拖入文件、侧栏点文件的语义是否改为「新建 tab」（现在会替换当前文档，多文档下这个行为会很别扭）。
3. 关窗与退出语义：逐 tab 确认未保存，还是先问一次「全部保存」；`handle_window_closed`（`src/app_menu.rs:1039`）现在「最后一个窗口关闭即退应用」，拖出/拖回会频繁开关窗口，要防止误退出。
4. 会话持久化（重开恢复 tab 集合）暂列第二期。

### 4.3 建议分期

**第一期：一窗多文档 tab（不含跨窗口拖拽）**

1. 新增窗口级文档容器（如 `DocumentHost`）：持有 `Vec<Entity<Editor>>` + 活动索引 + 脏标记，作为窗口根视图；`open_editor_window`（`src/app_menu.rs:52`）改为挂这个容器。
2. 路由改造清单：`with_active_editor`（`src/app_menu.rs:396`）、`dispatch_menu_action`（`:560`）、`try_load_into_active_blank_window`（`:87`）、`open_file_preferring_blank_window`（`:120`）、`current_window_candidates`（`:464`）/`request_close_current_editor_window`（`:507`）/`request_quit_application`（`:529`）、`src/editor/render.rs:1187-1235` 的动作注册链、CLI 开窗循环（`src/main.rs:248-274`）。
3. tab 条用 `TabBar`/`Tab` 渲染（视觉可照 `src/editor/workspace.rs:503` 的 `Tabs` 用法），实现切换/关闭/脏点；窗口标题与 `edited` 状态改为「活动 tab 的路径」「任一 tab 脏」。
4. 快捷键：`ToggleViewMode` 默认占着 `ctrl-tab`/`cmd-tab`（`src/components/actions.rs:585`、`:1160-1161`），要重排，把 `ctrl-tab`/`cmd-tab` 让给 tab 切换；另加 `ctrl-w` 关 tab（现在 `ctrl-w` 绑给工作区抽屉，见 `src/components/actions.rs:592`，需另选键）。
5. 拖入文件（`src/editor/file_drop.rs:31`）与侧栏点文件（`workspace.rs:378`）改为新建 tab。
6. 验收：手动开 3 个文件验证切换/关闭/保存互不影响、关窗逐 tab 提示；新增文档容器层单测（新增/切换/关闭后脏状态与活动索引正确）。

**第二期：tab 拖出成窗口 / 拖回合并**

7. 跨窗口拖拽 spike：用 `App::windows()`/`window_stack()`（`rgpui app.rs:1216/1228`）+ 鼠标屏幕坐标判定指针落在哪个窗口，再看能否复用 App 级 `active_drag`（`app.rs:685`）完成跨窗口 drop；**结论写回本文件**（可行性、需要的框架改动、替代方案）。
8. 若原生不可行，先提供等效键盘/菜单命令，把「拖拽」留到平台层支持之后。

## 5. 验证流程（每次改完照跑）

```pwsh
cargo fmt --check
cargo clippy -p rumark --all-targets          # 期望 0 warning
cargo test -p rumark                          # 当前基线 799 passed
cargo check -p rumark --no-default-features   # 期望 5 个已知 warning
git diff --check -- 'src/*.rs' 'AGENTS.md'    # 期望无输出
```

- `--no-default-features` 的 5 个 warning 是既有基线：`src/components/block/code_highlight.rs` 里的 `std::collections::HashMap` 导入、`source` 变量、`CodeHighlightClass`、`HIGHLIGHT_NAMES`、`push_highlight_span` 未按 `code-highlight-core` 门控。
- 所有 `.rs` 必须是 **CRLF 无 BOM**（校验：LF 计数 == CR 计数，首字节不是 `EF BB BF`）。
- 测试总量会随用例增删变化，比较时以「失败数为 0」为准。

## 6. 已知陷阱（踩过的坑）

1. **edit 工具偶发把整文件写成 LF**：本会话 `src/editor/persistence.rs`、`src/components/mermaid/mod.rs` 都被改成过 LF。发现后按字节转回（不要用文本读写）：

   ```pwsh
   $p = 'src/xxx.rs'
   $b = [IO.File]::ReadAllBytes($p)
   $o = New-Object System.Collections.Generic.List[byte]
   foreach($x in $b){ if($x -eq 10){ $o.Add(13); $o.Add(10) } else { $o.Add($x) } }
   [IO.File]::WriteAllBytes($p, $o.ToArray())
   ```

   新文件（`write` 工具创建）默认也是 LF，同样要转。
2. **禁止**用 PowerShell `Get-Content`/`Set-Content` 改源码文件 —— 本会话曾因此毁掉 `src/editor/source_mapping.rs` 的换行，只能去 opencode 快照仓按字节抢救。取 git 内文件请用 `cmd /c "git show HEAD:path > file"`。
3. **`cargo fmt` 会把被改写的文件落成 LF**（`cargo fmt --check` 不会）——跑完 `cargo fmt` 要按第 6.1 条转回 CRLF。
4. 跨块复制/删除的源码映射：任何字符串切片/替换都要经 `normalize_utf8_range` + `get()` 兜底（多字节 UTF-8 文本），不要按 usize 裸切。
5. 表格是**唯一**没有自身 `SourceTargetMapping` 的块（只推单元格映射），整表复制走 `src/editor/selection.rs` 的回退分支，改动表格相关映射时两边都要看。
6. `rgpui`/`rgpui-platform` 是 crates.io 的 `1.7`，版本号在 `Cargo.toml` 有**三处**（两个依赖 + `dev-dependencies` 的 `test-support`），升级要一起改。
7. 行内强调：`has_closing_delimiter` 预扫描与 `parse_until` 的闭合判定**必须共用 `emphasis_close_at`**，且递归解析前要 `builder.checkpoint`、闭不上时 `builder.rollback`。两者任一失配都会让未闭合区间的正文被外层重放，产出重复文本并且**保存即把重复写进文件**（`__foo__bar` 曾写出三段）。口径对齐只挡住扁平场景，区间内还有更短的嵌套分隔符时预扫描必然偏乐观，回滚才是兜底。已知与规范的偏差逐条写在 `commonmark_emphasis_documented_deviations`（`*` 的简化侧翼判定、多字符起始符整体字面量、同标记相邻游程的贪心配对），新增偏差要写明原因。

## 7. 参考

- `AGENTS.md`：项目约定、构建/验证顺序、结构与平台注意事项。
- `docs/render-sample.md`：手动渲染回归样本。
- 本轮改动涉及的核心文件：`src/editor/selection.rs`、`src/editor/source_mapping.rs`、`src/editor/tree.rs`、`src/atomic_file.rs`、`src/components/block/render.rs`。
