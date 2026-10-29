//! Calendar, iCal feed and wanted (missing / cutoff unmet) lists.

use std::collections::HashMap;

use chrono::{Duration, NaiveDate};
use reelix_api_types::library::{
    CalendarEntryDto, CalendarKind, CalendarQuery, CalendarStatus, IcalQuery, MovieDto, Paged, ReleaseKind,
    SeriesDto, SortDir, WantedItemDto, WantedQuery,
};

use crate::LibraryService;
use crate::error::Result;
use crate::movies::paginate;
use crate::repo::{files, movies, series};

fn status(has_file: bool, monitored: bool, date: NaiveDate, today: NaiveDate) -> CalendarStatus {
    if has_file {
        CalendarStatus::Downloaded
    } else if !monitored {
        CalendarStatus::Unmonitored
    } else if date > today {
        CalendarStatus::Upcoming
    } else {
        CalendarStatus::Missing
    }
}

/// The date that makes a movie "available" for its minimum availability.
fn movie_release(m: &MovieDto) -> Option<NaiveDate> {
    use reelix_api_types::library::MinimumAvailability as M;
    match m.minimum_availability {
        M::Released => [m.digital_release, m.physical_release]
            .into_iter()
            .flatten()
            .min()
            .or(m.in_cinemas),
        _ => m.in_cinemas.or(m.digital_release).or(m.physical_release),
    }
}

fn ical_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn cutoff_name(profile: Option<&reelix_decision::QualityProfile>) -> Option<String> {
    let p = profile?;
    p.items.get(p.cutoff_rank()?).map(|i| i.display_name())
}

impl LibraryService {
    pub async fn calendar(&self, q: &CalendarQuery) -> Result<Vec<CalendarEntryDto>> {
        let today = self.today();
        let start = q.start.unwrap_or(today);
        let end = q.end.unwrap_or(start + Duration::days(7)).max(start);
        self.calendar_range(start, end, q.unmonitored).await
    }

