use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rgpui::{
    AnyWindowHandle, AppContext, ClickEvent, KeyDownEvent, Keystroke, ParentElement, Styled,
    TestAppContext, VisualTestContext, WindowExt,
};

use super::{Editor, MountedRow, ViewMode};
use crate::components::{
    BlockKind, CloseWindow, FocusNext, ImageReferenceDefinitions, ImageResolvedSource,
    InlineTextTree, Newline, QuitApplication, Redo, SaveDocument, TableAlignColumnCenter,
    TableCellInlineImageSegment, TableColumnAlignment, TableDeleteColumn, TableDeleteRow,
    TableInsertColumnRight, TableInsertRowBelow, TableMoveColumnRight, TableMoveRowDown, Undo,
    parse_table_cell_inline_images, superscript_ordinal, update_entity_forgiving,
};
use crate::i18n::I18nManager;
use crate::theme::ThemeDimensions;
fn init_editor_test_app(cx: &mut TestAppContext) {
    cx.update(|cx| {
        I18nManager::init_with_language_id(cx, "en-US");
        rgpui::theme::init(cx);
        crate::components::init(cx);
    });
}

fn temp_markdown_path(test_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rumark-{test_name}-{}-{nanos}.md",
        std::process::id()
    ))
}

fn temp_export_path(test_name: &str, extension: &str) -> PathBuf {
    let mut path = temp_markdown_path(test_name);
    path.set_extension(extension);
    path
}

fn redraw(cx: &mut rgpui::VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

fn activate_visual_window(cx: &mut VisualTestContext) -> AnyWindowHandle {
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    cx.cx
        .update(|cx| cx.active_window().expect("window should be active"))
}

#[test]
fn centered_column_ratio_stays_full_before_shrink_start() {
    let dimensions = &ThemeDimensions::DEFAULT;
    assert_eq!(Editor::centered_column_ratio(900.0, dimensions), 1.0);
    assert_eq!(
        Editor::centered_column_ratio(dimensions.centered_shrink_start, dimensions),
        1.0
    );
}

#[test]
fn centered_column_ratio_reaches_new_minimum() {
    let dimensions = &ThemeDimensions::DEFAULT;
    let ratio = Editor::centered_column_ratio(dimensions.centered_shrink_end, dimensions);
    assert!((ratio - 0.58).abs() < f32::EPSILON);
}

#[test]
fn scrollbar_geometry_and_inverse_mapping_stay_aligned() {
    let geometry = Editor::scrollbar_geometry(400.0, 600.0, 300.0);
    assert_eq!(geometry.track_height, 400.0);
    assert!(geometry.thumb_height >= 28.0);
    assert!((geometry.thumb_top - (400.0 - geometry.thumb_height) * 0.5).abs() < 0.001);

    let scroll_y = Editor::scroll_offset_for_thumb_top(
        geometry.thumb_top,
        geometry.track_height,
        geometry.thumb_height,
        geometry.max_scroll_y,
    );
    assert!((scroll_y - 300.0).abs() < 0.001);
}

#[test]
fn scrollbar_offset_mapping_clamps_to_track_bounds() {
    let geometry = Editor::scrollbar_geometry(300.0, 450.0, 0.0);
    assert_eq!(
        Editor::scroll_offset_for_thumb_top(
            -25.0,
            geometry.track_height,
            geometry.thumb_height,
            geometry.max_scroll_y,
        ),
        0.0
    );
    assert_eq!(
        Editor::scroll_offset_for_thumb_top(
            999.0,
            geometry.track_height,
            geometry.thumb_height,
            geometry.max_scroll_y,
        ),
        geometry.max_scroll_y
    );
}

/// Equal-height rows as per-row footprints, the input `rendered_window` takes.
fn uniform_strides(count: usize, height: f32) -> Vec<f32> {
    vec![height; count]
}

#[test]
fn rendered_window_culls_offscreen_rows() {
    // 100 rows of 50px (total 5000). Scroll 2000, viewport 400 -> band [2000, 2400].
    let strides = uniform_strides(100, 50.0);
    let window = Editor::rendered_window(&strides, 2000.0, 400.0, 0.0, None);

    // Row i spans [50i, 50i+50). bottom>=2000 -> i>=39; top<=2400 -> i<=48.
    assert_eq!(window.run_start, 39);
    assert_eq!(window.run_end, 49);
    assert!((window.top_h - 1950.0).abs() < 0.01);
    assert!((window.bottom_h - 2550.0).abs() < 0.01);
}

#[test]
fn rendered_window_keeps_focus_row_mounted() {
    let strides = uniform_strides(100, 50.0);
    // Viewport at the top, caret parked far below at row 80.
    let window = Editor::rendered_window(&strides, 0.0, 400.0, 0.0, Some(80));

    // The caret rides its own island; the rows above it stay culled.
    assert_eq!(window.run_start, 0);
    assert_eq!(window.run_end, 9);
    let island = window.focus_island.expect("caret row stays mounted");
    assert_eq!(island.row, 80);
    assert!((island.lead_h - 3550.0).abs() < 0.01);
}

#[test]
fn rendered_window_focus_above_run_does_not_widen_it() {
    // Reading downward leaves the caret at the top of the document, so the rows
    // between it and the viewport must stay culled.
    let strides = uniform_strides(100, 50.0);
    let window = Editor::rendered_window(&strides, 2000.0, 400.0, 0.0, Some(0));

    assert_eq!(window.run_start, 39);
    assert_eq!(window.run_end, 49);
    let island = window.focus_island.expect("caret row stays mounted");
    assert_eq!(island.row, 0);
    assert_eq!(island.lead_h, 0.0);
    assert!((window.top_h - 1900.0).abs() < 0.01);
}

#[test]
fn rendered_window_focus_inside_run_needs_no_island() {
    let strides = uniform_strides(100, 50.0);
    let window = Editor::rendered_window(&strides, 2000.0, 400.0, 0.0, Some(42));

    assert_eq!(window.run_start, 39);
    assert_eq!(window.run_end, 49);
    assert_eq!(window.focus_island, None);
}

#[test]
fn rendered_window_tracks_current_scroll_offset() {
    // Scrolling by one row's height shifts the mounted run by exactly one row.
    let strides = uniform_strides(100, 50.0);

    let low = Editor::rendered_window(&strides, 2000.0, 400.0, 0.0, None);
    let high = Editor::rendered_window(&strides, 2050.0, 400.0, 0.0, None);

    assert_eq!(low.run_start, 39);
    assert_eq!(low.run_end, 49);
    assert_eq!(high.run_start, low.run_start + 1);
    assert_eq!(high.run_end, low.run_end + 1);
}

#[test]
fn rendered_window_has_no_spacer_at_document_edges() {
    let strides = uniform_strides(50, 40.0); // total 2000

    let at_top = Editor::rendered_window(&strides, 0.0, 400.0, 0.0, None);
    assert_eq!(at_top.run_start, 0);
    assert_eq!(at_top.top_h, 0.0);
    assert!(at_top.bottom_h > 0.0);

    let at_bottom = Editor::rendered_window(&strides, 1600.0, 400.0, 0.0, None);
    assert_eq!(at_bottom.run_end, 50);
    assert_eq!(at_bottom.bottom_h, 0.0);
    assert!(at_bottom.top_h > 0.0);
}

#[test]
fn rendered_window_preserves_total_height() {
    let strides = uniform_strides(200, 37.0);
    let total: f32 = strides.iter().sum();

    for &(scroll_y, viewport_height, focus) in &[
        (0.0f32, 500.0f32, None),
        (3000.0, 500.0, None),
        (37.0 * 150.0, 37.0 * 5.0, Some(10usize)),
    ] {
        let window = Editor::rendered_window(&strides, scroll_y, viewport_height, 200.0, focus);
        let rendered: f32 = strides[window.run_start..window.run_end].iter().sum();
        let island: f32 = window
            .focus_island
            .map_or(0.0, |island| island.lead_h + strides[island.row]);
        assert!(
            (window.top_h + rendered + island + window.bottom_h - total).abs() < 0.01,
            "height invariant broken at scroll {scroll_y}"
        );
    }
}

#[test]
fn rendered_window_estimated_row_keeps_culling_active() {
    // Row 60 is an estimated (unmeasured) row; it must not disable culling.
    let mut strides = uniform_strides(100, 50.0);
    strides[60] = 20.0;

    let window = Editor::rendered_window(&strides, 0.0, 400.0, 0.0, None);
    assert_eq!(window.run_start, 0);
    assert!(
        window.run_end < strides.len(),
        "a single estimated row must not disable culling"
    );
}

#[test]
fn rendered_window_all_estimated_windows_near_top() {
    // Cold start: all rows estimated. At the top the window still covers the
    // first rows, so the viewport is never blank while heights are learned.
    let strides = uniform_strides(500, 20.0);

    let window = Editor::rendered_window(&strides, 0.0, 400.0, 0.0, None);
    assert_eq!(window.run_start, 0);
    assert!(window.run_end < strides.len());
    // A viewport-plus-band worth of rows, not the whole document.
    assert!(window.run_end >= 20);
}

#[test]
fn rendered_window_scrolled_past_estimates_mounts_trailing_run() {
    // Rows the window has never mounted are lower bounds, so the scroll offset
    // can sit past their running sum. The tail must still fill the viewport.
    let strides = uniform_strides(100, 20.0); // total 2000
    let window = Editor::rendered_window(&strides, 9000.0, 400.0, 200.0, None);

    assert_eq!(window.run_end, 100);
    assert_eq!(window.bottom_h, 0.0);
    let mounted: f32 = strides[window.run_start..window.run_end].iter().sum();
    assert!(
        mounted >= 600.0,
        "a viewport plus overdraw must stay mounted, got {mounted}px"
    );
}

#[rgpui::test]
async fn footprints_are_dropped_when_the_scroll_column_changes_shape(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    // Footprints are read back by child index, so anything added to the scroll
    // column would pair them with the wrong rows. The count has to be re-checked
    // rather than assumed, or the mismatch is silent.
    let markdown = (0..60)
        .map(|index| format!("## Section {index}\n\nParagraph {index}.\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..3 {
        redraw(cx);
    }

    editor.read_with(cx, |editor, _cx| {
        let run = editor.prev_mounted_run.expect("a run was mounted");
        assert!(
            editor.mounted_run_is_addressable(run),
            "the run the column just emitted must be readable"
        );
        for drift in [run.child_count + 1, run.child_count - 1] {
            assert!(
                !editor.mounted_run_is_addressable(MountedRow {
                    child_count: drift,
                    ..run
                }),
                "a column emitting {drift} children instead of {} must not be trusted",
                run.child_count
            );
        }
    });
}

#[rgpui::test]
async fn reading_to_the_bottom_leaves_the_caret_row_behind(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    // Scrolling never moves the caret, so it stays where the document loaded.
    // The rows between it and the viewport must not ride along.
    let markdown = (0..200)
        .map(|index| format!("## Section {index}\n\nParagraph body for section {index}.\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..3 {
        redraw(cx);
    }

    for _ in 0..10 {
        editor.update(cx, |editor, _cx| {
            let max = editor.scroll_handle.max_offset().y;
            editor
                .scroll_handle
                .set_offset(rgpui::point(rgpui::px(0.0), -max));
        });
        redraw(cx);
        redraw(cx);
    }

    editor.read_with(cx, |editor, _cx| {
        let run = editor.prev_mounted_run.expect("a run was mounted");
        let (run_start, run_end) = (run.row_start, run.row_end);
        let rows = editor.document.visible_blocks().len();
        assert!(
            run_start > 0,
            "the caret's row dragged the whole prefix on screen"
        );
        assert!(
            run_end - run_start < rows / 4,
            "{} of {rows} rows mounted at the bottom of the document",
            run_end - run_start
        );
    });
}

#[rgpui::test]
async fn document_present_on_the_first_frame_is_measured_at_the_real_width(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);
    // Launching with a file renders one frame before the scroll bounds exist,
    // collapsing the content column to its floor so every block wraps a
    // character per line. Caching those footprints would size the document from
    // a layout the reader never sees.
    let markdown = (0..40)
        .map(|index| {
            format!(
                "## Section {index}\n\nA paragraph with enough words in it to wrap many times over \
                 once the content column narrows to a sliver.\n"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    for _ in 0..4 {
        redraw(cx);
    }

    editor.read_with(cx, |editor, _cx| {
        let widest = editor
            .row_stride_cache
            .values()
            .fold(0.0f32, |widest, stride| widest.max(*stride));
        assert!(
            widest > 0.0 && widest < 200.0,
            "headings and one-line paragraphs cannot be {widest}px tall; \
             the first frame's collapsed column was cached"
        );
        let run = editor.prev_mounted_run.expect("a run was mounted");
        assert_eq!(
            run.row_start, 0,
            "the top of the document must stay mounted"
        );
        assert!(
            run.row_end > 8,
            "only {} rows mounted, so the viewport is mostly spacer",
            run.row_end
        );
    });
}

#[rgpui::test]
async fn draw_test_md_fixture_does_not_overflow(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = include_str!("../../docs/render-sample.md").to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();

    // 夹具确实被解析并挂载，否则本用例只是空文档绘制，守不住回归。
    editor.read_with(cx, |editor, _cx| {
        let visible = editor.document.visible_blocks();
        assert!(
            visible.len() > 40,
            "fixture should parse into many blocks, got {}",
            visible.len()
        );
        let run = editor.prev_mounted_run.expect("a run should be mounted");
        assert!(
            run.row_end > 0,
            "mounted run should cover rows, got {:?}",
            (run.row_start, run.row_end)
        );
    });
}

/// 全夹具滚动回归：逐段滚到底并绘制每一窗，保证长文档滚动全程无崩溃。
#[rgpui::test]
async fn scrolling_full_fixture_renders_every_window(cx: &mut TestAppContext) {
    use rgpui::{point, px};
    init_editor_test_app(cx);
    let markdown = include_str!("../../docs/render-sample.md").to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    redraw(cx);
    let mut bottom_reached = false;
    for step in 0..=20 {
        cx.update(|window, cx| {
            editor.update(cx, |editor, _cx| {
                let max = editor.scroll_handle.max_offset().y.as_f32().max(0.0);
                editor
                    .scroll_handle
                    .set_offset(point(px(0.0), px(-max * step as f32 / 20.0)));
            });
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        editor.read_with(cx, |editor, _cx| {
            let run = editor.prev_mounted_run.expect("a run was mounted");
            bottom_reached = run.row_end >= 280;
        });
    }
    assert!(bottom_reached, "滚动应触达文档底部行");
}

/// 深嵌套渲染回归（主线程栈耗尽崩溃）。
///
/// 背景：Windows 主线程栈仅 1MB，深文档滚动时元素树层数多
/// （编辑器外壳 + 菜单包装 + 块外壳 + 内容）叠加 debug 胖帧，
/// 曾在滚动 `docs/render-sample.md` 时栈溢出崩溃（dump 定位：HTML 递归单帧约 170KB）。
/// 修复：HTML 递归改先收集 `Vec` 再挂载 + 各分支拆独立函数
/// （父分发帧只剩小局部量），仓库 `.cargo/config.toml` 另把主线程栈
/// 扩大到 4MB（与上游 rgpui 自身配置对齐）。
/// 本测试用 15 层 HTML + 12 层引用嵌套构造深树并滚动绘制全程，
/// 若单帧回退臃肿或层数失控会直接 abort，守住这条红线。
#[rgpui::test]
async fn deep_nesting_renders_within_main_thread_stack(cx: &mut TestAppContext) {
    use rgpui::{point, px};
    init_editor_test_app(cx);
    let mut markdown = String::new();
    for depth in 1..=12 {
        markdown.push_str(&"> ".repeat(depth));
        markdown.push_str(&format!("引用层 {depth}\n\n"));
    }
    markdown.push_str(&"<div>\n".repeat(15));
    markdown.push_str("深层正文\n");
    markdown.push_str(&"</div>\n".repeat(40));
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    redraw(cx);

    // 深树确实建立起来，否则后面的滚动绘制只是在测空文档。
    editor.read_with(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();
        assert!(
            visible.len() > 12,
            "nested quotes should produce a deep block tree, got {}",
            visible.len()
        );
        // 深树没有被层数上限截断：最里层的引用与 HTML 正文都还在文档里。
        let text = editor.document.markdown_text(cx);
        assert!(
            text.contains("引用层 12"),
            "deepest quote level missing from: {text}"
        );
        assert!(
            text.contains("深层正文"),
            "deep html body missing from: {text}"
        );
    });

    for step in 0..=10 {
        cx.update(|window, cx| {
            editor.update(cx, |editor, _cx| {
                let max = editor.scroll_handle.max_offset().y.as_f32().max(0.0);
                editor
                    .scroll_handle
                    .set_offset(point(px(0.0), px(-max * step as f32 / 10.0)));
            });
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
    }

    // 全程绘制后可观测状态：滚动偏移被推到最大且仍有挂载区间。
    editor.read_with(cx, |editor, _cx| {
        assert!(
            editor.scroll_handle.max_offset().y.as_f32() > 0.0,
            "deep document should scroll"
        );
        let run = editor
            .prev_mounted_run
            .expect("rows should stay mounted through the deep scroll");
        assert!(run.row_end > run.row_start, "run: {:?}", run);
    });
}
#[rgpui::test]
async fn about_menu_action_opens_dialog_on_active_editor(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    cx.update(|cx| {
        crate::theme::init(cx, "rumark-light");
        crate::app_menu::init(cx);
    });

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "hi".to_string(), None));
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();

    cx.update(|window, cx| {
        window.dispatch_action(Box::new(crate::components::ShowAbout), cx);
    });
    cx.run_until_parked();

    cx.update(|window, cx| {
        editor.read_with(cx, |editor, _| {
            assert!(
                !editor.show_unsaved_changes_dialog,
                "unsaved guard should not block about dialog"
            );
        });
        assert!(
            window.has_active_dialog(cx),
            "ShowAbout action should open the about dialog on the active editor window"
        );
    });
}

#[rgpui::test]
async fn about_dialog_opens_and_renders_layer(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "hi".to_string(), None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.open_about_dialog(window, cx);
        });
        assert!(
            window.has_active_dialog(cx),
            "about dialog should be active after open_about_dialog"
        );
        assert!(
            rgpui::Root::render_dialog_layer(window, cx).is_some(),
            "dialog layer should render when about dialog is open"
        );
    });
}

#[rgpui::test]
async fn about_github_link_uses_gpui_url_opening(cx: &mut TestAppContext) {
    cx.update(|cx| {
        super::render::open_about_github_url(cx);
    });

    assert_eq!(cx.opened_url(), Some(super::render::GITHUB_URL.to_string()));
}

#[rgpui::test]
async fn ctrl_s_saves_rendered_mode_edit_to_existing_file(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let path = temp_markdown_path("ctrl-s-rendered-save");
    fs::write(&path, "alpha").expect("write initial markdown");
    let cleanup_path = path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) = cx.add_window_view({
        let path = path.clone();
        move |_window, cx| Editor::from_markdown(cx, "alpha".to_string(), Some(path))
    });

    cx.simulate_input("!");
    redraw(cx);
    let expected = editor.read_with(cx, |editor, cx| {
        assert!(editor.document_dirty);
        assert!(!editor.pending_save);
        editor.current_document_source(cx)
    });
    assert_ne!(expected, "alpha");

    cx.simulate_keystrokes("ctrl-s");
    redraw(cx);

    assert_eq!(
        fs::read_to_string(&path).expect("read saved markdown"),
        expected
    );
    editor.read_with(cx, |editor, _cx| {
        assert!(!editor.document_dirty);
        assert!(!editor.pending_save);
    });
}

#[rgpui::test]
async fn saving_existing_document_replaces_file_without_leaving_temp_file(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    // 单独建目录：临时文件与目标同目录，目录里只剩预期文件才说明没有残留。
    let root = std::env::temp_dir().join(format!("rumark-atomic-save-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create temp dir");
    let path = root.join("doc.md");
    fs::write(&path, "alpha").expect("write initial markdown");
    // 硬链接指向旧文件对象：原子写是「同目录临时文件 + 重命名」，整块替换目录项，
    // 硬链接仍读到旧内容；原地覆写会把硬链接内容一起改掉。
    let linked = root.join("doc.link.md");
    fs::hard_link(&path, &linked).expect("hard link to the saved file");
    let cleanup_root = root.clone();
    cx.on_quit(move || {
        let _ = fs::remove_dir_all(&cleanup_root);
    });

    let (editor, cx) = cx.add_window_view({
        let path = path.clone();
        move |_window, cx| Editor::from_markdown(cx, "alpha".to_string(), Some(path))
    });

    cx.simulate_input("!");
    redraw(cx);
    let expected = editor.read_with(cx, |editor, cx| editor.current_document_source(cx));

    cx.simulate_keystrokes("ctrl-s");
    redraw(cx);

    assert_eq!(
        fs::read_to_string(&path).expect("read saved markdown"),
        expected
    );
    assert_eq!(
        fs::read_to_string(&linked).expect("read linked markdown"),
        "alpha",
        "保存应整块替换文件对象，而不是原地覆写"
    );
    let entries: Vec<_> = fs::read_dir(&root)
        .expect("read temp dir")
        .map(|entry| entry.expect("dir entry").file_name())
        .collect();
    assert_eq!(entries.len(), 2, "unexpected leftovers: {entries:?}");
    assert!(
        entries.iter().any(|name| name == "doc.md")
            && entries.iter().any(|name| name == "doc.link.md"),
        "unexpected leftovers: {entries:?}"
    );
}

#[rgpui::test]
async fn window_save_action_saves_current_editor_without_global_menu_route(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);

    let path = temp_markdown_path("window-action-save");
    fs::write(&path, "alpha").expect("write initial markdown");
    let cleanup_path = path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) = cx.add_window_view({
        let path = path.clone();
        move |_window, cx| Editor::from_markdown(cx, "alpha".to_string(), Some(path))
    });

    cx.simulate_input(" action");
    redraw(cx);
    let expected = editor.read_with(cx, |editor, cx| {
        assert!(editor.document_dirty);
        editor.current_document_source(cx)
    });
    assert_ne!(expected, "alpha");

    cx.dispatch_action(SaveDocument);
    redraw(cx);

    assert_eq!(
        fs::read_to_string(&path).expect("read saved markdown"),
        expected
    );
    editor.read_with(cx, |editor, _cx| {
        assert!(!editor.document_dirty);
        assert!(!editor.pending_save);
    });
}

