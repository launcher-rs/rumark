# 一窗多文档 tab 设计方案

> **状态:未来计划,尚未实施。** 本文档描述一个设想中的多 tab 功能方案,不属于当前版本的功能范围。当前版本每次打开一个文件窗口。

本文是「一个窗口内用 tab 打开多个文件，tab 可拖出成独立窗口、也能拖回合并」需求的
设计与实施方案。现状调查来自本轮代码走查，位置统一标注为 `文件:行号`（行号按提交
`6d7c87c` 时的代码）。

状态：**方案待评审，尚未实施**。需要先拍板的问题见 §0.2。

## 0. 结论速览

### 0.1 一句话方案

新增窗口级视图 `DocumentHost`（持有 `Vec<Entity<Editor>>` + 活动索引 + 每 tab 脏标记）
作为窗口根；`Editor` 从「窗口」降级为「文档视图」——窗口 chrome（标题栏 + tab 条）由
`DocumentHost` 画，活动 tab 的 `Editor` 只画正文区（正文 + 查找条 + 状态栏）。

### 0.2 需要拍板的四个问题

| # | 问题 | 建议 |
| --- | --- | --- |
| 1 | 拖出/拖回是否放进第一期 | 第一期只做「一窗多文档 + 同窗拖拽重排」；拖出/拖回放第二期，且先做跨窗口拖拽 spike（§4） |
| 2 | 拖入文件 / 侧栏点文件的语义 | 改为「新建 tab」（现在会替换当前文档，多文档下很别扭） |
| 3 | 关窗/退出语义 | 逐 tab 串行确认未保存（一次一个对话框），全部通过才关窗 |
| 4 | 会话持久化（重开恢复 tab 集合） | 第三期，第一期不做 |

### 0.3 为什么不是「改一小块」

「窗口 == 文档」在现有代码里是硬假设，散落在三处，必须一起改：

1. 窗口根视图就是 `Editor`（`src/app_menu.rs:52-72`）。
2. 菜单动作靠 `active_window().downcast::<Editor>()` 找文档（`src/app_menu.rs:396-410`）。
3. 窗口关闭守卫挂在 `Editor` 上（`src/editor/close.rs:14`、`:63`）。

漏改任何一处，都会出现「保存存到了另一个文件」「关窗漏掉脏 tab」这类**静默**错误。

## 1. 目标与非目标

**第一期目标**：一个窗口内多个文档 tab（新建/切换/关闭/拖拽重排）；打开文件的四条入口
语义统一为「进 tab」；单 tab 时视觉与行为零回归。

**第二期目标**：tab 拖出成独立窗口；tab 拖回目标窗口合并；拖拽过程中的视觉反馈
（放置高亮、拖出预览）。

**非目标**（明确不做，避免范围蔓延）：分屏（`split_pane`）、多窗口同步滚动、macOS 原生
窗口 tabbing（`NSWindow` tabbing）、跨进程多实例、会话持久化（第三期）、tab 分组的树状
布局（类似 IDE 的 dock 区域）。

## 2. 现状（事实）

### 2.1 开窗与打开文件

- 建窗唯一入口：`open_editor_window`（`src/app_menu.rs:52-72`）——`cx.open_window` +
  `cx.new(|cx| Editor::from_markdown(...))`，窗口选项基座 `rumark_window_options`
  （`src/app_menu.rs:33-41`），随后 `force_install_close_guard`（`:67`）。
- 打开文件的四条入口，现在是三种不同行为：
  | 入口 | 现状 | 位置 |
  | --- | --- | --- |
  | CLI 多文件 | **每个文件一个窗口** | `src/main.rs:248-274` |
  | macOS `on_open_urls` | 每个 URL 一个新窗口 | `src/main.rs:214-224` → `open_file_in_new_window`（`src/app_menu.rs:75`） |
  | 菜单「打开/最近文件」 | 复用空白窗口，否则新窗口 | `open_file_preferring_blank_window`（`src/app_menu.rs:120-140`，调用点 `:460`、`:907`） |
  | 拖入 `.md` / 侧栏点文件 | **替换当前文档** | `src/editor/file_drop.rs:31-46`、`:49-79`、`src/editor/workspace.rs:378` |
