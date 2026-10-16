//! 原生 Markdown 表格数据模型与解析/序列化辅助。
//!
//! 表格在渲染模式下支持为根级与类引用容器内的原生块。仍在运行时安全子集之外的复杂嵌套上下文继续走纯 Markdown 回退路径。

use rgpui::{Entity, FontStyle, FontWeight, Pixels, SharedString, TextRun, Theme, Window, px};

use crate::components::{Block, InlineTextTree};
use crate::theme::{ThemeDimensions, ThemeTypography};

/// 表格分隔行声明的水平对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableColumnAlignment {
    /// 无显式对齐标记（`---`）。渲染居左，但与 [`Left`](Self::Left) 保持区分，以免无标记列在下次序列化时被静默改写为前导冒号。
    Default,
    /// 显式左对齐（`:---`）。
    Left,
    /// 居中对齐单元格（`:---:`）。
    Center,
    /// 右对齐单元格（`---:`）。
    Right,
}

/// 渲染模式原生表格维护界面可寻址的坐标轴种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAxisKind {
    /// 表格行轴。
    Row,
    /// 表格列轴。
    Column,
}

/// 单个原生表格内的行或列标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisMarker {
    pub kind: TableAxisKind,
    pub index: usize,
}

/// 表格轴右键菜单可执行的操作（随右键菜单点击经 `BlockEvent` 发往编辑器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAxisMenuOp {
    /// 列恢复默认无标记对齐。
    AlignColumnLeft,
    /// 列设为居中对齐。
    AlignColumnCenter,
    /// 列设为右对齐。
    AlignColumnRight,
    /// 列与左一列交换。
    MoveColumnLeft,
    /// 列与右一列交换。
    MoveColumnRight,
    /// 删除列（仅一列时移除整表）。
    DeleteColumn,
    /// 切换表头行显示样式。
    ToggleHeaders,
    /// 行与上一行交换。
    MoveRowUp,
    /// 行与下一行交换。
    MoveRowDown,
    /// 删除行（末行则移除整表）。
    DeleteRow,
}

/// 预览或选中表格坐标轴时的视觉强调等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableAxisHighlight {
    /// 无坐标轴强调。
    #[default]
    None,
    /// 悬停预览强调。
    Preview,
    /// 持久的选中坐标轴强调。
    Selected,
}

/// 原生表格块的持久单元格内容。
#[derive(Debug, Clone)]
pub struct TableModel {
    pub header: Vec<InlineTextTree>,
    pub rows: Vec<Vec<InlineTextTree>>,
    pub alignments: Vec<TableColumnAlignment>,
}

impl PartialEq for TableModel {
    /// 判断表格是否相等。
    fn eq(&self, other: &Self) -> bool {
        self.header == other.header
            && self.rows == other.rows
            && self.alignments == other.alignments
    }
}

impl Eq for TableModel {}

