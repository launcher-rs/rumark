//! 原生表格单元格与坐标轴运行时状态。

use super::*;

impl Block {
    /// 判断是否为表格单元格。
    pub(crate) fn is_table_cell(&self) -> bool {
        self.table_cell_position.is_some()
    }

    /// 取单元格位置。
    pub(crate) fn table_cell_position(&self) -> Option<CellPosition> {
        self.table_cell_position
    }

    /// 取单元格列对齐方式。
    pub(crate) fn table_cell_alignment(&self) -> Option<TableColumnAlignment> {
        self.table_cell_alignment
    }

    /// 取单元格文本对齐方式。
    pub(crate) fn text_align(&self) -> TextAlign {
        match self
            .table_cell_alignment()
            .unwrap_or(TableColumnAlignment::Default)
        {
            TableColumnAlignment::Default | TableColumnAlignment::Left => TextAlign::Left,
            TableColumnAlignment::Center => TextAlign::Center,
            TableColumnAlignment::Right => TextAlign::Right,
        }
    }

    /// 设置表格单元格模式。
    pub(crate) fn set_table_cell_mode(
        &mut self,
        position: CellPosition,
        alignment: TableColumnAlignment,
    ) {
        self.table_cell_position = Some(position);
        self.table_cell_alignment = Some(alignment);
        self.edit_mode = EditMode::RenderedRich;
        self.reset_inline_projection();
        self.refresh_render_cache();
    }

    /// 设置表格运行时。
    pub(crate) fn set_table_runtime(&mut self, runtime: TableRuntime) {
        self.table_runtime = Some(runtime);
    }

    /// 清空表格运行时。
    pub(crate) fn clear_table_runtime(&mut self) {
        self.table_runtime = None;
        self.table_axis_preview = None;
        self.table_axis_selection = None;
        self.table_axis_highlight = TableAxisHighlight::None;
        self.table_append_column_edge_hovered = false;
        self.table_append_column_hovered = false;
        self.table_append_column_zone_hovered = false;
        self.table_append_column_button_hovered = false;
        self.table_append_column_close_task = None;
        self.table_append_row_edge_hovered = false;
        self.table_append_row_hovered = false;
        self.table_append_row_zone_hovered = false;
        self.table_append_row_button_hovered = false;
        self.table_append_row_close_task = None;
    }

    /// 设置表格坐标轴视觉状态。
    pub(crate) fn set_table_axis_visual_state(
        &mut self,
        preview: Option<AxisMarker>,
        selection: Option<AxisMarker>,
    ) {
        self.table_axis_preview = preview;
        self.table_axis_selection = selection;
    }

    /// 设置表格坐标轴高亮。
    pub(crate) fn set_table_axis_highlight(&mut self, highlight: TableAxisHighlight) {
        self.table_axis_highlight = highlight;
    }
}