- 空白窗口复用判据：`Editor::is_blank_document`（`src/editor/file_drop.rs:85-94`），调用点
  `try_load_into_active_blank_window`（`src/app_menu.rs:87-113`）。

### 2.2 动作路由

- 应用级菜单动作在 `app_menu::init` 里逐个 `cx.on_action`（`src/app_menu.rs:1051-1110`），
  统一转 `dispatch_menu_action`（`:560-647`）。
- 「当前文档」靠 `with_active_editor`（`src/app_menu.rs:396-410`）解析：`cx.active_window()`
  → `downcast::<Editor>()` → `cx.spawn` 里 `window.update(...)`（动作分发期间窗口被 take，
  同步 `update` 必失败，这是既有教训，改动时要保留这个形态）。
- 编辑器自身的动作挂在窗口根元素上（`src/editor/render.rs:1185-1235`），包含
  `can_drop::<ExternalPaths>`/`on_drop`（`:1185-1186`）、`SaveDocument`/`SaveDocumentAs`
  （`:1189-1190`）、`CloseWindow`（`:1192`）、`ToggleViewMode`/`ToggleWorkspace`
  （`:1193-1194`）、`Find`（`:1235`）等共 50 余项。

### 2.3 窗口级状态

- 窗口根视图 `Editor::render` 自绘全部 chrome：标题栏（`src/editor/render.rs:1245-1304`）、
  正文区含工作区抽屉与拖拽分隔条（`:1305-1354`）、查找条（`:1355-1359`）、状态栏
  （`:1360-1364`）、对话框/浮层（`:1365+`）。
- 标题：`Editor::window_title(file_path, document_dirty, i18n)`（`src/editor/window_state.rs:191-214`），
  经 `sync_window_title`（`src/editor/render.rs:410-416`）写入 `window.set_window_title`。
- 窗口 edited 状态：`window.set_window_edited(false)`（`src/editor/file_drop.rs:76`、
  `src/app_menu.rs:105`），保存链路在 `src/editor/persistence.rs`。
- 关闭守卫：`force_install_close_guard`（`src/editor/render.rs:241`）+
  `request_close_current_window`（`src/editor/close.rs:14`）+
  `on_window_should_close`（`:63`）——脏就弹框架三按钮对话框（`:88-126`），用
  `show_unsaved_changes_dialog` / `pending_close_after_save` 记录挂起态。
- 关闭/退出编排：`current_window_candidates`（`src/app_menu.rs:464-488`）逐窗口调
  `on_window_should_close`（`:529-557`）；`handle_window_closed`（`:1039-1043`）在
  **窗口数为 0 时直接 `cx.quit()`**。

### 2.4 框架能力（rgpui 1.4.1）

- **有 tab 组件**：`Tabs`/`TabsItem`（`src/tabs/tabs.rs:62`/`:25`，纯值驱动；项目已在用，
  见工作区抽屉 `src/editor/workspace.rs:503`）、`TabBar`（`src/tabs/tab_bar.rs:40`）、
  `Tab`（`src/tabs/tab.rs:411`）、同窗拖拽重排 `TabDragDrop`/`TabDragData`
  （`src/tabs/tab_drag.rs:154`/`:47`，**只支持同窗口内排序**）。
- **有布局件**：`components/split_pane.rs`、`components/drag_drop.rs`（`Draggable`/`DropZone`）、
  `components/dock.rs`（IDE 式停靠面板，不是文件 tab）。
- **多窗口**：`App::open_window`（`src/app.rs:1239`）、`windows()`（`:1216`）、
  `window_stack()`（`:1228`）、`active_window()`（`:1233`）。
- **跨窗口拖拽没有原生支持**：拖拽态是 App 级共享（`src/app.rs:685` 的 `active_drag`），
  但鼠标事件与放置监听按窗口各自派发（`src/window.rs:5400` 的 `dispatch_mouse_event`）。
  线索：OS 文件拖入的位置换算成 MouseMove 的路径是存在的（`src/window.rs:5334-5373`），
  说明「屏幕坐标 → 窗口内坐标」这条路框架里走过。

