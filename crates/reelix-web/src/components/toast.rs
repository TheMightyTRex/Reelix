//! Renders the global toast stack.

use leptos::prelude::*;

use super::Icon;
use crate::state::{ToastKind, Toasts};

#[component]
pub fn ToastHost() -> impl IntoView {
    let toasts = Toasts::expect();
    view! {
        <div class="toast-region" role="region" aria-label="Notifications" aria-live="polite">
            <For each=move || toasts.items.get() key=|t| t.id let:toast>
                {
                    let (class, icon) = match toast.kind {
                        ToastKind::Success => ("toast tone-success", "check"),
                        ToastKind::Error => ("toast tone-danger", "error"),
                        ToastKind::Info => ("toast tone-info", "info"),
                    };
                    let id = toast.id;
                    let role = if toast.kind == ToastKind::Error { "alert" } else { "status" };
                    view! {
                        <div class=class role=role>
                            <span class="toast-icon">
                                <Icon name=icon />
                            </span>
                            <p class="toast-message">{toast.message}</p>
                            <button
                                type="button"
                                class="icon-btn icon-btn-sm"
                                aria-label="Dismiss notification"
                                on:click=move |_| toasts.dismiss(id)
                            >
                                <Icon name="close" />
                            </button>
                        </div>
                    }
                }
            </For>
        </div>
    }
}
