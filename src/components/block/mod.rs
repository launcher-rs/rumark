//! 块运行时与语义状态。
//!
//! 本模块收拢块实体、块级 Markdown 解析、行内文本树处理、渲染、输入桥接与交互处理器。块持有本地编辑状态，编辑器持有树结构与跨块变更。

mod editing;
mod element;
mod input;
mod interactions;
mod model;
mod render;
mod table_axis_menu;

pub(crate) use crate::components::markdown::footnote::*;
pub(crate) use crate::components::markdown::highlight::*;
pub(crate) use crate::components::markdown::image::*;
pub use crate::components::markdown::inline::*;
pub(crate) use crate::components::markdown::link::*;
pub use editing::*;
pub use model::*;
pub(crate) use table_axis_menu::TableAxisMenuState;

use rgpui::{Hsla, Theme};

/// 按提示框类型返回强调色与背景色（块渲染与容器渲染共用）。
///
/// `*_hover` 是主要按钮的悬停填充，饱和度很高，铺成提示框底色会让标题
/// 与底色同色相、正文失去对比；底色一律用强调色的低透明度浅洗，
/// 明暗两套主题下标题（强调色）与正文（前景色）都读得清。
pub(crate) fn callout_accent_and_background(
    variant: CalloutVariant,
    theme: &Theme,
) -> (Hsla, Hsla) {
    let wash = |accent: Hsla| accent.opacity(0.12);
    match variant {
        CalloutVariant::Note => (theme.info, wash(theme.info)),
        CalloutVariant::Tip => (theme.success, wash(theme.success)),
        CalloutVariant::Important => (theme.primary, wash(theme.primary)),
        CalloutVariant::Warning => (theme.warning, wash(theme.warning)),
        CalloutVariant::Caution => (theme.danger, wash(theme.danger)),
    }
}