## 3. 第一期设计

### 3.1 数据模型

```rust
// src/editor/document_host.rs（新增）
pub(crate) struct DocumentHost {
    tabs: Vec<TabEntry>,        // TabEntry { editor: Entity<Editor>, file_path: Option<PathBuf> }
    active: usize,
    // 关闭流程的挂起状态（见 §3.5）
    pending_close: Option<CloseFlow>,
    // 拖拽重排（框架 TabDragDrop 的回调落地）
    drag: Option<TabDragState>,
}
```

要点：

- tab 的身份用 `Entity<Editor>`，**不用路径**——同一文件允许开两个 tab（未保存时路径可能为空）。
- 脏标记不单独存：按需 `editor.read(cx).document_dirty` 汇总（`Editor` 已有该字段）；
  `DocumentHost` 只维护「活动索引」与「关闭流程状态」这类窗口级状态。
- `DocumentHost` 是窗口根视图，负责画 chrome + 只渲染活动 tab 的正文（非活动 tab 的
  `Entity<Editor>` 仍然存活，状态不丢）。

### 3.2 渲染拆分（本期最大的机械改动）

把 `Editor::render` 拆成两个入口，`Editor` 单 tab 独占窗口时行为不变：

| 片段 | 现在位置 | 拆分后归属 |
| --- | --- | --- |
| 标题栏（品牌 + 菜单按钮） | `src/editor/render.rs:1245-1304` | `DocumentHost` 画（新增 tab 条紧贴其下） |
| 正文区（工作区抽屉 + 分隔条 + 内容） | `:1305-1354` | 保留在 `Editor`（`render_body`） |
| 查找条 | `:1355-1359` | 保留在 `Editor`（属于单文档状态） |
| 状态栏 | `:1360-1364` | 保留在 `Editor`（字数/光标跟随活动 tab） |
| 对话框与浮层 | `:1365+` | 保留在 `Editor`（只渲染活动 tab，天然不冲突） |

两个候选做法：

- **A（推荐）**：`Editor` 提供 `render_window_chrome(...)` 与 `render_body(...)` 两个方法，
  `Editor::render`（standalone，测试与「单文档」路径）内部依次调这两个；`DocumentHost::render`
  调 `render_window_chrome` + tab 条 + 活动 tab 的 `render_body`（`Entity<Editor>` 作为
  `IntoElement` 无法只渲染子片段，故 `render_body` 需要能被 host 直接调用，例如
  `editor.read(cx).render_body(window, cx)` 外加 `AnyElement` 包装）。
- **B（小改但要还债）**：`Editor` 加一个 `chrome_visible: bool`，host 每帧把它设为 `false`，
  `Editor::render` 据此跳过标题栏/状态栏。改动小，但依赖「每帧先设置再渲染」的隐式顺序，
  容易被后续改动踩坏。

同时要确认并保持的既有行为：

- 非活动 tab 不渲染 → 滚动位置与光标状态必须存在 `Editor` 状态里（`last_scroll_viewport_size`
  等字段已有；`request_active_block_scroll_into_view` 相关状态在 `src/editor/render.rs:400` 附近），
  **实施第一步就先验证这条**，否则切 tab 会丢滚动位置。
- 切 tab 后把焦点交回新活动 tab（`Editor::pending_focus` + 该 tab 上次的块焦点），
  否则键盘输入会落到空处。
- 切 tab 时关掉窗口级框架对话框（`window.close_dialog`），避免「对话框属于 A、活动 tab 是 B」。

### 3.3 动作路由改造（逐点清单）