#[rgpui::test]
async fn dropped_markdown_replaces_clean_editor_in_current_window(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let dropped_path = temp_markdown_path("drop-clean-replace");
    fs::write(
        &dropped_path,
        "# Dropped\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n",
    )
    .expect("write dropped markdown");
    let cleanup_path = dropped_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "old".to_string(), None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert_eq!(editor.view_mode, ViewMode::Source);
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.request_dropped_markdown_replace(dropped_path.clone(), window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.file_path.as_ref(), Some(&dropped_path));
        assert_eq!(editor.view_mode, ViewMode::Rendered);
        assert!(!editor.document_dirty);
        assert!(!editor.show_drop_replace_dialog);
        assert_eq!(editor.document.root_count(), 2);
        assert_eq!(
            editor
                .document
                .root_blocks()
                .last()
                .expect("table block")
                .read(cx)
                .kind(),
            BlockKind::Table
        );
        assert!(
            editor.current_document_source(cx).contains("# Dropped"),
            "text: {:?}",
            editor.current_document_source(cx)
        );
    });
    assert_eq!(cx.cx.windows().len(), 1);
}

#[rgpui::test]
async fn dropped_paths_pick_first_valid_markdown_file(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let text_path = temp_export_path("drop-ignore-non-markdown", "txt");
    let markdown_path = temp_export_path("drop-pick-markdown", "markdown");
    fs::write(&text_path, "plain").expect("write text");
    fs::write(&markdown_path, "markdown").expect("write markdown");
    let cleanup_text = text_path.clone();
    let cleanup_markdown = markdown_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_text);
        let _ = fs::remove_file(&cleanup_markdown);
    });

    assert_eq!(
        Editor::first_dropped_markdown_path(&[text_path, markdown_path.clone()]),
        Some(markdown_path)
    );
}

#[rgpui::test]
async fn dirty_drop_waits_for_replace_decision_and_cancel_preserves_document(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);

    let dropped_path = temp_markdown_path("drop-dirty-cancel");
    fs::write(&dropped_path, "dropped").expect("write dropped markdown");
    let cleanup_path = dropped_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "current".to_string(), None));
    editor.update(cx, |editor, cx| editor.mark_dirty(cx));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.request_dropped_markdown_replace(dropped_path, window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert!(editor.document_dirty);
        assert!(editor.show_drop_replace_dialog);
        assert_eq!(editor.current_document_source(cx), "current");
        assert!(editor.pending_drop_replace_path.is_some());
    });

    editor.update(cx, |editor, cx| editor.cancel_drop_replace_dialog(cx));

    editor.read_with(cx, |editor, cx| {
        assert!(editor.document_dirty);
        assert!(!editor.show_drop_replace_dialog);
        assert!(editor.pending_drop_replace_path.is_none());
        assert_eq!(editor.current_document_source(cx), "current");
    });
}

#[rgpui::test]
async fn unsaved_changes_dialog_tracks_framework_layer(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "dirty".to_string(), None));
    editor.update(cx, |editor, cx| editor.mark_dirty(cx));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            assert!(!editor.on_window_should_close(window, cx));
            assert!(editor.show_unsaved_changes_dialog);
        });
        assert!(window.has_active_dialog(cx));
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.on_cancel_close_dialog(&ClickEvent::default(), window, cx);
            assert!(!editor.show_unsaved_changes_dialog);
        });
        assert!(!window.has_active_dialog(cx));
    });
}

#[rgpui::test]
async fn dialog_button_click_routes_through_captured_entity(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "dirty".to_string(), None));

    // 用生产页脚同款链路（构建闭包捕获实体 + 直接 update）在对话框内容区放一个
    // 占满宽度的按钮：几何完全由固定像素决定，可算出中心点做真点击。
    // 背景：点击发生在事件派发中，此时窗口正被 `take`，
    // `downcast` + `WindowHandle::update` 会静默失败（见 `dialog_button_click`）。
    const DIALOG_WIDTH: f32 = 200.0;
    const BUTTON_HEIGHT: f32 = 32.0;
    cx.update(|window, cx| {
        let editor_handle = editor.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .width(rgpui::px(DIALOG_WIDTH))
                .keyboard(false)
                .overlay_closable(false)
                .close_button(false)
                .content({
                    let editor_handle = editor_handle.clone();
                    move |content, _, _| {
                        content.child(
                            rgpui::div().w_full().child(
                                rgpui::Button::new("probe-button")
                                    .w_full()
                                    .h(rgpui::px(BUTTON_HEIGHT))
                                    .label("probe")
                                    .on_click(Editor::dialog_button_click(
                                        editor_handle.clone(),
                                        Editor::on_cancel_close_dialog,
                                    )),
                            ),
                        )
                    }
                })
        });
        let _ = window.draw(cx);
    });
    // 入场动画走真实时钟，测试里播不完会导致按钮不在终态位置；
    // 开省流动效后动画首帧即定格终态（见 `reduce_motion`）。
    cx.update(|_, cx| cx.set_reduce_motion(true));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));

    // 对话框盒：上内边距16 + 内容区按钮32；按钮占满宽，
    // 中心横坐标即视口中点，纵坐标按固定像素累加。
    let viewport = cx.update(|window, _cx| window.viewport_size());
    let center_x = f32::from(viewport.width) / 2.0;
    let dialog_top = f32::from(viewport.height) / 10.0;
    let button_center_y = dialog_top + 16.0 + BUTTON_HEIGHT / 2.0;
    cx.simulate_click(
        rgpui::point(rgpui::px(center_x), rgpui::px(button_center_y)),
        rgpui::Modifiers::none(),
    );

    // 取消处理器应跑通并关闭对话框。
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    editor.read_with(cx, |editor, _cx| {
        assert!(!editor.show_unsaved_changes_dialog);
    });
}

#[rgpui::test]
async fn drop_replace_dialog_tracks_framework_layer(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let dropped_path = temp_markdown_path("drop-layer-track");
    fs::write(&dropped_path, "dropped").expect("write dropped markdown");
    let cleanup_path = dropped_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "current".to_string(), None));
    editor.update(cx, |editor, cx| editor.mark_dirty(cx));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.request_dropped_markdown_replace(dropped_path, window, cx);
            assert!(editor.show_drop_replace_dialog);
        });
        assert!(window.has_active_dialog(cx));
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.on_cancel_drop_replace_dialog(&ClickEvent::default(), window, cx);
            assert!(!editor.show_drop_replace_dialog);
        });
        assert!(!window.has_active_dialog(cx));
    });
}

