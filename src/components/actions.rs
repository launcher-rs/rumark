//! 块编辑与应用级窗口/菜单命令的动作定义与按键绑定。
//!
//! 文本编辑动作限定在每个块的 `"BlockEditor"` 按键上下文中。窗口与菜单命令使用全局绑定，即使焦点落在对话框或按钮等非块界面上仍然可用。

use std::collections::{BTreeMap, BTreeSet};

use rgpui::*;
use schemars::JsonSchema;
use serde::Deserialize;

actions!(
    rumark,
    [
        Newline,
        DeleteBack,
        Delete,
        WordDeleteBack,
        WordDeleteForward,
        FocusPrev,
        FocusNext,
        MoveLeft,
        MoveRight,
        WordMoveLeft,
        WordMoveRight,
        Home,
        End,
        BlockUp,
        BlockDown,
        PageUp,
        PageDown,
        JumpToTop,
        JumpToBottom,
        SelectLeft,
        SelectRight,
        WordSelectLeft,
        WordSelectRight,
        SelectHome,
        SelectEnd,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
        BoldSelection,
        ItalicSelection,
        UnderlineSelection,
        CodeSelection,
        IndentBlock,
        OutdentBlock,
        ExitCodeBlock,
        SaveDocument,
        NewWindow,
        OpenFile,
        OpenPreferences,
        NoRecentFiles,
        ClearRecentFiles,
        SaveDocumentAs,
        AddLanguageConfig,
        AddThemeConfig,
        QuitApplication,
        CloseWindow,
        ShowAbout,
        InstallCliTool,
        UninstallCliTool,
        DismissTransientUi,
        ToggleViewMode,
        ToggleWorkspace,
        InsertLink,
        InsertImage,
        InsertTable,
        InsertCodeBlock,
        InsertMathBlock,
        InsertQuote,
        InsertBulletedList,
        InsertNumberedList,
        InsertTaskList,
        InsertSeparator,
        InsertParagraphAbove,
        InsertParagraphBelow,
        ConvertHeading1,
        ConvertHeading2,
        ConvertHeading3,
        ConvertHeading4,
        ConvertHeading5,
        ConvertHeading6,
        ConvertParagraph,
        ExportHtml,
        ExportPdf,
        ExportDocx,
        CopyHtml,
        CopyMarkdown,
        FindInDocument,
        TableInsertRowAbove,
        TableInsertRowBelow,
        TableDeleteRow,
        TableInsertColumnLeft,
        TableInsertColumnRight,
        TableDeleteColumn,
        TableMoveRowUp,
        TableMoveRowDown,
        TableMoveColumnLeft,
        TableMoveColumnRight,
        TableAlignColumnLeft,
        TableAlignColumnCenter,
        TableAlignColumnRight,
        ToggleTableHeaders,
    ]
);

/// 从应用级主题注册表中选择主题。
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, rgpui::Action)]
#[action(namespace = rumark)]
#[serde(deny_unknown_fields)]
pub struct SelectTheme {
    /// 内置主题目录中的稳定主题标识。
    pub theme_id: String,
}

/// 从应用级语言注册表中选择界面语言。
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, rgpui::Action)]
#[action(namespace = rumark)]
#[serde(deny_unknown_fields)]
pub struct SelectLanguage {
    /// 内置语言目录中的稳定语言标识。
    pub language_id: String,
}

