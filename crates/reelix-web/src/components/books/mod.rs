//! Shared book components: option lists (per-format profiles, book root folders, tags),
//! format status helpers, the search hand-off and the author / book dialogs.

mod modals;

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::EnqueuedJob;
use reelix_api_types::books::{
    AuthorDto, AuthorMonitor, BookDto, BookFileDto, BookSearchRequest, NewBookMonitor,
};
use reelix_api_types::content::{BookFormat, ContentKind, ContentRootFolderDto, ContentSearchKind};
use reelix_api_types::profiles::QualityProfile;
use reelix_api_types::providers::TagDto;

use crate::api::{self, ApiResult};
use crate::components::Tone;
use crate::components::library_common::bytes;
use crate::state::Toasts;

pub use modals::{AddAuthorModal, AddBookModal, AuthorEditModal, BookRenameModal};

/// Ebook and audiobook quality profiles, book root folders and tags — loaded once per page.
#[derive(Clone, Copy)]
pub struct BookOptions {
    pub ebook_profiles: LocalResource<ApiResult<Vec<QualityProfile>>>,
    pub audio_profiles: LocalResource<ApiResult<Vec<QualityProfile>>>,
    pub roots: LocalResource<ApiResult<Vec<ContentRootFolderDto>>>,
    pub tags: LocalResource<ApiResult<Vec<TagDto>>>,
}

impl BookOptions {
    pub fn load() -> Self {
        Self {
            ebook_profiles: LocalResource::new(|| {
                api::get::<Vec<QualityProfile>>("/qualityprofiles?kind=ebook")
            }),
            audio_profiles: LocalResource::new(|| {
                api::get::<Vec<QualityProfile>>("/qualityprofiles?kind=audiobook")
            }),
            roots: LocalResource::new(|| api::get::<Vec<ContentRootFolderDto>>("/bookrootfolders")),
            tags: LocalResource::new(|| api::get::<Vec<TagDto>>("/tags")),
        }
    }

    pub fn profiles(&self, f: BookFormat) -> Vec<QualityProfile> {
        let r = match f {
            BookFormat::Ebook => self.ebook_profiles,
            BookFormat::Audiobook => self.audio_profiles,
        };
        r.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn roots(&self, f: BookFormat) -> Vec<ContentRootFolderDto> {
        let kind = f.content_kind();
        self.roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect()
    }

    pub fn all_roots(&self) -> Vec<ContentRootFolderDto> {
        self.roots.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn tag_list(&self) -> Vec<TagDto> {
        self.tags.get().and_then(Result::ok).unwrap_or_default()
    }

    pub fn tag_names(&self, ids: &[i64]) -> Vec<String> {
        let tags = self.tag_list();
        ids.iter()
            .map(|id| {
                tags.iter()
                    .find(|t| t.id == *id)
                    .map_or_else(|| format!("#{id}"), |t| t.label.clone())
            })
            .collect()
    }

    /// Profile name, "Not wanted" for `None`.
    pub fn profile_name(&self, f: BookFormat, id: Option<i64>) -> String {
        match id {
            None => "Not wanted".into(),
            Some(id) => self
                .profiles(f)
                .into_iter()
                .find(|p| p.id == id)
                .map_or_else(|| format!("Profile #{id}"), |p| p.name),
        }
    }
}

/// `0` ↔ `None` for the id selects.
pub fn opt_id(v: i64) -> Option<i64> {
    (v != 0).then_some(v)
}

pub fn format_label(f: BookFormat) -> &'static str {
    match f {
        BookFormat::Ebook => "Ebook",
        BookFormat::Audiobook => "Audiobook",
    }
}

pub fn kind_label(k: ContentKind) -> &'static str {
    match k {
        ContentKind::Ebook => "Ebook",
        ContentKind::Audiobook => "Audiobook",
        ContentKind::Music => "Music",
        ContentKind::Movie => "Movie",
        ContentKind::Series => "Series",
        ContentKind::Radio => "Radio",
    }
}

/// `<select>` of the quality profiles of one format; `0` = "None (not wanted)".
#[component]
pub fn FormatProfileSelect(
    opts: BookOptions,
    format: BookFormat,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
    #[prop(optional, into)] none_label: Option<String>,
) -> impl IntoView {
    let none_label = none_label.unwrap_or_else(|| "None (not wanted)".into());
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            <option value="0" selected=move || value.get() == 0>{none_label}</option>
            {move || opts.profiles(format).into_iter().map(|p| {
                let pid = p.id;
                view! { <option value=pid.to_string() selected=move || value.get() == pid>{p.name}</option> }
            }).collect_view()}
        </select>
    }
}