#[rgpui::test]
async fn dirty_drop_can_replace_without_saving(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let dropped_path = temp_markdown_path("drop-dirty-discard");
    fs::write(&dropped_path, "dropped").expect("write dropped markdown");
    let cleanup_path = dropped_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_path);
    });

    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "current".to_string(), None));
    editor.update(cx, |editor, cx| editor.mark_dirty(cx));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.request_dropped_markdown_replace(dropped_path.clone(), window, cx);
            editor.discard_pending_drop_replace(window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.file_path.as_ref(), Some(&dropped_path));
        assert_eq!(editor.current_document_source(cx), "dropped");
        assert!(!editor.document_dirty);
        assert!(!editor.show_drop_replace_dialog);
    });
}

#[rgpui::test]
async fn dirty_drop_saves_existing_document_before_replace(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let current_path = temp_markdown_path("drop-save-current");
    let dropped_path = temp_markdown_path("drop-save-replace");
    fs::write(&current_path, "original").expect("write current markdown");
    fs::write(&dropped_path, "dropped").expect("write dropped markdown");
    let cleanup_current = current_path.clone();
    let cleanup_dropped = dropped_path.clone();
    cx.on_quit(move || {
        let _ = fs::remove_file(&cleanup_current);
        let _ = fs::remove_file(&cleanup_dropped);
    });

    let (editor, cx) = cx.add_window_view({
        let current_path = current_path.clone();
        move |_window, cx| Editor::from_markdown(cx, "original".to_string(), Some(current_path))
    });

    editor.update(cx, |editor, cx| {
        let first = editor.document.first_root().expect("current root").clone();
        first.update(cx, |block, _cx| {
            block
                .record
                .set_title(InlineTextTree::plain("edited".to_string()));
            block.refresh_render_cache();
        });
        editor.mark_dirty(cx);
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.request_dropped_markdown_replace(dropped_path.clone(), window, cx);
            editor.save_and_replace_pending_drop(window, cx);
        });
    });
    redraw(cx);

    assert_eq!(
        fs::read_to_string(&current_path).expect("read saved current"),
        "edited"
    );
    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.file_path.as_ref(), Some(&dropped_path));
        assert_eq!(editor.current_document_source(cx), "dropped");
        assert!(!editor.document_dirty);
        assert!(!editor.pending_drop_replace_after_save);
    });
}

#[rgpui::test]
async fn close_window_menu_action_closes_only_active_editor_window(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (_first_editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "first".to_string(), None));
    let first_window = activate_visual_window(cx);

    let (_second_editor, cx) = cx
        .cx
        .add_window_view(|_window, cx| Editor::from_markdown(cx, "second".to_string(), None));
    let second_window = activate_visual_window(cx);

    assert_ne!(first_window.window_id(), second_window.window_id());
    assert_eq!(cx.cx.windows().len(), 2);

    cx.cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&CloseWindow, cx);
    });
    cx.run_until_parked();

    let remaining = cx.cx.windows();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].window_id(), first_window.window_id());
    assert_ne!(remaining[0].window_id(), second_window.window_id());
}

#[rgpui::test]
async fn app_menu_opened_windows_activate_and_close_independently(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let first_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "first".to_string(), None));
    cx.run_until_parked();
    let second_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "second".to_string(), None));
    cx.run_until_parked();

    let active_window = cx.update(|cx| cx.active_window().expect("window should be active"));
    assert_eq!(active_window.window_id(), second_window.window_id());
    assert_ne!(first_window.window_id(), second_window.window_id());
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);

    assert!(
        second_window
            .update(cx, |editor, _window, _cx| editor.close_guard_installed)
            .expect("second editor window should be open")
    );

    cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&CloseWindow, cx);
    });
    cx.run_until_parked();

    let remaining = cx.update(|cx| cx.windows());
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].window_id(), first_window.window_id());

    cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&CloseWindow, cx);
    });
    cx.run_until_parked();

    assert!(cx.update(|cx| cx.windows().is_empty()));
}

#[rgpui::test]
async fn app_menu_opened_file_window_reinstalls_close_guard_after_registration(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);

    let opened_path = temp_markdown_path("app-menu-opened-file-window-close");
    fs::write(&opened_path, "opened from file").expect("write opened markdown");

    let first_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "first".to_string(), None));
    cx.run_until_parked();
    let second_window = cx.update(|cx| {
        crate::app_menu::open_editor_window(
            cx,
            fs::read_to_string(&opened_path).expect("read opened markdown"),
            Some(opened_path.clone()),
        )
    });
    cx.run_until_parked();

    let active_window = cx.update(|cx| cx.active_window().expect("window should be active"));
    assert_eq!(active_window.window_id(), second_window.window_id());
    assert_ne!(first_window.window_id(), second_window.window_id());

    second_window
        .update(cx, |editor, window, cx| {
            assert!(editor.close_guard_installed);
            assert!(editor.on_window_should_close(window, cx));
        })
        .expect("second editor window should be open");

    cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&CloseWindow, cx);
    });
    cx.run_until_parked();

    let remaining = cx.update(|cx| cx.windows());
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].window_id(), first_window.window_id());
    assert_ne!(remaining[0].window_id(), second_window.window_id());

    let _ = fs::remove_file(opened_path);
}

#[rgpui::test]
async fn app_menu_opened_dirty_file_window_prompts_only_that_window(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let opened_path = temp_markdown_path("app-menu-opened-dirty-file-window-close");
    fs::write(&opened_path, "opened from file").expect("write opened markdown");

    let first_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "first".to_string(), None));
    let second_window = cx.update(|cx| {
        crate::app_menu::open_editor_window(
            cx,
            fs::read_to_string(&opened_path).expect("read opened markdown"),
            Some(opened_path.clone()),
        )
    });
    cx.run_until_parked();

    second_window
        .update(cx, |editor, window, cx| {
            editor.mark_dirty(cx);
            assert!(!editor.on_window_should_close(window, cx));
        })
        .expect("second editor window should be open");

    first_window
        .update(cx, |editor, _window, _cx| {
            assert!(!editor.show_unsaved_changes_dialog);
        })
        .expect("first editor window should be open");
    second_window
        .update(cx, |editor, _window, _cx| {
            assert!(editor.show_unsaved_changes_dialog);
        })
        .expect("second editor window should be open");

    let _ = fs::remove_file(opened_path);
}

#[rgpui::test]
async fn app_menu_opened_dirty_window_close_guard_prompts_only_that_window(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);

    let first_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "first".to_string(), None));
    let second_window =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "second".to_string(), None));
    cx.run_until_parked();

    second_window
        .update(cx, |editor, window, cx| {
            editor.mark_dirty(cx);
            assert!(!editor.on_window_should_close(window, cx));
        })
        .expect("second editor window should be open");

    first_window
        .update(cx, |editor, _window, _cx| {
            assert!(!editor.show_unsaved_changes_dialog);
        })
        .expect("first editor window should be open");
    second_window
        .update(cx, |editor, _window, _cx| {
            assert!(editor.show_unsaved_changes_dialog);
        })
        .expect("second editor window should be open");
}

#[test]
fn blank_untitled_editor_is_reusable_for_open_file() {
    let mut cx = TestAppContext::single();
    init_editor_test_app(&mut cx);

    // 刚打开软件的全新空白窗口：可复用。
    let blank = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));
    cx.update(|cx| {
        assert!(blank.read(cx).is_blank_document(cx));
    });

    // 已关联文件：不复用（应新建窗口）。
    let opened =
        cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), Some(PathBuf::from("a.md"))));
    cx.update(|cx| {
        assert!(!opened.read(cx).is_blank_document(cx));
    });

    // 无路径但已有内容：不复用。
    let typed = cx.new(|cx| Editor::from_markdown(cx, "hello".to_string(), None));
    cx.update(|cx| {
        assert!(!typed.read(cx).is_blank_document(cx));
    });

    // 无路径但已被标记有未保存修改：不复用。
    let dirty = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));
    dirty.update(&mut cx, |editor, cx| editor.mark_dirty(cx));
    cx.update(|cx| {
        assert!(!dirty.read(cx).is_blank_document(cx));
    });

    cx.quit();
}

#[rgpui::test]
async fn open_file_reuses_active_blank_window_instead_of_opening_another(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let opened_path = temp_markdown_path("open-file-reuses-blank-window");
    fs::write(&opened_path, "opened content").expect("write markdown");

    // 刚打开软件：一个空白窗口。
    let window = cx.update(|cx| crate::app_menu::open_editor_window(cx, String::new(), None));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);

    cx.update(|cx| {
        crate::app_menu::open_file_preferring_blank_window(
            cx,
            opened_path.clone(),
            cx.active_window(),
        );
    });
    cx.run_until_parked();

    // 不应新建窗口：内容直接载入原空白窗口。
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    window
        .update(cx, |editor, _window, cx| {
            assert_eq!(editor.file_path.as_deref(), Some(opened_path.as_path()));
            assert_eq!(editor.current_document_source(cx), "opened content");
            assert!(!editor.is_blank_document(cx));
        })
        .expect("window should stay open");

    // 载入会把文件记入最近列表，测试结束后清理，避免污染真实配置。
    let _ = crate::config::remove_recent_file(&opened_path);
    let _ = fs::remove_file(opened_path);
}

#[rgpui::test]
async fn open_file_opens_new_window_when_active_window_has_content(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let opened_path = temp_markdown_path("open-file-new-window-with-content");
    fs::write(&opened_path, "opened content").expect("write markdown");

    // 活动窗口已有内容：打开文件应新建窗口，而不是覆盖当前窗口。
    let first =
        cx.update(|cx| crate::app_menu::open_editor_window(cx, "已有内容".to_string(), None));
    cx.run_until_parked();

    cx.update(|cx| {
        crate::app_menu::open_file_preferring_blank_window(
            cx,
            opened_path.clone(),
            cx.active_window(),
        );
    });
    cx.run_until_parked();

    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    first
        .update(cx, |editor, _window, cx| {
            assert_eq!(editor.current_document_source(cx), "已有内容");
            assert!(editor.file_path.is_none());
        })
        .expect("first window should stay open");

    let _ = crate::config::remove_recent_file(&opened_path);
    let _ = fs::remove_file(opened_path);
}

#[rgpui::test]
async fn quit_application_allows_clean_editor_windows_to_quit(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (first_editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "first".to_string(), None));
    let _first_window = activate_visual_window(cx);

    let (second_editor, cx) = cx
        .cx
        .add_window_view(|_window, cx| Editor::from_markdown(cx, "second".to_string(), None));
    let _second_window = activate_visual_window(cx);

    assert_eq!(cx.cx.windows().len(), 2);

    cx.cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&QuitApplication, cx);
    });
    cx.run_until_parked();

    first_editor.read_with(cx, |editor, _cx| {
        assert!(!editor.show_unsaved_changes_dialog);
    });
    second_editor.read_with(cx, |editor, _cx| {
        assert!(!editor.show_unsaved_changes_dialog);
    });
}

#[rgpui::test]
async fn quit_application_prompts_dirty_editor_without_quitting(cx: &mut TestAppContext) {
    init_editor_test_app(cx);

    let (first_editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "first".to_string(), None));
    let first_window = activate_visual_window(cx);

    let (second_editor, cx) = cx
        .cx
        .add_window_view(|_window, cx| Editor::from_markdown(cx, "second".to_string(), None));
    let second_window = activate_visual_window(cx);

    second_editor.update(cx, |editor, cx| editor.mark_dirty(cx));
    assert_eq!(cx.cx.windows().len(), 2);

    cx.cx.update(|cx| {
        crate::app_menu::dispatch_menu_action(&QuitApplication, cx);
    });
    cx.run_until_parked();

    let open_windows = cx.cx.windows();
    assert_eq!(open_windows.len(), 2);
    assert!(
        open_windows
            .iter()
            .any(|window| window.window_id() == first_window.window_id())
    );
    assert!(
        open_windows
            .iter()
            .any(|window| window.window_id() == second_window.window_id())
    );
    first_editor.read_with(cx, |editor, _cx| {
        assert!(!editor.show_unsaved_changes_dialog);
    });
    second_editor.read_with(cx, |editor, _cx| {
        assert!(editor.show_unsaved_changes_dialog);
    });
}

#[rgpui::test]
async fn quit_application_via_action_dispatch_prompts_dirty_editor(cx: &mut TestAppContext) {
    // 回归：退出动作走真实分发路径时，活动窗口在分发期间被占用，旧实现里
    // `request_quit_application` 的同步 `window.update` 必失败，脏窗口不会弹出
    // 未保存确认框（菜单退出/关闭静默失效）。修复后应改为延后更新。
    init_editor_test_app(cx);

    let (_first_editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "first".to_string(), None));
    let _first_window = activate_visual_window(cx);

    let (second_editor, cx) = cx
        .cx
        .add_window_view(|_window, cx| Editor::from_markdown(cx, "second".to_string(), None));
    let _second_window = activate_visual_window(cx);

    second_editor.update(cx, |editor, cx| editor.mark_dirty(cx));
    assert_eq!(cx.cx.windows().len(), 2);

    cx.dispatch_action(QuitApplication);
    cx.run_until_parked();

    // 脏窗口被拦截，未退出；且该窗口的未保存确认框已弹出。
    assert_eq!(cx.cx.windows().len(), 2);
    second_editor.read_with(cx, |editor, _cx| {
        assert!(editor.show_unsaved_changes_dialog);
    });
}

#[rgpui::test]
async fn window_close_action_closes_current_editor_before_global_menu_route(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);

    let (_first_editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "first".to_string(), None));
    let first_window = activate_visual_window(cx);

    let (_second_editor, cx) = cx
        .cx
        .add_window_view(|_window, cx| Editor::from_markdown(cx, "second".to_string(), None));
    let second_window = activate_visual_window(cx);

    cx.dispatch_action(CloseWindow);
    cx.run_until_parked();

    let remaining = cx.cx.windows();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].window_id(), first_window.window_id());
    assert_ne!(remaining[0].window_id(), second_window.window_id());
}

