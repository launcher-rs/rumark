//! 窗口级编辑器状态：滚动、模式切换与菜单。

use super::*;

/// 行足迹的 Fenwick 前缀索引，单行高度更新与视口边界查询均为对数复杂度。
#[derive(Default)]
pub(super) struct StrideIndex {
    values: Vec<f32>,
    tree: Vec<f32>,
}

impl StrideIndex {
    /// 行数。
    pub(super) fn len(&self) -> usize {
        self.values.len()
    }

    /// 用当前行高重建索引。
    pub(super) fn rebuild(&mut self, values: Vec<f32>) {
        self.values = values;
        self.tree = vec![0.0; self.values.len() + 1];
        for index in 1..self.tree.len() {
            self.tree[index] += self.values[index - 1].max(0.0);
            let parent = index + index.isolate_lowest_one();
            if parent < self.tree.len() {
                self.tree[parent] += self.tree[index];
            }
        }
    }

    /// 更新单行足迹并回写前缀树。
    pub(super) fn update(&mut self, index: usize, value: f32) {
        let Some(slot) = self.values.get_mut(index) else {
            return;
        };
        let value = value.max(0.0);
        let delta = value - *slot;
        *slot = value;
        let mut node = index + 1;
        while node < self.tree.len() {
            self.tree[node] += delta;
            node += node.isolate_lowest_one();
        }
    }

    /// 前 `count` 行的高度。
    pub(super) fn prefix_height(&self, count: usize) -> f32 {
        let mut index = count.min(self.values.len());
        let mut sum = 0.0;
        while index > 0 {
            sum += self.tree[index];
            index &= index - 1;
        }
        sum
    }

    /// 返回满足前缀与目标关系的最大行数；严格模式只允许前缀小于目标。
    fn prefix_partition(&self, target: f32, strict: bool) -> usize {
        let n = self.values.len();
        let mut bit = 1usize;
        while bit.saturating_mul(2) <= n {
            bit *= 2;
        }
        let mut index = 0usize;
        let mut sum = 0.0;
        while bit > 0 {
            let next = index + bit;
            if next <= n {
                let candidate = sum + self.tree[next];
                let may_skip = if strict {
                    candidate < target
                } else {
                    candidate <= target
                };
                if may_skip {
                    index = next;
                    sum = candidate;
                }
            }
            bit >>= 1;
        }
        index
    }
}

impl Editor {
    /// 按视口高度与滚动范围计算滚动条几何。
    pub(super) fn scrollbar_geometry(
        viewport_height: f32,
        max_scroll_y: f32,
        current_scroll_y: f32,
    ) -> ScrollbarMetrics {
        let track_height = viewport_height.max(20.0);
        let content_height = viewport_height + max_scroll_y;
        let thumb_height = if max_scroll_y > 0.5 {
            (track_height * (viewport_height / content_height)).clamp(28.0, track_height)
        } else {
            track_height
        };
        let progress = if max_scroll_y > 0.0 {
            current_scroll_y.clamp(0.0, max_scroll_y) / max_scroll_y
        } else {
            0.0
        };
        let thumb_top = (track_height - thumb_height).max(0.0) * progress;
        ScrollbarMetrics {
            track_height,
            thumb_height,
            thumb_top,
            max_scroll_y,
        }
    }

    /// 由滑块顶部位置反推文档滚动偏移。
    pub(super) fn scroll_offset_for_thumb_top(
        thumb_top: f32,
        track_height: f32,
        thumb_height: f32,
        max_scroll_y: f32,
    ) -> f32 {
        if max_scroll_y <= 0.0 {
            return 0.0;
        }

        let travel = (track_height - thumb_height).max(0.0);
        if travel <= 0.0 {
            return 0.0;
        }

        let progress = (thumb_top / travel).clamp(0.0, 1.0);
        max_scroll_y * progress
    }

    /// 上一帧的子节点下标是否仍能定位到相同子节点。
    /// 占用高度按下标读回，滚动列中新增子节点会错配行；
    /// 子节点数变化说明记录的下标已过期，本次跳过刷新。
    pub(super) fn mounted_run_is_addressable(&self, run: MountedRow) -> bool {
        run.child_count > 0
            && self
                .scroll_handle
                .bounds_for_item(run.child_count - 1)
                .is_some()
            && self
                .scroll_handle
                .bounds_for_item(run.child_count)
                .is_none()
    }

