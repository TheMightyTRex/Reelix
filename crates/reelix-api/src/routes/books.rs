//! Books (M8): authors, books, editions, series, book files, book root folders, library import,
//! renaming and book settings.
//!
//! Reads and writes need `MANAGE_LIBRARY`; book settings and adding/removing book root folders
//! need `ADMIN` (like the movie root folders). DTOs: `reelix_api_types::books`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use reelix_api_types::books::{
    AddAuthorRequest, AddBookRequest, AuthorDto, AuthorEdit, AuthorListQuery, AuthorLookupDto, BookDto,
    BookEdit, BookFileDto, BookFilesQuery, BookImportCandidateDto, BookImportScanQuery,
    BookLibraryImportRequest, BookListQuery, BookLookupDto, BookMonitorRequest, BookRenameQuery,
    BookSearchRequest, BookSeriesDto, BookSettingsDto, BookWantedQuery, EditionDto, LookupTermQuery,
};
use reelix_api_types::content::{ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::library::{
    DeleteQuery, LibraryImportMatchRequest, LibraryImportResult, Paged, RenamePreviewDto,
};
use reelix_api_types::{ApiErrorBody, EnqueuedJob};
use reelix_books::{BooksError, BooksService};
use reelix_core::Permissions;
use utoipa::OpenApi;

use super::library::{accepted, created, require_lib};
use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;

/// Routes for this module, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/authors", get(list_authors).post(add_author))
        .route("/authors/lookup", get(lookup_authors))
        .route(
            "/authors/{id}",
            get(get_author).put(edit_author).delete(delete_author),
        )
        .route("/authors/{id}/refresh", post(refresh_author))
        .route("/authors/{id}/rescan", post(rescan_author))
        .route("/books", get(list_books).post(add_book))
        .route("/books/lookup", get(lookup_books))
        .route("/books/monitor", put(monitor_books))
        .route("/books/search", post(search_books))
        .route("/books/wanted", get(wanted_books))
        .route("/books/rename", get(rename_preview).post(rename_run))
        .route("/books/{id}", get(get_book).put(edit_book).delete(delete_book))
        .route("/books/{id}/editions", get(book_editions))
        .route("/bookfiles", get(list_book_files))
        .route("/bookfiles/{id}", delete(delete_book_file))
        .route("/bookfiles/{id}/merge", post(merge_book_file))
        .route("/bookseries", get(list_book_series))
        .route("/bookrootfolders", get(list_root_folders).post(add_root_folder))
        .route("/bookrootfolders/{id}", delete(delete_root_folder))
        .route("/bookimport/scan", get(import_scan))
        .route("/bookimport/match", post(import_match))
        .route("/bookimport", post(library_import))
        .route("/settings/books", get(get_settings).put(put_settings))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_authors, add_author, lookup_authors, get_author, edit_author, delete_author, refresh_author,
        rescan_author, list_books, add_book, lookup_books, monitor_books, search_books, wanted_books,
        rename_preview, rename_run, get_book, edit_book, delete_book, book_editions, list_book_files,
        delete_book_file, merge_book_file, list_book_series, list_root_folders, add_root_folder, delete_root_folder,
        import_scan, import_match, library_import, get_settings, put_settings,
    ),
    components(schemas(
        reelix_api_types::books::AuthorDto,
        reelix_api_types::books::AuthorStatisticsDto,
        reelix_api_types::books::AuthorMonitor,
        reelix_api_types::books::NewBookMonitor,
        reelix_api_types::books::AuthorLookupDto,
        reelix_api_types::books::AddAuthorRequest,
        reelix_api_types::books::AuthorEdit,
        reelix_api_types::books::BookDto,
        reelix_api_types::books::BookSeriesRefDto,
        reelix_api_types::books::BookCreditDto,
        reelix_api_types::books::BookFileDto,
        reelix_api_types::books::BookFilePartDto,
        reelix_api_types::books::BookLookupDto,
        reelix_api_types::books::AddBookRequest,
        reelix_api_types::books::BookEdit,
        reelix_api_types::books::BookMonitorRequest,
        reelix_api_types::books::BookSearchRequest,
        reelix_api_types::books::EditionDto,
        reelix_api_types::books::BookSeriesDto,
        reelix_api_types::books::BookSettingsDto,
        reelix_api_types::books::BookNamingDto,
        reelix_api_types::books::CalibreSettingsDto,
        reelix_api_types::books::BookImportCandidateDto,
        reelix_api_types::books::BookImportItem,
        reelix_api_types::books::BookLibraryImportRequest,
        reelix_api_types::content::ContentRootFolderDto,
        reelix_api_types::content::CreateContentRootFolderRequest,
        reelix_api_types::content::ContentSearchKind,
    )),
    tags((name = "books", description = "Authors, books, editions, series, book files, book root folders, library import, renaming and book settings")),
)]
pub struct Api;