impl TableModel {
    /// 以空表创建：一个表头行、`body_rows` 个正文行与 `columns` 个左对齐列。
    pub fn new_empty(body_rows: usize, columns: usize) -> Self {
        let columns = columns.max(1);
        let header = (0..columns)
            .map(|_| InlineTextTree::plain(String::new()))
            .collect::<Vec<_>>();
        let rows = (0..body_rows.max(1))
            .map(|_| {
                (0..columns)
                    .map(|_| InlineTextTree::plain(String::new()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let alignments = vec![TableColumnAlignment::Default; columns];
        Self {
            header,
            rows,
            alignments,
        }
    }

    /// 取列数。
    pub(crate) fn column_count(&self) -> usize {
        self.header
            .len()
            .max(self.alignments.len())
            .max(self.rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1)
    }

    /// 补齐形状。
    fn normalize_shape(&mut self) {
        let columns = self.column_count();
        while self.header.len() < columns {
            self.header.push(InlineTextTree::plain(String::new()));
        }
        while self.alignments.len() < columns {
            self.alignments.push(TableColumnAlignment::Default);
        }
        for row in &mut self.rows {
            while row.len() < columns {
                row.push(InlineTextTree::plain(String::new()));
            }
        }
    }

    /// 追加一空正文行并保持当前列数。
    pub fn add_row(&mut self) {
        self.normalize_shape();
        let columns = self.column_count();
        self.rows.push(
            (0..columns)
                .map(|_| InlineTextTree::plain(String::new()))
                .collect(),
        );
    }

    /// 给表头与每个正文行追加一空列。
    pub fn add_column(&mut self, alignment: TableColumnAlignment) {
        self.normalize_shape();
        self.header.push(InlineTextTree::plain(String::new()));
        self.alignments.push(alignment);
        for row in &mut self.rows {
            row.push(InlineTextTree::plain(String::new()));
        }
    }

    /// 设置指定列的对齐，列不存在时忽略。
    pub fn assign_column_alignment(&mut self, column: usize, alignment: TableColumnAlignment) {
        self.normalize_shape();
        if let Some(slot) = self.alignments.get_mut(column) {
            *slot = alignment;
        }
    }

    /// 按可视索引交换两行，行 `0` 为表头，`1..=rows.len()` 为正文行。表头与首正文行交换即互换表头与正文内容，与行手柄把表头视为普通可移行一致。
    pub fn exchange_visual_rows(&mut self, row_a: usize, row_b: usize) {
        self.normalize_shape();
        let total = self.rows.len() + 1;
        if row_a >= total || row_b >= total || row_a == row_b {
            return;
        }
        match (row_a, row_b) {
            (0, other) | (other, 0) => {
                std::mem::swap(&mut self.header, &mut self.rows[other - 1]);
            }
            (a, b) => self.rows.swap(a - 1, b - 1),
        }
    }

    /// 跨表头、正文与对齐向量交换两列。
    pub fn exchange_columns(&mut self, col_a: usize, col_b: usize) {
        self.normalize_shape();
        let columns = self.column_count();
        if col_a >= columns || col_b >= columns || col_a == col_b {
            return;
        }

        self.header.swap(col_a, col_b);
        self.alignments.swap(col_a, col_b);
        for row in &mut self.rows {
            row.swap(col_a, col_b);
        }
    }

    /// 删除一正文行，至少保留一正文行。
    pub fn delete_body_row(&mut self, row_index: usize) {
        self.normalize_shape();
        if row_index >= self.rows.len() {
            return;
        }
        // 表格可仅剩表头，表头再被删除时编辑器移除整个块。
        self.rows.remove(row_index);
    }

    /// 删除表头行并把首正文行提升为表头。无正文行时返回假且保持表格不变，管道表格须保留表头行。
    pub fn delete_header_row(&mut self) -> bool {
        self.normalize_shape();
        if self.rows.is_empty() {
            return false;
        }
        self.header = self.rows.remove(0);
        true
    }

    /// 删除一列，至少保留一列。
    pub fn remove_column(&mut self, col_index: usize) {
        self.normalize_shape();
        let columns = self.column_count();
        if columns <= 1 || col_index >= columns {
            return;
        }

        self.header.remove(col_index);
        self.alignments.remove(col_index);
        for row in &mut self.rows {
            row.remove(col_index);
        }
    }

    /// 按可视行号插入一空行：`0` 为表头上方（原表头降为首正文行），其余为正文行间/末尾。
    pub fn insert_row_at_visual(&mut self, visual_row: usize) {
        self.normalize_shape();
        let columns = self.column_count();
        let empty_row = || {
            (0..columns)
                .map(|_| InlineTextTree::plain(String::new()))
                .collect::<Vec<_>>()
        };
        if visual_row == 0 {
            let old_header = std::mem::replace(&mut self.header, empty_row());
            self.rows.insert(0, old_header);
        } else {
            let body_index = (visual_row - 1).min(self.rows.len());
            self.rows.insert(body_index, empty_row());
        }
    }

    /// 在指定列左侧插入一空列（含表头、对齐与全部正文行，越界时追到末尾）。
    pub fn insert_column_at(&mut self, column: usize, alignment: TableColumnAlignment) {
        self.normalize_shape();
        let columns = self.column_count();
        let index = column.min(columns);
        self.header.insert(
            index.min(self.header.len()),
            InlineTextTree::plain(String::new()),
        );
        self.alignments
            .insert(index.min(self.alignments.len()), alignment);
        for row in &mut self.rows {
            row.insert(index.min(row.len()), InlineTextTree::plain(String::new()));
        }
    }
}

/// 原生表格每行共享的响应式宽度占比。
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLayout {
    fractions: Vec<f32>,
}

impl ColumnLayout {
    /// 构造等宽布局。
    pub fn equal(column_count: usize) -> Self {
        let column_count = column_count.max(1);
        let fraction = 1.0 / column_count as f32;
        Self {
            fractions: vec![fraction; column_count],
        }
    }

    #[cfg(test)]
    pub(crate) fn fractions(&self) -> &[f32] {
        &self.fractions
    }

    /// 取指定列占比。
    pub fn fraction(&self, column: usize) -> f32 {
        self.fractions
            .get(column)
            .copied()
            .unwrap_or_else(|| 1.0 / self.fractions.len().max(1) as f32)
    }

    /// 测量表格列布局。
    pub fn measure(
        table: &TableModel,
        table_width: f32,
        window: &mut Window,
        theme: &Theme,
    ) -> Self {
        let preferred_widths = measure_preferred_column_widths(table, window, theme)
            .into_iter()
            .map(f32::from)
            .collect::<Vec<_>>();
        Self::from_preferred_widths(&preferred_widths, table_width, minimum_column_width())
    }

    /// 按期望宽度构造布局。
    pub fn from_preferred_widths(
        preferred_widths: &[f32],
        table_width: f32,
        min_column_width: f32,
    ) -> Self {
        if preferred_widths.is_empty() {
            return Self::equal(1);
        }

        let column_count = preferred_widths.len();
        let safe_table_width = table_width.max(1.0);
        let equal_share = safe_table_width / column_count as f32;
        if preferred_widths
            .iter()
            .all(|preferred| *preferred <= equal_share + f32::EPSILON)
        {
            return Self::equal(column_count);
        }

        let floor_width = min_column_width
            .max(0.0)
            .min(safe_table_width / column_count as f32);
        let weights = preferred_widths
            .iter()
            .map(|preferred| preferred.max(equal_share))
            .collect::<Vec<_>>();
        let mut assigned_widths = vec![0.0; column_count];
        let mut remaining_indices = (0..column_count).collect::<Vec<_>>();
        let mut remaining_width = safe_table_width;

        loop {
            if remaining_indices.is_empty() {
                break;
            }

            let weight_sum = remaining_indices
                .iter()
                .map(|index| weights[*index])
                .sum::<f32>();
            if weight_sum <= f32::EPSILON {
                let share = remaining_width / remaining_indices.len() as f32;
                for index in remaining_indices {
                    assigned_widths[index] = share;
                }
                break;
            }

            let mut newly_floored = Vec::new();
            for index in &remaining_indices {
                let width = remaining_width * (weights[*index] / weight_sum);
                if width < floor_width - f32::EPSILON {
                    newly_floored.push(*index);
                } else {
                    assigned_widths[*index] = width;
                }
            }

            if newly_floored.is_empty() {
                break;
            }

            if newly_floored.len() == remaining_indices.len() {
                let share = remaining_width / remaining_indices.len() as f32;
                for index in remaining_indices {
                    assigned_widths[index] = share;
                }
                break;
            }

            for index in &newly_floored {
                assigned_widths[*index] = floor_width;
                remaining_width -= floor_width;
            }
            remaining_indices.retain(|index| !newly_floored.contains(index));
        }

        let assigned_sum = assigned_widths.iter().sum::<f32>();
        if assigned_sum <= f32::EPSILON {
            return Self::equal(column_count);
        }

        let fractions = assigned_widths
            .into_iter()
            .map(|width| width / assigned_sum)
            .collect::<Vec<_>>();
        Self { fractions }
    }
}

/// 仅供运行时使用的表格内单元格位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPosition {
    /// 从零计数的可视行。表头为行 `0`，首正文行为 `1`。
    pub row: usize,
    pub column: usize,
}

impl CellPosition {
    /// 是否为表头。
    pub fn is_header(self) -> bool {
        self.row == 0
    }

    /// 取正文行索引。
    pub fn body_row_index(self) -> Option<usize> {
        self.row.checked_sub(1)
    }
}

/// 挂在单个原生表格块上的运行时单元格编辑器。
#[derive(Clone)]
pub struct TableRuntime {
    pub header: Vec<Entity<Block>>,
    pub rows: Vec<Vec<Entity<Block>>>,
}

impl TableRuntime {
    /// 取指定位置单元格。
    pub fn cell(&self, position: CellPosition) -> Option<Entity<Block>> {
        if position.is_header() {
            self.header.get(position.column).cloned()
        } else {
            self.rows
                .get(position.body_row_index()?)
                .and_then(|row| row.get(position.column))
                .cloned()
        }
    }
}

/// 测量列期望宽度。
fn measure_preferred_column_widths(
    table: &TableModel,
    window: &mut Window,
    theme: &Theme,
) -> Vec<Pixels> {
    let column_count = table.header.len().max(1);
    let mut preferred_widths = vec![Pixels::ZERO; column_count];

    for (column, cell) in table.header.iter().enumerate() {
        preferred_widths[column] =
            preferred_widths[column].max(measure_cell_preferred_width(cell, true, window, theme));
    }

    for row in &table.rows {
        for (column, cell) in row.iter().enumerate().take(column_count) {
            preferred_widths[column] = preferred_widths[column]
                .max(measure_cell_preferred_width(cell, false, window, theme));
        }
    }

    preferred_widths
}

/// 测量单元格期望宽度。
fn measure_cell_preferred_width(
    cell: &InlineTextTree,
    is_header: bool,
    window: &mut Window,
    theme: &Theme,
) -> Pixels {
    let cache = cell.render_cache();
    let text = cache.plain_text();
    let cell_chrome_width = cell_chrome_width();
    if text.is_empty() {
        return cell_chrome_width;
    }

    let rendered_text = SharedString::from(text.to_string());
    let mut font = window.text_style().font();
    if is_header && font.weight < FontWeight::MEDIUM {
        font.weight = FontWeight::MEDIUM;
    }
    let base_run = TextRun {
        len: rendered_text.len(),
        font,
        color: theme.foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = measurement_runs(&cache, &base_run);
    let font_size = px(ThemeTypography::DEFAULT.text_size);

    let text_width = window
        .text_system()
        .shape_text(rendered_text, font_size, &runs, None, None)
        .ok()
        .map(|lines| {
            lines
                .iter()
                .map(|line| line.width())
                .max()
                .unwrap_or(Pixels::ZERO)
        })
        .unwrap_or(Pixels::ZERO);

    text_width + cell_chrome_width
}

/// 构造测量用文本跑。
fn measurement_runs(
    cache: &crate::components::InlineRenderCache,
    base_run: &TextRun,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, cache.plain_text().len()];
    for span in cache.spans() {
        boundaries.push(span.range.start);
        boundaries.push(span.range.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut runs = Vec::new();
    for boundary_pair in boundaries.windows(2) {
        let start = boundary_pair[0];
        let end = boundary_pair[1];
        if start >= end {
            continue;
        }

        let inline_style = cache.style_at(start);
        let mut font = base_run.font.clone();
        if inline_style.bold && font.weight < FontWeight::BOLD {
            font.weight = FontWeight::BOLD;
        }
        if inline_style.italic {
            font.style = FontStyle::Italic;
        }

        runs.push(TextRun {
            len: end - start,
            font,
            color: base_run.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    if runs.is_empty() {
        vec![base_run.clone()]
    } else {
        runs
    }
}

/// 取单元格装饰宽度。
fn cell_chrome_width() -> Pixels {
    px(ThemeDimensions::DEFAULT.table_cell_padding_x * 2.0 + 2.0)
}

/// 取最小列宽。
fn minimum_column_width() -> f32 {
    ThemeDimensions::DEFAULT.table_cell_padding_x * 2.0
        + ThemeTypography::DEFAULT.text_size * 4.0
        + 2.0
}

/// 去掉表格行缩进。
fn strip_table_indent(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    (indent <= 3).then_some(&line[indent..])
}

/// 切分表格单元格。
fn split_table_cells(line: &str) -> Option<Vec<String>> {
    let rest = strip_table_indent(line)?.trim_end();
    if rest.is_empty() {
        return None;
    }
    // 外管道可选：存在时去掉，使无外管道行与有管道行切分一致。
    let inner = rest.strip_prefix('|').unwrap_or(rest);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaping = false;

    for ch in inner.chars() {
        if escaping {
            match ch {
                '|' | '\\' => current.push(ch),
                _ => {
                    current.push('\\');
                    current.push(ch);
                }
            }
            escaping = false;
            continue;
        }

        match ch {
            '\\' => escaping = true,
            '|' => {
                cells.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    cells.push(current.trim().to_string());
    Some(cells)
}

/// 解析对齐单元格。
fn parse_alignment_cell(cell: &str) -> Option<TableColumnAlignment> {
    let trimmed = cell.trim();
    if trimmed.len() < 3 {
        return None;
    }

    let left = trimmed.starts_with(':');
    let right = trimmed.ends_with(':');
    let core = trimmed.trim_start_matches(':').trim_end_matches(':');
    if core.len() < 3 || !core.chars().all(|ch| ch == '-') {
        return None;
    }

    Some(match (left, right) {
        (true, true) => TableColumnAlignment::Center,
        (false, true) => TableColumnAlignment::Right,
        (true, false) => TableColumnAlignment::Left,
        (false, false) => TableColumnAlignment::Default,
    })
}

/// 序列化对齐标记。
fn serialize_alignment(alignment: TableColumnAlignment) -> &'static str {
    match alignment {
        TableColumnAlignment::Default => "---",
        TableColumnAlignment::Left => ":---",
        TableColumnAlignment::Center => ":---:",
        TableColumnAlignment::Right => "---:",
    }
}

/// 序列化表格单元格 Markdown。
pub(crate) fn cell_to_markdown(tree: &InlineTextTree) -> String {
    tree.serialize_markdown()
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('\n', " ")
}

/// 序列化单行。
fn serialize_row<'a>(cells: impl IntoIterator<Item = &'a InlineTextTree>) -> String {
    let rendered = cells.into_iter().map(cell_to_markdown).collect::<Vec<_>>();
    format!("| {} |", rendered.join(" | "))
}

/// 判断一行是否为当前容器范围内的候选原生表格行。
pub fn is_table_candidate_line(line: &str) -> bool {
    strip_table_indent(line)
        .map(str::trim_end)
        .is_some_and(|rest| rest.starts_with('|'))
}

/// 取 `line` 的管道分隔单元格数，外层管道视为可选（GFM），故 `Name | Score` 这类无外管道行亦可识别。单列行返回 `None`，以免把含孤立 `|` 的散文误判为表格行。
pub fn table_cell_count(line: &str) -> Option<usize> {
    split_table_cells(line)
        .map(|cells| cells.len())
        .filter(|count| *count >= 2)
}

/// 判断 `line` 是否可能是表格行，含无外管道的 GFM 行。
pub fn is_table_row_candidate(line: &str) -> bool {
    table_cell_count(line).is_some()
}

/// 收集当前容器范围内连续的表格候选区。
pub fn collect_table_candidate(lines: &[String], start: usize) -> usize {
    let mut index = start + 1;
    while index < lines.len() && is_table_candidate_line(&lines[index]) {
        index += 1;
    }
    index
}

/// 把管道表格区解析为原生表格数据。
pub fn parse_table_region(lines: &[String]) -> Option<TableModel> {
    if lines.len() < 2 {
        return None;
    }

    let header = split_table_cells(&lines[0])?;
    let alignment_cells = split_table_cells(&lines[1])?;
    if header.is_empty() || alignment_cells.len() != header.len() {
        return None;
    }

    let alignments = alignment_cells
        .iter()
        .map(|cell| parse_alignment_cell(cell))
        .collect::<Option<Vec<_>>>()?;

    let mut rows = Vec::new();
    for line in &lines[2..] {
        // 按表头宽度归一化正文行：短行补空格，长行截断尾格，而非使整表失效。
        let mut cells = split_table_cells(line)?;
        cells.resize(header.len(), String::new());
        rows.push(
            cells
                .into_iter()
                .map(|cell| InlineTextTree::from_markdown(&cell))
                .collect::<Vec<_>>(),
        );
    }

    Some(TableModel {
        header: header
            .into_iter()
            .map(|cell| InlineTextTree::from_markdown(&cell))
            .collect(),
        rows,
        alignments,
    })
}

/// 判断 `line` 是否为恰好 `columns` 个单元格且每格均为合法对齐说明的分隔行。
fn is_delimiter_row(line: &str, columns: usize) -> bool {
    split_table_cells(line).is_some_and(|cells| {
        cells.len() == columns
            && cells
                .iter()
                .all(|cell| parse_alignment_cell(cell).is_some())
    })
}

/// 在不要求外管道时探测从 `start` 开始的表格：`lines[start]` 为多列表头且紧跟匹配分隔行时返回区域尾（不含）。正文行延至下一空行，符合 GFM。普通散文返回 `None`，孤立 `|` 永不误判；单列无管道候选亦拒绝，因其与 Setext 标题歧义。
pub fn collect_pipeless_table(lines: &[String], start: usize) -> Option<usize> {
    let header = split_table_cells(lines.get(start)?)?;
    if header.len() < 2 {
        return None;
    }
    if !is_delimiter_row(lines.get(start + 1)?, header.len()) {
        return None;
    }

    let mut end = start + 2;
    while end < lines.len() && !lines[end].trim().is_empty() {
        end += 1;
    }
    Some(end)
}

/// 判断根级行是否为候选原生表格行。
pub fn is_root_table_candidate_line(line: &str) -> bool {
    is_table_candidate_line(line)
}

/// 收集连续的根级表格候选区。
pub fn collect_root_table_candidate(lines: &[String], start: usize) -> usize {
    collect_table_candidate(lines, start)
}

/// 把根级管道表格区解析为原生表格数据。
pub fn parse_root_table_region(lines: &[String]) -> Option<TableModel> {
    parse_table_region(lines)
}

/// 解析单条表格正文行并归一化为 `columns` 格（短补空、长截断）。该行根本不是表格行时返回 `None`。
pub fn parse_table_body_row(line: &str, columns: usize) -> Option<Vec<InlineTextTree>> {
    let mut cells = split_table_cells(line)?;
    cells.resize(columns, String::new());
    Some(
        cells
            .into_iter()
            .map(|cell| InlineTextTree::from_markdown(&cell))
            .collect(),
    )
}

/// 把原生表格数据序列化为规范管道表格 Markdown 行。
pub fn table_to_markdown_lines(table: &TableModel) -> Vec<String> {
    let mut lines = Vec::with_capacity(2 + table.rows.len());
    lines.push(serialize_row(table.header.iter()));
    lines.push(format!(
        "| {} |",
        table
            .alignments
            .iter()
            .map(|alignment| serialize_alignment(*alignment))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    lines.extend(table.rows.iter().map(|row| serialize_row(row.iter())));
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        ColumnLayout, TableColumnAlignment, TableModel, collect_pipeless_table,
        collect_root_table_candidate, is_root_table_candidate_line, parse_root_table_region,
        table_to_markdown_lines,
    };
    use crate::components::InlineTextTree;

    /// 断言两浮点数近似相等（测试辅助）。
    fn assert_close(left: f32, right: f32) {
        assert!(
            (left - right).abs() < 0.0001,
            "expected {left} to be close to {right}"
        );
    }

    #[test]
    fn parses_valid_root_table_region() {
        let lines = vec![
            "| Left | Center | Right |".to_string(),
            "| :--- | :---: | ---: |".to_string(),
            "| a | b | c |".to_string(),
        ];
        let table = parse_root_table_region(&lines).expect("table should parse");
        assert_eq!(table.alignments.len(), 3);
        assert_eq!(
            table.alignments,
            vec![
                TableColumnAlignment::Left,
                TableColumnAlignment::Center,
                TableColumnAlignment::Right
            ]
        );
        assert_eq!(table.header[0].serialize_markdown(), "Left");
        assert_eq!(table.rows[0][2].serialize_markdown(), "c");
    }

    #[test]
    fn rejects_invalid_alignment_row() {
        let lines = vec!["| Left | Right |".to_string(), "| nope | --- |".to_string()];
        assert!(parse_root_table_region(&lines).is_none());
    }

    #[test]
    fn rejects_alignment_row_with_wrong_column_count() {
        let lines = vec!["| A | B | C |".to_string(), "| --- | --- |".to_string()];
        assert!(parse_root_table_region(&lines).is_none());
    }

    #[test]
    fn preserves_explicit_left_alignment_colon() {
        // ":---" is explicit left and must survive a parse/serialize round-trip
        // instead of being silently rewritten to a bare "---".
        let lines = vec![
            "| L | D | R |".to_string(),
            "| :--- | --- | ---: |".to_string(),
            "| a | b | c |".to_string(),
        ];
        let table = parse_root_table_region(&lines).expect("table should parse");
        assert_eq!(
            table.alignments,
            vec![
                TableColumnAlignment::Left,
                TableColumnAlignment::Default,
                TableColumnAlignment::Right
            ]
        );
        assert_eq!(table_to_markdown_lines(&table)[1], "| :--- | --- | ---: |");
    }

    #[test]
    fn pads_short_body_rows_and_truncates_long_ones() {
        let lines = vec![
            "| A | B | C |".to_string(),
            "| --- | --- | --- |".to_string(),
            "| short |".to_string(),
            "| 1 | 2 | 3 | 4 |".to_string(),
        ];
        let table = parse_root_table_region(&lines).expect("table should parse");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].len(), 3);
        assert_eq!(table.rows[0][0].serialize_markdown(), "short");
        assert!(table.rows[0][1].serialize_markdown().is_empty());
        assert!(table.rows[0][2].serialize_markdown().is_empty());
        assert_eq!(table.rows[1].len(), 3);
        assert_eq!(table.rows[1][2].serialize_markdown(), "3");
    }

    #[test]
    fn parses_pipeless_table() {
        let lines = vec![
            "Name | Score".to_string(),
            "--- | ---".to_string(),
            "Alice | 10".to_string(),
            "Bob | 7".to_string(),
        ];
        let end = collect_pipeless_table(&lines, 0).expect("region");
        assert_eq!(end, 4);
        let table = parse_root_table_region(&lines[..end]).expect("table should parse");
        assert_eq!(table.header.len(), 2);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.header[0].serialize_markdown(), "Name");
        assert_eq!(table.rows[1][1].serialize_markdown(), "7");
    }

    #[test]
    fn prose_with_pipe_is_not_a_pipeless_table() {
        let lines = vec!["this | that".to_string(), "and the next line".to_string()];
        assert!(collect_pipeless_table(&lines, 0).is_none());
    }

    #[test]
    fn pipeless_table_requires_valid_delimiter_row() {
        let lines = vec!["Name | Score".to_string(), "Alice | 10".to_string()];
        assert!(collect_pipeless_table(&lines, 0).is_none());
    }

    #[test]
    fn single_column_pipeless_is_not_a_table() {
        // Ambiguous with a setext heading; must not be captured as a table.
        let lines = vec!["Title".to_string(), "---".to_string()];
        assert!(collect_pipeless_table(&lines, 0).is_none());
    }

    #[test]
    fn serializes_canonical_pipe_table() {
        let table = TableModel {
            header: vec![
                InlineTextTree::from_markdown("**bold**"),
                InlineTextTree::from_markdown("[link](https://example.com)"),
            ],
            rows: vec![vec![
                InlineTextTree::plain("A | B".to_string()),
                InlineTextTree::plain("value".to_string()),
            ]],
            alignments: vec![TableColumnAlignment::Default, TableColumnAlignment::Right],
        };
        assert_eq!(
            table_to_markdown_lines(&table),
            vec![
                "| **bold** | [link](https://example.com) |".to_string(),
                "| --- | ---: |".to_string(),
                "| A \\| B | value |".to_string(),
            ]
        );
    }

    #[test]
    fn detects_root_table_candidate_runs() {
        let lines = vec![
            "| A | B |".to_string(),
            "| --- | --- |".to_string(),
            "| 1 | 2 |".to_string(),
            "paragraph".to_string(),
        ];
        assert!(is_root_table_candidate_line(&lines[0]));
        assert_eq!(collect_root_table_candidate(&lines, 0), 3);
    }

    #[test]
    fn equal_share_fast_path_keeps_columns_uniform() {
        let layout = ColumnLayout::from_preferred_widths(&[32.0, 64.0, 48.0], 360.0, 60.0);
        let fractions = layout.fractions();
        assert_eq!(fractions.len(), 3);
        assert_close(fractions[0], 1.0 / 3.0);
        assert_close(fractions[1], 1.0 / 3.0);
        assert_close(fractions[2], 1.0 / 3.0);
    }

    #[test]
    fn content_pressure_redistributes_width_across_the_whole_column() {
        let layout = ColumnLayout::from_preferred_widths(&[48.0, 220.0, 48.0], 360.0, 60.0);
        let fractions = layout.fractions();
        assert_eq!(fractions.len(), 3);
        assert!(fractions[1] > fractions[0]);
        assert!(fractions[1] > fractions[2]);
        assert_close(fractions[0], fractions[2]);
    }

    #[test]
    fn minimum_column_floor_prevents_neighbor_collapse() {
        let layout = ColumnLayout::from_preferred_widths(&[16.0, 900.0, 16.0], 300.0, 70.0);
        let fractions = layout.fractions();
        let widths = fractions
            .iter()
            .map(|fraction| fraction * 300.0)
            .collect::<Vec<_>>();
        assert!(widths[0] >= 70.0 - 0.001);
        assert!(widths[2] >= 70.0 - 0.001);
        assert_close(fractions.iter().sum::<f32>(), 1.0);
    }

    #[test]
    fn moderate_single_cell_growth_stays_equal_when_share_is_sufficient() {
        let layout = ColumnLayout::from_preferred_widths(&[56.0, 92.0, 56.0], 360.0, 60.0);
        let fractions = layout.fractions();
        assert_close(fractions[0], 1.0 / 3.0);
        assert_close(fractions[1], 1.0 / 3.0);
        assert_close(fractions[2], 1.0 / 3.0);
    }

    #[test]
    fn append_row_preserves_column_count_and_creates_empty_cells() {
        let mut table = TableModel::new_empty(1, 3);
        table.add_row();

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1].len(), 3);
        assert!(
            table.rows[1]
                .iter()
                .all(|cell| cell.serialize_markdown().is_empty())
        );
    }

    #[test]
    fn append_column_extends_every_row_and_uses_requested_alignment() {
        let mut table = TableModel {
            header: vec![
                InlineTextTree::plain("A".to_string()),
                InlineTextTree::plain("B".to_string()),
            ],
            rows: vec![
                vec![
                    InlineTextTree::plain("1".to_string()),
                    InlineTextTree::plain("2".to_string()),
                ],
                vec![
                    InlineTextTree::plain("3".to_string()),
                    InlineTextTree::plain("4".to_string()),
                ],
            ],
            alignments: vec![TableColumnAlignment::Left, TableColumnAlignment::Right],
        };

        table.add_column(TableColumnAlignment::Right);

        assert_eq!(table.header.len(), 3);
        assert_eq!(table.rows[0].len(), 3);
        assert_eq!(table.rows[1].len(), 3);
        assert_eq!(
            table.alignments,
            vec![
                TableColumnAlignment::Left,
                TableColumnAlignment::Right,
                TableColumnAlignment::Right,
            ]
        );
        assert!(table.header[2].serialize_markdown().is_empty());
        assert!(table.rows[0][2].serialize_markdown().is_empty());
        assert!(table.rows[1][2].serialize_markdown().is_empty());
    }

    #[test]
    fn append_column_pads_missing_alignments_with_default() {
        let mut table = TableModel {
            header: vec![InlineTextTree::plain("A".to_string())],
            rows: vec![vec![InlineTextTree::plain("1".to_string())]],
            alignments: Vec::new(),
        };

        table.add_column(TableColumnAlignment::Left);

        assert_eq!(
            table.alignments,
            vec![TableColumnAlignment::Default, TableColumnAlignment::Left]
        );
        assert_eq!(table.header.len(), 2);
        assert_eq!(table.rows[0].len(), 2);
    }

    #[test]
    fn set_column_alignment_updates_requested_column() {
        let mut table = TableModel::new_empty(2, 3);
        table.assign_column_alignment(1, TableColumnAlignment::Center);
        assert_eq!(
            table.alignments,
            vec![
                TableColumnAlignment::Default,
                TableColumnAlignment::Center,
                TableColumnAlignment::Default
            ]
        );
    }

    #[test]
    fn swap_visual_rows_exchanges_header_with_first_body_row() {
        let mut table = TableModel {
            header: vec![InlineTextTree::plain("A".to_string())],
            rows: vec![
                vec![InlineTextTree::plain("1".to_string())],
                vec![InlineTextTree::plain("2".to_string())],
            ],
            alignments: vec![TableColumnAlignment::Left],
        };
        // Visual row 0 is the header; swapping it with visual row 1 exchanges
        // header and first-body content.
        table.exchange_visual_rows(0, 1);
        assert_eq!(table.header[0].serialize_markdown(), "1");
        assert_eq!(table.rows[0][0].serialize_markdown(), "A");
        assert_eq!(table.rows[1][0].serialize_markdown(), "2");

        // Two body rows (visual 1 and 2) swap like ordinary rows.
        table.exchange_visual_rows(1, 2);
        assert_eq!(table.rows[0][0].serialize_markdown(), "2");
        assert_eq!(table.rows[1][0].serialize_markdown(), "A");
    }

    #[test]
    fn swap_columns_exchanges_header_body_and_alignment() {
        let mut table = TableModel {
            header: vec![
                InlineTextTree::plain("A".to_string()),
                InlineTextTree::plain("B".to_string()),
            ],
            rows: vec![vec![
                InlineTextTree::plain("1".to_string()),
                InlineTextTree::plain("2".to_string()),
            ]],
            alignments: vec![TableColumnAlignment::Left, TableColumnAlignment::Right],
        };
        table.exchange_columns(0, 1);
        assert_eq!(table.header[0].serialize_markdown(), "B");
        assert_eq!(table.rows[0][0].serialize_markdown(), "2");
        assert_eq!(
            table.alignments,
            vec![TableColumnAlignment::Right, TableColumnAlignment::Left]
        );
    }

    #[test]
    fn remove_body_row_can_empty_the_table() {
        let mut table = TableModel::new_empty(2, 2);
        table.delete_body_row(0);
        assert_eq!(table.rows.len(), 1);
        table.delete_body_row(0);
        // The last body row can be removed, leaving a header-only table.
        assert!(table.rows.is_empty());
        // Out-of-range removal is a no-op.
        table.delete_body_row(0);
        assert!(table.rows.is_empty());
    }

    #[test]
    fn remove_header_row_promotes_first_body_row() {
        let mut table = parse_root_table_region(&[
            "| A | B |".to_string(),
            "| --- | --- |".to_string(),
            "| 1 | 2 |".to_string(),
            "| 3 | 4 |".to_string(),
        ])
        .expect("valid table");

        assert!(table.delete_header_row());
        assert_eq!(table.header[0].serialize_markdown(), "1");
        assert_eq!(table.header[1].serialize_markdown(), "2");
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0][0].serialize_markdown(), "3");

        // Promoting the last remaining row leaves a header-only table.
        assert!(table.delete_header_row());
        assert!(table.rows.is_empty());
        assert!(!table.delete_header_row());
        assert_eq!(table.header[0].serialize_markdown(), "3");
    }

    #[test]
    fn remove_column_preserves_at_least_one_column() {
        let mut table = TableModel::new_empty(2, 2);
        table.remove_column(0);
        assert_eq!(table.column_count(), 1);
        table.remove_column(0);
        assert_eq!(table.column_count(), 1);
    }

    #[test]
    fn insert_row_at_visual_handles_header_body_and_overflow() {
        let mut table = parse_root_table_region(&[
            "| A |".to_string(),
            "| --- |".to_string(),
            "| 1 |".to_string(),
        ])
        .expect("valid table");

        // 可视行 0 上方插入：新空表头，原表头降为首正文行。
        table.insert_row_at_visual(0);
        assert!(table.header[0].serialize_markdown().is_empty());
        assert_eq!(table.rows[0][0].serialize_markdown(), "A");
        assert_eq!(table.rows[1][0].serialize_markdown(), "1");

        // 正文行间插入。
        table.insert_row_at_visual(2);
        assert_eq!(table.rows.len(), 3);
        assert!(table.rows[1][0].serialize_markdown().is_empty());
        assert_eq!(table.rows[2][0].serialize_markdown(), "1");

        // 越界追到末尾。
        table.insert_row_at_visual(99);
        assert_eq!(table.rows.len(), 4);
        assert!(table.rows[3][0].serialize_markdown().is_empty());
    }

    #[test]
    fn insert_column_at_extends_every_row_with_alignment() {
        let mut table = parse_root_table_region(&[
            "| A | B |".to_string(),
            "| --- | --- |".to_string(),
            "| 1 | 2 |".to_string(),
        ])
        .expect("valid table");

        table.insert_column_at(1, TableColumnAlignment::Center);
        assert_eq!(table.column_count(), 3);
        assert!(table.header[1].serialize_markdown().is_empty());
        assert!(table.rows[0][1].serialize_markdown().is_empty());
        assert_eq!(table.alignments[1], TableColumnAlignment::Center);
        assert_eq!(table.header[0].serialize_markdown(), "A");
        assert_eq!(table.header[2].serialize_markdown(), "B");

        // 越界追到末尾。
        table.insert_column_at(99, TableColumnAlignment::Right);
        assert_eq!(table.column_count(), 4);
        assert_eq!(table.alignments[3], TableColumnAlignment::Right);
    }
}
