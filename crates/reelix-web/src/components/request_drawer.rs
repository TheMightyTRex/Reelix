//! A right-hand side panel (request details), closing on Escape and on backdrop click.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;

use super::Icon;

#[component]
pub fn Drawer(
    open: RwSignal<bool>,
    #[prop(into)] title: Signal<String>,
    children: ChildrenFn,
) -> impl IntoView {
    let panel = NodeRef::<html::Aside>::new();
    super::focus::restore_focus_on_close(open);
    Effect::new(move |_| {
        if open.get()
            && let Some(el) = panel.get()
        {
            let _ = el.focus();
        }
    });
    let on_keydown = move |e: ev::KeyboardEvent| {
        if e.key() == "Escape" {
            e.stop_propagation();
            open.set(false);
        } else if let Some(el) = panel.get() {
            super::focus::trap_tab(&el, &e);
        }
    };
    let children = StoredValue::new(children);
    view! {
        <Show when=move || open.get()>
            <div class="drawer-backdrop" on:click=move |_| open.set(false)></div>
            <aside class="drawer" role="dialog" aria-modal="true" aria-label=move || title.get() tabindex="-1"
                node_ref=panel on:keydown=on_keydown>
                <header class="drawer-header">
                    <h2 class="modal-title">{move || title.get()}</h2>
                    <button type="button" class="icon-btn" aria-label="Close panel" on:click=move |_| open.set(false)>
                        <Icon name="close" />
                    </button>
                </header>
                <div class="drawer-body">{children.with_value(|c| c())}</div>
            </aside>
        </Show>
    }
}
