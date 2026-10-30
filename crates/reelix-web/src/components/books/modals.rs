//! Book dialogs: add author, add book, edit author and rename preview.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use reelix_api_types::books::{
    AddAuthorRequest, AddBookRequest, AuthorDto, AuthorEdit, AuthorLookupDto, AuthorMonitor, BookDto,
    BookLookupDto, NewBookMonitor, RenamePreviewDto,
};
use reelix_api_types::content::BookFormat;

use super::{AUTHOR_MONITOR, BookOptions, BookRootSelect, FormatProfileSelect, NEW_BOOK_MONITOR, opt_id};
use crate::api;
use crate::components::library_common::{pref_get, pref_set, toggle_id};
use crate::components::library_forms::EnumSelect;
use crate::components::poster::Poster;
use crate::components::{EmptyState, ErrorBox, Icon, Modal, Skeleton, Spinner};
use crate::state::Toasts;

/// Profile / root folder choices for both formats (`0` = none).
#[derive(Clone, Copy)]
pub struct FormatChoice {
    pub ebook_profile: RwSignal<i64>,
    pub ebook_root: RwSignal<i64>,
    pub audio_profile: RwSignal<i64>,
    pub audio_root: RwSignal<i64>,
}

impl FormatChoice {
    /// Remembered add-dialog defaults.
    pub fn remembered() -> Self {
        let saved = |k: &str| {
            pref_get(&format!("books.add.{k}"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0i64)
        };
        Self {
            ebook_profile: RwSignal::new(saved("ebook_profile")),
            ebook_root: RwSignal::new(saved("ebook_root")),
            audio_profile: RwSignal::new(saved("audio_profile")),
            audio_root: RwSignal::new(saved("audio_root")),
        }
    }

    pub fn remember(&self) {
        for (k, v) in [
            ("ebook_profile", self.ebook_profile),
            ("ebook_root", self.ebook_root),
            ("audio_profile", self.audio_profile),
            ("audio_root", self.audio_root),
        ] {
            pref_set(&format!("books.add.{k}"), &v.get_untracked().to_string());
        }
    }

    /// Error message when the choice is unusable.
    pub fn problem(&self) -> Option<&'static str> {
        let (ep, er) = (
            self.ebook_profile.get_untracked(),
            self.ebook_root.get_untracked(),
        );
        let (ap, ar) = (
            self.audio_profile.get_untracked(),
            self.audio_root.get_untracked(),
        );
        if ep == 0 && ap == 0 {
            Some("Choose a quality profile for ebooks, audiobooks or both.")
        } else if ep != 0 && er == 0 {
            Some("Choose an ebook root folder (add one under Settings → Books).")
        } else if ap != 0 && ar == 0 {
            Some("Choose an audiobook root folder (add one under Settings → Books).")
        } else {
            None
        }
    }
}

/// Profile + root folder fields for both formats.
#[component]
pub fn FormatFields(opts: BookOptions, choice: FormatChoice, #[prop(into)] prefix: String) -> impl IntoView {
    let row = move |format: BookFormat, profile: RwSignal<i64>, root: RwSignal<i64>, p: String| {
        let label = super::format_label(format);
        let pid = format!("{p}-{}-profile", format.as_str());
        let rid = format!("{p}-{}-root", format.as_str());
        view! {
            <div class="field-row">
                <div class="field">
                    <label for=pid.clone()>{format!("{label} quality profile")}</label>
                    <FormatProfileSelect opts=opts format=format value=profile id=pid />
                </div>
                <div class="field">
                    <label for=rid.clone()>{format!("{label} root folder")}</label>
                    <BookRootSelect opts=opts format=format value=root id=rid allow_none=true />
                    {move || (profile.get() != 0 && opts.roots(format).is_empty()).then(|| view! {
                        <p class="field-hint text-danger">
                            {format!("No {} root folders yet. ", label.to_lowercase())}
                            <A href="/settings/mediamanagement">"Add one in Media Management"</A>
                        </p>
                    })}
                </div>
            </div>
        }
    };
    // Default the root folder once a profile is chosen.
    Effect::new(move |_| {
        for (format, profile, root) in [
            (BookFormat::Ebook, choice.ebook_profile, choice.ebook_root),
            (BookFormat::Audiobook, choice.audio_profile, choice.audio_root),
        ] {
            let roots = opts.roots(format);
            if profile.get() != 0
                && !roots.iter().any(|r| r.id == root.get_untracked())
                && let Some(first) = roots.first()
            {
                root.set(first.id);
            }
        }
    });
    view! {
        {row(BookFormat::Ebook, choice.ebook_profile, choice.ebook_root, prefix.clone())}
        {row(BookFormat::Audiobook, choice.audio_profile, choice.audio_root, prefix)}
        <p class="field-hint">"Each format has its own profile and folder. Leave a profile on “None” when you don't want that format."</p>
    }
}