pub(crate) fn svc(state: &AppState) -> ApiResult<Arc<BooksService>> {
    state.ext::<BooksService>()
}

impl From<BooksError> for ApiError {
    fn from(e: BooksError) -> Self {
        match e {
            BooksError::NotFound(m) => ApiError::not_found(m),
            BooksError::Validation(m) => ApiError::validation(m),
            BooksError::Conflict(m) => ApiError::Conflict(m),
            BooksError::Metadata(meta) => {
                let msg = meta.to_string();
                if msg == "not found" {
                    ApiError::not_found("not found on Open Library")
                } else {
                    ApiError::Validation {
                        status: StatusCode::BAD_GATEWAY,
                        message: format!("metadata provider: {msg}"),
                    }
                }
            }
            BooksError::Files(f) => ApiError::validation(f.to_string()),
            BooksError::NotImplemented(what) => ApiError::Validation {
                status: StatusCode::NOT_IMPLEMENTED,
                message: format!("{what} is not implemented yet"),
            },
            other => ApiError::internal(other),
        }
    }
}

// ---------------- authors ----------------

/// Authors (filterable, sortable by `name|added|books|size`, optionally paged).
#[utoipa::path(get, path = "/api/v1/authors", tag = "books", params(AuthorListQuery),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Authors", body = Paged<AuthorDto>),
        (status = 403, description = "Requires MANAGE_LIBRARY", body = ApiErrorBody),
    ))]
pub async fn list_authors(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<AuthorListQuery>,
) -> ApiResult<ApiJson<Paged<AuthorDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_authors(&q).await?))
}

/// Adds an author with every work from Open Library (monitored per `monitor`).
#[utoipa::path(post, path = "/api/v1/authors", tag = "books", request_body = AddAuthorRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = AuthorDto),
        (status = 400, description = "Invalid profile/root folder", body = ApiErrorBody),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
        (status = 502, description = "Open Library failed", body = ApiErrorBody),
    ))]
pub async fn add_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddAuthorRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_author(req).await?))
}

/// Open Library author search (`olid:OL…A` looks up one author).
#[utoipa::path(get, path = "/api/v1/authors/lookup", tag = "books", params(LookupTermQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Matches", body = Vec<AuthorLookupDto>)))]
pub async fn lookup_authors(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupTermQuery>,
) -> ApiResult<ApiJson<Vec<AuthorLookupDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_authors(&q.term).await?))
}

/// One author.
#[utoipa::path(get, path = "/api/v1/authors/{id}", tag = "books",
    params(("id" = i64, Path, description = "Author id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Author", body = AuthorDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<AuthorDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_author(id).await?))
}

/// Edits an author (partial; `null` clears a format's profile/root folder).
#[utoipa::path(put, path = "/api/v1/authors/{id}", tag = "books", request_body = AuthorEdit,
    params(("id" = i64, Path, description = "Author id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Updated", body = AuthorDto),
        (status = 400, description = "Invalid edit", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn edit_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<AuthorEdit>,
) -> ApiResult<ApiJson<AuthorDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_author(id, edit).await?))
}

/// Deletes an author and their books (`?delete_files=true` also removes the folders).
#[utoipa::path(delete, path = "/api/v1/authors/{id}", tag = "books",
    params(("id" = i64, Path, description = "Author id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_author(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Queues a metadata refresh of the author (job `refresh_authors`).
#[utoipa::path(post, path = "/api/v1/authors/{id}/refresh", tag = "books",
    params(("id" = i64, Path, description = "Author id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn refresh_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_refresh(vec![id]).await?))
}

/// Queues a disk scan of the author's folders (job `book_scan`).
#[utoipa::path(post, path = "/api/v1/authors/{id}/rescan", tag = "books",
    params(("id" = i64, Path, description = "Author id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn rescan_author(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rescan(vec![id]).await?))
}

// ---------------- books ----------------

/// Books (by author, search, monitored, missing format; optionally paged).
#[utoipa::path(get, path = "/api/v1/books", tag = "books", params(BookListQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Books", body = Paged<BookDto>)))]
pub async fn list_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookListQuery>,
) -> ApiResult<ApiJson<Paged<BookDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.list_books(&q).await?))
}

/// Adds one work (the author is added with only this book monitored when needed).
#[utoipa::path(post, path = "/api/v1/books", tag = "books", request_body = AddBookRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Added", body = BookDto),
        (status = 400, description = "Invalid profile/root folder", body = ApiErrorBody),
        (status = 409, description = "Already in the library", body = ApiErrorBody),
    ))]
pub async fn add_book(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<AddBookRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(created(svc(&state)?.add_book(req).await?))
}

