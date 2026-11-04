//! Where downloaded show artwork and station logos are recorded.
//!
//! Downloading (on refresh, never on a page view), size limits, SVG cleaning and serving by
//! id are the artwork extension point (`docs/radio.md`); these are the columns it fills.
//! Paths are relative to the data folder.

use reelix_api_types::radio::RadioStationDto;

use crate::error::{RadioError, Result};
use crate::{RadioService, entities, repo};

impl RadioService {
    pub async fn get_station(&self, id: i64) -> Result<RadioStationDto> {
        repo::stations::get(&self.db, id)
            .await?
            .ok_or_else(|| RadioError::NotFound(format!("radio station {id} not found")))
    }

    pub async fn show_artwork_path(&self, show_id: i64) -> Result<Option<String>> {
        repo::shows::artwork_path(&self.db, show_id).await
    }

    pub async fn set_show_artwork_path(&self, show_id: i64, path: Option<&str>) -> Result<()> {
        repo::shows::set_artwork_path(&self.db, show_id, path).await?;
        self.emit(entities::SHOW, "updated", show_id);
        Ok(())
    }

    pub async fn station_logo_path(&self, station_id: i64) -> Result<Option<String>> {
        repo::stations::logo_path(&self.db, station_id).await
    }

    pub async fn set_station_logo_path(&self, station_id: i64, path: Option<&str>) -> Result<()> {
        repo::stations::set_logo_path(&self.db, station_id, path).await?;
        self.emit("radio_station", "updated", station_id);
        Ok(())
    }
}