/// `<select>` of the book root folders of one format (defaults to the first one).
#[component]
pub fn BookRootSelect(
    opts: BookOptions,
    format: BookFormat,
    value: RwSignal<i64>,
    #[prop(into)] id: String,
    #[prop(optional)] allow_none: bool,
) -> impl IntoView {
    Effect::new(move |_| {
        let list = opts.roots(format);
        if !allow_none
            && value.get_untracked() == 0
            && let Some(first) = list.first()
        {
            value.set(first.id);
        }
    });
    view! {
        <select id=id class="input" on:change=move |e| value.set(event_target_value(&e).parse().unwrap_or(0))>
            {allow_none.then(|| view! { <option value="0" selected=move || value.get() == 0>"—"</option> })}
            {move || opts.roots(format).into_iter().map(|r| {
                let rid = r.id;
                let free = r.free_space.map(|f| format!(" ({} free)", bytes(f))).unwrap_or_default();
                view! { <option value=rid.to_string() selected=move || value.get() == rid>{format!("{}{free}", r.path)}</option> }
            }).collect_view()}
        </select>
    }
}

pub const AUTHOR_MONITOR: [(AuthorMonitor, &str, &str); 5] = [
    (AuthorMonitor::All, "all", "All books"),
    (AuthorMonitor::Future, "future", "Future books"),
    (AuthorMonitor::Missing, "missing", "Missing books"),
    (AuthorMonitor::Existing, "existing", "Existing books"),
    (AuthorMonitor::None, "none", "None"),
];

pub const NEW_BOOK_MONITOR: [(NewBookMonitor, &str, &str); 2] = [
    (NewBookMonitor::All, "all", "Monitor new books"),
    (NewBookMonitor::None, "none", "Don't monitor new books"),
];

/// Whether the author wants `f` at all (has a profile for it).
pub fn wants(a: &AuthorDto, f: BookFormat) -> bool {
    match f {
        BookFormat::Ebook => a.quality_profile_id.is_some(),
        BookFormat::Audiobook => a.audio_quality_profile_id.is_some(),
    }
}

pub fn monitored(b: &BookDto, f: BookFormat) -> bool {
    match f {
        BookFormat::Ebook => b.monitored_ebook,
        BookFormat::Audiobook => b.monitored_audiobook,
    }
}

pub fn file_of(b: &BookDto, f: BookFormat) -> Option<&BookFileDto> {
    match f {
        BookFormat::Ebook => b.ebook_file.as_ref(),
        BookFormat::Audiobook => b.audiobook_file.as_ref(),
    }
}

/// Tone + label for one format of a book (`wanted` = the author has a profile for it).
pub fn format_status(b: &BookDto, f: BookFormat, wanted: bool) -> (Tone, String) {
    if let Some(file) = file_of(b, f) {
        let tone = if file.cutoff_met {
            Tone::Success
        } else {
            Tone::Warning
        };
        return (tone, file.quality_name.clone());
    }
    if !wanted {
        (Tone::Neutral, "Not wanted".into())
    } else if !monitored(b, f) {
        (Tone::Neutral, "Unmonitored".into())
    } else {
        (Tone::Danger, "Missing".into())
    }
}

/// Queues a book search (`POST /books/search`) and toasts the outcome.
pub fn start_book_search(kind: ContentSearchKind, ids: Vec<i64>, format: Option<BookFormat>, what: String) {
    let toasts = Toasts::expect();
    let req = BookSearchRequest { kind, ids, format };
    spawn_local(async move {
        match api::post::<_, EnqueuedJob>("/books/search", &req).await {
            Ok(job) if job.job_id.is_none() => toasts.info(format!("A search for {what} is already queued.")),
            Ok(_) => toasts.success(format!("Search queued for {what}.")),
            Err(e) => toasts.error(format!("Could not start the search: {e}")),
        }
    });
}

/// Sets the monitored flag of one or both formats of some books, then calls `done`.
pub fn set_book_monitored(ids: Vec<i64>, format: Option<BookFormat>, on: bool, done: impl Fn() + 'static) {
    let toasts = Toasts::expect();
    let req = reelix_api_types::books::BookMonitorRequest {
        book_ids: ids,
        format,
        monitored: on,
    };
    spawn_local(async move {
        match api::put::<_, serde_json::Value>("/books/monitor", &req).await {
            Ok(_) => done(),
            Err(e) => toasts.error(e.to_string()),
        }
    });
}

/// Series sort key: numeric position, unknown last.
pub fn position_key(p: Option<&str>) -> f64 {
    p.and_then(|p| p.trim().parse::<f64>().ok()).unwrap_or(f64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_sort_numerically() {
        assert!(position_key(Some("2.5")) < position_key(Some("10")));
        assert_eq!(position_key(None), f64::MAX);
        assert_eq!(opt_id(0), None);
        assert_eq!(opt_id(4), Some(4));
    }
}