| 位置 | 现状 | 改法 |
| --- | --- | --- |
| `with_active_editor` `src/app_menu.rs:396-410` | `active_window().downcast::<Editor>()` | 先试 `downcast::<Editor>()`（standalone/测试路径），失败再 `downcast::<DocumentHost>()` → 取活动 tab 的 `Entity<Editor>`，仍走 `spawn` + `window.update` |
| `dispatch_menu_action` `:560-647` | 依赖上一个函数 | 不改分发结构；`NewWindow` 改为「新窗口 + 空 tab」 |
| `try_load_into_active_blank_window` `:87-113` | 复用空白窗口 = 替换文档 | 改为「活动窗口新增 tab」，判据沿用 `Editor::is_blank_document`（空白 tab 就地载入，避免留一个空 tab） |
| `open_file_preferring_blank_window` `:120-140` | 复用或新开窗口 | 语义不变（无窗口才新开窗口），内部改为进 tab |
| `open_file_in_new_window` `:75-81` | 总是新开窗口 | 改名/拆分为「在新窗口打开」与「在活动窗口新 tab 打开」两个入口；macOS `on_open_urls`（`src/main.rs:218`）与 CLI（`src/main.rs:248-274`）改走后者 |
| 编辑器根元素动作链 `src/editor/render.rs:1185-1235` | 挂在 `Editor`（=窗口）根上 | 键盘动作仍可挂 `Editor`（活动 tab 在焦点链上）；`CloseWindow`/`ToggleWorkspace`/`Find` 等窗口级语义改由 host 承接，`Editor` 侧只保留文档内动作 |
| 拖入 / 侧栏 `src/editor/file_drop.rs:31`、`src/editor/workspace.rs:378` | 替换当前文档 | 改为「向 host 请求新 tab」；`request_dropped_markdown_replace` 只在「同路径已在该 tab」等少数情形保留 |

改造后的统一链路（建议）：

```
打开文件(path)
  → 找活动窗口（无则 open_editor_window 建窗）
  → host.open_in_tab(path)
      → 若活动 tab 是空白文档（is_blank_document）→ 就地载入
      → 否则 push 新 Editor::from_markdown(...)，active = 新索引
  → record_recent_file + install_menus + refresh_windows（沿用 record_recent_file_and_refresh，src/app_menu.rs:143）
```

### 3.4 tab 条渲染

- 用框架 `TabBar`/`Tab`（`src/tabs/tab_bar.rs:40`、`src/tabs/tab.rs:411`）或项目已在用的
  `Tabs`（`src/editor/workspace.rs:503` 是值驱动用法）二选一，成为**原型阶段第一件事**：
  需要现场确认 `Tab` 是否自带关闭按钮/脏点、`TabBar` 超宽时是否可滚动。
- 交互需求：点击切换、中键关闭、右键菜单（关闭/关闭其他/关闭右侧/在新窗口打开）、
  `ctrl-tab`/`ctrl-shift-tab` 循环、`ctrl-数字` 直达第 N 个。
- 拖拽重排直接复用 `TabDragDrop`（`src/tabs/tab_drag.rs:154`）的 `on_reorder`。
- 视觉：tab 条高度、选中态底色用 `theme` 令牌（与标题栏/状态栏风格一致），文案走 i18n。

### 3.5 关闭与退出（状态机）

现状是「`Editor` 自己问自己」（`src/editor/close.rs:63`），多 tab 后改为 host 编排：

```rust
enum CloseFlow {
    ClosingTabs { queue: Vec<usize>, current: usize, after: After },
    // after: 全部通过后要做什么（关窗 / 退出应用 / 只关这一个 tab）
}
```

规则：

1. 关单个 tab：该 tab 脏 → 走一次三按钮对话框（保存并关闭/放弃并关闭/取消）；
   干净 → 直接移除。
2. 关窗口 / 退出应用：按 `tabs` 顺序**逐个串行**确认（一次只弹一个对话框），
   任一取消 → 中止整个流程（与现状「任一窗口取消即中止」语义一致，`src/app_menu.rs:529-557`）。
3. 关掉最后一个 tab == 关窗（保留现在「最后窗口关闭即退出」的手感；
   第二期拖拽期间用标志位抑制，见 §4.4）。
4. 挂起状态从 `Editor` 迁到 host：`show_unsaved_changes_dialog`/`pending_close_after_save`
   （`src/editor/close.rs`）语义变为「当前 tab 的挂起态」，避免多 tab 互相覆盖。