#[rgpui::test]
async fn starting_and_ending_scrollbar_drag_updates_editor_state(cx: &mut TestAppContext) {
    let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    editor.update(cx, |editor, cx| {
        editor.pending_scroll_active_block_into_view = true;
        editor.pending_scroll_recheck_after_layout = true;

        editor.start_scrollbar_drag(12.0, 320.0, 64.0, 500.0, cx);
        assert_eq!(
            editor.scrollbar_drag,
            Some(super::ScrollbarDrag {
                pointer_offset_y: 12.0,
                track_height: 320.0,
                thumb_height: 64.0,
                max_scroll_y: 500.0,
            })
        );
        assert!(!editor.pending_scroll_active_block_into_view);
        assert!(!editor.pending_scroll_recheck_after_layout);

        editor.update_scrollbar_drag(172.0, cx);
        let offset_y = -f32::from(editor.scroll_handle.offset().y);
        assert!(offset_y > 0.0);

        editor.end_scrollbar_drag(cx);
        assert!(editor.scrollbar_drag.is_none());
    });
}

#[rgpui::test]
async fn forgiving_entity_update_applies_and_reports_release(cx: &mut TestAppContext) {
    let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha".to_string(), None));
    let weak = editor.downgrade();
    let mut async_cx = cx.to_async();

    // 实体存活时正常应用。
    let applied = update_entity_forgiving(&weak, &mut async_cx, |_, _| 42);
    assert!(matches!(applied, Some(Ok(42))));

    // 实体释放后上报 `Err` 而不是崩溃（调用方据此结束循环任务）。
    drop(editor);
    let gone = update_entity_forgiving(&weak, &mut async_cx, |_, _| 42);
    assert!(matches!(gone, Some(Err(_))));
}

#[rgpui::test]
async fn parsed_table_runtime_installs_column_alignment_on_cells(cx: &mut TestAppContext) {
    let markdown = [
        "| Left | Center | Right |",
        "| :--- | :---: | ---: |",
        "| a | b | c |",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        assert_eq!(table.read(cx).kind(), BlockKind::Table);
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime");
        assert_eq!(
            runtime.header[0].read(cx).table_cell_alignment(),
            Some(TableColumnAlignment::Left)
        );
        assert_eq!(
            runtime.header[1].read(cx).table_cell_alignment(),
            Some(TableColumnAlignment::Center)
        );
        assert_eq!(
            runtime.rows[0][2].read(cx).table_cell_alignment(),
            Some(TableColumnAlignment::Right)
        );
    });
}

#[rgpui::test]
async fn append_column_updates_table_and_focuses_new_header_cell(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | ---: |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.append_table_column(&table, cx);

        let record = table
            .read(cx)
            .record
            .table
            .as_ref()
            .expect("table record after append");
        assert_eq!(record.header.len(), 3);
        assert_eq!(record.rows[0].len(), 3);
        assert_eq!(
            record.alignments,
            vec![
                TableColumnAlignment::Default,
                TableColumnAlignment::Right,
                TableColumnAlignment::Right,
            ]
        );

        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("rebuilt runtime");
        let focused = runtime.header[2].entity_id();
        assert_eq!(editor.pending_focus, Some(focused));
    });
}

#[rgpui::test]
async fn append_row_updates_table_and_focuses_first_cell_of_new_row(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | :---: |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.append_table_row(&table, cx);

        let record = table
            .read(cx)
            .record
            .table
            .as_ref()
            .expect("table record after append");
        assert_eq!(record.rows.len(), 2);
        assert_eq!(record.rows[1].len(), 2);
        assert!(
            record.rows[1]
                .iter()
                .all(|cell| cell.serialize_markdown().is_empty())
        );

        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("rebuilt runtime");
        let focused = runtime.rows[1][0].entity_id();
        assert_eq!(editor.pending_focus, Some(focused));
    });
}

#[rgpui::test]
async fn table_insert_row_below_action_inserts_at_focused_row(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let header_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header[0]
                .entity_id();
            editor.active_entity_id = Some(header_cell);
            editor.on_table_insert_row_below_action(&TableInsertRowBelow, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after insert");
            // 表头下方插入一空行，原正文行下移。
            assert_eq!(record.rows.len(), 2);
            assert!(record.rows[0][0].serialize_markdown().is_empty());
            assert_eq!(record.rows[1][0].serialize_markdown(), "1");
        });
    });
}

#[rgpui::test]
async fn table_delete_row_action_removes_focused_body_row(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A |", "| --- |", "| 1 |", "| 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let body_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows[0][0]
                .clone();
            // 真实聚焦正文单元格：首帧自动聚焦的是表头，必须显式转移，
            // 否则动作会作用到表头而非本单测的目标行。
            let handle = body_cell.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
            editor.active_entity_id = Some(body_cell.entity_id());
            editor.on_table_delete_row_action(&TableDeleteRow, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after delete");
            assert_eq!(record.rows.len(), 1);
            assert_eq!(record.rows[0][0].serialize_markdown(), "2");
        });
    });
}

#[rgpui::test]
async fn table_insert_and_delete_column_actions_update_shape(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let header_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header[0]
                .entity_id();
            editor.active_entity_id = Some(header_cell);
            editor.on_table_insert_column_right_action(&TableInsertColumnRight, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after column insert");
            assert_eq!(record.header.len(), 3);
            assert!(record.header[1].serialize_markdown().is_empty());
            assert_eq!(record.header[2].serialize_markdown(), "B");

            // 新列单元格成为焦点，删列回到两列。
            let new_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("rebuilt runtime")
                .header[1]
                .entity_id();
            editor.active_entity_id = Some(new_cell);
            editor.on_table_delete_column_action(&TableDeleteColumn, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after column delete");
            assert_eq!(record.header.len(), 2);
        });
    });
}

#[rgpui::test]
async fn table_move_row_down_action_swaps_focused_row_content(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A |", "| --- |", "| 1 |", "| 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let body_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows[0][0]
                .clone();
            // 真实聚焦正文单元格：首帧自动聚焦的是表头，必须显式转移。
            let handle = body_cell.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
            editor.active_entity_id = Some(body_cell.entity_id());
            editor.on_table_move_row_down_action(&TableMoveRowDown, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after move");
            assert_eq!(record.rows[0][0].serialize_markdown(), "2");
            assert_eq!(record.rows[1][0].serialize_markdown(), "1");
        });
    });
}

#[rgpui::test]
async fn table_move_column_right_action_swaps_focused_column(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let header_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .header[0]
                .entity_id();
            editor.active_entity_id = Some(header_cell);
            editor.on_table_move_column_right_action(&TableMoveColumnRight, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after move");
            assert_eq!(record.header[0].serialize_markdown(), "B");
            assert_eq!(record.header[1].serialize_markdown(), "A");
            assert_eq!(record.rows[0][0].serialize_markdown(), "2");
            assert_eq!(record.rows[0][1].serialize_markdown(), "1");
        });
    });
}

#[rgpui::test]
async fn table_align_column_center_action_updates_alignment(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let table = editor.document.first_root().expect("table root").clone();
            let body_cell = table
                .read(cx)
                .table_runtime
                .as_ref()
                .expect("table runtime")
                .rows[0][1]
                .clone();
            let handle = body_cell.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
            editor.active_entity_id = Some(body_cell.entity_id());
            editor.on_table_align_column_center_action(&TableAlignColumnCenter, window, cx);

            let record = table
                .read(cx)
                .record
                .table
                .as_ref()
                .expect("table record after align");
            assert_eq!(
                record.alignments,
                vec![TableColumnAlignment::Default, TableColumnAlignment::Center]
            );
        });
    });
}

#[rgpui::test]
async fn setting_column_alignment_updates_record_and_selection(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.set_table_column_alignment(&table, 1, TableColumnAlignment::Right, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(
            record.alignments,
            vec![TableColumnAlignment::Default, TableColumnAlignment::Right]
        );
        assert_eq!(
            editor.table_axis_selection,
            Some(super::AxisSelection {
                table_block_id: table.entity_id(),
                kind: crate::components::TableAxisKind::Column,
                index: 1,
            })
        );
    });
}

#[rgpui::test]
async fn moving_table_row_updates_focus_and_selection(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "| 3 | 4 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        // Visual row 2 is the second body row; move it up above the first.
        editor.move_table_row(&table, 2, -1, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(record.rows[0][0].serialize_markdown(), "3");
        assert_eq!(
            editor.table_axis_selection,
            Some(super::AxisSelection {
                table_block_id: table.entity_id(),
                kind: crate::components::TableAxisKind::Row,
                index: 1,
            })
        );

        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("rebuilt runtime");
        assert_eq!(editor.pending_focus, Some(runtime.rows[0][0].entity_id()));
    });
}

#[rgpui::test]
async fn moving_first_body_row_up_swaps_with_header(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "| 3 | 4 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        // Visual row 1 (first body row) moves up into the header position.
        editor.move_table_row(&table, 1, -1, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(record.header[0].serialize_markdown(), "1");
        assert_eq!(record.rows[0][0].serialize_markdown(), "A");
        assert_eq!(
            editor.table_axis_selection,
            Some(super::AxisSelection {
                table_block_id: table.entity_id(),
                kind: crate::components::TableAxisKind::Row,
                index: 0,
            })
        );
    });
}

#[rgpui::test]
async fn moving_header_row_down_swaps_with_first_body(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "| 3 | 4 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        // Visual row 0 (header) moves down, swapping with the first body row.
        editor.move_table_row(&table, 0, 1, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(record.header[0].serialize_markdown(), "1");
        assert_eq!(record.rows[0][0].serialize_markdown(), "A");
        assert_eq!(
            editor.table_axis_selection,
            Some(super::AxisSelection {
                table_block_id: table.entity_id(),
                kind: crate::components::TableAxisKind::Row,
                index: 1,
            })
        );
    });
}

#[rgpui::test]
async fn selecting_first_body_row_does_not_highlight_header(cx: &mut TestAppContext) {
    use crate::components::{TableAxisHighlight, TableAxisKind};
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |", "| 3 | 4 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        // Visual row 1 is the first body row; the header (row 0) must stay clear.
        editor.select_table_axis(table.entity_id(), TableAxisKind::Row, 1, cx);

        let runtime = table.read(cx).table_runtime.clone().expect("runtime");
        for cell in &runtime.header {
            assert_eq!(
                cell.read(cx).table_axis_highlight,
                TableAxisHighlight::None,
                "header should not be highlighted"
            );
        }
        for cell in &runtime.rows[0] {
            assert_eq!(
                cell.read(cx).table_axis_highlight,
                TableAxisHighlight::Selected
            );
        }
        for cell in &runtime.rows[1] {
            assert_eq!(cell.read(cx).table_axis_highlight, TableAxisHighlight::None);
        }
    });
}

#[rgpui::test]
async fn selecting_header_row_highlights_only_header(cx: &mut TestAppContext) {
    use crate::components::{TableAxisHighlight, TableAxisKind};
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.select_table_axis(table.entity_id(), TableAxisKind::Row, 0, cx);

        let runtime = table.read(cx).table_runtime.clone().expect("runtime");
        for cell in &runtime.header {
            assert_eq!(
                cell.read(cx).table_axis_highlight,
                TableAxisHighlight::Selected
            );
        }
        for cell in &runtime.rows[0] {
            assert_eq!(cell.read(cx).table_axis_highlight, TableAxisHighlight::None);
        }
    });
}

#[rgpui::test]
async fn body_row_preview_survives_stale_header_leave(cx: &mut TestAppContext) {
    use crate::components::TableAxisKind;
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let id = table.entity_id();

        // Pointer crosses from the header handle down onto the first body row.
        // The body handle's enter arrives first, then the header handle's leave;
        // the stale leave must not clear the preview the pointer moved onto.
        editor.preview_table_axis(id, TableAxisKind::Row, 1, true, cx);
        editor.preview_table_axis(id, TableAxisKind::Row, 0, false, cx);
        assert_eq!(
            editor.table_axis_preview,
            Some(super::AxisSelection {
                table_block_id: id,
                kind: TableAxisKind::Row,
                index: 1,
            }),
            "body row preview must survive the header's stale leave"
        );

        // Leaving the body handle that owns the preview still clears it.
        editor.preview_table_axis(id, TableAxisKind::Row, 1, false, cx);
        assert_eq!(editor.table_axis_preview, None);
    });
}

#[rgpui::test]
async fn deleting_table_column_moves_selection_to_nearest_survivor(cx: &mut TestAppContext) {
    let markdown = ["| A | B | C |", "| --- | --- | --- |", "| 1 | 2 | 3 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.delete_table_column(&table, 2, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(record.header.len(), 2);
        assert_eq!(
            editor.table_axis_selection,
            Some(super::AxisSelection {
                table_block_id: table.entity_id(),
                kind: crate::components::TableAxisKind::Column,
                index: 1,
            })
        );
    });
}

#[rgpui::test]
async fn deleting_table_header_promotes_next_row(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.delete_table_header_row(&table, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert_eq!(record.header[0].serialize_markdown(), "1");
        assert_eq!(record.header[1].serialize_markdown(), "2");
        assert!(record.rows.is_empty());

        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("rebuilt runtime");
        assert_eq!(editor.pending_focus, Some(runtime.header[0].entity_id()));
    });
}

#[rgpui::test]
async fn deleting_last_body_row_leaves_header_only_table(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        // Deleting the only body row used to be blocked; now it leaves a
        // header-only table behind.
        editor.delete_table_row(&table, 0, cx);

        let record = table.read(cx).record.table.as_ref().expect("table record");
        assert!(record.rows.is_empty());
        assert_eq!(record.header[0].serialize_markdown(), "A");
        assert_eq!(editor.document.root_count(), 1);
        assert_eq!(table.read(cx).kind(), BlockKind::Table);
    });
}

#[rgpui::test]
async fn removing_table_block_replaces_it_with_empty_paragraph(cx: &mut TestAppContext) {
    let markdown = [
        "intro",
        "",
        "| A | B |",
        "| --- | --- |",
        "| 1 | 2 |",
        "",
        "outro",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.root_blocks()[1].clone();
        assert_eq!(table.read(cx).kind(), BlockKind::Table);
        editor.remove_table_block(&table, cx);

        let roots = editor.document.root_blocks();
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0].read(cx).rendered_text(), "intro");
        assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
        assert_eq!(roots[1].read(cx).rendered_text(), "");
        assert_eq!(roots[2].read(cx).rendered_text(), "outro");
        assert_eq!(editor.pending_focus, Some(roots[1].entity_id()));
    });
}