    async fn calendar_range(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        unmonitored: bool,
    ) -> Result<Vec<CalendarEntryDto>> {
        let today = self.today();
        let cache = self.profiles().await?;
        let mut out = Vec::new();
        for m in movies::load(&self.db, None, &cache).await? {
            if !unmonitored && !m.monitored {
                continue;
            }
            for (kind, date) in [
                (ReleaseKind::Cinema, m.in_cinemas),
                (ReleaseKind::Digital, m.digital_release),
                (ReleaseKind::Physical, m.physical_release),
            ] {
                let Some(d) = date.filter(|d| *d >= start && *d <= end) else {
                    continue;
                };
                out.push(CalendarEntryDto {
                    kind: CalendarKind::Movie,
                    release_kind: kind,
                    date: d,
                    status: status(m.has_file, m.monitored, d, today),
                    monitored: m.monitored,
                    has_file: m.has_file,
                    movie_id: Some(m.id),
                    series_id: None,
                    episode_id: None,
                    title: m.title.clone(),
                    year: m.year,
                    season_number: None,
                    episode_number: None,
                    episode_title: None,
                    network: m.studio.clone(),
                    poster: m.images.poster.clone(),
                    quality: m.movie_file.as_ref().map(|f| f.file.quality_name.clone()),
                });
            }
        }
        let eps = series::episodes_between(&self.db, start, end).await?;
        if !eps.is_empty() {
            let all = series::load(&self.db, None, today).await?;
            let by_id: HashMap<i64, &SeriesDto> = all.iter().map(|s| (s.id, s)).collect();
            let qualities: HashMap<i64, String> = files::list(&self.db, files::FileTable::Episode, None)
                .await?
                .into_iter()
                .map(|f| (f.id, f.quality.quality.name().to_owned()))
                .collect();
            for e in eps {
                let Some(s) = by_id.get(&e.series_id) else {
                    continue;
                };
                let monitored = e.monitored && s.monitored;
                if !unmonitored && !monitored {
                    continue;
                }
                let Some(d) = e.air_date else { continue };
                out.push(CalendarEntryDto {
                    kind: CalendarKind::Episode,
                    release_kind: ReleaseKind::Airing,
                    date: d,
                    status: status(e.has_file, monitored, d, today),
                    monitored,
                    has_file: e.has_file,
                    movie_id: None,
                    series_id: Some(s.id),
                    episode_id: Some(e.id),
                    title: s.title.clone(),
                    year: s.year,
                    season_number: Some(e.season_number),
                    episode_number: Some(e.episode_number),
                    episode_title: e.title.clone(),
                    network: s.network.clone(),
                    poster: s.images.poster.clone(),
                    quality: e.episode_file_id.and_then(|id| qualities.get(&id).cloned()),
                });
            }
        }
        out.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.title.cmp(&b.title)));
        Ok(out)
    }

    /// iCalendar (RFC 5545) feed with all-day events.
    pub async fn ical(&self, q: &IcalQuery) -> Result<String> {
        let today = self.today();
        let start = today - Duration::days(i64::from(q.past_days.unwrap_or(7)));
        let end = today + Duration::days(i64::from(q.future_days.unwrap_or(28)));
        let entries = self.calendar_range(start, end, q.unmonitored).await?;
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let mut s = String::from(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Reelix//Calendar//EN\r\nCALSCALE:GREGORIAN\r\nX-WR-CALNAME:Reelix\r\n",
        );
        for e in entries {
            let (uid, summary) = match e.kind {
                CalendarKind::Movie => (
                    format!(
                        "reelix-movie-{}-{}",
                        e.movie_id.unwrap_or(0),
                        crate::convert::enum_str(&e.release_kind)
                    ),
                    format!(
                        "{} ({})",
                        e.title,
                        match e.release_kind {
                            ReleaseKind::Cinema => "Cinemas",
                            ReleaseKind::Digital => "Digital",
                            ReleaseKind::Physical => "Physical",
                            ReleaseKind::Airing => "Airing",
                        }
                    ),
                ),
                CalendarKind::Episode => (
                    format!("reelix-episode-{}", e.episode_id.unwrap_or(0)),
                    format!(
                        "{} - {}x{:02}{}",
                        e.title,
                        e.season_number.unwrap_or(0),
                        e.episode_number.unwrap_or(0),
                        e.episode_title
                            .as_ref()
                            .map(|t| format!(" - {t}"))
                            .unwrap_or_default()
                    ),
                ),
            };
            let next = e.date + Duration::days(1);
            s.push_str(&format!(
                "BEGIN:VEVENT\r\nUID:{uid}@reelix\r\nDTSTAMP:{stamp}\r\nDTSTART;VALUE=DATE:{}\r\nDTEND;VALUE=DATE:{}\r\nSUMMARY:{}\r\nSTATUS:CONFIRMED\r\nEND:VEVENT\r\n",
                e.date.format("%Y%m%d"),
                next.format("%Y%m%d"),
                ical_escape(&summary)
            ));
        }
        s.push_str("END:VCALENDAR\r\n");
        Ok(s)
    }

    pub async fn wanted_missing(&self, q: &WantedQuery) -> Result<Paged<WantedItemDto>> {
        self.wanted(q, false).await
    }

    pub async fn wanted_cutoff(&self, q: &WantedQuery) -> Result<Paged<WantedItemDto>> {
        self.wanted(q, true).await
    }

    async fn wanted(&self, q: &WantedQuery, cutoff: bool) -> Result<Paged<WantedItemDto>> {
        let today = self.today();
        let only_monitored = q.monitored.unwrap_or(true);
        let cache = self.profiles().await?;
        let mut items: Vec<(WantedItemDto, chrono::DateTime<chrono::Utc>)> = Vec::new();
        match q.kind.unwrap_or(CalendarKind::Movie) {
            CalendarKind::Movie => {
                for m in movies::load(&self.db, None, &cache).await? {
                    if only_monitored && !m.monitored {
                        continue;
                    }
                    let wanted = if cutoff {
                        m.movie_file.as_ref().is_some_and(|f| !f.file.cutoff_met)
                    } else {
                        !m.has_file && m.is_available
                    };
                    if !wanted {
                        continue;
                    }
                    let f = m.movie_file.as_ref().map(|f| &f.file);
                    items.push((
                        WantedItemDto {
                            kind: CalendarKind::Movie,
                            id: m.id,
                            movie_id: Some(m.id),
                            series_id: None,
                            title: m.title.clone(),
                            year: m.year,
                            season_number: None,
                            episode_number: None,
                            episode_title: None,
                            date: movie_release(&m),
                            monitored: m.monitored,
                            poster: m.images.poster.clone(),
                            quality_profile_id: m.quality_profile_id,
                            quality: f.map(|f| f.quality_name.clone()),
                            custom_format_score: f.map(|f| f.custom_format_score),
                            cutoff: cutoff_name(cache.profile(m.quality_profile_id)),
                        },
                        m.added,
                    ));
                }
            }
            CalendarKind::Episode => {
                let all = series::load(&self.db, None, today).await?;
                for s in &all {
                    if only_monitored && !s.monitored {
                        continue;
                    }
                    let eps = series::episodes(&self.db, s.id, None).await?;
                    let file_map: HashMap<i64, reelix_api_types::library::EpisodeFileDto> = if cutoff {
                        self.episode_files_of(s)
                            .await?
                            .into_iter()
                            .map(|f| (f.file.id, f))
                            .collect()
                    } else {
                        HashMap::new()
                    };
                    for e in eps {
                        if e.season_number == 0 || (only_monitored && !e.monitored) {
                            continue;
                        }
                        let file = e.episode_file_id.and_then(|id| file_map.get(&id));
                        let wanted = if cutoff {
                            file.is_some_and(|f| !f.file.cutoff_met)
                        } else {
                            !e.has_file && e.air_date.is_some_and(|d| d <= today)
                        };
                        if !wanted {
                            continue;
                        }
                        items.push((
                            WantedItemDto {
                                kind: CalendarKind::Episode,
                                id: e.id,
                                movie_id: None,
                                series_id: Some(s.id),
                                title: s.title.clone(),
                                year: s.year,
                                season_number: Some(e.season_number),
                                episode_number: Some(e.episode_number),
                                episode_title: e.title.clone(),
                                date: e.air_date,
                                monitored: e.monitored && s.monitored,
                                poster: s.images.poster.clone(),
                                quality_profile_id: s.quality_profile_id,
                                quality: file.map(|f| f.file.quality_name.clone()),
                                custom_format_score: file.map(|f| f.file.custom_format_score),
                                cutoff: cutoff_name(cache.profile(s.quality_profile_id)),
                            },
                            s.added,
                        ));
                    }
                }
            }
        }
        let sort = q.sort.as_deref().unwrap_or("date");
        items.sort_by(|(a, aa), (b, ba)| {
            let o = match sort {
                "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                "added" => aa.cmp(ba),
                _ => a.date.cmp(&b.date),
            };
            o.then_with(|| a.title.cmp(&b.title))
                .then_with(|| a.season_number.cmp(&b.season_number))
                .then_with(|| a.episode_number.cmp(&b.episode_number))
        });
        // Default: newest first for dates.
        let desc = match q.dir {
            Some(d) => d == SortDir::Desc,
            None => sort == "date" || sort == "added",
        };
        if desc {
            items.reverse();
        }
        let items: Vec<WantedItemDto> = items.into_iter().map(|(w, _)| w).collect();
        Ok(paginate(items, q.page, Some(q.page_size.unwrap_or(20))))
    }
}
