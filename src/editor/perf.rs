//! 大文档性能基线测量。
//!
//! 这些用例默认 `#[ignore]`，只在显式执行时读取真实大文件并打印各阶段耗时：
//! ```text
//! RUMARK_BENCH_FILES="F:/Desktop/ceshi3/releases.md;F:/Desktop/ceshi3/THIRD-PARTY-NOTICES.md" \
//!   cargo test -p rumark --release editor::perf -- --ignored --nocapture
//! ```
//! 未提供 `RUMARK_BENCH_FILES` 时退回上面两份样张路径；文件不存在则跳过，
//! 因此普通 `cargo test` 永远不会因本机缺少样张而失败。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use rgpui::{TestAppContext, WindowExt, point, px};

use super::Editor;
use crate::i18n::I18nManager;

/// 初始化测量用测试应用（与编辑器测试一致）。
fn init_perf_app(cx: &mut TestAppContext) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
        crate::components::init(cx);
    });
}

/// 待测量的样张：环境变量分号分隔，缺省为两份已知大文件。
fn bench_files() -> Vec<PathBuf> {
    let raw = std::env::var("RUMARK_BENCH_FILES").unwrap_or_else(|_| {
        "F:/Desktop/ceshi3/releases.md;F:/Desktop/ceshi3/THIRD-PARTY-NOTICES.md".to_string()
    });
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

///  elapsed 转毫秒（测量辅助）。
fn elapsed_ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

/// CPU 阶段基线：读取、分批解析建树、安装、首次派生、保存序列化。
///
/// 走的是大型文档真实路径：`build_root_blocks_chunk` 分批 + `replace_document_from_root_blocks`
/// 直接复用载入文本，避免把“未挂载”误当作“未加载”。
#[rgpui::test]
#[ignore = "性能基线：需显式提供 RUMARK_BENCH_FILES 样张"]
async fn large_document_baseline(cx: &mut TestAppContext) {
    init_perf_app(cx);
    for path in bench_files() {
        if !path.exists() {
            eprintln!("[perf] 样张不存在，跳过：{}", path.display());
            continue;
        }

        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let t = Instant::now();
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("[perf] 读取失败 {}：{}", path.display(), err);
                continue;
            }
        };
        let read_ms = elapsed_ms(t);
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = normalized.split('\n').map(ToOwned::to_owned).collect();
        let line_count = lines.len();

        // 先造轻量占位编辑器，再按 32 根块/批复刻分批构建。
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "placeholder".to_string(), None)
        });

        // 清零归因探针，确保只统计当前文件的建树阶段。
        let _ = crate::components::take_inline_parse_stats();
        let _ = crate::components::take_highlight_time_ms();
        let _ = super::document::take_document_parse_stats();

        let t = Instant::now();
        let mut roots = Vec::new();
        let mut pos = 0usize;
        let mut chunk_work_ms = 0.0f64;
        let mut chunk_count = 0usize;
        let mut update_ms = 0.0f64;
        let mut extend_ms = 0.0f64;
        let mut prev_is_list = false;
        loop {
            let update_started = Instant::now();
            let (blocks, next) = cx.update(|_window, cx| {
                editor.update(cx, |_editor, cx| {
                    let chunk_started = Instant::now();
                    let result = Editor::build_root_blocks_chunk(
                        cx,
                        &lines,
                        pos,
                        super::ROOT_BLOCKS_PER_CHUNK,
                        prev_is_list,
                    );
                    chunk_work_ms += elapsed_ms(chunk_started);
                    result
                })
            });
            update_ms += elapsed_ms(update_started);
            chunk_count += 1;
            let extend_started = Instant::now();
            if let Some(last) = blocks.last() {
                prev_is_list = cx.read(|app| last.read(app).kind().is_list_item());
            }
            roots.extend(blocks);
            extend_ms += elapsed_ms(extend_started);
            if next >= lines.len() {
                break;
            }
            pos = next;
        }
        let parse_ms = elapsed_ms(t);
        let (inline_calls, inline_fast, inline_ms) = crate::components::take_inline_parse_stats();
        let highlight_ms = crate::components::take_highlight_time_ms();
        let (
            native_blocks,
            native_ms,
            root_checks,
            root_check_ms,
            boundary_checks,
            boundary_ms,
            paragraph_collects,
            paragraph_collect_ms,
        ) = super::document::take_document_parse_stats();
        let root_count = roots.len();

        // 模拟 defer_large_document_load 的完成态：直接复用载入文本，不强制整篇映射。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.document_loading = true;
                editor.replace_document_from_root_blocks(
                    roots,
                    Some(path.clone()),
                    Arc::from(normalized.as_str()),
                    cx,
                );
            });
        });
        let install_ms = elapsed_ms(t);

        // 首次精确派生：序列化、源码映射与引用定义全量计算一次。
        let t = Instant::now();
        let _fingerprint =
            editor.read_with(cx, |editor, cx| editor.derived_document(cx).fingerprint());
        let derived_ms = elapsed_ms(t);

        // 保存序列化（命中派生缓存）。
        let t = Instant::now();
        let _text = editor.read_with(cx, |editor, cx| editor.serialized_document_text(cx));
        let save_ms = elapsed_ms(t);

        let visible = editor.read_with(cx, |editor, _cx| editor.document.visible_block_count());

        eprintln!(
            "[perf] {}\n  bytes={} lines={} roots={} visible={}\n  read={:.1}ms parse={:.1}ms install={:.1}ms derived={:.1}ms save={:.1}ms\n  inline_calls={} inline_fast={} inline_ms={:.1}ms highlight_ms={:.1}ms\n  native_blocks={} native_ms={:.1}ms root_checks={} root_check_ms={:.1}ms boundary_checks={} boundary_ms={:.1}ms paragraph_collects={} paragraph_collect_ms={:.1}ms\n  chunks={} chunk_work_ms={:.1}ms update_ms={:.1}ms extend_ms={:.1}ms other_parse_ms={:.1}ms",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            bytes,
            line_count,
            root_count,
            visible,
            read_ms,
            parse_ms,
            install_ms,
            derived_ms,
            save_ms,
            inline_calls,
            inline_fast,
            inline_ms,
            highlight_ms,
            native_blocks,
            native_ms,
            root_checks,
            root_check_ms,
            boundary_checks,
            boundary_ms,
            paragraph_collects,
            paragraph_collect_ms,
            chunk_count,
            chunk_work_ms,
            update_ms,
            extend_ms,
            (parse_ms - inline_ms - highlight_ms).max(0.0),
        );
    }
}

