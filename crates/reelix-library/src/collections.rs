//! Movie collections: listing, editing and syncing (monitored collections add missing movies).

use std::collections::HashMap;

use reelix_api_types::library::{
    AddMovieRequest, CollectionDto, CollectionEdit, CollectionMovieDto, MovieMonitor,
};
use reelix_db::NewJob;

use crate::LibraryService;
use crate::error::{LibraryError, Result};
use crate::kinds;
use crate::repo::misc::{self, CollectionPart, CollectionRow};
use crate::repo::movies;

impl LibraryService {
    pub async fn list_collections(&self) -> Result<Vec<CollectionDto>> {
        let rows = misc::collections(&self.db, None).await?;
        self.collection_dtos(rows).await
    }

    async fn collection_dtos(&self, rows: Vec<CollectionRow>) -> Result<Vec<CollectionDto>> {
        let cache = self.profiles().await?;
        let all = movies::load(&self.db, None, &cache).await?;
        let by_tmdb: HashMap<u32, &reelix_api_types::library::MovieDto> =
            all.iter().map(|m| (m.tmdb_id, m)).collect();
        Ok(rows
            .into_iter()
            .map(|c| {
                let mut parts = c.parts.clone();
                // Movies in the library linked to the collection but not (yet) in the cached parts.
                for m in all
                    .iter()
                    .filter(|m| m.collection.as_ref().is_some_and(|x| x.id == c.id))
                {
                    if !parts.iter().any(|p| p.tmdb_id == m.tmdb_id) {
                        parts.push(CollectionPart {
                            tmdb_id: m.tmdb_id,
                            title: m.title.clone(),
                            year: m.year,
                            overview: m.overview.clone(),
                            poster: m.images.poster.clone(),
                        });
                    }
                }
                let movies: Vec<CollectionMovieDto> = parts
                    .iter()
                    .map(|p| {
                        let m = by_tmdb.get(&p.tmdb_id);
                        CollectionMovieDto {
                            tmdb_id: p.tmdb_id,
                            title: p.title.clone(),
                            year: p.year,
                            overview: p.overview.clone(),
                            poster: p.poster.clone(),
                            movie_id: m.map(|m| m.id),
                            monitored: m.is_some_and(|m| m.monitored),
                            has_file: m.is_some_and(|m| m.has_file),
                        }
                    })
                    .collect();
                CollectionDto {
                    id: c.id,
                    tmdb_id: c.tmdb_id,
                    title: c.title,
                    overview: c.overview,
                    images: c.images,
                    monitored: c.monitored,
                    quality_profile_id: c.quality_profile_id,
                    root_folder_id: c.root_folder_id,
                    minimum_availability: c.minimum_availability,
                    search_on_add: c.search_on_add,
                    tags: c.tags,
                    missing_movies: movies.iter().filter(|m| m.movie_id.is_none()).count() as u32,
                    movies,
                }
            })
            .collect())
    }

    pub async fn update_collection(&self, id: i64, edit: CollectionEdit) -> Result<CollectionDto> {
        if let Some(p) = edit.quality_profile_id {
            self.check_profile(p).await?;
        }
        if let Some(r) = edit.root_folder_id {
            self.require_root(r, reelix_core::MediaKind::Movie).await?;
        }
        if !misc::update_collection(&self.db, id, &edit).await? {
            return Err(LibraryError::not_found("collection", id));
        }
        self.emit("collection", "updated", id);
        if edit.monitored == Some(true) {
            self.enqueue(
                NewJob::new(
                    kinds::COLLECTION_SYNC,
                    serde_json::json!({ "collection_ids": [id] }),
                )
                .dedupe(format!("collection_sync:{id}")),
            )
            .await?;
        }
        let rows = misc::collections(&self.db, Some(&[id])).await?;
        self.collection_dtos(rows)
            .await?
            .pop()
            .ok_or_else(|| LibraryError::not_found("collection", id))
    }

    /// Refreshes collection parts from TMDB; monitored collections add their missing movies.
    /// Returns the number of movies added.
    pub async fn sync_collections(&self, ids: Option<Vec<i64>>) -> Result<usize> {
        let rows = misc::collections(&self.db, ids.as_deref().filter(|v| !v.is_empty())).await?;
        let mut added = 0;
        for c in rows {
            let meta = match self.metadata.collection(c.tmdb_id).await {
                Ok(m) => m,
                Err(reelix_metadata::MetaError::NotConfigured(p)) => {
                    return Err(reelix_metadata::MetaError::NotConfigured(p).into());
                }
                Err(e) => {
                    tracing::warn!(collection = c.id, error = %e, "collection sync failed");
                    continue;
                }
            };
            let parts: Vec<CollectionPart> = meta
                .parts
                .iter()
                .filter_map(|h| {
                    Some(CollectionPart {
                        tmdb_id: h.tmdb_id?,
                        title: h.title.clone(),
                        year: h.year,
                        overview: h.overview.clone(),
                        poster: h.poster.clone(),
                    })
                })
                .collect();
            misc::upsert_collection(
                &self.db,
                &CollectionRow {
                    title: meta.name.clone(),
                    overview: meta.overview.clone(),
                    images: crate::convert::images(&meta.images),
                    parts: parts.clone(),
                    ..c.clone()
                },
            )
            .await?;
            misc::set_collection_synced(&self.db, c.id, &parts).await?;
            if c.monitored {
                for p in &parts {
                    if movies::id_by_tmdb(&self.db, p.tmdb_id).await?.is_some() {
                        continue;
                    }
                    let req = AddMovieRequest {
                        tmdb_id: p.tmdb_id,
                        quality_profile_id: c.quality_profile_id,
                        root_folder_id: c.root_folder_id,
                        monitor: MovieMonitor::MovieOnly,
                        minimum_availability: c.minimum_availability,
                        tags: c.tags.clone(),
                        search_on_add: c.search_on_add,
                        path: None,
                        added_by_request_id: None,
                    };
                    match self.add_movie(req).await {
                        Ok(_) => added += 1,
                        Err(e) => {
                            tracing::warn!(collection = c.id, tmdb_id = p.tmdb_id, error = %e, "could not add collection movie")
                        }
                    }
                }
            }
            self.emit("collection", "updated", c.id);
        }
        misc::prune_collections(&self.db).await?;
        Ok(added)
    }
}