/// Toggleable tag chips.
#[component]
pub fn BookTagPicker(opts: BookOptions, value: RwSignal<Vec<i64>>) -> impl IntoView {
    view! {
        {move || {
            let tags = opts.tag_list();
            if tags.is_empty() {
                return view! { <p class="field-hint">"No tags yet — create them under Settings → Tags."</p> }.into_any();
            }
            view! {
                <div class="chip-list" role="group" aria-label="Tags">
                    {tags.into_iter().map(|t| {
                        let tid = t.id;
                        let on = move || value.with(|v| v.contains(&tid));
                        view! {
                            <button type="button" class="chip tag-chip" class:on=on aria-pressed=move || on().to_string()
                                on:click=move |_| toggle_id(value, tid)>
                                {t.label}
                            </button>
                        }
                    }).collect_view()}
                </div>
            }.into_any()
        }}
    }
}

fn busy_icon(busy: RwSignal<bool>, icon: &'static str) -> impl IntoView {
    move || {
        if busy.get() {
            view! { <Spinner /> }.into_any()
        } else {
            view! { <Icon name=icon /> }.into_any()
        }
    }
}

/// Adds an author from an Open Library lookup result.
#[component]
pub fn AddAuthorModal(
    open: RwSignal<bool>,
    target: RwSignal<Option<AuthorLookupDto>>,
    opts: BookOptions,
    #[prop(optional)] on_added: Option<Callback<i64>>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let choice = FormatChoice::remembered();
    let monitor = RwSignal::new(AuthorMonitor::All);
    let new_items = RwSignal::new(NewBookMonitor::All);
    let tags = RwSignal::new(Vec::<i64>::new());
    let search = RwSignal::new(pref_get("books.add.search").is_none_or(|v| v == "1"));
    let busy = RwSignal::new(false);

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(t) = target.get_untracked() else { return };
        if let Some(p) = choice.problem() {
            toasts.error(p);
            return;
        }
        choice.remember();
        pref_set("books.add.search", if search.get_untracked() { "1" } else { "0" });
        let ep = opt_id(choice.ebook_profile.get_untracked());
        let ap = opt_id(choice.audio_profile.get_untracked());
        let req = AddAuthorRequest {
            openlibrary_id: t.openlibrary_id.clone(),
            quality_profile_id: ep,
            root_folder_id: ep.and(opt_id(choice.ebook_root.get_untracked())),
            audio_quality_profile_id: ap,
            audio_root_folder_id: ap.and(opt_id(choice.audio_root.get_untracked())),
            monitored: monitor.get_untracked() != AuthorMonitor::None,
            monitor: monitor.get_untracked(),
            monitor_new_items: new_items.get_untracked(),
            selected_work_ids: Vec::new(),
            tags: tags.get_untracked(),
            search_on_add: search.get_untracked(),
        };
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = api::post::<_, AuthorDto>("/authors", &req).await;
            busy.set(false);
            match res {
                Ok(a) => {
                    toasts.success(format!("Added “{}”.", a.name));
                    open.set(false);
                    match on_added {
                        Some(cb) => cb.run(a.id),
                        None => navigate(&format!("/books/authors/{}", a.id), Default::default()),
                    }
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Add author">
            <form class="form" on:submit=submit.clone()>
                {move || target.get().map(|t| view! {
                    <div class="add-head">
                        <Poster title=t.name.clone() url=t.image.clone() />
                        <div class="overview-body">
                            <span class="overview-title">{t.name.clone()}</span>
                            <div class="meta-line">
                                {t.birth_date.clone().map(|b| view! { <span>{format!("Born {b}")}</span> })}
                                <span>{format!("{} works", t.work_count)}</span>
                            </div>
                            {t.top_work.clone().map(|w| view! { <p class="overview-text">{format!("Best known for {w}")}</p> })}
                        </div>
                    </div>
                })}
                <FormatFields opts=opts choice=choice prefix="aa" />
                <div class="field-row">
                    <div class="field">
                        <label for="aa-monitor">"Monitor"</label>
                        <EnumSelect table=&AUTHOR_MONITOR value=monitor id="aa-monitor" />
                    </div>
                    <div class="field">
                        <label for="aa-new">"New books"</label>
                        <EnumSelect table=&NEW_BOOK_MONITOR value=new_items id="aa-new" />
                    </div>
                </div>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <BookTagPicker opts=opts value=tags />
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search.get() on:change=move |e| search.set(event_target_checked(&e)) />
                    "Start searching for missing books"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "plus")}
                        "Add author"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Adds one book (and its author, unmonitored, when needed).
#[component]
pub fn AddBookModal(
    open: RwSignal<bool>,
    target: RwSignal<Option<BookLookupDto>>,
    opts: BookOptions,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let choice = FormatChoice::remembered();
    let tags = RwSignal::new(Vec::<i64>::new());
    let search = RwSignal::new(pref_get("books.add.search").is_none_or(|v| v == "1"));
    let busy = RwSignal::new(false);

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(t) = target.get_untracked() else { return };
        if let Some(p) = choice.problem() {
            toasts.error(p);
            return;
        }
        choice.remember();
        let ep = opt_id(choice.ebook_profile.get_untracked());
        let ap = opt_id(choice.audio_profile.get_untracked());
        let mut formats = Vec::new();
        if ep.is_some() {
            formats.push(BookFormat::Ebook);
        }
        if ap.is_some() {
            formats.push(BookFormat::Audiobook);
        }
        let req = AddBookRequest {
            openlibrary_work_id: t.openlibrary_work_id.clone(),
            formats,
            quality_profile_id: ep,
            root_folder_id: ep.and(opt_id(choice.ebook_root.get_untracked())),
            audio_quality_profile_id: ap,
            audio_root_folder_id: ap.and(opt_id(choice.audio_root.get_untracked())),
            tags: tags.get_untracked(),
            search_on_add: search.get_untracked(),
            added_by_request_id: None,
        };
        busy.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            let res = api::post::<_, BookDto>("/books", &req).await;
            busy.set(false);
            match res {
                Ok(b) => {
                    toasts.success(format!("Added “{}”.", b.title));
                    open.set(false);
                    navigate(&format!("/books/book/{}", b.id), Default::default());
                }
                Err(e) => toasts.error(format!("Could not add: {e}")),
            }
        });
    };

    view! {
        <Modal open=open title="Add book">
            <form class="form" on:submit=submit.clone()>
                {move || target.get().map(|t| view! {
                    <div class="add-head">
                        <Poster title=t.title.clone() url=t.cover.clone() />
                        <div class="overview-body">
                            <span class="overview-title">{t.title.clone()}{t.year.map(|y| format!(" ({y})"))}</span>
                            {t.author_name.clone().map(|a| view! { <p class="muted">{format!("by {a}")}</p> })}
                        </div>
                    </div>
                })}
                <FormatFields opts=opts choice=choice prefix="ab" />
                <p class="field-hint">"Only this book is monitored; the author is added without monitoring their other books."</p>
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <BookTagPicker opts=opts value=tags />
                </div>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || search.get() on:change=move |e| search.set(event_target_checked(&e)) />
                    "Start searching for the book"
                </label>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "plus")}
                        "Add book"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Edits an author: monitoring, per-format profiles / folders and tags.