#[rgpui::test]
async fn removing_the_only_table_leaves_one_empty_paragraph(cx: &mut TestAppContext) {
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        editor.remove_table_block(&table, cx);

        let roots = editor.document.root_blocks();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].read(cx).kind(), BlockKind::Paragraph);
        assert_eq!(roots[0].read(cx).rendered_text(), "");
    });
}

#[rgpui::test]
async fn standalone_root_image_installs_runtime_and_resolves_relative_path(
    cx: &mut TestAppContext,
) {
    let markdown = "![diagram](./assets/diagram.png \"System diagram\")".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.title.as_deref(), Some("System diagram"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn standalone_root_image_with_underscores_installs_runtime(cx: &mut TestAppContext) {
    let markdown =
        "![1.1_进制转换例子](./NetworkEngineerSummer.assets/1.1_进制转换例子.jpg)".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "1.1_进制转换例子");
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("NetworkEngineerSummer.assets/1.1_进制转换例子.jpg")
            )
        );
        assert_eq!(editor.document.markdown_text(cx), markdown);
    });
}

#[rgpui::test]
async fn indented_root_images_install_runtime_before_indented_code(cx: &mut TestAppContext) {
    let url1 = "https://gitee.com/jikeyang/typera_picgo/raw/master/sias/202508201435626.png";
    let url2 = "https://gitee.com/jikeyang/typera_picgo/raw/master/sias/202508201438742.png";
    let url3 = "https://gitee.com/jikeyang/typera_picgo/raw/master/sias/202508201439288.png";
    let url4 = "https://gitee.com/jikeyang/typera_picgo/raw/master/sias/202508201419865.png";
    let markdown = [
        format!("![image-1]({})", url1.replace("_", "\\_")),
        String::new(),
        format!("   ![image-2]({})", url2.replace("_", "\\_")),
        String::new(),
        format!("        ![image-3]({})", url3.replace("_", "\\_")),
        String::new(),
        "   所有组或用户名均对**Anaconda安装目录**的权限设置为**完全控制**后，如下图所示："
            .to_string(),
        String::new(),
        format!("![image-4]({})", url4.replace("_", "\\_")),
        String::new(),
        "    plain indented code".to_string(),
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let roots = editor.document.root_blocks();
        let image_sources = roots
            .iter()
            .filter_map(|block| {
                block
                    .read(cx)
                    .image_runtime()
                    .map(|runtime| runtime.src.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(image_sources, vec![url1, url2, url3, url4]);
        assert!(
            roots
                .iter()
                .any(|block| matches!(block.read(cx).kind(), BlockKind::CodeBlock { .. }))
        );
    });
}

#[rgpui::test]
async fn mixed_text_does_not_activate_image_runtime(cx: &mut TestAppContext) {
    let markdown = "before ![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        assert!(block.read(cx).image_runtime().is_none());
    });
}

#[rgpui::test]
async fn reference_style_root_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown =
        "![reference image][ref-image]\n\n[ref-image]: ./assets/ref-image.png \"Caption\""
            .to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "reference image");
        assert_eq!(runtime.src, "./assets/ref-image.png");
        assert_eq!(runtime.title.as_deref(), Some("Caption"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/ref-image.png")
            )
        );
    });
}

#[rgpui::test]
async fn quote_child_standalone_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = ">     ![diagram](./assets/diagram.png \"Caption\")".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let quote = editor.document.first_root().expect("quote root").clone();
        let image_block = quote
            .read(cx)
            .children
            .first()
            .expect("quote image child")
            .clone();
        let runtime = image_block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Caption"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn bulleted_list_item_standalone_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = "-     ![diagram](./assets/diagram.png \"Caption\")".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Caption"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn html_fallback_before_image_does_not_swallow_standalone_image(cx: &mut TestAppContext) {
    let image_url = "https://gitee.com/jikeyang/typera_picgo/raw/master/sias/202508200941158.png";
    let markdown = format!(
        "<span style='color:blue;'>Anaconda下载地址</span>：https://mirrors.tuna.tsinghua.edu.cn/anaconda/archive/\n\n![image-20250820094109009]({image_url})"
    );
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.document.root_count(), 2);
        {
            let html = editor.document.root_blocks()[0].read(cx);
            assert_eq!(html.kind(), BlockKind::HtmlBlock);
            assert!(
                html.rendered_text()
                    .starts_with("<span style='color:blue;'>")
            );
            assert!(
                html.record
                    .html
                    .as_ref()
                    .is_some_and(|html| html.is_semantic())
            );
        }

        let image = editor.document.root_blocks()[1].read(cx);
        let runtime = image.image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "image-20250820094109009");
        assert_eq!(runtime.src, image_url);
        match &runtime.resolved_source {
            ImageResolvedSource::Remote(uri) => assert_eq!(uri.to_string(), image_url),
            other => panic!("expected remote image, got {other:?}"),
        }
    });
}

#[rgpui::test]
async fn unclosed_html_fallback_stops_before_standalone_image_without_blank(
    cx: &mut TestAppContext,
) {
    let image_url = "https://example.com/image.png";
    let markdown = format!("<span>unclosed html\n![image]({image_url})");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.document.root_count(), 2);
        assert_eq!(
            editor.document.root_blocks()[0].read(cx).kind(),
            BlockKind::RawMarkdown
        );
        let image = editor.document.root_blocks()[1].read(cx);
        let runtime = image.image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "image");
        assert_eq!(runtime.src, image_url);
    });
}

#[rgpui::test]
async fn numbered_list_item_standalone_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = "1. ![diagram](https://example.com/diagram.gif \"Caption\")".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.title.as_deref(), Some("Caption"));
        match &runtime.resolved_source {
            ImageResolvedSource::Remote(uri) => {
                assert_eq!(uri.to_string(), "https://example.com/diagram.gif");
            }
            other => panic!("expected remote source, got {other:?}"),
        }
    });
}

#[rgpui::test]
async fn task_list_item_reference_style_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = "- [ ] ![diagram][cover]\n\n[cover]: ./assets/diagram.png \"Cover\"".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("task list item root")
            .clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Cover"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn mixed_list_item_title_does_not_activate_image_runtime(cx: &mut TestAppContext) {
    let markdown = "- text ![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        assert!(block.read(cx).image_runtime().is_none());
    });
}

#[rgpui::test]
async fn list_child_reference_style_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = [
        "- item",
        "  ![diagram][cover]",
        "",
        "[cover]: ./assets/diagram.png \"Cover\"",
    ]
    .join("\n");
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let list_item = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        let image_block = list_item
            .read(cx)
            .children
            .first()
            .expect("list child image")
            .clone();
        let runtime = image_block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Cover"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn list_scoped_reference_definition_supports_list_item_image_runtime(
    cx: &mut TestAppContext,
) {
    let markdown = [
        "- ![diagram][cover]",
        "  [cover]: ./assets/diagram.png \"Cover\"",
    ]
    .join("\n");
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let list_item = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        let runtime = list_item.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Cover"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
        assert_eq!(
            list_item
                .read(cx)
                .children
                .first()
                .expect("reference definition child")
                .read(cx)
                .kind(),
            BlockKind::RawMarkdown
        );
    });
}

#[rgpui::test]
async fn quote_list_item_standalone_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = "> - ![diagram](./assets/diagram.png)".to_string();
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let quote = editor.document.first_root().expect("quote root").clone();
        let list_item = quote
            .read(cx)
            .children
            .first()
            .expect("quote list child")
            .clone();
        let runtime = list_item.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn callout_task_list_reference_style_image_uses_container_scoped_definition(
    cx: &mut TestAppContext,
) {
    let markdown = [
        "> [!NOTE]",
        "> - [ ] ![diagram][cover]",
        ">",
        "> [cover]: ./assets/diagram.png \"Cover\"",
    ]
    .join("\n");
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let callout = editor.document.first_root().expect("callout root").clone();
        let list_item = callout
            .read(cx)
            .children
            .first()
            .expect("callout list child")
            .clone();
        let runtime = list_item.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Cover"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn callout_list_child_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = [
        "> [!NOTE]",
        "> - item",
        ">   ![diagram](./assets/diagram.png)",
    ]
    .join("\n");
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let callout = editor.document.first_root().expect("callout root").clone();
        let list_item = callout
            .read(cx)
            .children
            .first()
            .expect("callout list child")
            .clone();
        let image_block = list_item
            .read(cx)
            .children
            .first()
            .expect("list child image")
            .clone();
        let runtime = image_block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn callout_child_reference_style_image_uses_container_scoped_definition(
    cx: &mut TestAppContext,
) {
    let markdown = [
        "> [!NOTE]",
        ">     ![diagram][anim]",
        ">",
        "> [anim]: ./assets/diagram.png \"Animated\"",
    ]
    .join("\n");
    let file_path = PathBuf::from("D:/workspace/docs/note.md");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, Some(file_path.clone())));

    editor.read_with(cx, |editor, cx| {
        let callout = editor.document.first_root().expect("callout root").clone();
        let image_block = callout
            .read(cx)
            .children
            .iter()
            .find(|child| {
                child.read(cx).kind() == BlockKind::Paragraph
                    && child.read(cx).image_runtime().is_some()
            })
            .expect("callout image child")
            .clone();
        let runtime = image_block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.alt, "diagram");
        assert_eq!(runtime.src, "./assets/diagram.png");
        assert_eq!(runtime.title.as_deref(), Some("Animated"));
        assert_eq!(
            runtime.resolved_source,
            ImageResolvedSource::Local(
                file_path
                    .parent()
                    .expect("file parent")
                    .join("assets/diagram.png")
            )
        );
    });
}

#[rgpui::test]
async fn table_cell_with_standalone_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = [
        "| Preview |",
        "| --- |",
        "|    ![diagram](https://example.com/diagram.gif \"Animated\") |",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime");
        let cell_runtime = runtime.rows[0][0]
            .read(cx)
            .image_runtime()
            .expect("cell image runtime");
        assert_eq!(cell_runtime.alt, "diagram");
        assert_eq!(cell_runtime.title.as_deref(), Some("Animated"));
        match &cell_runtime.resolved_source {
            ImageResolvedSource::Remote(uri) => {
                assert_eq!(uri.to_string(), "https://example.com/diagram.gif");
            }
            other => panic!("expected remote source, got {other:?}"),
        }
    });
}

#[rgpui::test]
async fn table_cell_with_mixed_inline_image_uses_inline_image_segments(cx: &mut TestAppContext) {
    let markdown = [
        "| Preview |",
        "| --- |",
        "| image ![alt](https://example.com/x.png) |",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime");
        let cell = runtime.rows[0][0].read(cx);
        assert!(cell.image_runtime().is_none());

        let segments = parse_table_cell_inline_images(&cell.record.title_to_markdown());
        assert_eq!(segments.len(), 2);
        assert_eq!(
            segments[0],
            TableCellInlineImageSegment::Text("image ".to_string())
        );
        assert!(matches!(
            &segments[1],
            TableCellInlineImageSegment::Image { syntax, .. }
                if syntax.alt == "alt"
                    && syntax
                        .resolve_target(&ImageReferenceDefinitions::default())
                        .is_some_and(|target| target.src == "https://example.com/x.png")
        ));
    });
}

#[rgpui::test]
async fn table_cell_with_reference_style_image_installs_runtime(cx: &mut TestAppContext) {
    let markdown = [
        "| Preview |",
        "| --- |",
        "| ![diagram][anim] |",
        "",
        "[anim]: https://example.com/diagram.gif \"Animated\"",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime");
        let cell_runtime = runtime.rows[0][0]
            .read(cx)
            .image_runtime()
            .expect("cell image runtime");
        assert_eq!(cell_runtime.alt, "diagram");
        assert_eq!(cell_runtime.title.as_deref(), Some("Animated"));
        match &cell_runtime.resolved_source {
            ImageResolvedSource::Remote(uri) => {
                assert_eq!(uri.to_string(), "https://example.com/diagram.gif");
            }
            other => panic!("expected remote source, got {other:?}"),
        }
    });
}

#[rgpui::test]
async fn reference_style_link_in_root_paragraph_resolves_document_wide(cx: &mut TestAppContext) {
    let markdown = [
        "[reference link][ref-link]",
        "",
        "[ref-link]: https://example.com",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        assert_eq!(block.read(cx).rendered_text(), "reference link");
        assert_eq!(
            block.read(cx).inline_link_at(0),
            Some("https://example.com")
        );
    });
}

#[rgpui::test]
async fn reference_style_link_in_table_cell_resolves_document_wide(cx: &mut TestAppContext) {
    let markdown = [
        "| Link |",
        "| --- |",
        "| [reference link][ref-link] |",
        "",
        "[ref-link]: https://example.com",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.read_with(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime");
        let cell = runtime.rows[0][0].clone();
        assert_eq!(cell.read(cx).rendered_text(), "reference link");
        assert_eq!(cell.read(cx).inline_link_at(0), Some("https://example.com"));
    });
}