5. `force_install_close_guard`（`src/editor/render.rs:241`）的守卫安装点从 `Editor` 移到
   `DocumentHost`。

### 3.6 标题、edited 与最近文件

- 窗口标题 = 活动 tab 的 `window_title(...)`；只要任一 tab 脏，标题带脏标记（复用
  `src/editor/window_state.rs:191-214` 的格式，脏标记取「任一 tab 脏」）。
- `window.set_window_edited(any_dirty)`：host 汇总所有 tab 后统一设置；
  `Editor` 内部的单点调用（`src/editor/file_drop.rs:76`、`src/app_menu.rs:105`）改为通知 host。
- 最近文件仍按「打开过」记录，不变（`src/app_menu.rs:143`）。

### 3.7 工作区抽屉归属

抽屉现在是 `Editor` 状态（`src/editor/workspace.rs`，含文件树/大纲与宽度等），多 tab 下
应该是**窗口级**（一个窗口一份抽屉）。建议：

- 抽屉实体升到 `DocumentHost`，内容跟随**活动 tab 的文档目录**（切换 tab 时刷新，
  沿用 `sync_workspace_after_document_path_change`，`src/editor/workspace.rs:110`）；
- 抽屉里点文件 → 新 tab（与 §3.3 统一链路一致）；
- 抽屉宽度/开关状态属于窗口，切 tab 不重置。

### 3.8 键位与文案

- `ToggleViewMode` 现在默认占 `ctrl-tab`/`cmd-tab`（`src/components/actions.rs:585`），
  必须让位给 tab 切换；`ctrl-w` 现在绑工作区抽屉（`actions.rs:592`），若要让给「关 tab」
  需另选键（建议保留抽屉、关 tab 用 `ctrl-f4`/`cmd-w` 视平台定）。
- 新增默认键：下一个/上一个 tab、关 tab、关闭其他 tab、在新窗口打开。
- 新增文案键：`tab_close`、`tab_close_others`、`tab_move_to_new_window`、
  `window_merge_into_active` 等，写进 `src/i18n/zh-CN.json` 与 `en-US.json`。
- 兼容红线不变：动作命名空间仍是 `rumark::...`（用户键位配置依赖），只改默认键值。

## 4. 第二期：跨窗口拖拽（先 spike，后实施）

拆成两件事，风险差一个量级：

- **2a 拖出成窗口**：判定条件只是「松开时指针不在任何窗口内（或按住修饰键强制）」，
  不需要命中检测，可行性高。
- **2b 拖回合并**：需要把 drop 派发到**另一个窗口**，框架没有原生路由（§2.4），
  是本期唯一的硬骨头。

### 4.1 spike 清单（半天到一天，结论写回本文）

1. 拖拽期间能否拿到**屏幕坐标**的指针位置：源窗口在指针移出后是否继续收到 mouse move
   （win/mac 差异），框架是否暴露屏幕坐标（`display`/platform 层），或需要平台补齐。
2. `App::windows()`（`src/app.rs:1216`）/`window_stack()`（`:1228`）能否逐个窗口取
   屏幕 `bounds`，用于「指针落在哪个窗口」的命中判定。
3. App 级 `active_drag`（`src/app.rs:685`）能否跨窗口复用；目标窗口不在事件链上时，
   drop 如何完成（自建「目标窗口接管」路径，或请框架补 `App` 级 drop 路由）。
4. 非焦点窗口被拖到时是否会被点击激活、从而吃掉 drop（Windows 尤为明显）；
   若会，退化为「拖到目标窗口的 tab 条区域高亮，松开由目标窗口接管」。
5. 平台差异：macOS 系统手势（`cmd-tab`）、`--detach` 重启路径与拖拽的交互。

### 4.2 接管流程（草案）

- **拖出（2a）**：松开时指针在所有窗口外 → **先建后删**：新建窗口（把该 tab 的文档
  内容与路径在新 `Editor` 里重建），插入完成后再从源 host 移除该 tab；源 host 若变空则
  关掉源窗口。先建后删可避免「0 窗口瞬时态」。
- **拖回（2b）**：指针落在目标窗口的 tab 条 → 目标 host 插入 tab，源 host 移除；
  源 host 变空即关源窗口（同样是先插入后移除）。
