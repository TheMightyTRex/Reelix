//! Metadata access for the library, abstracted so tests (and the server) can swap sources.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use reelix_core::MediaKind;
use reelix_metadata::{
    CollectionMetadata, ExternalSource, MetaError, MetadataProvider, MovieMetadata, SearchHit,
    SeriesExternalId, SeriesMetadata, Tmdb,
};

/// What the library needs from a metadata provider.
#[async_trait]
pub trait MetadataSource: Send + Sync {
    async fn movie(&self, tmdb_id: u32) -> Result<MovieMetadata, MetaError>;
    /// Series with every season and episode.
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError>;
    async fn collection(&self, tmdb_id: u32) -> Result<CollectionMetadata, MetaError>;
    async fn search(
        &self,
        query: &str,
        kind: MediaKind,
        year: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError>;
    /// Resolve an IMDb (`tt…`) or TVDB id.
    async fn find(
        &self,
        source: ExternalSource,
        id: &str,
        kind: MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError>;
}

#[async_trait]
impl MetadataSource for Tmdb {
    async fn movie(&self, tmdb_id: u32) -> Result<MovieMetadata, MetaError> {
        self.movie_details(tmdb_id).await
    }

    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        MetadataProvider::series(self, id).await
    }

    async fn collection(&self, tmdb_id: u32) -> Result<CollectionMetadata, MetaError> {
        Tmdb::collection(self, tmdb_id).await
    }

    async fn search(
        &self,
        query: &str,
        kind: MediaKind,
        year: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        let page = match kind {
            MediaKind::Movie => self.search_movies(query, year, 1).await?,
            MediaKind::Series => self.search_series(query, year, 1).await?,
        };
        Ok(page.results)
    }

    async fn find(
        &self,
        source: ExternalSource,
        id: &str,
        kind: MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError> {
        let r = Tmdb::find(self, source, id).await?;
        Ok(match kind {
            MediaKind::Movie => r.movies,
            MediaKind::Series => r.series,
        })
    }
}

type ResolveFut = Pin<Box<dyn Future<Output = Result<Arc<dyn MetadataSource>, MetaError>> + Send>>;

/// A source resolved on every call, so configuration changes (API key set later) apply
/// without a restart. The resolver typically returns a cached client.
#[derive(Clone)]
pub struct DynMetadata {
    resolve: Arc<dyn Fn() -> ResolveFut + Send + Sync>,
}

impl DynMetadata {
    pub fn new<F, Fut>(resolve: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Arc<dyn MetadataSource>, MetaError>> + Send + 'static,
    {
        Self {
            resolve: Arc::new(move || Box::pin(resolve())),
        }
    }

    /// Always fails with `NotConfigured` (no metadata provider).
    pub fn unconfigured() -> Self {
        Self::new(|| async { Err(MetaError::NotConfigured("TMDB")) })
    }

    async fn get(&self) -> Result<Arc<dyn MetadataSource>, MetaError> {
        (self.resolve)().await
    }
}

#[async_trait]
impl MetadataSource for DynMetadata {
    async fn movie(&self, tmdb_id: u32) -> Result<MovieMetadata, MetaError> {
        self.get().await?.movie(tmdb_id).await
    }
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        self.get().await?.series(id).await
    }
    async fn collection(&self, tmdb_id: u32) -> Result<CollectionMetadata, MetaError> {
        self.get().await?.collection(tmdb_id).await
    }
    async fn search(
        &self,
        query: &str,
        kind: MediaKind,
        year: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        self.get().await?.search(query, kind, year).await
    }
    async fn find(
        &self,
        source: ExternalSource,
        id: &str,
        kind: MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError> {
        self.get().await?.find(source, id, kind).await
    }
}