/// 打开历史记录中的 Markdown 文件路径。
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, rgpui::Action)]
#[action(namespace = rumark)]
#[serde(deny_unknown_fields)]
pub struct OpenRecentFile {
    /// 存放在拾墨最近文件历史中的路径。
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 快捷键分组。
pub(crate) enum ShortcutCategory {
    File,
    Edit,
    Navigation,
    Formatting,
    Block,
    Other,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// 快捷键命令。
pub(crate) enum ShortcutCommand {
    Newline,
    DeleteBack,
    Delete,
    WordDeleteBack,
    WordDeleteForward,
    FocusPrev,
    FocusNext,
    MoveLeft,
    MoveRight,
    WordMoveLeft,
    WordMoveRight,
    Home,
    End,
    BlockUp,
    BlockDown,
    PageUp,
    PageDown,
    JumpToTop,
    JumpToBottom,
    SelectLeft,
    SelectRight,
    WordSelectLeft,
    WordSelectRight,
    SelectHome,
    SelectEnd,
    SelectAll,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    BoldSelection,
    ItalicSelection,
    UnderlineSelection,
    CodeSelection,
    IndentBlock,
    OutdentBlock,
    ExitCodeBlock,
    SaveDocument,
    SaveDocumentAs,
    NewWindow,
    OpenFile,
    QuitApplication,
    CloseWindow,
    DismissTransientUi,
    ToggleViewMode,
    ToggleWorkspace,
    InsertLink,
    InsertImage,
    InsertTable,
    InsertCodeBlock,
    InsertMathBlock,
    InsertQuote,
    InsertBulletedList,
    InsertNumberedList,
    InsertTaskList,
    InsertSeparator,
    InsertParagraphAbove,
    InsertParagraphBelow,
    ConvertHeading1,
    ConvertHeading2,
    ConvertHeading3,
    ConvertHeading4,
    ConvertHeading5,
    ConvertHeading6,
    ConvertParagraph,
    FindInDocument,
    TableInsertRowAbove,
    TableInsertRowBelow,
    TableDeleteRow,
    TableInsertColumnLeft,
    TableInsertColumnRight,
    TableDeleteColumn,
    TableMoveRowUp,
    TableMoveRowDown,
    TableMoveColumnLeft,
    TableMoveColumnRight,
    TableAlignColumnLeft,
    TableAlignColumnCenter,
    TableAlignColumnRight,
    ToggleTableHeaders,
}

#[derive(Clone, Copy, Debug)]
/// 快捷键定义。
pub(crate) struct ShortcutDefinition {
    pub(crate) command: ShortcutCommand,
    pub(crate) id: &'static str,
    pub(crate) category: ShortcutCategory,
    pub(crate) default_keys: &'static [&'static str],
    pub(crate) context: Option<&'static str>,
}

const BLOCK_KEY_CONTEXT: Option<&str> = Some("BlockEditor");
const SELECT_ALL_ID: &str = "select_all";
const LEGACY_SELECT_ALL_IDS: &[&str] = &[
    "select_all_source_text",
    "select_focused_block_text_rendered",
];

// 在 macOS 上 cmd-q 为系统退出快捷键；Windows/Linux 使用 Alt+F4（由系统处理）。
#[cfg(target_os = "macos")]
const QUIT_KEYS: &[&str] = &["cmd-q"];
#[cfg(not(target_os = "macos"))]
const QUIT_KEYS: &[&str] = &[];

// 在 macOS 上 cmd-w 关闭当前窗口；其他平台无需应用级绑定。
#[cfg(target_os = "macos")]
const CLOSE_WINDOW_KEYS: &[&str] = &["cmd-w"];
#[cfg(not(target_os = "macos"))]
const CLOSE_WINDOW_KEYS: &[&str] = &["ctrl-q"];

const KEY_BINDINGS: &[ShortcutDefinition] = &[
    ShortcutDefinition {
        command: ShortcutCommand::Newline,
        id: "newline",
        category: ShortcutCategory::Block,
        default_keys: &["enter"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::DeleteBack,
        id: "delete_back",
        category: ShortcutCategory::Edit,
        default_keys: &["backspace"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Delete,
        id: "delete",
        category: ShortcutCategory::Edit,
        default_keys: &["delete"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordDeleteBack,
        id: "word_delete_back",
        category: ShortcutCategory::Edit,
        default_keys: &["ctrl-backspace", "alt-backspace"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordDeleteForward,
        id: "word_delete_forward",
        category: ShortcutCategory::Edit,
        default_keys: &["ctrl-delete", "alt-delete"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::FocusPrev,
        id: "focus_prev",
        category: ShortcutCategory::Navigation,
        default_keys: &["up"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::FocusNext,
        id: "focus_next",
        category: ShortcutCategory::Navigation,
        default_keys: &["down"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::MoveLeft,
        id: "move_left",
        category: ShortcutCategory::Navigation,
        default_keys: &["left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::MoveRight,
        id: "move_right",
        category: ShortcutCategory::Navigation,
        default_keys: &["right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordMoveLeft,
        id: "word_move_left",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-left", "alt-left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordMoveRight,
        id: "word_move_right",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-right", "alt-right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Home,
        id: "home",
        category: ShortcutCategory::Navigation,
        default_keys: &["home"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::End,
        id: "end",
        category: ShortcutCategory::Navigation,
        default_keys: &["end"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::BlockUp,
        id: "block_up",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-up", "alt-up"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::BlockDown,
        id: "block_down",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-down", "alt-down"],
        context: BLOCK_KEY_CONTEXT,
    },
    // 页面滚动与文档跳转作用于编辑器视口而非单个块，因此使用全局绑定（无上下文），在渲染与源码模式下均保持可用。
    ShortcutDefinition {
        command: ShortcutCommand::PageUp,
        id: "page_up",
        category: ShortcutCategory::Navigation,
        default_keys: &["pageup"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::PageDown,
        id: "page_down",
        category: ShortcutCategory::Navigation,
        default_keys: &["pagedown"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::JumpToTop,
        id: "jump_to_top",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-home", "cmd-up"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::JumpToBottom,
        id: "jump_to_bottom",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-end", "cmd-down"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SelectLeft,
        id: "select_left",
        category: ShortcutCategory::Navigation,
        default_keys: &["shift-left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SelectRight,
        id: "select_right",
        category: ShortcutCategory::Navigation,
        default_keys: &["shift-right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordSelectLeft,
        id: "word_select_left",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-shift-left", "alt-shift-left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::WordSelectRight,
        id: "word_select_right",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-shift-right", "alt-shift-right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SelectHome,
        id: "select_home",
        category: ShortcutCategory::Navigation,
        default_keys: &["shift-home"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SelectEnd,
        id: "select_end",
        category: ShortcutCategory::Navigation,
        default_keys: &["shift-end"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SelectAll,
        id: SELECT_ALL_ID,
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-a", "ctrl-a"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Copy,
        id: "copy",
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-c", "ctrl-c"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Cut,
        id: "cut",
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-x", "ctrl-x"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Paste,
        id: "paste",
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-v", "ctrl-v"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Undo,
        id: "undo",
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-z", "ctrl-z"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::Redo,
        id: "redo",
        category: ShortcutCategory::Edit,
        default_keys: &["cmd-shift-z", "ctrl-y"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::BoldSelection,
        id: "bold_selection",
        category: ShortcutCategory::Formatting,
        default_keys: &["cmd-b", "ctrl-b"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ItalicSelection,
        id: "italic_selection",
        category: ShortcutCategory::Formatting,
        default_keys: &["cmd-i", "ctrl-i"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::UnderlineSelection,
        id: "underline_selection",
        category: ShortcutCategory::Formatting,
        default_keys: &["cmd-u", "ctrl-u"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::CodeSelection,
        id: "code_selection",
        category: ShortcutCategory::Formatting,
        default_keys: &["cmd-`", "ctrl-`"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::IndentBlock,
        id: "indent_block",
        category: ShortcutCategory::Block,
        default_keys: &["tab"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::OutdentBlock,
        id: "outdent_block",
        category: ShortcutCategory::Block,
        default_keys: &["shift-tab"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ExitCodeBlock,
        id: "exit_code_block",
        category: ShortcutCategory::Block,
        default_keys: &["cmd-enter", "ctrl-enter"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SaveDocument,
        id: "save_document",
        category: ShortcutCategory::File,
        default_keys: &["cmd-s", "ctrl-s"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::SaveDocumentAs,
        id: "save_document_as",
        category: ShortcutCategory::File,
        default_keys: &["cmd-shift-s", "ctrl-shift-s"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::NewWindow,
        id: "new_window",
        category: ShortcutCategory::File,
        default_keys: &["cmd-n", "ctrl-n"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::OpenFile,
        id: "open_file",
        category: ShortcutCategory::File,
        default_keys: &["cmd-o", "ctrl-o"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::QuitApplication,
        id: "quit_application",
        category: ShortcutCategory::File,
        default_keys: QUIT_KEYS,
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::CloseWindow,
        id: "close_window",
        category: ShortcutCategory::File,
        default_keys: CLOSE_WINDOW_KEYS,
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::DismissTransientUi,
        id: "dismiss_transient_ui",
        category: ShortcutCategory::Other,
        default_keys: &["escape"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ToggleViewMode,
        id: "toggle_view_mode",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-/", "ctrl-tab", "cmd-tab"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ToggleWorkspace,
        id: "toggle_workspace",
        category: ShortcutCategory::Navigation,
        default_keys: &["ctrl-w"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertLink,
        id: "insert_link",
        category: ShortcutCategory::Edit,
        default_keys: &["ctrl-k"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertImage,
        id: "insert_image",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-i"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertTable,
        id: "insert_table",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-t"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertCodeBlock,
        id: "insert_code_block",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-e"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertMathBlock,
        id: "insert_math",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-m"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertQuote,
        id: "insert_quote",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-q"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertBulletedList,
        id: "insert_bulleted_list",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-b"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertNumberedList,
        id: "insert_numbered_list",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-o"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertTaskList,
        id: "insert_task_list",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-g"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertSeparator,
        id: "insert_separator",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-h"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertParagraphAbove,
        id: "insert_paragraph_above",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-up"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::InsertParagraphBelow,
        id: "insert_paragraph_below",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-shift-down"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading1,
        id: "convert_heading_1",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-1"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading2,
        id: "convert_heading_2",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-2"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading3,
        id: "convert_heading_3",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-3"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading4,
        id: "convert_heading_4",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-4"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading5,
        id: "convert_heading_5",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-5"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertHeading6,
        id: "convert_heading_6",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-6"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ConvertParagraph,
        id: "convert_paragraph",
        category: ShortcutCategory::Formatting,
        default_keys: &["ctrl-0"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::FindInDocument,
        id: "find_in_document",
        category: ShortcutCategory::Edit,
        // 全局绑定：焦点在工作区树或查找框时同样可用。
        default_keys: &["ctrl-f"],
        context: None,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableInsertRowAbove,
        id: "table_insert_row_above",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-up"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableInsertRowBelow,
        id: "table_insert_row_below",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-down"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableDeleteRow,
        id: "table_delete_row",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-backspace"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableInsertColumnLeft,
        id: "table_insert_column_left",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableInsertColumnRight,
        id: "table_insert_column_right",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableDeleteColumn,
        id: "table_delete_column",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-delete"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableMoveRowUp,
        id: "table_move_row_up",
        category: ShortcutCategory::Block,
        default_keys: &["alt-shift-up"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableMoveRowDown,
        id: "table_move_row_down",
        category: ShortcutCategory::Block,
        default_keys: &["alt-shift-down"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableMoveColumnLeft,
        id: "table_move_column_left",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-shift-left"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableMoveColumnRight,
        id: "table_move_column_right",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-shift-right"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableAlignColumnLeft,
        id: "table_align_column_left",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-l"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableAlignColumnCenter,
        id: "table_align_column_center",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-c"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::TableAlignColumnRight,
        id: "table_align_column_right",
        category: ShortcutCategory::Block,
        default_keys: &["ctrl-alt-r"],
        context: BLOCK_KEY_CONTEXT,
    },
    ShortcutDefinition {
        command: ShortcutCommand::ToggleTableHeaders,
        id: "toggle_table_headers",
        category: ShortcutCategory::Block,
        // 全局绑定：无需聚焦表格即可切换表头显示。
        default_keys: &["ctrl-alt-h"],
        context: None,
    },
];

/// 返回全部快捷键定义。
pub(crate) fn shortcut_definitions() -> &'static [ShortcutDefinition] {
    KEY_BINDINGS
}

/// 规范化快捷键按键列表。
pub(crate) fn normalize_shortcut_keys(keys: &[String]) -> Option<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for key in keys {
        let parsed = Keystroke::parse(key.trim()).ok()?;
        if parsed.is_ime_in_progress() {
            return None;
        }
        let key = parsed.unparse();
        if seen.insert(key.clone()) {
            normalized.push(key);
        }
    }
    (!normalized.is_empty()).then_some(normalized)
}

/// 返回快捷键定义的默认按键。
fn default_keys(definition: ShortcutDefinition) -> Vec<String> {
    definition
        .default_keys
        .iter()
        .map(|key| key.to_string())
        .collect()
}

/// 应并入现代快捷键定义的旧偏好键。
///
/// 全选曾用源码/渲染两个独立命令表示，现已合并为一个动作循环处理，因此旧偏好向前映射到 `select_all`，加载时不再静默丢弃。
fn legacy_shortcut_ids(definition: ShortcutDefinition) -> &'static [&'static str] {
    match definition.command {
        ShortcutCommand::SelectAll => LEGACY_SELECT_ALL_IDS,
        _ => &[],
    }
}

/// 读取用户快捷键覆盖，优先使用当前标识再看别名。
fn configured_shortcut_keys(
    definition: ShortcutDefinition,
    config: &BTreeMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    config
        .get(definition.id)
        .and_then(|keys| normalize_shortcut_keys(keys))
        .or_else(|| {
            legacy_shortcut_ids(definition).iter().find_map(|id| {
                config
                    .get(*id)
                    .and_then(|keys| normalize_shortcut_keys(keys))
            })
        })
}

/// 判断两组快捷键是否冲突。
fn shortcuts_conflict(
    left: ShortcutDefinition,
    left_keys: &[String],
    right: ShortcutDefinition,
    right_keys: &[String],
) -> bool {
    left.context == right.context && left_keys.iter().any(|key| right_keys.contains(key))
}

/// 规范化快捷键配置。
pub(crate) fn normalize_shortcut_config(
    config: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let mut effective: BTreeMap<&'static str, (bool, Vec<String>)> = BTreeMap::new();
    for definition in KEY_BINDINGS {
        let custom = configured_shortcut_keys(*definition, config);
        effective.insert(
            definition.id,
            match custom {
                Some(keys) if keys != default_keys(*definition) => (true, keys),
                _ => (false, default_keys(*definition)),
            },
        );
    }

    loop {
        let mut conflicted = BTreeSet::new();
        for (index, left) in KEY_BINDINGS.iter().enumerate() {
            let (left_custom, left_keys) = effective.get(left.id).expect("known shortcut");
            for right in KEY_BINDINGS.iter().skip(index + 1) {
                let (right_custom, right_keys) = effective.get(right.id).expect("known shortcut");
                if shortcuts_conflict(*left, left_keys, *right, right_keys) {
                    if *left_custom {
                        conflicted.insert(left.id);
                    }
                    if *right_custom {
                        conflicted.insert(right.id);
                    }
                }
            }
        }

        if conflicted.is_empty() {
            break;
        }

        for id in conflicted {
            if let Some(definition) = KEY_BINDINGS.iter().find(|definition| definition.id == id) {
                effective.insert(definition.id, (false, default_keys(*definition)));
            }
        }
    }

    effective
        .into_iter()
        .filter_map(|(id, (custom, keys))| custom.then_some((id.to_string(), keys)))
        .collect()
}

/// 解析指定命令的最终快捷键。
pub(crate) fn resolved_shortcut_keys(
    config: &BTreeMap<String, Vec<String>>,
    command: ShortcutCommand,
) -> Vec<String> {
    let normalized = normalize_shortcut_config(config);
    let definition = KEY_BINDINGS
        .iter()
        .find(|definition| definition.command == command)
        .expect("known shortcut command");
    normalized
        .get(definition.id)
        .cloned()
        .unwrap_or_else(|| default_keys(*definition))
}

/// 检查提议按键是否与其他命令冲突。
pub(crate) fn shortcut_conflict_for(
    command: ShortcutCommand,
    proposed_keys: &[String],
    config: &BTreeMap<String, Vec<String>>,
) -> Option<ShortcutDefinition> {
    let definition = KEY_BINDINGS
        .iter()
        .find(|definition| definition.command == command)?;
    let proposed_keys = normalize_shortcut_keys(proposed_keys)?;
    for other in KEY_BINDINGS.iter().filter(|other| other.command != command) {
        let other_keys = resolved_shortcut_keys(config, other.command);
        if shortcuts_conflict(*definition, &proposed_keys, *other, &other_keys) {
            return Some(*other);
        }
    }
    None
}

/// 为指定命令构造按键绑定。
fn key_binding_for(
    command: ShortcutCommand,
    key: &str,
    context: Option<&'static str>,
) -> KeyBinding {
    match command {
        ShortcutCommand::Newline => KeyBinding::new(key, Newline, context),
        ShortcutCommand::DeleteBack => KeyBinding::new(key, DeleteBack, context),
        ShortcutCommand::Delete => KeyBinding::new(key, Delete, context),
        ShortcutCommand::WordDeleteBack => KeyBinding::new(key, WordDeleteBack, context),
        ShortcutCommand::WordDeleteForward => KeyBinding::new(key, WordDeleteForward, context),
        ShortcutCommand::FocusPrev => KeyBinding::new(key, FocusPrev, context),
        ShortcutCommand::FocusNext => KeyBinding::new(key, FocusNext, context),
        ShortcutCommand::MoveLeft => KeyBinding::new(key, MoveLeft, context),
        ShortcutCommand::MoveRight => KeyBinding::new(key, MoveRight, context),
        ShortcutCommand::WordMoveLeft => KeyBinding::new(key, WordMoveLeft, context),
        ShortcutCommand::WordMoveRight => KeyBinding::new(key, WordMoveRight, context),
        ShortcutCommand::Home => KeyBinding::new(key, Home, context),
        ShortcutCommand::End => KeyBinding::new(key, End, context),
        ShortcutCommand::BlockUp => KeyBinding::new(key, BlockUp, context),
        ShortcutCommand::BlockDown => KeyBinding::new(key, BlockDown, context),
        ShortcutCommand::PageUp => KeyBinding::new(key, PageUp, context),
        ShortcutCommand::PageDown => KeyBinding::new(key, PageDown, context),
        ShortcutCommand::JumpToTop => KeyBinding::new(key, JumpToTop, context),
        ShortcutCommand::JumpToBottom => KeyBinding::new(key, JumpToBottom, context),
        ShortcutCommand::SelectLeft => KeyBinding::new(key, SelectLeft, context),
        ShortcutCommand::SelectRight => KeyBinding::new(key, SelectRight, context),
        ShortcutCommand::WordSelectLeft => KeyBinding::new(key, WordSelectLeft, context),
        ShortcutCommand::WordSelectRight => KeyBinding::new(key, WordSelectRight, context),
        ShortcutCommand::SelectHome => KeyBinding::new(key, SelectHome, context),
        ShortcutCommand::SelectEnd => KeyBinding::new(key, SelectEnd, context),
        ShortcutCommand::SelectAll => KeyBinding::new(key, SelectAll, context),
        ShortcutCommand::Copy => KeyBinding::new(key, Copy, context),
        ShortcutCommand::Cut => KeyBinding::new(key, Cut, context),
        ShortcutCommand::Paste => KeyBinding::new(key, Paste, context),
        ShortcutCommand::Undo => KeyBinding::new(key, Undo, context),
        ShortcutCommand::Redo => KeyBinding::new(key, Redo, context),
        ShortcutCommand::BoldSelection => KeyBinding::new(key, BoldSelection, context),
        ShortcutCommand::ItalicSelection => KeyBinding::new(key, ItalicSelection, context),
        ShortcutCommand::UnderlineSelection => KeyBinding::new(key, UnderlineSelection, context),
        ShortcutCommand::CodeSelection => KeyBinding::new(key, CodeSelection, context),
        ShortcutCommand::IndentBlock => KeyBinding::new(key, IndentBlock, context),
        ShortcutCommand::OutdentBlock => KeyBinding::new(key, OutdentBlock, context),
        ShortcutCommand::ExitCodeBlock => KeyBinding::new(key, ExitCodeBlock, context),
        ShortcutCommand::SaveDocument => KeyBinding::new(key, SaveDocument, context),
        ShortcutCommand::SaveDocumentAs => KeyBinding::new(key, SaveDocumentAs, context),
        ShortcutCommand::NewWindow => KeyBinding::new(key, NewWindow, context),
        ShortcutCommand::OpenFile => KeyBinding::new(key, OpenFile, context),
        ShortcutCommand::QuitApplication => KeyBinding::new(key, QuitApplication, context),
        ShortcutCommand::CloseWindow => KeyBinding::new(key, CloseWindow, context),
        ShortcutCommand::DismissTransientUi => KeyBinding::new(key, DismissTransientUi, context),
        ShortcutCommand::ToggleViewMode => KeyBinding::new(key, ToggleViewMode, context),
        ShortcutCommand::ToggleWorkspace => KeyBinding::new(key, ToggleWorkspace, context),
        ShortcutCommand::InsertLink => KeyBinding::new(key, InsertLink, context),
        ShortcutCommand::InsertImage => KeyBinding::new(key, InsertImage, context),
        ShortcutCommand::InsertTable => KeyBinding::new(key, InsertTable, context),
        ShortcutCommand::InsertCodeBlock => KeyBinding::new(key, InsertCodeBlock, context),
        ShortcutCommand::InsertMathBlock => KeyBinding::new(key, InsertMathBlock, context),
        ShortcutCommand::InsertQuote => KeyBinding::new(key, InsertQuote, context),
        ShortcutCommand::InsertBulletedList => KeyBinding::new(key, InsertBulletedList, context),
        ShortcutCommand::InsertNumberedList => KeyBinding::new(key, InsertNumberedList, context),
        ShortcutCommand::InsertTaskList => KeyBinding::new(key, InsertTaskList, context),
        ShortcutCommand::InsertSeparator => KeyBinding::new(key, InsertSeparator, context),
        ShortcutCommand::InsertParagraphAbove => {
            KeyBinding::new(key, InsertParagraphAbove, context)
        }
        ShortcutCommand::InsertParagraphBelow => {
            KeyBinding::new(key, InsertParagraphBelow, context)
        }
        ShortcutCommand::ConvertHeading1 => KeyBinding::new(key, ConvertHeading1, context),
        ShortcutCommand::ConvertHeading2 => KeyBinding::new(key, ConvertHeading2, context),
        ShortcutCommand::ConvertHeading3 => KeyBinding::new(key, ConvertHeading3, context),
        ShortcutCommand::ConvertHeading4 => KeyBinding::new(key, ConvertHeading4, context),
        ShortcutCommand::ConvertHeading5 => KeyBinding::new(key, ConvertHeading5, context),
        ShortcutCommand::ConvertHeading6 => KeyBinding::new(key, ConvertHeading6, context),
        ShortcutCommand::ConvertParagraph => KeyBinding::new(key, ConvertParagraph, context),
        ShortcutCommand::FindInDocument => KeyBinding::new(key, FindInDocument, context),
        ShortcutCommand::TableInsertRowAbove => KeyBinding::new(key, TableInsertRowAbove, context),
        ShortcutCommand::TableInsertRowBelow => KeyBinding::new(key, TableInsertRowBelow, context),
        ShortcutCommand::TableDeleteRow => KeyBinding::new(key, TableDeleteRow, context),
        ShortcutCommand::TableInsertColumnLeft => {
            KeyBinding::new(key, TableInsertColumnLeft, context)
        }
        ShortcutCommand::TableInsertColumnRight => {
            KeyBinding::new(key, TableInsertColumnRight, context)
        }
        ShortcutCommand::TableDeleteColumn => KeyBinding::new(key, TableDeleteColumn, context),
        ShortcutCommand::TableMoveRowUp => KeyBinding::new(key, TableMoveRowUp, context),
        ShortcutCommand::TableMoveRowDown => KeyBinding::new(key, TableMoveRowDown, context),
        ShortcutCommand::TableMoveColumnLeft => KeyBinding::new(key, TableMoveColumnLeft, context),
        ShortcutCommand::TableMoveColumnRight => {
            KeyBinding::new(key, TableMoveColumnRight, context)
        }
        ShortcutCommand::TableAlignColumnLeft => {
            KeyBinding::new(key, TableAlignColumnLeft, context)
        }
        ShortcutCommand::TableAlignColumnCenter => {
            KeyBinding::new(key, TableAlignColumnCenter, context)
        }
        ShortcutCommand::TableAlignColumnRight => {
            KeyBinding::new(key, TableAlignColumnRight, context)
        }
        ShortcutCommand::ToggleTableHeaders => KeyBinding::new(key, ToggleTableHeaders, context),
    }
}

/// 解析全部快捷键绑定。
pub(crate) fn resolved_keybindings(config: &BTreeMap<String, Vec<String>>) -> Vec<KeyBinding> {
    let normalized = normalize_shortcut_config(config);
    let mut bindings = Vec::new();
    for definition in KEY_BINDINGS {
        let keys = normalized
            .get(definition.id)
            .cloned()
            .unwrap_or_else(|| default_keys(*definition));
        bindings.extend(
            keys.iter()
                .map(|key| key_binding_for(definition.command, key, definition.context)),
        );
    }
    bindings
}

/// 安装快捷键绑定。
pub(crate) fn install_keybindings(cx: &mut App, config: &BTreeMap<String, Vec<String>>) {
    cx.bind_keys(resolved_keybindings(config));
}

/// 注册块编辑器的按键绑定。
#[allow(dead_code)]
pub fn init(cx: &mut App) {
    install_keybindings(cx, &BTreeMap::new());
}

/// 使用给定配置初始化快捷键绑定。
pub(crate) fn init_with_keybindings(cx: &mut App, config: &BTreeMap<String, Vec<String>>) {
    install_keybindings(cx, config);
}

#[cfg(test)]
mod tests {
    use super::{
        ShortcutCommand, normalize_shortcut_config, resolved_shortcut_keys, shortcut_conflict_for,
    };
    use std::collections::BTreeMap;

    #[test]
    fn custom_shortcut_replaces_command_defaults() {
        let mut config = BTreeMap::new();
        config.insert("save_document".to_string(), vec!["ctrl-alt-s".to_string()]);

        assert_eq!(
            resolved_shortcut_keys(&config, ShortcutCommand::SaveDocument),
            vec!["ctrl-alt-s".to_string()]
        );
    }

    #[test]
    fn toggle_view_mode_has_default_shortcuts() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::ToggleViewMode),
            vec![
                "ctrl-/".to_string(),
                "ctrl-tab".to_string(),
                "cmd-tab".to_string()
            ]
        );
    }

    #[test]
    fn toggle_workspace_has_default_shortcuts() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::ToggleWorkspace),
            vec!["ctrl-w".to_string()]
        );
    }

    #[test]
    fn insert_link_has_default_shortcut_without_conflicts() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::InsertLink),
            vec!["ctrl-k".to_string()]
        );
        assert!(
            shortcut_conflict_for(
                ShortcutCommand::InsertLink,
                &["ctrl-k".to_string()],
                &BTreeMap::new()
            )
            .is_none()
        );
    }

    #[test]
    fn insert_submenu_has_distinct_default_shortcuts_without_conflicts() {
        let cases = [
            (ShortcutCommand::InsertImage, "ctrl-shift-i"),
            (ShortcutCommand::InsertTable, "ctrl-shift-t"),
            (ShortcutCommand::InsertCodeBlock, "ctrl-shift-e"),
            (ShortcutCommand::InsertMathBlock, "ctrl-shift-m"),
            (ShortcutCommand::InsertQuote, "ctrl-shift-q"),
            (ShortcutCommand::InsertBulletedList, "ctrl-shift-b"),
            (ShortcutCommand::InsertNumberedList, "ctrl-shift-o"),
            (ShortcutCommand::InsertTaskList, "ctrl-shift-g"),
            (ShortcutCommand::InsertSeparator, "ctrl-shift-h"),
            (ShortcutCommand::InsertParagraphAbove, "ctrl-shift-up"),
            (ShortcutCommand::InsertParagraphBelow, "ctrl-shift-down"),
        ];
        for (command, expected) in cases {
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), command),
                vec![expected.to_string()],
                "{command:?} default"
            );
            assert!(
                shortcut_conflict_for(command, &[expected.to_string()], &BTreeMap::new()).is_none(),
                "{command:?} should not conflict"
            );
        }
    }

    #[test]
    fn convert_heading_shortcuts_have_ctrl_number_defaults_without_conflicts() {
        let cases = [
            (ShortcutCommand::ConvertHeading1, "ctrl-1"),
            (ShortcutCommand::ConvertHeading2, "ctrl-2"),
            (ShortcutCommand::ConvertHeading3, "ctrl-3"),
            (ShortcutCommand::ConvertHeading4, "ctrl-4"),
            (ShortcutCommand::ConvertHeading5, "ctrl-5"),
            (ShortcutCommand::ConvertHeading6, "ctrl-6"),
            (ShortcutCommand::ConvertParagraph, "ctrl-0"),
        ];
        for (command, expected) in cases {
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), command),
                vec![expected.to_string()],
                "{command:?} default"
            );
            assert!(
                shortcut_conflict_for(command, &[expected.to_string()], &BTreeMap::new()).is_none(),
                "{command:?} should not conflict"
            );
        }
    }

    #[test]
    fn find_in_document_has_global_ctrl_f_default_without_conflicts() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::FindInDocument),
            vec!["ctrl-f".to_string()]
        );
        assert!(
            shortcut_conflict_for(
                ShortcutCommand::FindInDocument,
                &["ctrl-f".to_string()],
                &BTreeMap::new()
            )
            .is_none()
        );
    }

    #[test]
    fn table_row_column_shortcuts_have_distinct_defaults_without_conflicts() {
        // 行列增删/移动/对齐/表头开关共 14 个命令，默认键互不相同且无冲突。
        let cases = [
            (ShortcutCommand::TableInsertRowAbove, "ctrl-alt-up"),
            (ShortcutCommand::TableInsertRowBelow, "ctrl-alt-down"),
            (ShortcutCommand::TableDeleteRow, "ctrl-alt-backspace"),
            (ShortcutCommand::TableInsertColumnLeft, "ctrl-alt-left"),
            (ShortcutCommand::TableInsertColumnRight, "ctrl-alt-right"),
            (ShortcutCommand::TableDeleteColumn, "ctrl-alt-delete"),
            (ShortcutCommand::TableMoveRowUp, "alt-shift-up"),
            (ShortcutCommand::TableMoveRowDown, "alt-shift-down"),
            (ShortcutCommand::TableMoveColumnLeft, "ctrl-alt-shift-left"),
            (
                ShortcutCommand::TableMoveColumnRight,
                "ctrl-alt-shift-right",
            ),
            (ShortcutCommand::TableAlignColumnLeft, "ctrl-alt-l"),
            (ShortcutCommand::TableAlignColumnCenter, "ctrl-alt-c"),
            (ShortcutCommand::TableAlignColumnRight, "ctrl-alt-r"),
            (ShortcutCommand::ToggleTableHeaders, "ctrl-alt-h"),
        ];
        for (command, expected) in cases {
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), command),
                vec![expected.to_string()],
                "{command:?} default"
            );
            assert!(
                shortcut_conflict_for(command, &[expected.to_string()], &BTreeMap::new()).is_none(),
                "{command:?} should not conflict"
            );
        }
    }

    #[test]
    fn select_all_has_default_shortcuts() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::SelectAll),
            vec!["cmd-a".to_string(), "ctrl-a".to_string()]
        );
        assert!(
            shortcut_conflict_for(
                ShortcutCommand::SelectAll,
                &["cmd-a".to_string(), "ctrl-a".to_string()],
                &BTreeMap::new()
            )
            .is_none()
        );
    }

    #[test]
    fn select_all_shortcut_can_be_customized() {
        let mut config = BTreeMap::new();
        config.insert("select_all".to_string(), vec!["ctrl-shift-a".to_string()]);

        assert_eq!(
            resolved_shortcut_keys(&config, ShortcutCommand::SelectAll),
            vec!["ctrl-shift-a".to_string()]
        );
    }

    #[test]
    fn legacy_split_select_all_shortcut_config_maps_to_unified_command() {
        let mut config = BTreeMap::new();
        config.insert(
            "select_all_source_text".to_string(),
            vec!["ctrl-shift-a".to_string()],
        );

        assert_eq!(
            resolved_shortcut_keys(&config, ShortcutCommand::SelectAll),
            vec!["ctrl-shift-a".to_string()]
        );

        let normalized = normalize_shortcut_config(&config);
        assert_eq!(
            normalized.get("select_all"),
            Some(&vec!["ctrl-shift-a".to_string()])
        );
        assert!(!normalized.contains_key("select_all_source_text"));
        assert!(!normalized.contains_key("select_focused_block_text_rendered"));

        config.clear();
        config.insert(
            "select_focused_block_text_rendered".to_string(),
            vec!["ctrl-alt-shift-a".to_string()],
        );

        assert_eq!(
            resolved_shortcut_keys(&config, ShortcutCommand::SelectAll),
            vec!["ctrl-alt-shift-a".to_string()]
        );

        let normalized = normalize_shortcut_config(&config);
        assert_eq!(
            normalized.get("select_all"),
            Some(&vec!["ctrl-alt-shift-a".to_string()])
        );
        assert!(!normalized.contains_key("select_all_source_text"));
        assert!(!normalized.contains_key("select_focused_block_text_rendered"));
    }

    #[test]
    fn close_and_quit_defaults_are_platform_specific() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::CloseWindow),
                vec!["cmd-w".to_string()]
            );
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::QuitApplication),
                vec!["cmd-q".to_string()]
            );
        }

        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(
                resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::CloseWindow),
                vec!["ctrl-q".to_string()]
            );
            assert!(
                resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::QuitApplication)
                    .is_empty()
            );
        }
    }