/// Open Library work search (`isbn:…`, `olid:OL…W`, `asin:…` supported).
#[utoipa::path(get, path = "/api/v1/books/lookup", tag = "books", params(LookupTermQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Matches", body = Vec<BookLookupDto>)))]
pub async fn lookup_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<LookupTermQuery>,
) -> ApiResult<ApiJson<Vec<BookLookupDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.lookup_books(&q.term).await?))
}

/// Sets the monitored flag of one or both formats of several books.
#[utoipa::path(put, path = "/api/v1/books/monitor", tag = "books", request_body = BookMonitorRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated books", body = Vec<BookDto>)))]
pub async fn monitor_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BookMonitorRequest>,
) -> ApiResult<ApiJson<Vec<BookDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.monitor_books(req).await?))
}

/// Queues a book search (job kind `book_search`, executed by the pipeline).
#[utoipa::path(post, path = "/api/v1/books/search", tag = "books", request_body = BookSearchRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 400, description = "Invalid request", body = ApiErrorBody),
    ))]
pub async fn search_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BookSearchRequest>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.request_search(req).await?))
}

/// Monitored books missing a format (`kind=missing`) or below the cutoff (`kind=cutoff`).
#[utoipa::path(get, path = "/api/v1/books/wanted", tag = "books", params(BookWantedQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Books", body = Paged<BookDto>)))]
pub async fn wanted_books(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookWantedQuery>,
) -> ApiResult<ApiJson<Paged<BookDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.wanted(&q).await?))
}

/// Files of an author whose names differ from the book naming templates (`media_id` = book id).
#[utoipa::path(get, path = "/api/v1/books/rename", tag = "books", params(BookRenameQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Planned renames", body = Vec<RenamePreviewDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn rename_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookRenameQuery>,
) -> ApiResult<ApiJson<Vec<RenamePreviewDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.rename_preview(q.author_id).await?))
}

/// Queues the rename of an author's files (job `book_rename`).
#[utoipa::path(post, path = "/api/v1/books/rename", tag = "books", params(BookRenameQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 202, description = "Queued", body = EnqueuedJob), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn rename_run(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookRenameQuery>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_rename(q.author_id).await?))
}

/// One book with its ebook and audiobook files.
#[utoipa::path(get, path = "/api/v1/books/{id}", tag = "books",
    params(("id" = i64, Path, description = "Book id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Book", body = BookDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn get_book(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<BookDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.get_book(id).await?))
}

/// Queues the merge of a multi-part audiobook into one chaptered M4B (job `book_merge`): one
/// chapter per part, ordered by track tags (or file names), AAC copied as it is and other audio
/// encoded to AAC. The parts are kept until the merged file is checked, then removed like
/// replaced files (into the recycle bin when one is set). Needs ffmpeg and ffprobe.
#[utoipa::path(post, path = "/api/v1/bookfiles/{id}/merge", tag = "books",
    params(("id" = i64, Path, description = "Book file id")),
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 202, description = "Queued", body = EnqueuedJob),
        (status = 400, description = "Not a multi-part audiobook, or more than 2,000 parts", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn merge_book_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Response> {
    require_lib(&user)?;
    Ok(accepted(svc(&state)?.queue_merge(id).await?))
}

/// Edits a book (format monitoring, preferred edition).
#[utoipa::path(put, path = "/api/v1/books/{id}", tag = "books", request_body = BookEdit,
    params(("id" = i64, Path, description = "Book id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Updated", body = BookDto), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn edit_book(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(edit): ApiJson<BookEdit>,
) -> ApiResult<ApiJson<BookDto>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.update_book(id, edit).await?))
}

/// Deletes a book (`?delete_files=true` also removes its files).
#[utoipa::path(delete, path = "/api/v1/books/{id}", tag = "books",
    params(("id" = i64, Path, description = "Book id"), DeleteQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_book(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_book(id, q.delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Editions of a book (fetched from Open Library the first time).
#[utoipa::path(get, path = "/api/v1/books/{id}/editions", tag = "books",
    params(("id" = i64, Path, description = "Book id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Editions", body = Vec<EditionDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn book_editions(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiJson<Vec<EditionDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.editions(id).await?))
}

// ---------------- files & series ----------------

/// Files of a book (one per format).
#[utoipa::path(get, path = "/api/v1/bookfiles", tag = "books", params(BookFilesQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Files", body = Vec<BookFileDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn list_book_files(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookFilesQuery>,
) -> ApiResult<ApiJson<Vec<BookFileDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.book_files(q.book_id).await?))
}

