# TODO — 拾墨（rumark）进度交接单

> 用途：把当前进度、待决事项与验收方式落成文件，**下一次照此直接执行**，不必翻聊天记录。
> 约定：完成一项就删掉该条目；每条按「背景 / 做法 / 验收」写清。

## 1. 工作区状态与改动分组

2026-11-21 **检查更新功能（未提交）**：新增 `src/update.rs`（纯逻辑：语义版本比较、平台资产名、镜像 URL 拼接、Release JSON 解析、reqwest blocking 下载与 zip 解压；内置镜像 ghfast.top/ghproxy.net + 直连兜底，实测 moeyy 已失效）+ `src/editor/update.rs`（UI 驱动：`cx.spawn` + `run_blocking` 独立线程跑网络、`window.prompt` 弹结果/下载/错误，忙态防重入）。Help 菜单加「检查更新」（`CheckForUpdates` 动作）、偏好设置加「更新」页（自定义镜像多行输入 + 检查按钮），偏好文件新增 `update.custom_mirrors`。**手动触发、不自动检查**。zip 从 dev-dep 提为正式依赖。验证：1046 passed（原 1041 + 5 个 update 单测）、clippy --all-targets 0 警告。设计见 `docs/auto-update.md`。遗留：Linux/macOS 的 tar.gz 解包未实现（提示手动下载）；跨进程自动替换 exe 脚本未做（当前落地 `.new` + 手动替换说明）。

2026-11-21 ~ 11-22 **rgpui 1.8 兼容性测试与正式版切换（未提交）**：发版前先把 `Cargo.toml` 的 `rgpui`、`rgpui-platform`（含 dev-dependencies）从 crates.io `1.7` 临时切到 git `https://github.com/launcher-rs/rgpui` 的 `feat/1.8.0` 分支做兼容性测试（锁定 `#924299da`）——`cargo test --bin rumark` **1041 passed / 0 failed / 9 ignored**，无需任何 rumark 侧 API 适配，确认 1.8 向后兼容。1.8 正式发布后（2026-11-22）已切回 crates.io：`rgpui` / `rgpui-platform` 均为 `version = "1.8.1"`（1.8.0 发布后用户指定再升到 1.8.1；主依赖保留 `inspector`，dev-dependencies 保留 `test-support` 且 1.8 起必须在 platform 上显式转发），`Cargo.lock` 锁定 registry 源 `rgpui v1.8.1`，**无任何 git 依赖残留**。1.8.0 切换验证：`cargo test --bin rumark` **1053 passed / 0 failed / 9 ignored**、clippy `--all-targets -D warnings` 0 警告、fmt `--check`、`--no-default-features` 全绿（crates.io 版与 git 测试分支 API 完全一致，无需适配）。1.8.1 用 `cargo update -p rgpui --precise 1.8.1` 升级：rgpui 全家桶（含 platform/macros/wgpu/web/linux/windows/dom）同步到 1.8.1，依赖树调整较大（字体栈换 zed-font-kit→rust-fontconfig/skrifa，新增 x11rb/zbus/rgpui-xim，移除 sha2/shlex/tiny-skia-path 等）。升级后全套验证同样全绿：`cargo check --bin rumark --tests`、`cargo test --bin rumark` **1053 passed / 0 failed / 9 ignored**、clippy `--all-targets -D warnings` 0 警告、fmt `--check`、`--no-default-features`，无需 rumark 侧 API 适配。注意：1.8 引入 `aws-lc-sys`（TLS 链）导致首次全量编译很慢；Windows 上 `cargo update` 写 lock 偶发 os error 5（被并发 `cargo check` 占用），重试即可。

2026-11-21 **整体代码审查修复批次（未提交）**：发布 0.2.4 前的全面审查（2 个并行子代理 + 人工复核）发现并修复 2 个 P0、3 个 P1、5 个 P2。① P0：`document.rs` 导入器 Setext 分支无段落守卫（`# 标题\n---` 打开→保存即损坏）——分支移到所有块类型判定之后（段落兜底前），顺带拦截列表项后顶格分隔线被懒续行吞掉（`collect_list_item_region`，CommonMark 分隔线可打断段落）；② P0：`events.rs` `RequestMergeIntoPrev` 无前块类型守卫（段首退格把文本 set_title 进分隔线/表格，保存即丢）——用 `is_atomic_block()` 拦截；③ P1：有序列表起始号保留——`BlockRecord.list_number`（解析时捕获）+ `sync_block_list` 分组起始号感知（组内按位置续排，CommonMark 只认首项）；④ P1：HTML 导出 svg/tiff/ico 丢图——`is_safe_data_url` 拆出 `is_supported_image_data_url` 仅对 `img[src]` 放行（`a[href]` 不放行 svg：顶层文档可执行内嵌脚本），`embedded_mime_for_extension` 与清洗白名单对齐；⑤ P1：代码围栏变体保留——`BlockRecord.code_fence` + `code_fence_markdown`（原文变体对当前内容可安全闭合时原样保留，冲突回退重算），tree.rs/source_map.rs 共用；⑥ P2：Mermaid 空正文两行围栏映射比序列化多一行（后续块偏移漂移，曾致越界 panic 的同类）——`push_mermaid_block_mapping` 对无正文行走 `build_empty_fence_mapping`；⑦ P2：数学/Mermaid 非聚焦块补 `last_bounds` 兜底（跨块选区按点命中）；⑧ P2：DOCX 每个有序列表独立编号定义（起始号落盘，嵌套列表栈式压弹）；⑨ P2 三处每键热点：引用块 Changed 走树修订号守卫（`quote_metadata_revision`，纯文本编辑跳过全树重建）、引用重建按块级 `may_affect_global_references` 标志门控（sync 时刷新，撤销恢复也能拿到准确初值）、单表编辑改 `rebuild_table_runtime_for_block` 作用域化（全量重建只留载入/撤销/视图切换）；⑩ P2：嵌套列表缩进从「每层 2 空格」改为「父项标记正文列宽」（`1. ` 下 3 列、`10. ` 下 4 列，`marker_content_columns`；`markdown_line` 参数语义 depth→columns；tree/source_map/selection 三处镜像同步改；引用/提示框内部仍归零由 `> ` 前缀承担，脚注子块 +4 列保持既有绝对语义）。验证：fmt/clippy 0 警告、1014 passed（原 1007 + 新增 7 回归）、`--no-default-features` 绿。

