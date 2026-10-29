//! Metadata profiles: which release-group types (and release statuses) an artist gets.

use reelix_api_types::music::{AlbumType, MetadataProfileDto, ReleaseStatus};
use reelix_metadata::ReleaseGroupMetadata;

use crate::convert::{album_type, secondary_type};
use crate::error::{MusicError, Result};
use crate::{MusicService, repo};

/// Whether a release group passes the profile's primary/secondary type filters.
pub(crate) fn allows_group(p: &MetadataProfileDto, rg: &ReleaseGroupMetadata) -> bool {
    let primary: AlbumType = album_type(rg.primary_type.as_deref());
    if !p.primary_types.contains(&primary) {
        return false;
    }
    rg.secondary_types.iter().all(|s| match secondary_type(s) {
        Some(t) => p.secondary_types.contains(&t),
        // Unknown secondary types are treated as disallowed unless the profile allows all.
        None => false,
    })
}

/// Whether a release status passes (unknown status counts as official).
pub(crate) fn allows_status(p: &MetadataProfileDto, status: Option<ReleaseStatus>) -> bool {
    p.release_statuses.is_empty()
        || p.release_statuses
            .contains(&status.unwrap_or(ReleaseStatus::Official))
}

fn validate(p: &MetadataProfileDto) -> Result<()> {
    if p.name.trim().is_empty() {
        return Err(MusicError::Validation("name is required".into()));
    }
    if p.primary_types.is_empty() {
        return Err(MusicError::Validation(
            "at least one primary type is required".into(),
        ));
    }
    if p.release_statuses.is_empty() {
        return Err(MusicError::Validation(
            "at least one release status is required".into(),
        ));
    }
    Ok(())
}

impl MusicService {
    pub async fn metadata_profiles(&self) -> Result<Vec<MetadataProfileDto>> {
        repo::metadata_profiles::list(&self.db).await
    }

    pub(crate) async fn metadata_profile(&self, id: i64) -> Result<MetadataProfileDto> {
        repo::metadata_profiles::get(&self.db, id)
            .await?
            .ok_or_else(|| MusicError::Validation(format!("metadata profile {id} does not exist")))
    }

    pub async fn create_metadata_profile(&self, p: MetadataProfileDto) -> Result<MetadataProfileDto> {
        validate(&p)?;
        let id = repo::metadata_profiles::insert(&self.db, &p).await?;
        self.emit("metadataprofile", "created", id);
        self.metadata_profile(id).await
    }

    pub async fn update_metadata_profile(
        &self,
        id: i64,
        p: MetadataProfileDto,
    ) -> Result<MetadataProfileDto> {
        validate(&p)?;
        if !repo::metadata_profiles::update(&self.db, id, &p).await? {
            return Err(MusicError::NotFound(format!("metadata profile {id} not found")));
        }
        self.emit("metadataprofile", "updated", id);
        self.metadata_profile(id).await
    }

    /// Refuses while artists use the profile.
    pub async fn delete_metadata_profile(&self, id: i64) -> Result<()> {
        let used = repo::artists::count_using(&self.db, "metadata_profile_id", id).await?;
        if used > 0 {
            return Err(MusicError::Conflict(format!(
                "metadata profile {id} is used by {used} artist(s)"
            )));
        }
        if !repo::metadata_profiles::delete(&self.db, id).await? {
            return Err(MusicError::NotFound(format!("metadata profile {id} not found")));
        }
        self.emit("metadataprofile", "deleted", id);
        Ok(())
    }
}