#[rgpui::test]
async fn root_level_footnotes_number_by_first_reference_and_render_in_place(
    cx: &mut TestAppContext,
) {
    let markdown = [
        "Here is a footnote reference.[^1]",
        "",
        "Here is another footnote reference.[^longnote]",
        "",
        "A footnote can appear after multiple paragraphs, lists, and code blocks.",
        "",
        "[^1]: Footnote text.",
        "",
        "[^longnote]: Footnote text with **bold**, `code`, and a nested list:",
        "    - item 1",
        "    - item 2",
        "    ",
        "    Second paragraph in the footnote.",
    ]
    .join("\n");
    let canonical_markdown = [
        "Here is a footnote reference.[^1]",
        "",
        "Here is another footnote reference.[^longnote]",
        "",
        "A footnote can appear after multiple paragraphs, lists, and code blocks.",
        "",
        "[^1]: Footnote text.",
        "",
        "[^longnote]: Footnote text with **bold**, `code`, and a nested list:",
        "",
        "    - item 1",
        "    - item 2",
        "",
        "    Second paragraph in the footnote.",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

    editor.read_with(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();

        let first_ref = visible
            .iter()
            .find(|visible| {
                visible
                    .entity
                    .read(cx)
                    .rendered_text()
                    .contains("Here is a footnote reference.")
            })
            .expect("first footnote reference")
            .entity
            .clone();
        assert_eq!(
            first_ref.read(cx).rendered_text(),
            format!("Here is a footnote reference.{}", superscript_ordinal(1))
        );

        let second_ref = visible
            .iter()
            .find(|visible| {
                visible
                    .entity
                    .read(cx)
                    .rendered_text()
                    .contains("Here is another footnote reference.")
            })
            .expect("second footnote reference")
            .entity
            .clone();
        assert_eq!(
            second_ref.read(cx).rendered_text(),
            format!(
                "Here is another footnote reference.{}",
                superscript_ordinal(2)
            )
        );

        let footnote_defs = visible
            .iter()
            .filter_map(|visible| {
                let block = visible.entity.read(cx);
                (block.kind() == BlockKind::FootnoteDefinition).then_some(visible.entity.clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(footnote_defs.len(), 2);
        assert_eq!(footnote_defs[0].read(cx).rendered_text(), "1");
        assert_eq!(
            footnote_defs[0].read(cx).footnote_definition_ordinal(),
            Some(1)
        );
        assert_eq!(footnote_defs[1].read(cx).rendered_text(), "longnote");
        assert_eq!(
            footnote_defs[1].read(cx).footnote_definition_ordinal(),
            Some(2)
        );

        assert_eq!(editor.document.markdown_text(cx), canonical_markdown);
    });
}

#[rgpui::test]
async fn callout_footnotes_number_and_render_in_place(cx: &mut TestAppContext) {
    let markdown = [
        "> [!WARNING]",
        "> Callout footnote reference.[^final]",
        "> ",
        "> [^final]: Nested footnote text.",
        "> Tail paragraph.",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

    editor.read_with(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();

        let reference_block = visible
            .iter()
            .find(|visible| {
                visible
                    .entity
                    .read(cx)
                    .rendered_text()
                    .contains("Callout footnote reference.")
            })
            .expect("callout footnote reference")
            .entity
            .clone();
        assert_eq!(
            reference_block.read(cx).rendered_text(),
            format!("Callout footnote reference.{}", superscript_ordinal(1))
        );

        let definition = visible
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::FootnoteDefinition)
            .expect("callout footnote definition")
            .entity
            .clone();
        assert_eq!(definition.read(cx).rendered_text(), "final");
        assert_eq!(definition.read(cx).quote_depth, 1);
        assert_eq!(definition.read(cx).footnote_definition_ordinal(), Some(1));
        assert_eq!(editor.document.markdown_text(cx), markdown);
    });
}

#[rgpui::test]
async fn root_reference_binds_to_nested_quote_footnote_definition(cx: &mut TestAppContext) {
    let markdown = "Root reference.[^note]\n\n> [^note]: Nested quote footnote".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

    editor.read_with(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();

        let root_reference = visible
            .iter()
            .find(|visible| visible.entity.read(cx).quote_depth == 0)
            .expect("root reference block")
            .entity
            .clone();
        assert_eq!(
            root_reference.read(cx).rendered_text(),
            format!("Root reference.{}", superscript_ordinal(1))
        );

        let definition = visible
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::FootnoteDefinition)
            .expect("nested quote footnote definition")
            .entity
            .clone();
        assert_eq!(definition.read(cx).rendered_text(), "note");
        assert_eq!(definition.read(cx).quote_depth, 1);
        assert_eq!(definition.read(cx).footnote_definition_ordinal(), Some(1));
        assert_eq!(editor.document.markdown_text(cx), markdown);
    });
}

#[rgpui::test]
async fn unresolved_footnote_reference_stays_literal_and_unlinked(cx: &mut TestAppContext) {
    let markdown = "Missing footnote[^missing].".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown.clone(), None));

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("root paragraph")
            .clone();
        assert_eq!(block.read(cx).rendered_text(), markdown);
        assert!(
            block
                .read(cx)
                .inline_footnote_hit_at("Missing footnote".len())
                .is_none()
        );
        assert!(editor.footnote_registry.binding("missing").is_none());
        assert_eq!(editor.document.markdown_text(cx), markdown);
    });
}

#[rgpui::test]
async fn toggling_source_mode_preserves_root_image_runtime(cx: &mut TestAppContext) {
    let markdown = "![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        assert!(block.read(cx).image_runtime().is_some());
    });
}

#[rgpui::test]
async fn toggling_source_mode_preserves_reference_style_root_image_runtime(
    cx: &mut TestAppContext,
) {
    let markdown = "![diagram][ref]\n\n[ref]: ./assets/diagram.png".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root block").clone();
        let runtime = block.read(cx).image_runtime().expect("image runtime");
        assert_eq!(runtime.src, "./assets/diagram.png");
    });
}

#[rgpui::test]
async fn toggling_source_mode_preserves_quote_child_image_runtime(cx: &mut TestAppContext) {
    let markdown = "> ![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });

    editor.read_with(cx, |editor, cx| {
        let quote = editor.document.first_root().expect("quote root").clone();
        let image_block = quote
            .read(cx)
            .children
            .first()
            .expect("quote image child")
            .clone();
        assert!(image_block.read(cx).image_runtime().is_some());
    });
}

#[rgpui::test]
async fn toggling_source_mode_preserves_list_item_image_runtime(cx: &mut TestAppContext) {
    let markdown = "- ![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });

    editor.read_with(cx, |editor, cx| {
        let block = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        assert!(block.read(cx).image_runtime().is_some());
    });
}

#[rgpui::test]
async fn toggling_source_mode_preserves_list_child_image_runtime(cx: &mut TestAppContext) {
    let markdown = "- item\n  ![diagram](./assets/diagram.png)".to_string();
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });

    editor.read_with(cx, |editor, cx| {
        let list_item = editor
            .document
            .first_root()
            .expect("list item root")
            .clone();
        let image_block = list_item
            .read(cx)
            .children
            .first()
            .expect("list child image")
            .clone();
        assert!(image_block.read(cx).image_runtime().is_some());
    });
}

#[rgpui::test]
async fn undo_reverts_recent_rendered_typing(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    let block = editor.update(cx, |editor, _cx| {
        let block = editor.document.first_root().expect("root").clone();
        editor.focus_block(block.entity_id());
        block
    });
    redraw(cx);

    block.update(cx, |block, cx| {
        block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
        block.replace_text_in_visible_range(5..5, " beta", None, false, cx);
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "alpha beta");
    });

    for _ in 0..2 {
        cx.dispatch_action(Undo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha");
        });
    }
}

#[rgpui::test]
async fn consecutive_text_edits_within_window_coalesce_into_one_undo(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "a".to_string(), None));

    let block = editor.update(cx, |editor, _cx| {
        let block = editor.document.first_root().expect("root").clone();
        editor.focus_block(block.entity_id());
        block
    });
    redraw(cx);

    block.update(cx, |block, cx| {
        block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
        block.replace_text_in_visible_range(1..1, "b", None, false, cx);
    });
    cx.run_until_parked();
    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "ab");
    });

    block.update(cx, |block, cx| {
        block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
        block.replace_text_in_visible_range(2..2, "c", None, false, cx);
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "abc");
    });

    for _ in 0..2 {
        cx.dispatch_action(Undo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "a");
        });
    }
}

#[rgpui::test]
async fn redo_restores_text_reverted_by_undo(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    let block = editor.update(cx, |editor, _cx| {
        let block = editor.document.first_root().expect("root").clone();
        editor.focus_block(block.entity_id());
        block
    });
    redraw(cx);

    block.update(cx, |block, cx| {
        block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
        block.replace_text_in_visible_range(5..5, " beta", None, false, cx);
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "alpha beta");
    });

    for _ in 0..2 {
        cx.dispatch_action(Undo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha");
        });
    }

    for _ in 0..2 {
        cx.dispatch_action(Redo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha beta");
        });
    }
}

#[rgpui::test]
async fn fresh_edit_clears_pending_redo_history(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    let block = editor.update(cx, |editor, _cx| {
        let block = editor.document.first_root().expect("root").clone();
        editor.focus_block(block.entity_id());
        block
    });
    redraw(cx);

    block.update(cx, |block, cx| {
        block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
        block.replace_text_in_visible_range(5..5, " beta", None, false, cx);
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "alpha beta");
    });

    cx.dispatch_action(Undo);
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "alpha");
    });

    editor.update(cx, |editor, cx| {
        let block = editor.document.first_root().expect("root").clone();
        block.update(cx, |block, cx| {
            block.prepare_undo_capture(crate::components::UndoKind::CoalescibleText, cx);
            block.replace_text_in_visible_range(5..5, " gamma", None, false, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.current_document_source(cx), "alpha gamma");
    });

    for _ in 0..2 {
        cx.dispatch_action(Redo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha gamma");
        });
    }

    for _ in 0..2 {
        cx.dispatch_action(Undo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha");
        });
    }

    for _ in 0..2 {
        cx.dispatch_action(Redo);
        redraw(cx);

        editor.read_with(cx, |editor, cx| {
            assert_eq!(editor.current_document_source(cx), "alpha gamma");
        });
    }
}

#[rgpui::test]
async fn toggle_view_mode_preserves_paragraph_caret_position(cx: &mut TestAppContext) {
    let editor = cx.new(|cx| Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None));

    editor.update(cx, |editor, cx| {
        let target = editor.document.visible_blocks()[1].entity.clone();
        target.update(cx, |block, _cx| {
            block.selected_range = 2..2;
        });
        editor.active_entity_id = Some(target.entity_id());

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        let source = editor.document.first_root().expect("source root").clone();
        assert_eq!(source.read(cx).selected_range, 9..9);
        assert!(source.read(cx).show_source_line_numbers());

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
        let visible = editor.document.visible_blocks();
        assert_eq!(visible.len(), 2);
        assert!(
            visible
                .iter()
                .all(|visible| !visible.entity.read(cx).show_source_line_numbers())
        );
        assert_eq!(visible[1].entity.read(cx).rendered_text(), "beta");
        assert_eq!(visible[1].entity.read(cx).selected_range, 2..2);
        assert_eq!(editor.pending_focus, Some(visible[1].entity.entity_id()));
    });
}

#[rgpui::test]
async fn toggle_view_mode_ends_stale_code_block_pointer_selection(cx: &mut TestAppContext) {
    let editor =
        cx.new(|cx| Editor::from_markdown(cx, "```rust\nfn main() {}\n```".to_string(), None));

    editor.update(cx, |editor, cx| {
        let target = editor.document.visible_blocks()[0].entity.clone();
        target.update(cx, |block, _cx| {
            block.selected_range = 3..7;
            block.is_selecting = true;
            block.code_language_is_selecting = true;
        });
        editor.active_entity_id = Some(target.entity_id());

        editor.toggle_view_mode(cx);

        assert!(matches!(editor.view_mode, ViewMode::Source));
        target.read_with(cx, |block, _cx| {
            assert!(!block.is_selecting);
            assert!(!block.code_language_is_selecting);
            assert_eq!(block.selected_range, 3..7);
        });
    });
}

#[rgpui::test]
async fn ctrl_tab_toggles_view_mode(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    redraw(cx);
    cx.simulate_keystrokes("ctrl-tab");
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        assert!(matches!(editor.view_mode, ViewMode::Source));
    });

    cx.simulate_keystrokes("ctrl-tab");
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
    });
}

#[rgpui::test]
async fn ctrl_a_selects_entire_source_document_in_source_mode(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None)
    });

    editor.update(cx, |editor, cx| {
        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));
        let source = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(source.entity_id());
        source.update(cx, |block, _cx| {
            block.selected_range = 1..3;
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let source = editor.document.visible_blocks()[0].entity.read(cx);
        assert_eq!(source.selected_range, 0..source.plain_len());
        assert_eq!(
            editor.selected_markdown_text(cx).as_deref(),
            Some("alpha\n\nbeta")
        );
        assert_eq!(editor.capture_source_selection_snapshot(cx).range, 0..11);
    });
}

#[rgpui::test]
async fn ctrl_a_selects_only_focused_block_text_in_rendered_mode(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None)
    });

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[1].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, _cx| {
            block.selected_range = 1..1;
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let first = editor.document.visible_blocks()[0].entity.read(cx);
        let second = editor.document.visible_blocks()[1].entity.read(cx);
        assert_eq!(first.selected_range, 0..0);
        assert_eq!(second.selected_range, 0..second.plain_len());
        assert_eq!(editor.selected_markdown_text(cx).as_deref(), Some("beta"));
        assert_eq!(editor.capture_source_selection_snapshot(cx).range, 7..11);
    });
}

#[rgpui::test]
async fn repeated_ctrl_a_selects_all_rendered_blocks(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown =
        "alpha\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n\ngamma";
    let (editor, cx) = cx.add_window_view({
        let markdown = markdown.to_string();
        move |_window, cx| Editor::from_markdown(cx, markdown.clone(), None)
    });

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(0, block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let first = editor.document.visible_blocks()[0].entity.read(cx);
        assert_eq!(first.selected_range, 0..first.plain_len());
        assert_eq!(editor.selected_markdown_text(cx).as_deref(), Some("alpha"));
        assert_eq!(editor.capture_source_selection_snapshot(cx).range, 0..5);
    });

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    let selected_after_second = editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.selected_markdown_text(cx).as_deref(), Some(markdown));
        let selection = editor.capture_source_selection_snapshot(cx);
        assert_eq!(selection.range, 0..markdown.len());
        assert!(!selection.reversed);
        for visible in editor.document.visible_blocks() {
            let block = visible.entity.read(cx);
            let len = block.plain_len();
            if len > 0 {
                assert_eq!(block.editor_selection_range, Some(0..len));
            }
        }
        selection
    });

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(editor.selected_markdown_text(cx).as_deref(), Some(markdown));
        assert_eq!(
            editor.capture_source_selection_snapshot(cx),
            selected_after_second,
            "第三次 Ctrl+A 应保持整篇渲染文档的选区"
        );
        for visible in editor.document.visible_blocks() {
            let block = visible.entity.read(cx);
            let len = block.plain_len();
            if len > 0 {
                assert_eq!(block.editor_selection_range, Some(0..len));
            }
        }
    });
}