    /// 选择要挂载的连续行区间；被裁行变为占位符，聚焦行始终挂载，落在区间外时独立成岛。
    /// `strides[i]` 为第 `i` 行的占用高度（高度加尾部间隙）；因与滚动无关，对其累加即可按当前滚动偏移定位每行。
    /// 未实测行用下界估计；当估计值小于滚动偏移时挂载尾部区间，保证窗口永不落在占位符上。纯函数，可无头单测。
    #[cfg(test)]
    pub(super) fn rendered_window(
        strides: &[f32],
        scroll_y: f32,
        viewport_height: f32,
        overdraw: f32,
        focus_row: Option<usize>,
    ) -> ViewportWindow {
        let n = strides.len();
        if n == 0 {
            return ViewportWindow {
                run_start: 0,
                run_end: 0,
                top_h: 0.0,
                bottom_h: 0.0,
                focus_island: None,
            };
        }

        let band_top = scroll_y - overdraw;
        let band_bottom = scroll_y + viewport_height + overdraw;

        let mut run_start = n;
        let mut run_end = 0usize;
        let mut top_of_start = 0.0f32;
        let mut bottom_of_end = 0.0f32;
        let mut cursor = 0.0f32;
        for (index, &stride) in strides.iter().enumerate() {
            let top = cursor;
            let bottom = cursor + stride.max(0.0);
            if bottom >= band_top && top <= band_bottom {
                if index < run_start {
                    run_start = index;
                    top_of_start = top;
                }
                run_end = index + 1;
                bottom_of_end = bottom;
            }
            cursor = bottom;
        }
        let total = cursor;

        // 未命中：滚动偏移超出已有估计的总和，未挂载行的高度仍是下界估计。
        // 此时回退到尾部区间而非单行，保证视口填满并继续学习剩余高度。
        if run_start >= run_end {
            run_end = n;
            bottom_of_end = total;
            run_start = n - 1;
            top_of_start = total - strides[n - 1].max(0.0);
            let floor = (total - viewport_height - overdraw).max(0.0);
            while run_start > 0 && top_of_start > floor {
                run_start -= 1;
                top_of_start -= strides[run_start].max(0.0);
            }
        }

        // 聚焦行保持挂载，未挂载时光标会失焦。聚焦行独立成岛而不扩大主区间，
        // 避免阅读时遗留的光标把其与视口之间的所有行都拖入绘制。
        let mut top_h = top_of_start;
        let mut bottom_h = total - bottom_of_end;
        let mut focus_island = None;
        if let Some(focus_row) = focus_row.map(|row| row.min(n - 1)) {
            let focus_top: f32 = strides[..focus_row].iter().map(|s| s.max(0.0)).sum();
            let focus_bottom = focus_top + strides[focus_row].max(0.0);
            if focus_row < run_start {
                focus_island = Some(FocusHighlight {
                    row: focus_row,
                    lead_h: focus_top,
                });
                top_h = top_of_start - focus_bottom;
            } else if focus_row >= run_end {
                focus_island = Some(FocusHighlight {
                    row: focus_row,
                    lead_h: focus_top - bottom_of_end,
                });
                bottom_h = total - focus_bottom;
            }
        }

        ViewportWindow {
            run_start,
            run_end,
            top_h: top_h.max(0.0),
            bottom_h: bottom_h.max(0.0),
            focus_island: focus_island.map(|island| FocusHighlight {
                lead_h: island.lead_h.max(0.0),
                ..island
            }),
        }
    }