2026-11-21 **Mermaid 导出成图（未提交）**：用户反馈「导出为其他格式时 Mermaid 只有源码文字」。① `components/mermaid` 新增 `render_mermaid_svg_for_export`，复用基础 SVG 缓存并把根尺寸重写为明确像素（渲染器原输出常见 `width="100%"`，不能直接栅格化）；② 新增 `export/mermaid.rs`：SVG data URI（HTML 用）+ resvg/usvg 栅格化 PNG（PDF/DOCX 用，进程级系统字体库缓存，否则 usvg 默认空字体表会把中文标签画成空白），resvg/usvg 作为可选依赖挂两个导出 feature（两库已在 lock 中由 rgpui 引入，非新增重型依赖）；③ `export::Block` 新增 `Mermaid` 变体，PDF/DOCX 按版心嵌 PNG、失败回退等宽源码，HTML 事件流把 mermaid 围栏替换为 `<img src="data:image/svg+xml;base64,...">`（清洗白名单已放行 svg data URI）；④ 视觉核对：中文流程图 PNG 节点/连线/中文标签清晰，HTML img 标签保留且源码未泄漏。验证：fmt/clippy(-D warnings) 0 警告、1023 passed（原 1014 + 新增 9）、`--no-default-features` 绿、diff --check 干净、改动 `.rs` 均 CRLF 无 BOM。后续注意：HTML 导出仍在主线程同步渲染，大图首次导出会卡一下（与「保存/HTML 导出移后台」旧遗留同源）。

### 3.3a 审查遗留（2026-11-21，低优先）