    #[test]
    fn word_and_block_shortcuts_have_ctrl_and_alt_defaults() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::WordMoveLeft),
            vec!["ctrl-left".to_string(), "alt-left".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::WordDeleteBack),
            vec!["ctrl-backspace".to_string(), "alt-backspace".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::BlockUp),
            vec!["ctrl-up".to_string(), "alt-up".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::WordSelectRight),
            vec![
                "ctrl-shift-right".to_string(),
                "alt-shift-right".to_string()
            ]
        );
    }

    #[test]
    fn page_navigation_shortcuts_have_defaults() {
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::PageUp),
            vec!["pageup".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::PageDown),
            vec!["pagedown".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::JumpToTop),
            vec!["ctrl-home".to_string(), "cmd-up".to_string()]
        );
        assert_eq!(
            resolved_shortcut_keys(&BTreeMap::new(), ShortcutCommand::JumpToBottom),
            vec!["ctrl-end".to_string(), "cmd-down".to_string()]
        );
    }

    #[test]
    fn invalid_or_empty_shortcuts_fall_back_to_defaults() {
        let mut config = BTreeMap::new();
        config.insert("save_document".to_string(), vec!["".to_string()]);
        config.insert("open_file".to_string(), vec!["a".to_string()]);

        let normalized = normalize_shortcut_config(&config);
        assert!(!normalized.contains_key("save_document"));
        assert!(!normalized.contains_key("open_file"));
    }

    #[test]
    fn conflicting_custom_shortcut_falls_back_to_default() {
        let mut config = BTreeMap::new();
        config.insert("copy".to_string(), vec!["ctrl-x".to_string()]);

        let normalized = normalize_shortcut_config(&config);
        assert!(!normalized.contains_key("copy"));
        assert_eq!(
            resolved_shortcut_keys(&config, ShortcutCommand::Copy),
            vec!["cmd-c".to_string(), "ctrl-c".to_string()]
        );
    }

    #[test]
    fn detects_shortcut_conflicts_for_preferences_drafts() {
        let conflict = shortcut_conflict_for(
            ShortcutCommand::Copy,
            &["ctrl-x".to_string()],
            &BTreeMap::new(),
        )
        .expect("copy should conflict with cut");

        assert_eq!(conflict.id, "cut");
    }
}