/// Deletes a book file (every part) from disk and the library.
#[utoipa::path(delete, path = "/api/v1/bookfiles/{id}", tag = "books",
    params(("id" = i64, Path, description = "Book file id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn delete_book_file(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    require_lib(&user)?;
    svc(&state)?.delete_book_file(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every series with its books ordered by position.
#[utoipa::path(get, path = "/api/v1/bookseries", tag = "books",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Series", body = Vec<BookSeriesDto>)))]
pub async fn list_book_series(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<BookSeriesDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.book_series().await?))
}

// ---------------- root folders & library import ----------------

/// Ebook and audiobook root folders with free space and unmapped folders.
#[utoipa::path(get, path = "/api/v1/bookrootfolders", tag = "books",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Root folders", body = Vec<ContentRootFolderDto>)))]
pub async fn list_root_folders(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<Vec<ContentRootFolderDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.root_folders().await?))
}

/// Adds an ebook or audiobook root folder (must exist; may not overlap any root folder).
#[utoipa::path(post, path = "/api/v1/bookrootfolders", tag = "books", request_body = CreateContentRootFolderRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 201, description = "Created", body = ContentRootFolderDto),
        (status = 400, description = "Missing/relative path or wrong kind", body = ApiErrorBody),
        (status = 409, description = "Duplicate or overlapping", body = ApiErrorBody),
    ))]
pub async fn add_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<CreateContentRootFolderRequest>,
) -> ApiResult<Response> {
    user.require(Permissions::ADMIN)?;
    Ok(created(svc(&state)?.add_root_folder(req).await?))
}

/// Removes a book root folder (refused while authors use it).
#[utoipa::path(delete, path = "/api/v1/bookrootfolders/{id}", tag = "books",
    params(("id" = i64, Path, description = "Root folder id")),
    security(("session" = []), ("api_key" = [])),
    responses((status = 204, description = "Deleted"), (status = 409, description = "In use", body = ApiErrorBody)))]
pub async fn delete_root_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    user.require(Permissions::ADMIN)?;
    svc(&state)?.delete_root_folder(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unmapped folders of a book root folder with Open Library author suggestions.
#[utoipa::path(get, path = "/api/v1/bookimport/scan", tag = "books", params(BookImportScanQuery),
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Candidates", body = Vec<BookImportCandidateDto>), (status = 404, description = "Not found", body = ApiErrorBody)))]
pub async fn import_scan(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiQuery(q): ApiQuery<BookImportScanQuery>,
) -> ApiResult<ApiJson<Vec<BookImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?
            .import_scan(q.root_folder_id, q.lookup.unwrap_or(true))
            .await?,
    ))
}

/// Open Library suggestions for up to 25 folders from a scan (`lookup=false`), so large
/// libraries are matched in short requests.
#[utoipa::path(post, path = "/api/v1/bookimport/match", tag = "books", request_body = LibraryImportMatchRequest,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Candidates with suggestions", body = Vec<BookImportCandidateDto>),
        (status = 400, description = "Too many folders or a folder outside the root folder", body = ApiErrorBody),
        (status = 404, description = "Not found", body = ApiErrorBody),
    ))]
pub async fn import_match(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<LibraryImportMatchRequest>,
) -> ApiResult<ApiJson<Vec<BookImportCandidateDto>>> {
    require_lib(&user)?;
    Ok(ApiJson(
        svc(&state)?.import_match(req.root_folder_id, req.paths).await?,
    ))
}

/// Adds existing author folders (each is scanned and monitored per its option).
#[utoipa::path(post, path = "/api/v1/bookimport", tag = "books", request_body = BookLibraryImportRequest,
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Result", body = LibraryImportResult)))]
pub async fn library_import(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(req): ApiJson<BookLibraryImportRequest>,
) -> ApiResult<ApiJson<LibraryImportResult>> {
    require_lib(&user)?;
    Ok(ApiJson(svc(&state)?.library_import(req).await?))
}

// ---------------- settings ----------------

/// Book naming, metadata/cover writing and Calibre settings.
#[utoipa::path(get, path = "/api/v1/settings/books", tag = "books",
    security(("session" = []), ("api_key" = [])),
    responses((status = 200, description = "Settings", body = BookSettingsDto), (status = 403, description = "Requires ADMIN", body = ApiErrorBody)))]
pub async fn get_settings(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<ApiJson<BookSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.settings().await?))
}

/// Validates (template tokens, Calibre path) and stores the book settings.
#[utoipa::path(put, path = "/api/v1/settings/books", tag = "books", request_body = BookSettingsDto,
    security(("session" = []), ("api_key" = [])),
    responses(
        (status = 200, description = "Stored", body = BookSettingsDto),
        (status = 400, description = "Invalid template", body = ApiErrorBody),
        (status = 403, description = "Requires ADMIN", body = ApiErrorBody),
    ))]
pub async fn put_settings(
    State(state): State<AppState>,
    user: CurrentUser,
    ApiJson(s): ApiJson<BookSettingsDto>,
) -> ApiResult<ApiJson<BookSettingsDto>> {
    user.require(Permissions::ADMIN)?;
    Ok(ApiJson(svc(&state)?.set_settings(s).await?))
}