/// 渲染阶段基线：首帧与逐段滚动帧耗时。
#[rgpui::test]
#[ignore = "性能基线：需显式提供 RUMARK_BENCH_FILES 样张"]
async fn large_document_render_baseline(cx: &mut TestAppContext) {
    init_perf_app(cx);
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let (editor, cx) = cx
            .add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, Some(path.clone())));

        // 首帧。
        let t = Instant::now();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        let first_frame = elapsed_ms(t);

        // 逐段滚到底，逐帧计时。
        let mut frame_times = Vec::new();
        for step in 0..=20 {
            let t = Instant::now();
            cx.update(|window, cx| {
                editor.update(cx, |editor, _cx| {
                    let max = editor.scroll_handle.max_offset().y.as_f32().max(0.0);
                    editor
                        .scroll_handle
                        .set_offset(point(px(0.0), px(-max * step as f32 / 20.0)));
                });
                window.draw(cx).clear(cx);
            });
            frame_times.push(elapsed_ms(t));
            cx.run_until_parked();
        }
        let scroll_avg = frame_times.iter().sum::<f64>() / frame_times.len() as f64;
        let scroll_max = frame_times.iter().cloned().fold(0.0_f64, f64::max);

        eprintln!(
            "[perf-render] {}\n  first_frame={:.1}ms scroll_avg={:.1}ms scroll_max={:.1}ms ({} frames)",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            first_frame,
            scroll_avg,
            scroll_max,
            frame_times.len(),
        );
    }
}