2026-11-21 **第二批审查遗留修复（未提交）**：接 0.2.4 发布后的收口，修复上一批留存的 7 项并清理死代码。① Mermaid 块正文输入裸 ```` ``` ```` 行导致保存重载后图表截断——`rebuilt_mermaid_fallback` 按正文最长同类围栏 run 加长开闭围栏（沿用原围栏字符与 info 串），与代码块 `safe_code_fence` 同策略；② PDF `wrap_text` 行首长 URL 溢出——单元自身超出版心时强制按字硬断，不再因 `current.is_empty()` 整段塞入；③ DOCX 目录标题改 `Title` 样式，不再被 TOC 域（Heading1-3）自收录；④ HTML 导出 TOC 改为 `<ul>` 嵌在父 `<li>` 内的合法结构（旧实现连续 `<ul>` 裸露嵌套），跳级标题压平避免空嵌套；⑤ `find.rs` 替换回绕分支由传 `&[]`（不替换）改为替换首个匹配，并判空；⑥ 交互路径输入 `3. ` 捕获原始编号（`numbered_list_shortcut_number` + `set_list_number`），与导入/粘贴路径一致；⑦ `normalize_block_list` 双克隆 children 改 move。`document.rs` `collect_quote_block` 不可达空行分支删除（region 由 `collect_quote_raw_region` 保证无空行）。验证：fmt/clippy(-D warnings) 0 警告、1028 passed（原 1023 + 新增 5）、4 个新增回归测试逐条确认。

**评估后保留（低优先 / 需单独回归，暂不修）**：

- 脚注定义嵌在列表项内且含 2+ 子块时，树序列化子块缩进（绝对 4 列）与源码映射（相对 +4）在嵌套列表下本就不一致（映射偏长，既有偏差）。
- 渲染模式查找基于规范 Markdown，围栏 info 串等不可见语法区也会命中并可被替换（设计取舍，可文档化或过滤纯语法命中）。
- `model.rs` `markdown_line` 的 FootnoteDefinition 分支输出（`[^id]: `）与 `tree.rs` 实际序列化不一致——但脚注块有专门源码映射（`push_footnote_definition_head_mapping`），跨块复制走逐字映射，该分支仅无映射的罕见回退路径可达，收益极低。
- `may_change_global_references` 场景：文档含定义时普通段落打字已不重建，但含 `]:`/`[^` 字样的普通文本（如行内代码示例）每次编辑仍触发 `rebuild_image_runtimes`——底层 `derive.rs` 已对引用集合做「相等则复用 Arc」短路，再优化收益有限。
- `RequestFocus` 点击路径 notify 全部可见块（万级块文档每击 O(N)）；两处自动补尾段落无撤销捕获（`ensure_trailing_paragraph_after_structural` 与空白处点击，需确认是否已在撤销捕获窗口内）；拖拽选区每 mousemove 全量扫描可见块 bounds；undo/redo 不去重。以上均为交互性能/语义边界，需单独回归，不在本批改动面内。
- 列表项懒续行吞顶格 ATX 标题/引用/围栏行——已由提交 `13c2bcc` 修复（含回归测试 `top_level_blocks_break_list_item_lazy_continuation`），无需再处理。

2026-11-19 **Mermaid 渲染库拆分独立仓（未提交）**：`crates/mermaid-rs-renderer` 抽出为独立仓 [launcher-rs/mermaid-renderer](https://github.com/launcher-rs/mermaid-renderer)（GitHub 主仓 + Gitee 镜像），包名改 `mermaid-renderer`、库名改 `mermaid_renderer`，版本 `0.1.0`，暂不发布 crates.io、暂不打标签；拆分时清掉了 62 个误提交的 0 字节文件（文件名带 U+F00D 私用区字符），并重写了历史去掉它们。rumark 侧改 crates.io 版本依赖（`mermaid-renderer = "0.1"`），删除 vendor 目录与 workspace 成员，`src/components/mermaid/mod.rs` 引用同步改名，AGENTS/README/CONTRIBUTING/TODO 同步。已发布 0.1.0 到 crates.io。验证：新仓 `fmt/clippy/test` 全绿（含 10 个 suite）；rumark 侧待跑。

2026-11-19 **Mermaid 渲染 vendor 化与状态图修复（未提交）**：用户拍板把项目改成 cargo workspace 并将 `mermaid-rs-renderer` vendor 进仓自维护（MIT，基于上游 0.3.1）。落地内容：① 根 `Cargo.toml` 加 `[workspace]`（members = `[".", "crates/mermaid-rs-renderer"]`，resolver 3；`[profile.*]` 留在根），依赖改 path+version，`release.yml` 两处 `cargo build` 加 `-p rumark`；② 新增 `crates/mermaid-rs-renderer/`（上游裁剪：去 cli/png feature 与 mmdr bin、clap/resvg/usvg 依赖、criterion bench、cli_suite；`benches/` 只留 fixtures/typical 语料，因 5 个测试 suite 用 `include_str!`/read_dir 引用它们与 `docs/comparison_sources/`——后者不在 crates.io 包里，从上游 GitHub tag v0.3.1 补齐；`clippy.toml` 必须带上，否则 1.100 工具链报 46 个 lint；测试套件文件头加了与上游 lib.rs 一致的 clippy allow）；③ 状态图三处修复（改动点均以 `rumark` 注释标注）：`render.rs` 圆角分支加入 State（应用层 SVG 后处理随之删除）、`objectives.rs` 的 `relax_edge_span_constraints` 排除 State（消除同 rank 推移造成的 zigzag）、`edge_pipeline.rs` State 不再把自身标签障碍传给 `cell_blocked`（消除绕自身标签的 S 形走线）；④ 标签贴线两处：`label_placement.rs` State 的 own-edge gap 常量改贴线语义（target 0.8，对齐 class）+ 走线预置锚点不再视为固定位置（否则同节点多出边标签在分叉点重叠且无法消解）——这两条是「最后一个图不自然」的最终根因，标签互叠在渲染阶段根本没有去重叠 pass 生效；⑤ rumark 侧：删 `round_state_edge_corners`/`rounded_polyline_path`，`MERMAID_RENDERER_CACHE_VERSION` 11→12，标签重叠回归测试的内边距改按渲染库 State 实际值（3.0/1.6+0.5 余量）；⑥ 文档：AGENTS.md 依赖约定重写 workspace 条目（明确 vendored crate 是唯一允许的 path 依赖，红线仅针对 rgpui）、README/CONTRIBUTING 补校验项。上游 1 个测试因本机字体度量差异宽松化：`cycle_fixture_subgraph_entry_aligns_with_spine` 允许一次 16px 网格折角。新增 crate 回归测试 2 个（状态图正交近直线走线 + 圆角 Q 指令），vendored 套件 424 passed、rumark 993 passed、两 crate clippy -D warnings 全绿、fmt --check 绿、`--no-default-features` 绿、`--profile size` 构建绿；目检用 mmdr/resvg 栅格化（`F:\Desktop\ceshi3\out\`），状态图标签压线带背景框、直线边、圆角。暂存实验台 `D:\tmp-mermaid-tune`（path 依赖渲染不同 LayoutConfig 变体出 SVG）已确认可删。

2026-11-19 **Mermaid 全类型样张扩充与第二轮渲染修复（未提交，同上一批一起提交）**：新增 `docs/mermaid-samples.md`——13 种受支持类型 × 各 2~3 个变体共 28 张图（中文标注，覆盖子图/样式/链式边/激活/分组框/fork-join/泛型/自引用等），文档头记录已知不支持语法（类图 note for、`<br/>`、fontAwesome 图标等）。按 rumark 真实路径（parse_mermaid_strict + 渲染）逐图验证，暴露并修复 5 个 crate 缺陷（均带 `rumark` 注释与回归测试）：① `sequence.rs` 帧与注释的归属判断改用语句序号（`SequenceNote.order` + `SequenceFrame.open_order/close_order`，ir.rs/parser.rs/layout/types.rs 同步加字段）——此前 Note 写在 `loop` 之前但 index 等于帧起点时会被算进帧内，框顶吞掉前面的消息；② `sequence.rs` 激活条 deactivate 的 y 改取前后消息行间隙的中点——此前延伸到下一条消息的行，多覆盖一条消息；③ `sequence.rs` box 分组包围盒改按参与者头部完整矩形 + 标签居中 + 延迟到参与者绘制之后画——此前按生命线中心算、矩形比头部还窄且标签被头部盖住；④ `validator.rs` `check_leading_arrow` 跳过 `<<` 开头的类图刻板注解行——此前 strict 解析拒绝官方 `<<interface>>` 语法；⑤ `parser.rs` `split_edge_chain` 不再对管道标签整体放弃链式拆分（新增 `mask_pipe_label_content` 按字节长度掩掉标签内容防止 `high-risk` 被误判为箭头）——此前 `A -->|否| B[文本] -.-> C` 第二条边目标被整段当 id、标签被吞。`MERMAID_RENDERER_CACHE_VERSION` 12→13。新增回归测试 3 个（帧注释归属归 sequence、刻板注解 strict、管道标签链式边）。基线：vendored 426 passed、rumark 993 passed、fmt/clippy --all-targets -D warnings 全绿。目检结论：28 张样张全部正常（栅格化对比在 `F:\Desktop\ceshi3\out\samples\`）；遗留已知小瑕疵：gitgraph 提交标签沿分支线竖排、时序图自消息钩形箭头可跨分组分隔线、类图成员 `$` 静态标记渲染为 `: $` 后缀——均不影响可读性，暂不修。实验台 `D:\tmp-mermaid-tune` 仍在（按 rumark 路径批量渲染样张用），可删。

2026-11-19 **大文档样张与 Exact 路由档阈值收紧（未提交）**：新增 `docs/large-doc-sample.md`（84.8KB，超过 issue #84 的 84.1KB）——13 种 Mermaid 图各 1 张大尺寸图（40 节点流程图含 4 层子图、40+ 消息时序图、10 实体 ER、24 状态机、40 任务甘特、60+ 节点脑图等）穿插长段落/大表格（60~80 行）/长列表/代码块/公式/脚注/任务列表，由生成器 `D:\tmp-mermaid-tune\gen.pl` 构造（可调参数重生成）。实测（release 构建、按 rumark 路径渲染并计时）：**35 边大流程图 1310ms，其中端口分配 830ms + 边走线 380ms**——根因是 `flowchart_routing_performance_profile` 的 Exact 档（≤64 条边）对每条边做 5 候选 × 4 偏移的网格 A* 试路由。修复：把 Exact 档上限收紧到 24 条边（`edge_pipeline.rs`，带 rumark 注释），24~64 条边的中型图改走 Bounded 快速档（fast_route、无网格试路由）；实测大流程图 **1310ms → 323ms（4 倍）**，改动前后栅格化对比结构几乎一致（`F:\Desktop\ceshi3\out\large\00-before.png` vs `large2\00-flowchart.png`）；上游 `routing_profile_uses_exact_search_for_small_compound_flowcharts` 按新策略改写（40 边断言 Bounded + 补充 ≤24 边仍 Exact 的断言）。其余图表耗时：时序 6ms、ER 18ms、状态 59ms、脑图 1.5ms、其余 <1ms——均无需处理。`MERMAID_RENDERER_CACHE_VERSION` 13→14（24~64 边流程图输出变化）。基线：vendored 426 passed（含改写后的 profile 测试）、rumark 993 passed、fmt/clippy 全绿、28 张小样张复渲染全部 OK。**后续可做**：大流程图剩余 323ms 中走线 A*（网格 cell=16、max_steps=160k）仍占大头，若大文档体验仍卡可考虑预算式走线（按边预算截断 A* 或复用前一边的候选路径）。

2026-11-18 **CI 修复与精简（已提交）**：导出嵌图修复推送后 CI 红了两处——① CI stable 升到 1.100，`needless_range_loop` 新覆盖任意区间索引循环，Linux lint 报 7 处错误（全在 `components/markdown/inline/parse.rs` 映射写入循环，本机 1.98 不报）；改 `iter_mut().enumerate()` / `slice::fill`、单点循环改直接赋值（`4561562`）。② `release.yml` 的 `--notes` 多行字符串续行顶格，破坏 `run: |` 块标量缩进，GitHub 判 workflow file issue，每次 push 到 main 都产生 0 秒失败的 release 跑；补齐缩进（`e611af6`）。修复后 lint 绿，但 Linux 构建 job 的 release 单测挂 8 个：路径测试用 Windows 盘符绝对路径（Unix 下不算绝对路径，`3b716c6` 改平台中立）+ 7 个 PDF 测试因 runner 无 CJK 字体（内嵌字体用户自备不进仓库，系统候选目录落空）。**用户拍板精简 CI**：普通推送只跑 `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`，三平台编译/冒烟/单测/最小构建 job 整体移除（`fc2e937`），字体问题随之消失；release 保持编译→打包→传附件（本无冒烟）。验证：本机装 1.100.0 工具链，debug/release 档 clippy --all-targets 均 0 警告、fmt 两版通过、debug/release 单测均 992 passed、最小构建通过、actionlint 两工作流通过。教训：**本机 clippy 全绿不等于 CI 全绿**（工具链版本漂移，先对比 CI 日志里的 rustc 版本）；工作流 YAML 用 actionlint 本地校验（`gh release download --repo rhysd/actionlint` 取二进制）；CI 门禁只剩静态检查后，编译/测试回归全靠本地按第 5 节校验。精简后首跑（44232212471）全绿，旧跑被 concurrency 自动取消。唯一 annotation 是 `actions/checkout@v4` 的 Node 20 弃用提示（被强制跑在 Node 24，不影响结果，暂不处理）。

2026-11-18 **导出嵌图修复（已提交）**：HTML/PDF/DOCX 导出此前丢图——共享块模型 `Inline::Image` 只留 alt 丢 URL，PDF/DOCX 渲染 `[图片：alt]` 占位；HTML 的 `<img src>` 原样输出，相对路径图换个目录即失效。修复：`Inline::Image` 保留 `url`、`ExportOptions` 增加 `base_dir`（取 `image_paste_root_dir`，与粘贴落盘语义一致）；新增 `export/image.rs` 统一解析 data URI/本地/远程来源（32MB 上限、解码长边 2000px 降采样）；HTML 事件流把本地图替换为 data URI（远程保留原地址）；DOCX 解码转 PNG 按版心宽缩放（EMU，小图不放大、高钳页高六成）`Run::add_image`；PDF 段落/列表按图片切分流，RGBA8 `RawImage` + `add_image`/`UseXobject`（dpi=72 二次缩放）嵌图并处理跨页。`image` 0.25 提为可选直接依赖挂 `export-pdf`/`export-docx`（本就在依赖树，无新增编译负担）；printpdf 不开 `images` feature（无该 feature 时走 raw+FlateDecode+SMask 路径）。加载失败一律回退占位文本，不中断导出。验收：新增 8 个单测（URL 保留、本地加载、HTML 内嵌、DOCX media/blip、PDF XObject、缺图回退）全量 992+ passed；实测产物浏览器目检——HTML 复制到无图目录三处图直显、PDF 查看器三处图按版心嵌入、DOCX 解包 media 为合法 PNG 且 extent=3048000×1905000 EMU（320×200px）。标题/表格内图片仍走 alt 占位（v1 取舍）。

2026-11-18 **工作区干净**：开源准备已提交（`eb0525c` 社区文件 + 移除内部迁移代码，`05a890e` CI / 发布工作流）。此前 2026-11-14 那批代码审查修复（查找回车双扫描与替换后搜索键失效、PDF CJK 字体重复解析、偏好校验和只读前 4 KiB、文件树截断上限被削弱且嵌套截断丢失、另存后无改动也重新序列化、撤销字节裁剪可能清空历史）连同粘贴图片落盘单测与原子写沿用原文件 Unix 权限位（含 `#[cfg(unix)]` 断言，Windows 开发机不会编译到）均已提交。**手动渲染回归已做**：用 `docs/render-sample.md` 逐屏截图核对引用内空行、脚注内空行缩进、列表组空段落折叠、H1/H2 细灰下划线、H6 灰阶、表格对齐与竖线转义、脚注/任务列表/提示块，并用临时文档确认图片 / Mermaid / 公式聚焦后「源码 + 渲染同屏」正常；未见回归。表格整块命中（点单元格进编辑、点框选中整块）由 `selection.rs` 两个单测覆盖。

2026-11-18 **补齐 CI 与换行约定**：新增 `.gitattributes`（`*.rs` 索引存 LF、检出到工作区转 CRLF，二进制资源标 `binary`），把 AGENTS.md / CONTRIBUTING.md 的「`.rs` 必须 CRLF 无 BOM」落到 git 层——此前 CONTRIBUTING 声称由 `.gitattributes` 保证，实际该文件并不存在（本机 `core.autocrlf=true` 掩盖了这一点）。`ci.yml` 的 clippy 全部改为 `--all-targets`，与 README / CONTRIBUTING 的本地校验口径一致（原来不 lint 测试代码）。验证基线（本机跑 CI 等价命令）：`cargo fmt --check` 通过、`cargo clippy --all-targets -- -D warnings` 0 警告、全量 983 passed、`cargo check --no-default-features` 与 `cargo build --release` 通过。

2026-11-13 的**性能与体验审查 backlog（第 3.3 节）已收口并提交**（见 `git log --oneline -3`）。首个提交 `0abf8f4` 是派生数据缓存层；后续提交覆盖步骤 1～9 的余下优化、两个正确性修复（任务复选框绕过 setter、跨块复制把词内 `_` 误转义成 `\_`）与步骤 9 的应用层图片缓存。步骤 7/10 中的异步化子集未做，理由见 3.3 节「剩余」。

2026-11-14 **两项待决已拍板**：① **保留 Setext**——`段落\n---` 解析为 H2 与 CommonMark/GFM、Typora 一致，且「先空一行再输 `---`」仍是分隔线（空段落没有标题目标，走分隔线分支），行为由 `events.rs` 两个单测锁定；已在 `document.rs` 导入解析与 `events.rs` 回车分支补注记，避免复审误判为缺陷。② **导出能力保留**——`AGENTS.md` 第 51 行已是「HTML 单文件导出、PDF/DOCX 导出仍在」，与 `Cargo.toml` 默认 feature（`export-pdf`/`export-docx`）及菜单/动作门控一致，无文档矛盾，不做下线。

`docs/render-sample.md` 被 `include_str!` 进多处测试（`components/markdown/inline/tests.rs`、`editor/document.rs`、`editor/tests.rs`、`export/{pdf,docx}.rs`），**改它就是改测试夹具**，改完必须跑全量 `cargo test -p rumark`。

后续改动按主题归组提交（多主题文件按「主主题」归组；要严格按主题落提交就用 `git add -p` 分块暂存）。提交信息风格 `type: 中文描述`（`fix:` / `perf:` / `test:` / `chore:` / `docs:`）。

## 2. 待决事项（需要拍板）

暂无。

## 3. 待办任务（可直接执行）

### 3.3 性能与体验审查 backlog（2026-11-13 全面审查，按序执行）

> 根因：文档树没有修订号，派生数据（序列化文本、源码映射、引用定义、大纲、状态栏统计、表格布局、代码高亮、公式 SVG）在**每帧或每键**全量重算。
> 下面按杠杆排序，**做完一项删一项**。每条都带实读证据行号（行号会随改动漂移，以函数名为准）。
> 框架侧问题可改 rgpui 源码：`D:\code\rgpui`（用户已授权切 path 依赖并改框架）。**实际结论：未切、未改**，理由与验收记录见下方「rgpui 框架侧结论」。

**步骤 1（已完成 2026-11-13）：文档修订号 + 派生数据缓存层**

- 落地：`BlockRecord.content_version` 仅由 `set_title`/`set_kind`/`set_table` 递增，散落的直接赋值已全部改走 setter；`DocumentTree.revision` 在 `rebuild_metadata_and_snapshot` 递增；新增 `src/editor/derive.rs`：指纹 = revision + 逐可见块 `(entity_id, content_version)` 哈希，缓存 `Arc` 化的 markdown / raw_source / 源码映射+块区间 / 图片与链接引用定义；`serialized_document_text`、`current_document_source`、`build_source_target_mappings*`、`rebuild_image_runtimes` 的解析部分改读缓存；`SourceMapEntry` 偏移表改 `Arc<Vec<usize>>` 使整份映射可廉价克隆。
- 红线：任何内容变更必须走 setter，否则缓存陈旧（`model.rs` 有注释）。
- 验证：新增 4 个回归测试（setter 递增、不改内容复用同一 `Arc`、改字失效、改 kind/结构失效）；当时全量 932 passed，clippy / fmt / 最小构建全绿；实机（仅调试构建）打字、跨块 Shift 连选（166/3081 字）、跨块删除、undo 恢复内容与选区、源码视图序列化与选区映射均正确。
- 尾巴：状态栏/大纲当时仍每帧自算，已由步骤 2 收口。

**rgpui 框架侧结论（步骤 9 要求在此记录）**：只读核对了 `D:\code\rgpui`（分支 `feat/diff`，HEAD `4ddd9345`，工作区干净）。`crates/rgpui/src/app.rs:2994` 的 `fetch_asset` 确实把 `Err` 也永久留在 `loading_assets`，且无 LRU/TTL；`StatusBar` 是纯值 `RenderOnce`、每帧重建元素树，但**它不是「每帧全量文档计算」的根因**，根因在应用层（已由步骤 1/2 解决）。**未切 path 依赖、未改框架源码**：本仓改用 `Div::image_cache` 挂自定义 `ImageCache`（`src/editor/image_cache.rs`）覆盖整棵编辑器元素树，已满足「失败可重试 + 内存有上限」的验收；`Cargo.toml` 三处 `1.7` 版本依赖保持不变，符合 AGENTS.md「不要改回 path 依赖」红线。若将来启用框架其他 `Asset`，再评估上游修复。

**已完成（2026-11-13）**

2. **状态栏与大纲按变更重算**：`status_stats` 复用 `DerivedDocument` 的文本 + 指纹，一次取用同时拿到两种视图文本；`capture_source_selection_snapshot` 改走 `mapping_index_by_entity`，不再克隆整份映射 Vec；`document_fingerprint` 引入 `DOCUMENT_CONTENT_EPOCH` + `(epoch, revision, fingerprint)` 快路，空闲帧零逐块哈希；大纲用 `(fingerprint, is_source)` 短路。验证：空闲帧复用、输入后统计更新两个回归测试。
3. **代码高亮按内容版本缓存**：`Block.code_highlight_key: Option<(Option<SharedString>, u64)>`，`sync_code_highlight` 键相同直接 return，不再每次 `refresh_render_cache` 全量 tree-sitter 重解析。验证：`code_highlight_reuses_result_until_content_changes` 计数 hook。
4. **公式/Mermaid 缓存修正**：新增 `src/components/svg_cache.rs`（内存命中 → 磁盘命中 → 才渲染；磁盘淘汰上限 512、内存 256；内存命中可恢复被删的缓存文件）；LaTeX 改为先查缓存再渲染；Mermaid display SVG 与导出路径都走 `load_svg`；`font_size` / `scale` 的 `f32::to_bits()` 入键改为量化到 0.01，止住 resize 时无限生成缓存文件。验证：重复加载不重渲染、删除缓存文件可恢复、亚像素变化键稳定三个测试。
5. **撤销快照瘦身 + 上限**：`UndoEntry.source_text` 与 `last_stable_source_text` 改 `Arc<str>`，直接复用派生缓存；新增 `HISTORY_BYTES_LIMIT = 64 MiB` 与统一 `trim_history`，undo/redo 都受「200 条 + 字节总量」双重限制。验证：条数上限、字节上限、两者叠加顺序、快照共享分配四个测试。
6. **查找防抖 + 工作区扫描健壮化**：`FIND_DEBOUNCE = 120ms`，用 `Task` drop 取消旧任务；`FindSearchKey = (fingerprint, is_source, query, match_case, whole_word)` 短路重搜；`compute_find_matches` 复用 `String` 窗口 buffer，大小写不敏感比较改 `chars().flat_map(char::to_lowercase).eq(...)`，不再每候选分配。工作区扫描加忽略目录、深度上限 12、单目录 2000 条上限，子目录不可读降级为空节点（根目录失败仍报错）。验证：Unicode 折行语义、防抖复用、忽略目录、深度上限、子目录降级五个测试。
7. **另存为数据安全 + PDF 字体缓存**：弹框前不再序列化旧快照，写盘前从 `weak_editor` 取「此刻」文本，写盘后若当前文本已变则保留 `document_dirty` / `pending_window_edited`；PDF 的 `Fonts::load` 改走进程级 `OnceLock` 字体缓存（原来每次导出重读并解析 ~20MB CJK 字体）。验证：`save_as_writes_latest_text_edited_during_dialog`、`cjk_font_is_parsed_once_across_exports`。**保存/HTML 导出移后台 + 进度提示未做**，见「剩余」。
8. **配置内存缓存 + 变更才写**：`read_app_preferences_with_dirs` 按 `app_config_file()` 路径缓存，用 `(SystemTime, u64)` 签名感知外部改动，本进程写盘后主动失效；启动路径只在「文件不存在 / 损坏重建 / 规范化后内容真的变化」时落盘（原来每次启动都原子写 + fsync，会抹掉用户手写注释）。验证：重复读取命中缓存、保存后失效、外部编辑被检测、未变更不重写四个测试。
9. **图片缓存有界化与失败重试**：`BoundedImageCache` 自行 spawn 加载任务、不经框架的 `fetch_asset`；成功结果按 LRU 保留 256 条并显式 `drop_image`，失败结果保留 5s 后丢弃条目并请求重绘，断网恢复后自动重新加载。验证：LRU 淘汰顺序、条目数上限、失败 TTL、资源键稳定性四个测试。框架源码未改（见上方结论）。
10. **启动/图片热路径（部分）**：`ImageResolvedSource::Embedded` 改存 `Arc<Image>`，base64 解码与 `Image::from_bytes` 全字节哈希只在图片运行时重建时做一次，渲染帧直接复用；最近文件去重由两两比对（O(N²) 次 `canonicalize`）改为 `HashSet` 预计算比较键（O(N)）。验证：`embedded_data_uri_decodes_to_bytes`、`normalizing_recent_files_canonicalizes_each_path_once`。

**额外修复（不在原 backlog）**：`BlockEvent::ToggleTaskChecked` 直接赋值 `kind` 绕过 setter，导致派生缓存读到旧勾选状态 → 改走 `set_kind`；跨块复制时 `escape_literal_text_with_offset_map` 无条件把字面 `_` 转义成 `\_`（`111111_22222222` 复制成 `111111\_22222222`）→ 按 CommonMark 侧翼规则，同一片段内词内 `_` 不再转义，片段边界仍保守转义。

**剩余（低优先，需单独回归）**

- 保存 / HTML 导出移后台 + 进度提示（步骤 7 的另一半）：涉及 `atomic_file` 的 `sync_all`/rename 重试与对话框/关闭保护的状态机，改动面大且难用单测覆盖，暂留 UI 线程同步写；数据丢失风险已由上面的「写盘前重取 + 写后比对」消除。
- 粘贴图片后台编码/落盘、内嵌图改引用、启动先出窗再并行加载、最近文件写入防抖合并（步骤 10 的异步化子集）：都属于启动/事件时序重构，建议连同 4.1 的多文档 tab 改造一起做，避免重复改动 `app_menu.rs` / `main.rs`。

**已确认无问题（别误改）**：光标闪烁已睡到边界（`editing/mod.rs:2184`）、滚动条淡出是一次性定时器（`events.rs:1145`）、拖拽自动滚动仅拖拽期 16ms 运行（`selection.rs:547`）、右键/图片菜单轻量（`context_menu.rs:253`、`image_menu.rs:181`）、PDF `wrap_text` 为 O(n)、DOCX 不加载字体文件。

**已完成的同类优化**：渲染「先分组、后建元素」两趟 + 只为视口内行建元素 + 每帧不再克隆与块数等长的实体句柄（`render.rs`）；光标闪烁前半秒睡到边界（`editing/mod.rs`）。

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

### 4.2 已拍板的问题

1. tab 拖出/拖回放第二期，第一期只做「一窗多文档 + 同窗切换/关闭」。用户选择**薄壳 host** 渲染架构：窗口根改 `DocumentHost`，活动 `Editor` 作为子元素正常挂载、`Editor::render` 不动，tab 条经 `top_strip` 插槽插在标题栏下方。
2. 拖入文件、侧栏点文件的语义在第一期改为「新建 tab」（有宿主时）；standalone 编辑器保持原「替换当前文档」语义。
3. 关窗与退出语义：逐 tab 串行确认未保存（一次一个对话框），任一取消即中止整个流程。
4. 会话持久化（重开恢复 tab 集合）列后续期，不做。

### 4.3 分期与进度

**第一期：一窗多文档 tab（不含跨窗口拖拽）—— 已完成（未提交）**

1. ✅ 新增 `src/editor/document_host.rs`：薄壳 `DocumentHost`（`Vec<TabEntry>` + `active` + `TabStrip`），窗口根由 `Editor` 改为 host；`open_editor_window` 返回 `WindowHandle<DocumentHost>`。standalone 编辑器（测试路径）完全不变。
2. ✅ 路由改造：`with_active_editor` / `with_active_host` / `request_close_editor_window` / `request_quit_application` / `try_load_into_active_blank_window`（空白 tab 就地载入）；菜单与 CLI 开窗循环适配新返回类型。
3. ✅ tab 条用 `TabBar`/`Tab` 渲染，点击切换、`×` 关闭（走脏检查）、脏点（标签前缀 `●`）、无路径显示「未命名」。
4. ✅ 键位重排：`ctrl-tab`/`cmd-tab` 让给「下一个 tab」，`ctrl/cmd-shift-tab` 上一个，`ctrl/cmd-alt-n` 新建，`ctrl/cmd-f4` 关闭；`ToggleViewMode` 改 `ctrl-/`、`ctrl/cmd-alt-v`。新增 `NewTab`/`CloseTab`/`NextTab`/`PreviousTab` 动作 + 菜单项 + `zh-CN`/`en-US` 文案。
5. ✅ 拖入 Markdown / 侧栏点文件：有宿主时经 `DocumentHost::open_path_in_tab` 新建 tab（空白 tab 复用），否则替换当前文档。
6. ✅ 关闭状态机：Editor 加 `close_scope`（Window/Tab/WindowFlow）+ `owner_host` 弱引用；关单 tab 与关窗/退出均走未保存确认，保存/放弃后按目标关闭 tab 或推进下一个脏 tab，取消则中止。用 spawn 打破 host→editor→host 的重入。
7. ✅ 验收：新增/切换/关闭/回绕/空白复用/保存只写活动 tab/键位动作等单测；全量 `cargo test -p rumark` 通过。

2026-11-21 **用户实测反馈收尾（未提交）**：① 拖入 Markdown 崩溃——根因是在 `Editor::update` 中同步 `host.update` 又读回同一 Editor，触发实体重入；改为先同步校验文件/记录 recent，`host.update` 用 `cx.spawn` 延后，并加回归测试（修复前可复现同一 panic）。② tab 条贴左边缘——`TabStrip` 外层补 `pl(TITLE_BAR_LEFT_PADDING)` / `pr(12px)`，抵消 `TabBar` 默认 `px(-1.)` 的负 padding，与标题栏菜单按钮对齐。③ tab 点击无效——点击派发从单个 `Tab::on_click` 改为 `TabBar::on_change`（框架文档明确设置后由栏统一处理选中），并用真实坐标点击单测锁住；关闭按钮仍以 `stop_propagation` 独立处理。④ 新增偏好 `editor.multi_tab_enabled`（`AppPreferences` / `PreferencesFile` / `EditorSettings` / File-Startup Switch，默认 true，写入 `config.toml`，重开窗口生效）：关闭时 `DocumentHost` 只保留一个 tab、不注入 tab 条，tab 动作无操作，关闭走原「关闭窗口」语义，窗口返回类型不变以免回归。⑤ 偏好设置窗口由 800×560 调大到 900×640。验证：`cargo test --bin rumark` 1040 passed / 0 failed、`cargo clippy --all-targets -- -D warnings` 0 警告、`cargo fmt --check` 通过、改动 `.rs` 均 CRLF 无 BOM。

2026-11-21 **窗口脏状态聚合（未提交）**：`DocumentHost` 现在每帧汇总所有 tab 的 `document_dirty` 写入系统 edited 标记与标题脏标记；标题文件名仍取活动 tab。Editor 在 host 模式下不再自行写窗口标题/edited；子 Editor 脏状态变化时通过 `App::notify(host.entity_id())` 通知 host，避免 `editor.update → host.update → editor.read` 重入。新增 `window_chrome_aggregates_dirty_state_across_tabs`：非活动 tab 变脏时标题仍带标记，全部干净后标记清除。

**第一期遗留（低优先）—— 2026-11-21 已完成（未提交）**

- ✅ 同窗拖拽重排、tab 条中键关闭、右键菜单：用框架 `Tab` 的 `on_drag`/`drag_over`/`on_drop` + 自持 `PopupMenu`（照 `table_axis_menu.rs` 的 deferred+anchored 模式）实现。`TabStrip` 加 `menu` 字段；右键菜单含「关闭/关闭其他/关闭右侧/新窗口打开」，i18n 加 4 个 key（zh-CN/en-US 一致）。拖拽用 `TabDragData`（框架已导出），落点即目标 tab 下标，`move_tab(from,to)` 让被拖 tab 保持活动。中键关闭走 `request_close_tab_at`。
- ✅ 关闭非活动 tab 不切换：`request_close_tab_at(index, window, cx)` 直接对指定 editor 设 `CloseScope::Tab` 走确认，`close_editor_tab` 按实体定位移除，全程不改 `active`；× 按钮、中键、右键「关闭」共用此路径。单文档模式退化为关窗。
- ✅ 批量关闭（右键「关闭其他/右侧」）：`DocumentHost.pending_batch_close` 存待关 editor 集合，`advance_batch_close` 先移除干净 tab、脏 tab 逐个设 `CloseScope::BatchFlow` 弹确认，确认后由 `close_editor_tab` 推进下一个；取消时 `cancel_batch_close` 清空。close.rs 的取消分支按 scope 分派 WindowFlow/BatchFlow。
- ✅ 多窗口混合退出：`MenuState` 加 `quit_remaining`/`quitting`，`request_quit_application` 收集所有编辑器窗口（host + standalone）进队列，`advance_quit_window` 逐个请求关闭；`DocumentHost::advance_window_close` 的 Quit 流程不再直接 `cx.quit()`，只 remove 本窗口，`handle_window_closed` 推进下一个、归零时 quit；用户在任一窗口点「继续编辑」经 `cancel_quit_flow` 中止整个退出。新增 4 个 host 层测试（拖拽重排、非活动关闭、关闭其他/右侧）+ 2 个真实坐标 UI 测试（中键关闭、右键菜单）。
- 手动开 3 文件逐屏目检尚未做（仅自动化单测覆盖）。

**第二期：tab 拖出成窗口 / 拖回合并（未开始）**

8. ✅ 跨窗口拖拽 spike（2026-11-21 完成，结论：原生不可行）：rgpui 1.8 的拖拽**状态**是 App 级共享（`app.rs:804` `active_drag`），但**事件派发与命中检测是单窗口级**——`Window::dispatch_event` 按单个 `&mut self` 派发、监听器只取自本窗口 `rendered_frame`（`window.rs:5572/5733-5792`），`on_drop` 用本窗口 `hitbox.is_hovered(window)` 判定（`elements/div.rs:2869-2899`）；Windows 平台按下即 `SetCapture`（`rgpui-windows/src/events.rs:509`），整个拖拽过程的 move/up 被锁在源窗口 HWND；`MouseDownEvent.position` 是窗口客户区局部坐标（`interactive.rs:137`），没有「屏幕点 → 窗口」的 App 级 API（全仓 `window_at*` 搜索无结果）。因此标准 `on_drag`/`on_drop` 无法实现跨窗口 tab 拖出/拖回；纯应用层只能靠 `Window::dispatch_event`（pub）+ `AnyWindowHandle::update` + `window.bounds()` 近似换算来合成跨窗口事件，但 bounds 含非客户区、多显示器/DPI/重叠/焦点切换均需自理，且 drag ghost 绑在源窗口越过边界即被裁剪——能跑特例但不可靠，不建议。
9. 若原生不可行，先提供等效键盘/菜单命令，把「拖拽」留到平台层支持之后。**建议路线**：第二期先用纯应用层命令实现 tab「移出新窗口 / 合入目标窗口」——`cx.open_window` 建窗、`cx.windows()` 遍历 + `downcast::<DocumentHost>()` + `AnyWindowHandle::update` 跨窗口改 tab 列表，`Entity<Editor>` 是 App 级可跨窗口共享；复杂度低到中、零框架改动。原生跨窗口拖拽需框架三处增强：① 平台拖拽期放开 `SetCapture`/透传全局屏幕坐标（`rgpui-windows/events.rs:503-539` + macOS/Linux 对应）；② 新增 `App::window_at_point(screen)` 与 `Window` 局部↔屏幕坐标换算（处理非客户区与 DPI）；③ 把 active_drag 的 MouseMove/MouseUp 按屏幕点路由到目标窗口、drag ghost 改覆盖层。`active_drag` 已是 App 级，①+③ 后 `div.rs` 现有 drop 命中逻辑基本可复用。

## 5. 验证流程（每次改完照跑）

```pwsh
cargo fmt --check
cargo clippy -p rumark --all-targets          # 期望 0 warning
cargo test -p rumark                          # 当前基线 1014 passed
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
8. 实机脚本合成 Shift：`keybd_event` 送 Shift 时 `GetAsyncKeyState/GetKeyState` 偶发拿不到修饰键，Shift+方向键会退化成单键；改用 `System.Windows.Forms.SendKeys` 的 `"+{RIGHT}"`。键盘行为以 `cx.simulate_keystrokes` 的单测为准。

## 7. 参考

- `AGENTS.md`：项目约定、构建/验证顺序、结构与平台注意事项。
- `docs/render-sample.md`：手动渲染回归样本。
- 本轮改动涉及的核心文件：`src/editor/selection.rs`、`src/editor/source_mapping.rs`、`src/editor/tree.rs`、`src/atomic_file.rs`、`src/components/block/render.rs`。