#[rgpui::test]
async fn rendered_ctrl_a_cycle_expires_before_second_press(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "alpha\n\nbeta".to_string(), None)
    });

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[1].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[1].entity.clone();
        block.update(cx, |block, _cx| {
            block.selected_range = 1..1;
        });
        let cycle = editor
            .rendered_select_all_cycle
            .as_mut()
            .expect("first Ctrl+A should arm the rendered select-all cycle");
        cycle.last_pressed_at =
            Instant::now() - (Editor::SELECT_ALL_CYCLE_WINDOW + Duration::from_millis(1));
    });

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let second = editor.document.visible_blocks()[1].entity.read(cx);
        assert_eq!(second.selected_range, 0..second.plain_len());
        assert_eq!(editor.selected_markdown_text(cx).as_deref(), Some("beta"));
        assert_eq!(editor.capture_source_selection_snapshot(cx).range, 7..11);
    });

    cx.simulate_keystrokes("ctrl-a");
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        assert_eq!(
            editor.selected_markdown_text(cx).as_deref(),
            Some("alpha\n\nbeta")
        );
        assert_eq!(editor.capture_source_selection_snapshot(cx).range, 0..11);
        for visible in editor.document.visible_blocks() {
            let block = visible.entity.read(cx);
            assert_eq!(block.editor_selection_range, Some(0..block.plain_len()));
        }
    });
}

#[rgpui::test]
async fn tab_key_inserts_tab_in_focused_paragraph(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "ab".to_string(), None));

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("tab");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        assert_eq!(block.read(cx).rendered_text(), "a    b");
        assert_eq!(editor.current_document_source(cx), "a    b");
    });
}

#[rgpui::test]
async fn tab_key_inserts_tab_in_focused_code_block(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None)
    });

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("tab");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        assert_eq!(block.read(cx).rendered_text(), "a    b");
        assert_eq!(editor.current_document_source(cx), "```rust\na    b\n```");
    });
}

#[rgpui::test]
async fn captured_tab_key_inserts_visible_indent_in_paragraph(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "ab".to_string(), None));

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
        });
    });
    redraw(cx);

    let event = KeyDownEvent {
        keystroke: Keystroke::parse("tab").expect("valid tab keystroke"),
        is_held: false,
        prefer_character_input: false,
    };
    editor.update_in(cx, |editor, window, cx| {
        editor.on_editor_key_down_capture(&event, window, cx);
    });
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        assert_eq!(block.read(cx).rendered_text(), "a    b");
    });
}

#[rgpui::test]
async fn down_from_code_content_focuses_language_input(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None)
    });

    // Settle focus on the code content first (and clear any pending focus that a
    // later redraw would otherwise re-apply and steal back).
    editor.update_in(cx, |editor, _window, _cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
    });
    redraw(cx);

    editor.update_in(cx, |editor, window, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        block.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
            block.on_focus_next(&FocusNext, window, block_cx);
        });
        assert!(
            block.read(cx).code_language_focus_handle.is_focused(window),
            "Down from the last code line should focus the language field"
        );
    });
}

#[rgpui::test]
async fn down_from_code_language_at_document_end_creates_trailing_paragraph(
    cx: &mut TestAppContext,
) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None)
    });

    editor.update_in(cx, |editor, window, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            block.on_code_language_focus_next(&FocusNext, window, block_cx);
        });
    });
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let roots = editor.document.root_blocks();
        assert_eq!(roots.len(), 2, "a trailing paragraph should be created");
        assert_eq!(roots[1].read(cx).kind(), BlockKind::Paragraph);
        assert_eq!(roots[1].read(cx).rendered_text(), "");
    });
}

#[rgpui::test]
async fn enter_in_code_language_does_not_exit_block(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None)
    });

    editor.update_in(cx, |editor, window, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
            block.on_code_language_newline(&Newline, window, block_cx);
        });
    });
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        // Enter must not leave the block, so no trailing paragraph appears.
        assert_eq!(editor.document.root_count(), 1);
    });
}

#[rgpui::test]
async fn captured_tab_key_does_not_modify_code_language_input(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) = cx.add_window_view(|_window, cx| {
        Editor::from_markdown(cx, "```rust\nab\n```".to_string(), None)
    });

    editor.update_in(cx, |editor, window, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(1, block_cx);
        });
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
        });
    });
    redraw(cx);

    editor.update_in(cx, |editor, window, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        block.update(cx, |block, block_cx| {
            block.code_language_focus_handle.focus(window, block_cx);
        });
    });

    let event = KeyDownEvent {
        keystroke: Keystroke::parse("tab").expect("valid tab keystroke"),
        is_held: false,
        prefer_character_input: false,
    };
    editor.update_in(cx, |editor, window, cx| {
        editor.on_editor_key_down_capture(&event, window, cx);
    });
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        let block = block.read(cx);
        assert_eq!(block.code_language_text(), "rust");
        assert_eq!(block.rendered_text(), "ab");
    });
}

#[rgpui::test]
async fn tab_key_keeps_list_indent_semantics(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "- a\n- b".to_string(), None));

    editor.update(cx, |editor, cx| {
        let second = editor.document.visible_blocks()[1].entity.clone();
        editor.focus_block(second.entity_id());
        second.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("tab");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[1].entity.read(cx).render_depth, 1);
        assert_eq!(editor.current_document_source(cx), "- a\n  - b");
    });
}

#[rgpui::test]
async fn tab_key_keeps_table_cell_navigation(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) =
        cx.add_window_view(move |_window, cx| Editor::from_markdown(cx, markdown, None));

    let second_cell_id = editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .clone();
        let first = runtime.rows[0][0].clone();
        let second = runtime.rows[0][1].clone();
        editor.focus_block(first.entity_id());
        first.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
        });
        second.entity_id()
    });
    redraw(cx);

    cx.simulate_keystrokes("tab");
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        assert_eq!(editor.active_entity_id, Some(second_cell_id));
    });
}

#[rgpui::test]
async fn right_arrow_at_cell_end_moves_to_next_cell(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) =
        cx.add_window_view(move |_window, cx| Editor::from_markdown(cx, markdown, None));

    let second_cell_id = editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .clone();
        let first = runtime.rows[0][0].clone();
        let second = runtime.rows[0][1].clone();
        editor.focus_block(first.entity_id());
        first.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
        });
        second.entity_id()
    });
    redraw(cx);

    cx.simulate_keystrokes("right");
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        assert_eq!(editor.active_entity_id, Some(second_cell_id));
    });
}

#[rgpui::test]
async fn left_arrow_at_cell_start_moves_to_previous_cell(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) =
        cx.add_window_view(move |_window, cx| Editor::from_markdown(cx, markdown, None));

    let first_cell_id = editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let runtime = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .clone();
        let first = runtime.rows[0][0].clone();
        let second = runtime.rows[0][1].clone();
        editor.focus_block(second.entity_id());
        second.update(cx, |block, block_cx| {
            block.move_to(0, block_cx);
        });
        first.entity_id()
    });
    redraw(cx);

    cx.simulate_keystrokes("left");
    redraw(cx);

    editor.update(cx, |editor, _cx| {
        assert_eq!(editor.active_entity_id, Some(first_cell_id));
    });
}

#[rgpui::test]
async fn inserting_table_at_document_end_adds_trailing_paragraph(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let editor = cx.new(|cx| Editor::from_markdown(cx, String::new(), None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.table_insert_dialog = Some(super::context_menu::TableInsertDialogState {
                target: super::context_menu::TableInsertTarget::Append,
                body_rows: 2,
                columns: 2,
            });
            editor.on_confirm_table_insert_dialog(&ClickEvent::default(), window, cx);
        });
    });

    editor.update(cx, |editor, cx| {
        let roots = editor.document.visible_blocks();
        let kinds = roots
            .iter()
            .map(|visible| visible.entity.read(cx).kind())
            .collect::<Vec<_>>();
        let table_index = kinds
            .iter()
            .position(|kind| *kind == BlockKind::Table)
            .expect("table inserted");
        // The table is the last meaningful block, so an empty paragraph is
        // appended after it to give the caret somewhere to land.
        assert_eq!(kinds.get(table_index + 1), Some(&BlockKind::Paragraph));
        assert_eq!(table_index + 1, kinds.len() - 1);
        assert_eq!(roots[table_index + 1].entity.read(cx).rendered_text(), "");
    });
}

#[rgpui::test]
async fn table_insert_dialog_renders_in_framework_shell(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "alpha".to_string(), None));

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.table_insert_dialog = Some(super::context_menu::TableInsertDialogState {
                target: super::context_menu::TableInsertTarget::Append,
                body_rows: 2,
                columns: 3,
            });
            // 步进器加减各一次，确认行列数实时回写。
            editor.on_table_rows_increment(&ClickEvent::default(), window, cx);
            editor.on_table_columns_decrement(&ClickEvent::default(), window, cx);
            let dialog = editor.table_insert_dialog.as_ref().expect("dialog open");
            assert_eq!((dialog.body_rows, dialog.columns), (3, 2));
        });
    });
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

#[rgpui::test]
async fn ctrl_enter_exits_focused_math_block(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let (editor, cx) =
        cx.add_window_view(|_window, cx| Editor::from_markdown(cx, "$$n^2$$".to_string(), None));

    editor.update(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        editor.focus_block(block.entity_id());
        block.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-enter");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::MathBlock);
        assert_eq!(visible[0].entity.read(cx).rendered_text(), "$$n^2$$");
        assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
        assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
        assert_eq!(editor.current_document_source(cx), "$$n^2$$\n\n");
    });
}

#[rgpui::test]
async fn ctrl_enter_exits_focused_table_cell(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = ["| A | B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
    let (editor, cx) =
        cx.add_window_view(move |_window, cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let cell = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .rows[0][0]
            .clone();
        editor.focus_block(cell.entity_id());
        cell.update(cx, |block, block_cx| {
            block.move_to(block.plain_len(), block_cx);
        });
    });
    redraw(cx);

    cx.simulate_keystrokes("ctrl-enter");
    redraw(cx);

    editor.update(cx, |editor, cx| {
        let visible = editor.document.visible_blocks();
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].entity.read(cx).kind(), BlockKind::Table);
        assert_eq!(visible[1].entity.read(cx).kind(), BlockKind::Paragraph);
        assert_eq!(visible[1].entity.read(cx).rendered_text(), "");
        assert_eq!(editor.active_entity_id, Some(visible[1].entity.entity_id()));
    });
}

#[rgpui::test]
async fn ending_editor_pointer_selection_sessions_keeps_normal_selection(cx: &mut TestAppContext) {
    let editor =
        cx.new(|cx| Editor::from_markdown(cx, "```rust\nfn main() {}\n```".to_string(), None));

    editor.update(cx, |editor, cx| {
        let target = editor.document.visible_blocks()[0].entity.clone();
        target.update(cx, |block, _cx| {
            block.selected_range = 3..7;
            block.marked_range = Some(4..6);
            block.is_selecting = true;
        });
        editor.active_entity_id = Some(target.entity_id());

        assert!(editor.end_block_pointer_selection_sessions(cx));
        target.read_with(cx, |block, _cx| {
            assert!(!block.is_selecting);
            assert_eq!(block.selected_range, 3..7);
            assert_eq!(block.marked_range, Some(4..6));
        });

        assert!(!editor.end_block_pointer_selection_sessions(cx));
    });
}

#[rgpui::test]
async fn toggle_view_mode_preserves_table_cell_position(cx: &mut TestAppContext) {
    let markdown = ["| Name | Value |", "| --- | --- |", "| alpha | beta |"].join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let table = editor.document.first_root().expect("table root").clone();
        let cell = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .rows[0][1]
            .clone();
        cell.update(cx, |block, _cx| {
            block.selected_range = 2..2;
        });
        editor.active_entity_id = Some(cell.entity_id());

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
        let restored_table = editor
            .document
            .first_root()
            .expect("restored table")
            .clone();
        let restored_cell = restored_table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("restored runtime")
            .rows[0][1]
            .clone();
        assert_eq!(restored_cell.read(cx).rendered_text(), "beta");
        assert_eq!(restored_cell.read(cx).selected_range, 2..2);
        assert_eq!(editor.pending_focus, Some(restored_cell.entity_id()));
    });
}

#[rgpui::test]
async fn clicking_html_block_focuses_source_for_editing(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "<div class=\"note\">\n  <p>HTML 块。</p>\n</div>\n".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..4 {
        redraw(cx);
    }
    let (entity, bounds) = editor.read_with(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        let bounds = block.read(cx).last_bounds.expect("html block bounds");
        (block, bounds)
    });
    let point = rgpui::point(
        bounds.left() + rgpui::px(20.0),
        bounds.top() + rgpui::px(8.0),
    );
    cx.simulate_click(point, rgpui::Modifiers::none());
    redraw(cx);
    cx.update(|window, cx| {
        let block = entity.read(cx);
        assert!(
            block.focus_handle.is_focused(window),
            "点击 HTML 块后源码行应获得焦点"
        );
        let sel = block.selected_range.clone();
        assert_eq!(sel.start, sel.end, "点击后光标应落在源码行内");
    });
}

