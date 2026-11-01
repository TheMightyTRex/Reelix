//! Keyboard focus helpers for dialogs and panels: keep Tab inside, and give focus back to
//! whatever opened them.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, KeyboardEvent};

const FOCUSABLE: &str = "a[href], button:not([disabled]), input:not([disabled]):not([type=hidden]), \
    select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])";

/// The element that has focus now.
pub fn active_element() -> Option<HtmlElement> {
    document().active_element()?.dyn_into::<HtmlElement>().ok()
}

/// Wraps Tab / Shift+Tab between the first and last focusable elements inside `container`.
pub fn trap_tab(container: &web_sys::Element, e: &KeyboardEvent) {
    if e.key() != "Tab" {
        return;
    }
    let Ok(list) = container.query_selector_all(FOCUSABLE) else {
        return;
    };
    let items: Vec<HtmlElement> = (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<HtmlElement>().ok())
        .filter(|el| el.offset_parent().is_some())
        .collect();
    let (Some(first), Some(last)) = (items.first(), items.last()) else {
        e.prevent_default();
        return;
    };
    let active = active_element();
    let at = |el: &HtmlElement| active.as_ref().is_some_and(|a| a == el);
    let outside = active
        .as_ref()
        .is_none_or(|a| !container.contains(Some(a.unchecked_ref())));
    if e.shift_key() && (at(first) || outside) {
        e.prevent_default();
        let _ = last.focus();
    } else if !e.shift_key() && (at(last) || outside) {
        e.prevent_default();
        let _ = first.focus();
    }
}

/// Remembers the focused element whenever `open` turns true and focuses it again when
/// `open` turns false.
pub fn restore_focus_on_close(open: RwSignal<bool>) {
    let opener = StoredValue::new_local(None::<HtmlElement>);
    Effect::new(move |was_open: Option<bool>| {
        let is_open = open.get();
        match (was_open.unwrap_or(false), is_open) {
            (false, true) => opener.set_value(active_element()),
            (true, false) => {
                if let Some(el) = opener.get_value() {
                    let _ = el.focus();
                }
                opener.set_value(None);
            }
            _ => {}
        }
        is_open
    });
}