#[component]
pub fn AuthorEditModal(
    open: RwSignal<bool>,
    author: Signal<Option<AuthorDto>>,
    opts: BookOptions,
    on_saved: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let monitored = RwSignal::new(true);
    let new_items = RwSignal::new(NewBookMonitor::All);
    let choice = FormatChoice {
        ebook_profile: RwSignal::new(0),
        ebook_root: RwSignal::new(0),
        audio_profile: RwSignal::new(0),
        audio_root: RwSignal::new(0),
    };
    let tags = RwSignal::new(Vec::<i64>::new());
    let busy = RwSignal::new(false);

    // Load the author's values whenever the dialog opens.
    Effect::new(move |_| {
        if open.get()
            && let Some(a) = author.get_untracked()
        {
            monitored.set(a.monitored);
            new_items.set(a.monitor_new_items);
            choice.ebook_profile.set(a.quality_profile_id.unwrap_or(0));
            choice.ebook_root.set(a.root_folder_id.unwrap_or(0));
            choice.audio_profile.set(a.audio_quality_profile_id.unwrap_or(0));
            choice.audio_root.set(a.audio_root_folder_id.unwrap_or(0));
            tags.set(a.tags.clone());
        }
    });

    let submit = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let Some(a) = author.get_untracked() else { return };
        if let Some(p) = choice.problem() {
            toasts.error(p);
            return;
        }
        let ep = opt_id(choice.ebook_profile.get_untracked());
        let ap = opt_id(choice.audio_profile.get_untracked());
        let edit = AuthorEdit {
            monitored: Some(monitored.get_untracked()),
            monitor_new_items: Some(new_items.get_untracked()),
            quality_profile_id: Some(ep),
            root_folder_id: Some(ep.and(opt_id(choice.ebook_root.get_untracked()))),
            audio_quality_profile_id: Some(ap),
            audio_root_folder_id: Some(ap.and(opt_id(choice.audio_root.get_untracked()))),
            tags: Some(tags.get_untracked()),
            ..Default::default()
        };
        busy.set(true);
        spawn_local(async move {
            match api::put::<_, AuthorDto>(&format!("/authors/{}", a.id), &edit).await {
                Ok(_) => {
                    toasts.success("Author saved.");
                    open.set(false);
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            busy.set(false);
        });
    };

    view! {
        <Modal open=open title="Edit author">
            <form class="form" on:submit=submit>
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || monitored.get() on:change=move |e| monitored.set(event_target_checked(&e)) />
                    "Monitored"
                </label>
                <div class="field">
                    <label for="ae-new">"New books"</label>
                    <EnumSelect table=&NEW_BOOK_MONITOR value=new_items id="ae-new" />
                </div>
                <FormatFields opts=opts choice=choice prefix="ae" />
                <div class="field">
                    <span class="field-label">"Tags"</span>
                    <BookTagPicker opts=opts value=tags />
                </div>
                <div class="modal-footer">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary" disabled=move || busy.get()>
                        {busy_icon(busy, "check")}
                        "Save"
                    </button>
                </div>
            </form>
        </Modal>
    }
}

/// Book files whose names differ from the book naming templates; renames on confirm.
#[component]
pub fn BookRenameModal(open: RwSignal<bool>, author_id: i64) -> impl IntoView {
    let toasts = Toasts::expect();
    let preview = LocalResource::new(move || {
        let o = open.get();
        async move {
            if o {
                Some(api::get::<Vec<RenamePreviewDto>>(&format!("/books/rename?author_id={author_id}")).await)
            } else {
                None
            }
        }
    });
    let busy = RwSignal::new(false);
    let rename = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::post_empty::<serde_json::Value>(&format!("/books/rename?author_id={author_id}")).await
            {
                Ok(_) => {
                    toasts.success("Rename queued.");
                    open.set(false);
                }
                Err(e) => toasts.error(format!("Could not rename: {e}")),
            }
            busy.set(false);
        });
    };
    view! {
        <Modal open=open title="Rename book files">
            <div class="modal-wide">
                {move || match preview.get().flatten() {
                    None => view! { <Skeleton lines=3 /> }.into_any(),
                    Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
                    Some(Ok(list)) if list.is_empty() => view! {
                        <EmptyState icon="check" title="Nothing to rename" description="Every file already matches your book naming settings." compact=true />
                    }.into_any(),
                    Some(Ok(list)) => {
                        let n = list.len();
                        view! {
                            <p class="modal-text">{format!("{n} file(s) will be renamed:")}</p>
                            <div class="rename-list">
                                {list.into_iter().map(|r| view! {
                                    <div class="rename-item">
                                        <div class="from">{format!("− {}", r.existing_path)}</div>
                                        <div class="to">{format!("+ {}", r.new_path)}</div>
                                    </div>
                                }).collect_view()}
                            </div>
                            <div class="modal-footer">
                                <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                                <button type="button" class="btn btn-primary" disabled=move || busy.get() on:click=rename>
                                    {busy_icon(busy, "check")}
                                    "Rename"
                                </button>
                            </div>
                        }.into_any()
                    }
                }}
            </div>
        </Modal>
    }
}