#[rgpui::test]
async fn clicking_html_block_source_line_keeps_focus(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "<div class=\"note\">\n  <p>HTML 块。</p>\n</div>\n".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..4 {
        redraw(cx);
    }
    let entity = editor.read_with(cx, |editor, _cx| {
        editor.document.visible_blocks()[0].entity.clone()
    });
    let bounds = entity.read_with(cx, |block, _cx| block.last_bounds.expect("bounds"));
    // 首次点击聚焦，源码行出现。
    cx.simulate_click(
        rgpui::point(
            bounds.left() + rgpui::px(20.0),
            bounds.top() + rgpui::px(6.0),
        ),
        rgpui::Modifiers::none(),
    );
    for _ in 0..2 {
        redraw(cx);
    }
    let focused_bounds = entity.read_with(cx, |block, _cx| {
        block.last_layout.as_ref().and(block.last_bounds)
    });
    let point = rgpui::point(
        focused_bounds.unwrap_or(bounds).left() + rgpui::px(20.0),
        bounds.top() + rgpui::px(6.0),
    );
    // 再次点击源码行：应保持聚焦、光标落在源码内，不隐藏源码。
    cx.simulate_click(point, rgpui::Modifiers::none());
    for _ in 0..2 {
        redraw(cx);
    }
    cx.update(|window, cx| {
        let block = entity.read(cx);
        assert!(
            block.focus_handle.is_focused(window),
            "点击 HTML 源码行后不应失焦/隐藏源码"
        );
        assert!(
            block.last_layout.is_some(),
            "点击源码行后源码文本布局应仍在"
        );
    });

    // 再点预览区域（源码行下方）：同样应保持聚焦，不隐藏源码。
    let preview_bounds = entity.read_with(cx, |block, _cx| block.last_bounds.expect("bounds"));
    cx.simulate_click(
        rgpui::point(
            preview_bounds.left() + rgpui::px(20.0),
            preview_bounds.bottom() - rgpui::px(6.0),
        ),
        rgpui::Modifiers::none(),
    );
    for _ in 0..2 {
        redraw(cx);
    }
    cx.update(|window, cx| {
        let block = entity.read(cx);
        assert!(
            block.focus_handle.is_focused(window),
            "点击 HTML 预览区后不应失焦/隐藏源码"
        );
    });

    // 真实鼠标点击常带 1-2px 抖动（按下 → 小幅移动 → 抬起）：
    // 在源码行上模拟，仍应保持聚焦、不隐藏源码。
    let drag_bounds = entity.read_with(cx, |block, _cx| block.last_bounds.expect("bounds"));
    let source_start = rgpui::point(
        drag_bounds.left() + rgpui::px(30.0),
        drag_bounds.top() + rgpui::px(6.0),
    );
    let source_end = rgpui::point(
        drag_bounds.left() + rgpui::px(34.0),
        drag_bounds.top() + rgpui::px(9.0),
    );
    cx.simulate_mouse_down(
        source_start,
        rgpui::MouseButton::Left,
        rgpui::Modifiers::none(),
    );
    cx.simulate_mouse_move(
        source_end,
        rgpui::MouseButton::Left,
        rgpui::Modifiers::none(),
    );
    cx.simulate_mouse_up(
        source_end,
        rgpui::MouseButton::Left,
        rgpui::Modifiers::none(),
    );
    for _ in 0..2 {
        redraw(cx);
    }
    cx.update(|window, cx| {
        let block = entity.read(cx);
        assert!(
            block.focus_handle.is_focused(window),
            "小幅拖拽点击源码行后不应失焦/隐藏源码"
        );
    });
}

/// 开一个测试窗口加载 `markdown`（前置一个段落以满足聚焦块、使 HTML 块保持非聚焦预览），
/// 返回该 HTML 块的渲染高度与窗口上下文。
fn html_block_height(
    cx: &mut TestAppContext,
    markdown: String,
) -> (f32, &mut rgpui::VisualTestContext) {
    let markdown = format!("前导段落\n\n{markdown}");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..4 {
        redraw(cx);
    }
    let height = editor.read_with(cx, |editor, cx| {
        editor
            .document
            .visible_blocks()
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::HtmlBlock)
            .and_then(|visible| visible.entity.read(cx).last_shell_bounds)
            .map(|bounds| f32::from(bounds.size.height))
            .expect("html table block bounds")
    });
    (height, cx)
}

#[rgpui::test]
async fn html_table_structural_whitespace_does_not_add_height(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    // 同一张表两种写法：带缩进换行 vs 紧凑单行。table/thead/tbody/tr 内的换行与缩进
    // 属结构性空白，HTML 折叠规则应将其丢弃，两种写法渲染高度须一致
    //（回归：曾因未折叠空白而在表内多出空行）。
    let pretty = concat!(
        "<table>\n",
        "  <thead>\n",
        "    <tr><th>列 A</th><th>列 B</th></tr>\n",
        "  </thead>\n",
        "  <tbody>\n",
        "    <tr><td>1</td><td>2</td></tr>\n",
        "  </tbody>\n",
        "</table>\n",
    )
    .to_string();
    let compact = concat!(
        "<table><thead><tr><th>列 A</th><th>列 B</th></tr></thead>",
        "<tbody><tr><td>1</td><td>2</td></tr></tbody></table>\n",
    )
    .to_string();

    let (pretty_height, cx) = html_block_height(cx, pretty);
    let (compact_height, _cx) = html_block_height(cx, compact);
    assert!(
        (pretty_height - compact_height).abs() < 0.5,
        "表格结构空白不应改变渲染高度：pretty={pretty_height} compact={compact_height}"
    );
}

#[rgpui::test]
async fn stray_mouse_down_outside_block_shell_does_not_steal_focus(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "段落\n\n---\n\n<div class=\"note\">\n  <p>HTML 块。</p>\n</div>\n".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..4 {
        redraw(cx);
    }
    let (separator, far_point) = editor.read_with(cx, |editor, cx| {
        let separator = editor
            .document
            .visible_blocks()
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::Separator)
            .expect("separator")
            .entity
            .clone();
        let shell = separator
            .read(cx)
            .last_shell_bounds
            .expect("separator shell bounds");
        let far_point = rgpui::point(
            shell.left() + rgpui::px(20.0),
            shell.bottom() + rgpui::px(80.0),
        );
        (separator, far_point)
    });
    let event = rgpui::MouseDownEvent {
        button: rgpui::MouseButton::Left,
        position: far_point,
        modifiers: rgpui::Modifiers::none(),
        click_count: 1,
        first_mouse: false,
    };
    cx.update(|window, app| {
        separator.update(app, |block, cx| block.on_mouse_down(&event, window, cx));
    });
    editor.read_with(cx, |editor, _cx| {
        assert_ne!(
            editor.active_entity_id,
            Some(separator.entity_id()),
            "落在块外壳之外的误投递鼠标按下不应让它抢走焦点"
        );
    });
}

#[rgpui::test]
async fn clicking_html_block_below_separator_keeps_focus(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "---\n\n<div class=\"note\">\n  <p>HTML 块。</p>\n</div>\n".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..4 {
        redraw(cx);
    }
    let (separator, html_block, anchor) = editor.read_with(cx, |editor, cx| {
        let separator = editor
            .document
            .visible_blocks()
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::Separator)
            .expect("separator")
            .entity
            .clone();
        let html_block = editor
            .document
            .visible_blocks()
            .iter()
            .find(|visible| visible.entity.read(cx).kind() == BlockKind::HtmlBlock)
            .expect("html block")
            .entity
            .clone();
        let separator_bounds = separator.read(cx).last_bounds.expect("separator bounds");
        (separator, html_block, separator_bounds)
    });
    for offset in [16.0f32, 40.0, 52.0] {
        let point = rgpui::point(
            anchor.left() + rgpui::px(40.0),
            anchor.bottom() + rgpui::px(offset),
        );
        cx.simulate_click(point, rgpui::Modifiers::none());
        for _ in 0..2 {
            redraw(cx);
        }
    }
    cx.update(|window, cx| {
        assert!(
            html_block.read(cx).focus_handle.is_focused(window),
            "点击分隔线下方的 HTML 块后应保持聚焦（源码行不应被隐藏）"
        );
        assert!(
            !separator.read(cx).focus_handle.is_focused(window),
            "分隔线不应因误投递的鼠标按下而抢走焦点"
        );
    });
}

#[rgpui::test]
async fn toggle_view_mode_preserves_callout_table_cell_position(cx: &mut TestAppContext) {
    let markdown = [
        "> [!NOTE]",
        "> | Name | Value |",
        "> | --- | --- |",
        "> | alpha | beta |",
    ]
    .join("\n");
    let editor = cx.new(|cx| Editor::from_markdown(cx, markdown, None));

    editor.update(cx, |editor, cx| {
        let callout = editor.document.first_root().expect("callout root").clone();
        let table = callout
            .read(cx)
            .children
            .iter()
            .find(|child| child.read(cx).kind() == BlockKind::Table)
            .expect("nested table child")
            .clone();
        let cell = table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("table runtime")
            .rows[0][1]
            .clone();
        cell.update(cx, |block, _cx| {
            block.selected_range = 2..2;
        });
        editor.active_entity_id = Some(cell.entity_id());

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Source));

        editor.toggle_view_mode(cx);
        assert!(matches!(editor.view_mode, ViewMode::Rendered));
        let restored_callout = editor
            .document
            .first_root()
            .expect("restored callout")
            .clone();
        let restored_table = restored_callout
            .read(cx)
            .children
            .iter()
            .find(|child| child.read(cx).kind() == BlockKind::Table)
            .expect("restored nested table")
            .clone();
        let restored_cell = restored_table
            .read(cx)
            .table_runtime
            .as_ref()
            .expect("restored runtime")
            .rows[0][1]
            .clone();
        assert_eq!(restored_cell.read(cx).rendered_text(), "beta");
        assert_eq!(restored_cell.read(cx).selected_range, 2..2);
        assert_eq!(editor.pending_focus, Some(restored_cell.entity_id()));
    });
}

#[rgpui::test]
async fn right_click_over_short_blocks_builds_single_menu(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = fs::read_to_string("docs/image-sample.md").expect("sample doc");
    // 该样本文档短块密集、块间留白多，历史上会同时命中相邻块行与空白域兜底菜单，
    // 造成多层菜单叠加。逐点断言每次右键只构建一个菜单。
    for y in [50, 130, 400, 665, 740, 1029, 1031] {
        let markdown = markdown.clone();
        let (_editor, cx) =
            cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
        for _ in 0..3 {
            redraw(cx);
        }
        super::MENU_BUILD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
        cx.simulate_event(rgpui::MouseDownEvent {
            button: rgpui::MouseButton::Right,
            position: rgpui::point(rgpui::px(631.), rgpui::px(y as f32)),
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        cx.run_until_parked();
        redraw(cx);
        redraw(cx);
        assert_eq!(
            super::MENU_BUILD_COUNT.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "right-click at y={y} should build exactly one context menu"
        );
    }
}

#[rgpui::test]
async fn right_click_opens_exactly_one_context_menu(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "![cover](./assets/red.png \"caption\")\n\nafter paragraph\n".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    for _ in 0..3 {
        redraw(cx);
    }

    // 块行自带菜单，空白域的兜底菜单必须让位，否则两层菜单会叠加（多层阴影、
    // 上层吞掉点击）。
    super::MENU_BUILD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    cx.simulate_event(rgpui::MouseDownEvent {
        button: rgpui::MouseButton::Right,
        position: rgpui::point(rgpui::px(631.), rgpui::px(90.)),
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    cx.run_until_parked();
    redraw(cx);
    redraw(cx);
    assert_eq!(
        super::MENU_BUILD_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "right-clicking a block row should build exactly one menu"
    );

    // 空白处右键仍应弹出追加语义菜单。
    super::MENU_BUILD_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
    cx.simulate_event(rgpui::MouseDownEvent {
        button: rgpui::MouseButton::Right,
        position: rgpui::point(rgpui::px(20.), rgpui::px(300.)),
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    cx.run_until_parked();
    redraw(cx);
    redraw(cx);
    assert_eq!(
        super::MENU_BUILD_COUNT.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "right-clicking blank space should build exactly one menu"
    );
    let _ = &editor;
}

#[rgpui::test]
async fn image_menu_detects_markdown_and_html_images(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = [
        "![cover](./assets/red.png \"caption\")",
        "",
        "<img src=\"./assets/blue.png\" alt=\"banner\" style=\"zoom:50%;\" />",
        "",
        "plain text",
    ]
    .join("\n");
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let ids = editor
            .document
            .visible_blocks()
            .iter()
            .map(|visible| visible.entity.entity_id())
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);

        let markdown_image = editor
            .describe_image_menu(Some(ids[0]), cx)
            .expect("markdown image should expose image menu");
        assert_eq!(markdown_image.alt, "cover");
        assert_eq!(markdown_image.src, "./assets/red.png");
        assert_eq!(markdown_image.title.as_deref(), Some("caption"));
        assert!(!markdown_image.from_html);
        assert_eq!(markdown_image.zoom, 1.0);

        let html_image = editor
            .describe_image_menu(Some(ids[1]), cx)
            .expect("html image should expose image menu");
        assert!(html_image.from_html);
        assert_eq!(html_image.alt, "banner");
        assert_eq!(html_image.src, "./assets/blue.png");
        assert!((html_image.zoom - 0.5).abs() < 1e-3);

        assert!(editor.describe_image_menu(Some(ids[2]), cx).is_none());
    });
}

#[rgpui::test]
async fn image_menu_zoom_rewrites_between_markdown_and_html(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let markdown = "![cover](./assets/red.png \"caption\")".to_string();
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    redraw(cx);

    let block = editor.read_with(cx, |editor, _cx| {
        editor.document.visible_blocks()[0].entity.clone()
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let data = editor
                .describe_image_menu(Some(block.entity_id()), cx)
                .expect("image menu data");
            Editor::on_image_zoom(editor, &data, 0.5, window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        let block = block.read(cx);
        assert_eq!(block.kind(), BlockKind::HtmlBlock);
        assert_eq!(
            block.rendered_text(),
            "<img src=\"./assets/red.png\" alt=\"cover\" title=\"caption\" style=\"zoom:50%;\" />"
        );
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let data = editor
                .describe_image_menu(Some(block.entity_id()), cx)
                .expect("image menu data");
            Editor::on_image_zoom(editor, &data, 1.0, window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        let block = block.read(cx);
        assert_eq!(block.kind(), BlockKind::Paragraph);
        assert_eq!(
            block.rendered_text(),
            "![cover](./assets/red.png \"caption\")"
        );
        // 缩放归位后块类型回到段落，编辑模式必须同步出 SourceRaw，
        // 否则段落按纯源码渲染、图片不再显示（残留源码文本）。
        assert!(!block.is_source_raw_mode());
    });
}

#[rgpui::test]
async fn image_menu_embeds_local_image_as_base64(cx: &mut TestAppContext) {
    init_editor_test_app(cx);
    let image_path = temp_export_path("image-menu-embed", "png");
    fs::write(
        &image_path,
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    )
    .expect("write fixture png");

    let markdown = format!("![cover]({})", image_path.to_string_lossy());
    let (editor, cx) = cx.add_window_view(|_window, cx| Editor::from_markdown(cx, markdown, None));
    redraw(cx);

    let block = editor.read_with(cx, |editor, _cx| {
        editor.document.visible_blocks()[0].entity.clone()
    });

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            let data = editor
                .describe_image_menu(Some(block.entity_id()), cx)
                .expect("image menu data");
            Editor::on_image_embed_base64(editor, &data, window, cx);
        });
    });
    redraw(cx);

    editor.read_with(cx, |editor, cx| {
        let block = editor.document.visible_blocks()[0].entity.clone();
        let text = block.read(cx).rendered_text().to_string();
        assert!(
            text.starts_with("![cover](data:image/png;base64,"),
            "unexpected embedded markdown: {text}"
        );
    });

    let _ = fs::remove_file(image_path);
}
