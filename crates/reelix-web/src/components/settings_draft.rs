//! Load / edit / save state for a settings document that several pages edit a part of
//! (e.g. `/settings/books`, whose naming lives on Naming and metadata options on Metadata).

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api::{self, ApiResult};
use crate::components::{ErrorBox, Icon, Skeleton, Spinner};
use crate::state::Toasts;

/// A settings document fetched from `path`, an editable copy of it and its save state.
pub struct SettingsDraft<T: Send + Sync + 'static> {
    pub loaded: LocalResource<ApiResult<T>>,
    pub value: RwSignal<Option<T>>,
    pub saving: RwSignal<bool>,
    path: &'static str,
}

impl<T: Send + Sync + 'static> Clone for SettingsDraft<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Send + Sync + 'static> Copy for SettingsDraft<T> {}

impl<T> SettingsDraft<T>
where
    T: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Loads `path` and keeps the draft in sync with each (re)load.
    pub fn new(path: &'static str) -> Self {
        let loaded = LocalResource::new(move || api::get::<T>(path));
        let value = RwSignal::new(None::<T>);
        Effect::new(move |_| {
            if let Some(Ok(v)) = loaded.get() {
                value.set(Some(v));
            }
        });
        Self {
            loaded,
            value,
            saving: RwSignal::new(false),
            path,
        }
    }

    /// Applies `f` to the draft (no-op before it has loaded).
    pub fn edit(&self, f: impl FnOnce(&mut T)) {
        self.value.update(|d| {
            if let Some(d) = d.as_mut() {
                f(d)
            }
        })
    }

    /// Reads from the draft, falling back to the default while it loads.
    pub fn get<R: Default>(&self, f: impl FnOnce(&T) -> R) -> R {
        self.value.with(|d| d.as_ref().map(f).unwrap_or_default())
    }

    /// Saves this page's part of the draft: re-fetches the document, lets `merge` copy the
    /// edited fields onto it and PUTs the result, so sections saved from other pages are kept.
    /// `what` names the section in toasts.
    pub fn save(&self, what: &'static str, merge: impl FnOnce(&mut T, &T) + 'static) {
        let Some(draft) = self.value.get_untracked() else {
            return;
        };
        let (value, saving, path) = (self.value, self.saving, self.path);
        let toasts = Toasts::expect();
        saving.set(true);
        spawn_local(async move {
            let res = match api::get::<T>(path).await {
                Ok(mut fresh) => {
                    merge(&mut fresh, &draft);
                    api::put::<_, T>(path, &fresh).await
                }
                Err(e) => Err(e),
            };
            match res {
                Ok(s) => {
                    toasts.success(format!("{what} saved."));
                    value.set(Some(s));
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    }
}

/// "Save changes" submit button for a settings form.
pub fn save_button(saving: RwSignal<bool>) -> impl IntoView {
    view! {
        <div class="modal-footer">
            <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                {move || if saving.get() { view! { <Spinner /> }.into_any() } else { view! { <Icon name="check" /> }.into_any() }}
                "Save changes"
            </button>
        </div>
    }
}

/// Renders `form` once `draft` has loaded, with a skeleton / retryable error before that.
pub fn loaded_view<T, V>(
    draft: SettingsDraft<T>,
    lines: usize,
    form: impl Fn() -> V + Send + Sync + 'static,
) -> impl IntoView
where
    T: Clone + Send + Sync + 'static,
    V: IntoView + 'static,
{
    let loaded = draft.loaded;
    move || match loaded.get() {
        None => view! { <Skeleton lines=lines /> }.into_any(),
        Some(Err(e)) => {
            view! { <ErrorBox error=e on_retry=Callback::new(move |()| loaded.refetch()) /> }.into_any()
        }
        Some(Ok(_)) => form().into_any(),
    }
}
