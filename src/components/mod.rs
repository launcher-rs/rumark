//! 通用界面组件与 Markdown 编辑基础模块。

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use rgpui::{AsyncApp, Context, WeakEntity};

mod actions;
mod block;
pub(crate) mod latex;
pub(crate) mod markdown;
pub(crate) mod mermaid;

pub use crate::editor::Editor;
pub use actions::*;
pub use block::*;
#[allow(unused_imports)]
pub(crate) use latex::*;
#[allow(unused_imports)]
pub(crate) use markdown::footnote::*;
#[allow(unused_imports)]
pub(crate) use markdown::highlight::*;
#[allow(unused_imports)]
pub(crate) use markdown::html::*;
#[allow(unused_imports)]
pub(crate) use markdown::image::*;
#[allow(unused_imports)]
pub use markdown::inline::*;
#[allow(unused_imports)]
pub(crate) use markdown::link::*;
pub use markdown::table::*;
#[allow(unused_imports)]
pub(crate) use mermaid::*;

/// 前台任务中容错地更新实体：嵌套派发借用冲突时返回 `None` 而不崩溃。
///
/// Windows 下拖拽标题栏移动窗口会进入 `SendMessageW` 的嵌套消息循环，
/// 嵌套派发轮询前台任务时 `App` 可能正被外层借用，此时直接 `update`
/// 会以 `RefCell already borrowed` 崩溃。本 helper 的调用点均为装饰性
/// 重绘（光标闪烁、滚动条淡出、滚动重试），借用冲突时跳过本拍即可。
///
/// 只吞掉 `RefCell` 重借冲突这一种 panic；其他 panic 属于真实缺陷，
/// 一律照常向上抛，避免被静默吞掉而无法定位。
/// 返回 `Some(Ok)` 表示已应用，`Some(Err)` 表示实体已释放，`None` 表示借用冲突。
pub(crate) fn update_entity_forgiving<T, R>(
    entity: &WeakEntity<T>,
    cx: &mut AsyncApp,
    update: impl FnOnce(&mut T, &mut Context<T>) -> R,
) -> Option<anyhow::Result<R>>
where
    T: 'static,
{
    match catch_unwind(AssertUnwindSafe(|| entity.update(cx, update))) {
        Ok(result) => Some(result),
        // 嵌套派发借用冲突：跳过本拍，由调用方下拍重试。
        Err(payload) if is_refcell_reborrow_panic(payload.as_ref()) => None,
        // 其他 panic 是真实缺陷，恢复 unwind 让上层感知。
        Err(payload) => resume_unwind(payload),
    }
}

/// 判断 panic 载荷是否为 `RefCell` 重借冲突。
///
/// 只认 std 在 `RefCell::borrow` / `borrow_mut` 冲突时抛出的文案（不同 Rust
/// 版本措辞略有差异，如 `already borrowed: BorrowMutError` 与
/// `RefCell already borrowed`），其余 panic 都不算“可安全跳过”的借用冲突。
fn is_refcell_reborrow_panic(payload: &(dyn Any + Send)) -> bool {
    let text = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied());
    matches!(
        text,
        Some(text)
            if text.contains("already borrowed") || text.contains("already mutably borrowed")
    )
}

#[cfg(test)]
mod tests {
    use super::is_refcell_reborrow_panic;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn refcell_reborrow_conflict_is_recognized() {
        let cell = std::cell::RefCell::new(0u32);
        let _held = cell.borrow_mut();
        let payload = catch_unwind(AssertUnwindSafe(|| {
            let _second = cell.borrow_mut();
        }))
        .expect_err("二次 borrow_mut 应当 panic");
        assert!(is_refcell_reborrow_panic(payload.as_ref()));
    }

    #[test]
    fn refcell_shared_borrow_conflict_is_recognized() {
        let cell = std::cell::RefCell::new(0u32);
        let _held = cell.borrow_mut();
        let payload = catch_unwind(AssertUnwindSafe(|| {
            let _shared = cell.borrow();
        }))
        .expect_err("可变借用期间再共享借用应当 panic");
        assert!(is_refcell_reborrow_panic(payload.as_ref()));
    }

    #[test]
    fn ordinary_panics_are_not_reborrow_conflicts() {
        let payload =
            catch_unwind(|| -> () { panic!("boom") }).expect_err("显式 panic 应当返回载荷");
        assert!(!is_refcell_reborrow_panic(payload.as_ref()));
    }
}