    /// 用 Fenwick 前缀索引定位挂载范围，避免每帧线性累加全部行高。
    pub(super) fn rendered_window_indexed(
        strides: &StrideIndex,
        scroll_y: f32,
        viewport_height: f32,
        overdraw: f32,
        focus_row: Option<usize>,
    ) -> ViewportWindow {
        let n = strides.values.len();
        if n == 0 {
            return ViewportWindow {
                run_start: 0,
                run_end: 0,
                top_h: 0.0,
                bottom_h: 0.0,
                focus_island: None,
            };
        }

        let band_top = scroll_y - overdraw;
        let band_bottom = scroll_y + viewport_height + overdraw;
        let mut run_start = strides.prefix_partition(band_top, true).min(n);
        let mut run_end = strides
            .prefix_partition(band_bottom, false)
            .saturating_add(1)
            .min(n);
        let mut top_of_start = strides.prefix_height(run_start);
        let mut bottom_of_end = strides.prefix_height(run_end);
        let total = strides.prefix_height(n);

        if run_start >= run_end {
            run_end = n;
            bottom_of_end = total;
            let floor = (total - viewport_height - overdraw).max(0.0);
            run_start = strides.prefix_partition(floor, false).min(n - 1);
            top_of_start = strides.prefix_height(run_start);
        }

        let mut top_h = top_of_start;
        let mut bottom_h = total - bottom_of_end;
        let mut focus_island = None;
        if let Some(focus_row) = focus_row.map(|row| row.min(n - 1)) {
            let focus_top = strides.prefix_height(focus_row);
            let focus_bottom = strides.prefix_height(focus_row + 1);
            if focus_row < run_start {
                focus_island = Some(FocusHighlight {
                    row: focus_row,
                    lead_h: focus_top,
                });
                top_h = top_of_start - focus_bottom;
            } else if focus_row >= run_end {
                focus_island = Some(FocusHighlight {
                    row: focus_row,
                    lead_h: focus_top - bottom_of_end,
                });
                bottom_h = total - focus_bottom;
            }
        }

        ViewportWindow {
            run_start,
            run_end,
            top_h: top_h.max(0.0),
            bottom_h: bottom_h.max(0.0),
            focus_island: focus_island.map(|island| FocusHighlight {
                lead_h: island.lead_h.max(0.0),
                ..island
            }),
        }
    }

    /// 按视口宽度线性插值编辑器内容列宽比例。
    /// 视口窄于 `centered_shrink_start` 时保持全宽，之后收缩，
    /// 到 `centered_shrink_end` 时收至 `centered_min_ratio`。
    pub(super) fn centered_column_ratio(
        viewport_width: f32,
        dimensions: &crate::theme::ThemeDimensions,
    ) -> f32 {
        if viewport_width <= dimensions.centered_shrink_start {
            return 1.0;
        }

        let t = ((viewport_width - dimensions.centered_shrink_start)
            / (dimensions.centered_shrink_end - dimensions.centered_shrink_start))
            .clamp(0.0, 1.0);
        1.0 - t * (1.0 - dimensions.centered_min_ratio)
    }

    /// 按视口宽度计算居中内容列的实际像素宽。
    pub(crate) fn centered_column_width(
        viewport_width: f32,
        dimensions: &crate::theme::ThemeDimensions,
    ) -> f32 {
        let available_content_width = (viewport_width - dimensions.editor_padding * 2.0).max(1.0);
        let centered_ratio = Self::centered_column_ratio(viewport_width, dimensions);
        (available_content_width * centered_ratio)
            .max(320.0)
            .min(available_content_width)
    }

    /// 构造系统窗口标题；有未保存修改时带上脏标记。
    pub(super) fn window_title(
        file_path: Option<&Path>,
        is_dirty: bool,
        i18n: &crate::i18n::I18nSnapshot,
    ) -> String {
        let base_title = if let Some(path) = file_path {
            format!(
                "Rumark - {}",
                path.file_name().map_or_else(
                    || path.to_string_lossy().to_string(),
                    |name| name.to_string_lossy().to_string()
                )
            )
        } else {
            "Rumark".to_string()
        };

        let dirty_marker = i18n.t("win_dirty_marker", &[]);
        if is_dirty && !dirty_marker.is_empty() {
            format!("{} {}", dirty_marker, base_title)
        } else {
            base_title
        }
    }

    /// 响应切换视图模式动作（渲染/源码互切）。
    pub(crate) fn on_toggle_view_mode_action(
        &mut self,
        _: &crate::components::ToggleViewMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_view_mode_from_ui(cx);
    }