/// 安装阶段细分：单独测量 replace_roots、表格/图片运行时、工作区同步。
#[rgpui::test]
#[ignore = "性能基线：细分安装阶段"]
async fn large_document_install_profile(cx: &mut TestAppContext) {
    init_perf_app(cx);
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = normalized.split('\n').map(ToOwned::to_owned).collect();
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "placeholder".to_string(), None)
        });

        let mut roots = Vec::new();
        let mut pos = 0usize;
        let mut prev_is_list = false;
        loop {
            let (blocks, next) = cx.update(|_window, cx| {
                editor.update(cx, |_editor, cx| {
                    Editor::build_root_blocks_chunk(cx, &lines, pos, 32, prev_is_list)
                })
            });
            if let Some(last) = blocks.last() {
                prev_is_list = cx.read(|app| last.read(app).kind().is_list_item());
            }
            roots.extend(blocks);
            if next >= lines.len() {
                break;
            }
            pos = next;
        }

        // 逐段计时：replace_roots 内的元数据/快照重建。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.document.replace_roots(roots, cx);
            });
        });
        let roots_ms = elapsed_ms(t);

        // 表格 + 图片/脚注运行时。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.rebuild_table_runtimes_from_source(&normalized, cx);
            });
        });
        let runtime_ms = elapsed_ms(t);

        // 工作区路径变更同步。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.sync_workspace_after_document_path_change(cx);
            });
        });
        let workspace_ms = elapsed_ms(t);

        eprintln!(
            "[perf-install] {}\n  replace_roots={:.1}ms runtime={:.1}ms workspace={:.1}ms",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            roots_ms,
            runtime_ms,
            workspace_ms,
        );
    }
}

/// 运行时细分：compare update+notify vs update-only vs read-only across all visible blocks.
#[rgpui::test]
#[ignore = "性能基线：细分运行时同步"]
async fn large_document_runtime_profile(cx: &mut TestAppContext) {
    init_perf_app(cx);
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = normalized.split('\n').map(ToOwned::to_owned).collect();
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "placeholder".to_string(), None)
        });
        let mut roots = Vec::new();
        let mut pos = 0usize;
        let mut prev_is_list = false;
        loop {
            let (blocks, next) = cx.update(|_window, cx| {
                editor.update(cx, |_editor, cx| {
                    Editor::build_root_blocks_chunk(cx, &lines, pos, 32, prev_is_list)
                })
            });
            if let Some(last) = blocks.last() {
                prev_is_list = cx.read(|app| last.read(app).kind().is_list_item());
            }
            roots.extend(blocks);
            if next >= lines.len() {
                break;
            }
            pos = next;
        }
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.document.replace_roots(roots, cx);
            });
        });

        let visible = editor.read_with(cx, |editor, _cx| editor.document.visible_blocks().to_vec());
        let count = visible.len();

        // 纯 read。
        let t = Instant::now();
        cx.update(|_window, cx| {
            for vb in &visible {
                vb.entity.update(cx, |_, _| {});
            }
        });
        let update_only_ms = elapsed_ms(t);

        // update + notify。
        let t = Instant::now();
        cx.update(|_window, cx| {
            for vb in &visible {
                vb.entity.update(cx, |_, cx| {
                    cx.notify();
                });
            }
        });
        let update_notify_ms = elapsed_ms(t);

        // 实际 set_runtime_context（含 notify）。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                for vb in &visible {
                    editor.sync_runtime_context_for_block(&vb.entity, None, cx);
                }
            });
        });
        let set_context_ms = elapsed_ms(t);

        // rebuild_footnote_registry。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.rebuild_footnote_registry(cx);
            });
        });
        let footnote_ms = elapsed_ms(t);

        eprintln!(
            "[perf-runtime] {} blocks={}\n  update_only={:.1}ms update_notify={:.1}ms set_context={:.1}ms footnote={:.1}ms",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            count,
            update_only_ms,
            update_notify_ms,
            set_context_ms,
            footnote_ms,
        );
    }
}