- 视觉反馈：拖动中在目标窗口 tab 条上显示插入位高亮（`DropZone`，`src/components/drag_drop.rs`），
  指针在所有窗口外时显示「松手将新建窗口」的提示（跟随指针的 overlay 或状态栏文案）。

### 4.3 防误退出的两处配套

- `handle_window_closed`（`src/app_menu.rs:1039-1043`）现在「窗口数为 0 → `cx.quit()`」，
  拖出/拖回会频繁制造瞬时 0 窗口：改为延后一个循环再判（或加 `tab_move_in_flight: bool`
  标志位抑制）。
- 同窗拖拽重排（框架 `TabDragDrop`）与「拖出」必须能区分：建议拖出用「拖离 tab 条一定
  距离/松手在窗口外」判定，重排只在 tab 条内生效。

### 4.4 失败回退

若 spike 证明框架层缺屏幕坐标或跨窗口 drop 不可补（不上游框架改动的红线）：
第二期只做 2a（拖出），2b 退化为显式命令——「把当前 tab 合并到活动窗口」，
用户先激活目标窗口再执行（菜单 + 键位，走 i18n 文案 `window_merge_into_active`）。

## 5. 兼容与迁移

- **测试兼容**：现有约 800 个测试直接把 `Editor` 当窗口根视图构造
  （`TestAppContext::single()` + `cx.new(|cx| Editor::from_markdown(...))`）。因此第一期
  **保留 `Editor` 可独立当根**（standalone 模式仍画完整 chrome），`DocumentHost` 是加法；
  §3.3 的 `with_active_editor` 双路径正好兼容两套测试。第二期再评估是否统一为单一根。
- **平台**：`on_open_urls`（`src/main.rs:214-224`）与 CLI 多文件（`:248-274`）改为
  「首个文件开窗、其余进 tab」；`--detach`（仅 macOS）与 `file_url` 不受影响。
- **启动路径**：`open_startup_window`（`src/main.rs:49`）与 `startup_open` 偏好不变。
- **开发者惯例**：所有落盘仍走 `atomic_file`（新增的 tab 会话持久化也要遵守，
  见 `AGENTS.md` 的原子写红线）。

## 6. 测试与验收

单测（新建 `src/editor/document_host.rs` 内的 `#[cfg(test)]`，沿用
`TestAppContext::single()` + `#[rgpui::test]` 风格）：

1. 新增/切换/关闭 tab 后活动索引正确；关闭活动 tab 落到相邻 tab；关掉最后一个 tab == 关窗。
2. 脏标记汇总：任一 tab 脏 → 窗口标题带脏标记、`window.set_window_edited(true)`。
3. **动作路由**：两个 tab 各指向 `temp_dir()/rumark-*` 下的真实文件，活动 tab 为第二个
   时派发 `SaveDocument`，断言只有第二个文件被写、第一个未被写（这是本次改造最该锁住的回归）。
4. 入口语义：已有窗口时 `open_file_preferring_blank_window` 得到新 tab 而不是新窗口；
   空白 tab 复用规则仍在（`Editor::is_blank_document`）。
5. 关闭流程状态机：两个脏 tab + 取消 → 流程中止且两个 tab 都在；保存并关闭 → 依次通过。
6. 回归：现有全量测试保持绿（`cargo test -p rumark`，当前基线 799 passed）。

手动验收：

- 开 3 个文件（含 1 个脏）→ 切换/关闭/保存/查找/视图切换都只作用于当前 tab；关窗时脏 tab
  依次提示；`ctrl-tab` 循环；拖动 tab 重排后顺序与活动态正确。
- 拖入 `.md` 得到新 tab；侧栏点文件得到新 tab；空白窗口复用规则仍在。
- 单 tab 时视觉/交互与改造前完全一致。
- 第二期：拖出成窗口、拖回合并、目标窗口高亮；期间不出现意外退出。

## 7. 工作量粗估与文件清单（第一期）