    /// 从界面切换视图模式（先收尾指针选区并记录选区快照）。
    pub(super) fn toggle_view_mode_from_ui(&mut self, cx: &mut Context<Self>) {
        self.end_block_pointer_selection_sessions(cx);
        self.last_selection_snapshot = self.capture_source_selection_snapshot(cx);
        self.toggle_view_mode(cx);
    }

    /// 响应撤销动作。
    pub(crate) fn on_undo(
        &mut self,
        _: &crate::components::Undo,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_document(cx);
    }

    /// 响应重做动作。
    pub(crate) fn on_redo(
        &mut self,
        _: &crate::components::Redo,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.redo_document(cx);
    }

    /// 响应保存文档动作。
    pub(crate) fn on_save_document(
        &mut self,
        _: &crate::components::SaveDocument,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_save_document(cx);
    }

    /// 响应另存文档动作。
    pub(crate) fn on_save_document_as(
        &mut self,
        _: &crate::components::SaveDocumentAs,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_save_document_as(cx);
    }

    /// 响应退出应用动作。
    pub(crate) fn on_quit_application(
        &mut self,
        _: &crate::components::QuitApplication,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::app_menu::request_quit_application(cx);
    }

    /// 响应关闭窗口动作（先走未保存检查流程）。
    pub(crate) fn on_close_window(
        &mut self,
        _: &crate::components::CloseWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_close_current_window(window, cx);
    }

    /// 响应安装命令行工具动作。
    pub(crate) fn on_install_cli_tool(
        &mut self,
        _: &crate::components::InstallCliTool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::app_menu::install_cli_tool(cx);
    }

    /// 响应卸载命令行工具动作。
    pub(crate) fn on_uninstall_cli_tool(
        &mut self,
        _: &crate::components::UninstallCliTool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::app_menu::uninstall_cli_tool(cx);
    }

    /// 在渲染与源码视图之间切换并重建文档块。
    pub(crate) fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        if self.document_loading {
            return;
        }
        self.end_block_pointer_selection_sessions(cx);
        let selection_snapshot = self.capture_source_selection_snapshot(cx);
        self.clear_cross_block_selection(cx);
        match self.view_mode {
            ViewMode::Rendered => {
                let markdown = self.document.markdown_text(cx);
                let block = Self::new_block(cx, BlockRecord::paragraph(markdown));
                block.update(cx, |block, _cx| block.enable_source_document_mode());
                self.document.replace_roots(vec![block], cx);
                self.view_mode = ViewMode::Source;
                self.table_cells.clear();
            }
            ViewMode::Source => {
                let source = self.document.raw_source_text(cx);
                let mut roots = Self::build_root_blocks_from_markdown(cx, &source);
                if roots.is_empty() {
                    roots.push(Self::new_block(cx, BlockRecord::paragraph(String::new())));
                }
                self.document.replace_roots(roots, cx);
                self.view_mode = ViewMode::Rendered;
                self.rebuild_table_runtimes(cx);
                self.rebuild_image_runtimes(cx);
            }
        }