/// 细分 from_source 运行时的剩余成本：引用解析 vs 上下文同步 vs 表格清空。
#[rgpui::test]
#[ignore = "性能基线：细分 from_source 运行时"]
async fn large_document_from_source_profile(cx: &mut TestAppContext) {
    init_perf_app(cx);
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = normalized.split('\n').map(ToOwned::to_owned).collect();
        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "placeholder".to_string(), None)
        });
        let mut roots = Vec::new();
        let mut pos = 0usize;
        let mut prev_is_list = false;
        loop {
            let (blocks, next) = cx.update(|_window, cx| {
                editor.update(cx, |_editor, cx| {
                    Editor::build_root_blocks_chunk(cx, &lines, pos, 32, prev_is_list)
                })
            });
            if let Some(last) = blocks.last() {
                prev_is_list = cx.read(|app| last.read(app).kind().is_list_item());
            }
            roots.extend(blocks);
            if next >= lines.len() {
                break;
            }
            pos = next;
        }
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.document.replace_roots(roots, cx);
            });
        });

        // 引用定义解析（独立，不含块同步）。
        let t = Instant::now();
        let _img = crate::components::parse_image_reference_definitions(&normalized);
        let _link = crate::components::parse_link_reference_definitions(&normalized);
        let parse_refs_ms = elapsed_ms(t);

        // 完整 from_source 运行时（解析 + 安静上下文同步）。
        let t = Instant::now();
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.rebuild_image_runtimes_from_source(&normalized, cx);
            });
        });
        let from_source_ms = elapsed_ms(t);

        eprintln!(
            "[perf-from-source] {}\n  parse_refs={:.1}ms total_image_runtime={:.1}ms",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            parse_refs_ms,
            from_source_ms,
        );
    }
}

/// 关闭链路基线：冷/热保存序列化、完整落盘、脏文档弹窗、窗口销毁（实体级联释放）。
#[rgpui::test]
#[ignore = "性能基线：关闭链路"]
async fn large_document_close_profile(cx: &mut TestAppContext) {
    init_perf_app(cx);
    // 常驻一个空窗口，避免样张窗口移除后触发 LastWindowClosed 退出影响后续迭代。
    cx.add_empty_window();
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let (editor, cx) = cx
            .add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, Some(path.clone())));

        // 1. 冷保存序列化：保存并关闭的主 CPU 开销（全篇派生 + 序列化）。
        let t = Instant::now();
        let _text = editor.read_with(cx, |editor, cx| editor.serialized_document_text(cx));
        let cold_serialize_ms = elapsed_ms(t);

        // 2. 热保存序列化（命中派生缓存）。
        let t = Instant::now();
        let _text = editor.read_with(cx, |editor, cx| editor.serialized_document_text(cx));
        let warm_serialize_ms = elapsed_ms(t);

        // 3. 完整保存到临时文件（序列化 + 原子写盘 + sync_all）。
        let temp_dir = std::env::temp_dir().join("rumark-close-bench");
        std::fs::create_dir_all(&temp_dir).ok();
        let temp_path = temp_dir.join(
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bench.md"),
        );
        let t = Instant::now();
        let saved = cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.save_to_existing_path(&temp_path, window, cx)
            })
        });
        let save_to_disk_ms = elapsed_ms(t);

        // 4. 脏文档点击关闭：弹确认框路径（含对话框首个渲染帧）。
        let t = Instant::now();
        cx.update(|window, cx| {
            editor.update(cx, |editor, _cx| editor.document_dirty = true);
            editor.update(cx, |editor, cx| {
                let _ = editor.on_window_should_close(window, cx);
            });
            window.draw(cx).clear(cx);
        });
        let dirty_dialog_ms = elapsed_ms(t);
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| editor.hide_unsaved_changes_dialog(cx));
            editor.update(cx, |editor, _cx| editor.document_dirty = false);
            // 用框架路径真正移除对话框对象，避免其闭包继续持有编辑器实体强引用，
            // 否则第 5 步的窗口销毁无法级联释放块实体，teardown 计时失真。
            window.close_dialog(cx);
            window.draw(cx).clear(cx);
        });

        // 5. 干净文档点击关闭：检查放行 + 窗口销毁 + 全部块实体级联释放。
        //    先释放测试持有的实体引用，让窗口移除能真正级联 drop 整棵树。
        drop(editor);
        let t = Instant::now();
        cx.update(|window, cx| {
            window.remove_window();
            let _ = cx;
        });
        cx.run_until_parked();
        let teardown_ms = elapsed_ms(t);

        eprintln!(
            "[perf-close] {}\n  cold_serialize={:.1}ms warm_serialize={:.1}ms save_to_disk={:.1}ms(saved={}) dirty_dialog={:.1}ms teardown={:.1}ms",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            cold_serialize_ms,
            warm_serialize_ms,
            save_to_disk_ms,
            saved,
            dirty_dialog_ms,
            teardown_ms,
        );
    }
}

