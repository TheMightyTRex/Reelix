//! Modal dialog and a confirm-dialog built on it.

use std::sync::atomic::{AtomicUsize, Ordering};

use leptos::ev;
use leptos::html;
use leptos::prelude::*;

use super::Icon;

/// A centred dialog. Closes on Escape, on backdrop click and via the close button.
#[component]
pub fn Modal(
    open: RwSignal<bool>,
    #[prop(into)] title: String,
    #[prop(optional)] on_close: Option<Callback<()>>,
    children: ChildrenFn,
) -> impl IntoView {
    let close = move || {
        open.set(false);
        if let Some(cb) = on_close {
            cb.run(());
        }
    };
    let dialog_ref = NodeRef::<html::Div>::new();
    super::focus::restore_focus_on_close(open);

    // Move focus into the dialog when it opens so keyboard users land inside it.
    Effect::new(move |_| {
        if open.get()
            && let Some(el) = dialog_ref.get()
        {
            let target = el
                .query_selector("[autofocus], input, button.btn-primary, button")
                .ok()
                .flatten()
                .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok());
            match target {
                Some(t) => {
                    let _ = t.focus();
                }
                None => {
                    let _ = el.focus();
                }
            }
        }
    });

    let on_keydown = move |e: ev::KeyboardEvent| {
        if e.key() == "Escape" {
            e.stop_propagation();
            close();
        } else if let Some(el) = dialog_ref.get() {
            super::focus::trap_tab(&el, &e);
        }
    };

    static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
    let title_id = format!("modal-title-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let labelled = title_id.clone();
    let children = StoredValue::new(children);
    view! {
        <Show when=move || open.get()>
            <div class="modal-backdrop" on:click=move |_| close()>
                <div
                    class="modal"
                    role="dialog"
                    aria-modal="true"
                    aria-labelledby=labelled.clone()
                    tabindex="-1"
                    node_ref=dialog_ref
                    on:click=|e| e.stop_propagation()
                    on:keydown=on_keydown
                >
                    <header class="modal-header">
                        <h2 class="modal-title" id=title_id.clone()>
                            {title.clone()}
                        </h2>
                        <button type="button" class="icon-btn" aria-label="Close dialog" on:click=move |_| close()>
                            <Icon name="close" />
                        </button>
                    </header>
                    <div class="modal-body">{children.with_value(|c| c())}</div>
                </div>
            </div>
        </Show>
    }
}

/// "Are you sure?" dialog with a destructive confirm button.
#[component]
pub fn ConfirmDialog(
    open: RwSignal<bool>,
    #[prop(into)] title: String,
    #[prop(into)] message: Signal<String>,
    #[prop(into)] confirm_label: String,
    on_confirm: Callback<()>,
) -> impl IntoView {
    let confirm_label = StoredValue::new(confirm_label);
    view! {
        <Modal open=open title=title>
            <p class="modal-text">{move || message.get()}</p>
            <div class="modal-footer">
                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>
                    "Cancel"
                </button>
                <button
                    type="button"
                    class="btn btn-danger"
                    autofocus
                    on:click=move |_| {
                        open.set(false);
                        on_confirm.run(());
                    }
                >
                    {confirm_label.get_value()}
                </button>
            </div>
        </Modal>
    }
}