| 区域 | 文件 | 规模 |
| --- | --- | --- |
| 新增 host（tab 模型 + chrome + 关闭状态机 + 测试） | `src/editor/document_host.rs` | 约 800-1000 行（含测试约 300 行） |
| 渲染拆分（chrome/body 两入口） | `src/editor/render.rs` | 约 150 行改动 |
| 动作路由与开窗/关窗入口 | `src/app_menu.rs` | 约 200 行改动 |
| 入口语义（拖入/侧栏/CLI/macOS） | `src/editor/file_drop.rs`、`src/editor/workspace.rs`、`src/main.rs` | 约 60 行 |
| 键位与文案 | `src/components/actions.rs`、`src/i18n/{zh-CN,en-US}.json` | 约 30 行 |
| 文档 | `TODO.md`、`docs/rgpui-gaps.md`（框架缺口回填） | 少量 |

建议拆 4-6 个提交落地：① 渲染拆分与 host 骨架（单 tab 等价）→ ② 开 tab 与入口语义统一
→ ③ 关闭/退出编排 → ④ 键位与文案 → ⑤ 测试补全 → ⑥ 文档回填。
第二期（跨窗口拖拽）的工作量取决于 §4.1 的 spike 结论，暂不估。

## 8. 风险与未决问题

1. **跨窗口拖拽（最高风险）**：见 §4，可能需要框架补齐；回退方案已备（§4.4）。
2. **只渲染活动 tab 的前提**：滚动/光标/查找状态必须都在 `Editor` 状态里；
   实施第一步先验证，若滚动位置存在元素树里，需要先搬进 `Editor`。
3. **窗口级对话框与 tab 的错配**：切 tab 要关掉框架对话框；后台 tab 不应持有窗口级对话框。
4. **关闭挂起态的迁移**：`show_unsaved_changes_dialog`/`pending_close_after_save`
   （`src/editor/close.rs`）要从「窗口唯一」改为「按 tab」，是容易漏的边界。
5. **工作区抽屉归属**：升到窗口级后，「跟随窗口目录」还是「跟随活动 tab 目录」需要定；
   本文建议后者。
6. **未决**：tab 上限与超宽滚动行为；tab 是否显示脏点/关闭按钮（取决于框架 `Tab` 能力）；
   同一文件开两个 tab 时保存冲突如何处理（建议：保存后就地刷新另一个同路径 tab，
   或直接禁止重复打开同一路径——倾向「允许重复，保存后不自动同步」，简单可预期）。

## 9. 落地补充（2026-11-21）

第一期已按「薄壳 host」落地；与本文早期草案的差异和后续修正记录在此。

- 窗口根为 `DocumentHost`，活动 `Editor` 作为唯一子元素挂载，`Editor::render` 保持不变；
  tab 条经 `top_strip` 插槽插入标题栏下方，不在 host 中复制标题栏/状态栏逻辑。
- tab 条用 `TabBar::on_change` 统一派发点击与左右方向键（`Tab::on_click` 单点派发会导致
  选中态不同步）；外层补 `pl(TITLE_BAR_LEFT_PADDING)` / `pr(12px)`，抵消 `TabBar` 默认
  `px(-1.)` 的负 padding，避免首个 tab 贴住窗口边缘。
- 关闭单 tab / 关窗 / 退出共用一套 `CloseScope` 状态机；`Editor::update` 内访问 host
  必须通过 `cx.spawn` 延后，否则拖入文件等路径会触发实体重入 panic。
- 偏好 `editor.multi_tab_enabled`（默认 `true`）可在「文件与启动」页关闭；关闭后新窗口
  退化为单文档行为（不注入 tab 条、tab 动作无操作、关闭走原关窗语义），窗口返回类型
  仍是 `DocumentHost`，避免为「完全恢复旧窗口类型」引入额外回归面。该设置对新窗口生效。
- 偏好设置窗口尺寸由 800×560 调整为 900×640。
- 窗口标题与系统 edited 标记由 host 汇总所有 tab 的脏状态统一写入（标题文件名仍取活动 tab）。子 Editor 脏状态变化只按实体 ID 通知 host，不 `update` host，避免编辑器 update 期间重入。