        self.apply_selection_snapshot_in_current_mode(&selection_snapshot, cx);
        self.pending_scroll_active_block_into_view = true;
        self.pending_scroll_recheck_after_layout = true;
        self.last_scroll_viewport_size = None;
        self.pending_window_title_refresh = true;
        self.close_dialog_restore_focus = None;
        self.table_axis_preview = None;
        self.table_axis_selection = None;
        self.dismiss_contextual_overlays(cx);
        self.sync_table_axis_visuals(cx);
        self.refresh_stable_document_snapshot(cx);
        cx.notify();
    }

    /// 标记文档为脏并排队到下一渲染帧刷新窗口标题与编辑状态。
    pub(super) fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        self.last_document_edit = Some(Instant::now());
        if !self.document_dirty {
            self.document_dirty = true;
            self.pending_window_edited = true;
            self.pending_window_title_refresh = true;
            cx.notify();
        }
    }

    /// 请求把活动块滚动到视口内（延迟到下一帧执行）。
    pub(super) fn request_active_block_scroll_into_view(&mut self, cx: &mut Context<Self>) {
        self.pending_scroll_recheck_after_layout = true;
        if !self.pending_scroll_active_block_into_view {
            self.pending_scroll_active_block_into_view = true;
            cx.notify();
        }
    }

    /// 判断视口尺寸是否发生有效变化（容差内忽略抖动）。
    pub(super) fn viewport_size_changed(previous: Size<Pixels>, current: Size<Pixels>) -> bool {
        const EPSILON: f32 = 0.5;

        (f32::from(previous.width) - f32::from(current.width)).abs() > EPSILON
            || (f32::from(previous.height) - f32::from(current.height)).abs() > EPSILON
    }

    pub(crate) fn request_save_document(&mut self, cx: &mut Context<Self>) {
        if !self.pending_save {
            self.pending_save = true;
            cx.notify();
        }
    }

    /// 请求另存文档（延迟到渲染阶段执行）。
    pub(crate) fn request_save_document_as(&mut self, cx: &mut Context<Self>) {
        if !self.pending_save_as {
            self.pending_save_as = true;
            cx.notify();
        }
    }

    /// 请求弹出链接跳转确认（延迟到有窗口可用时执行）。
    pub(crate) fn request_open_link_prompt(
        &mut self,
        prompt_target: String,
        open_target: String,
        cx: &mut Context<Self>,
    ) {
        self.pending_open_link = Some(PendingLinkOpen {
            prompt_target,
            open_target,
        });
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::Editor;
    use crate::theme::ThemeDimensions;

    /// 构造视口尺寸（测试辅助）。
    fn size(width: f32, height: f32) -> rgpui::Size<rgpui::Pixels> {
        rgpui::Size {
            width: rgpui::px(width),
            height: rgpui::px(height),
        }
    }

    #[test]
    fn rendered_window_of_no_rows_is_an_empty_run() {
        let window = Editor::rendered_window(&[], 0.0, 300.0, 0.0, None);

        assert_eq!((window.run_start, window.run_end), (0, 0));
        assert_eq!(window.top_h, 0.0);
        assert_eq!(window.bottom_h, 0.0);
        assert_eq!(window.focus_island, None);
    }

    #[test]
    fn centered_column_width_stays_within_available_content_width() {
        let dimensions = ThemeDimensions::DEFAULT;

        // 窄视口保持全宽。
        let narrow = Editor::centered_column_width(1000.0, &dimensions);
        assert_eq!(narrow, 1000.0 - dimensions.editor_padding * 2.0);

        // 到达收缩终点后按最小比例取宽。
        let wide = Editor::centered_column_width(dimensions.centered_shrink_end, &dimensions);
        let available = dimensions.centered_shrink_end - dimensions.editor_padding * 2.0;
        assert!(
            (wide - available * dimensions.centered_min_ratio).abs() < 0.01,
            "wide: {wide}"
        );

        // 极窄视口下 320px 下限不得反噬可用宽度。
        assert_eq!(
            Editor::centered_column_width(200.0, &dimensions),
            200.0 - dimensions.editor_padding * 2.0
        );
    }

    #[test]
    fn window_title_shows_file_name_and_dirty_marker() {
        let manager = crate::i18n::I18nManager::new_with_language_id("en-US");
        let i18n = manager.snapshot();

        assert_eq!(Editor::window_title(None, false, &i18n), "Rumark");
        assert_eq!(
            Editor::window_title(Some(std::path::Path::new("/tmp/notes.md")), false, &i18n),
            "Rumark - notes.md"
        );

        let dirty = Editor::window_title(Some(std::path::Path::new("/tmp/notes.md")), true, &i18n);
        let marker = i18n.t("win_dirty_marker", &[]);
        assert_eq!(dirty, format!("{marker} Rumark - notes.md"));
    }

    #[test]
    fn viewport_size_change_ignores_sub_pixel_jitter() {
        assert!(!Editor::viewport_size_changed(
            size(800.0, 600.0),
            size(800.0, 600.0)
        ));
        assert!(!Editor::viewport_size_changed(
            size(800.0, 600.0),
            size(800.4, 600.4)
        ));
        assert!(Editor::viewport_size_changed(
            size(800.0, 600.0),
            size(802.0, 600.0)
        ));
        assert!(Editor::viewport_size_changed(
            size(800.0, 600.0),
            size(800.0, 601.0)
        ));
    }
}