/// 真实样张对拍：分批构建的块树（含安装后序列化结果）必须与一次完整构建一致。
///
/// 批次边界落在空行/列表组分隔处时，空行保留规则依赖跨批上下文；该用例用
/// 真实文件分布守护这条对齐关系，样张缺失即跳过。
#[rgpui::test]
#[ignore = "需要本机样张（RUMARK_BENCH_FILES），缺失即跳过"]
async fn chunked_build_matches_full_parse_on_samples(cx: &mut TestAppContext) {
    init_perf_app(cx);
    cx.add_empty_window();
    for path in bench_files() {
        if !path.exists() {
            continue;
        }
        let markdown = match super::read_markdown_file(&path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<String> = normalized.split('\n').map(ToOwned::to_owned).collect();

        let (editor, cx) = cx.add_window_view(|_window, cx| {
            Editor::from_markdown(cx, "placeholder".to_string(), None)
        });
        let mut roots = Vec::new();
        let mut pos = 0usize;
        let mut prev_is_list = false;
        loop {
            let (blocks, next) = cx.update(|_window, cx| {
                editor.update(cx, |_editor, cx| {
                    Editor::build_root_blocks_chunk(
                        cx,
                        &lines,
                        pos,
                        super::ROOT_BLOCKS_PER_CHUNK,
                        prev_is_list,
                    )
                })
            });
            if let Some(last) = blocks.last() {
                prev_is_list = cx.read(|app| last.read(app).kind().is_list_item());
            }
            roots.extend(blocks);
            if next >= lines.len() {
                break;
            }
            pos = next;
        }
        let source = Arc::from(normalized.as_str());
        cx.update(|_window, cx| {
            editor.update(cx, |editor, cx| {
                editor.document_loading = true;
                editor.replace_document_from_root_blocks(roots, None, source, cx);
            });
        });
        // 注意与参照比较的是安装后的可见块数（含子块），根块数不含列表项子块。
        let chunked_visible =
            editor.read_with(cx, |editor, _cx| editor.document.visible_block_count());

        let (reference_count, reference_serialized) = {
            let (reference, refcx) = cx
                .add_window_view(|_window, cx| Editor::from_markdown(cx, normalized.clone(), None));
            let count =
                reference.read_with(refcx, |editor, _cx| editor.document.visible_block_count());
            let text = reference.read_with(refcx, |editor, cx| editor.serialized_document_text(cx));
            (count, text)
        };
        let chunked_serialized =
            editor.read_with(cx, |editor, cx| editor.serialized_document_text(cx));

        assert_eq!(
            chunked_visible,
            reference_count,
            "{} 分批构建的可见块数应与完整构建一致",
            path.display()
        );
        assert_eq!(
            chunked_serialized,
            reference_serialized,
            "{} 分批构建的序列化结果应与完整构建一致",
            path.display()
        );
    }
}
