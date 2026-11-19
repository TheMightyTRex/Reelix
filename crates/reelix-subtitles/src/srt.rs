//! Checks on a downloaded subtitle: that it is a subtitle at all, WebVTT to SRT, and a simple
//! timing check against the video's length.

/// The most a subtitle may run past the end of the video: 10% of its length, at least 2 minutes.
pub fn timing_allowance_secs(video_secs: f64) -> f64 {
    (video_secs * 0.1).max(120.0)
}

/// Why a subtitle was turned down.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    /// No cue with a start and end time.
    NoCues,
    /// The last cue ends far beyond the video's end (both in seconds).
    TooLong { last_cue: f64, video: f64 },
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCues => f.write_str("no subtitle cues found"),
            Self::TooLong { last_cue, video } => write!(
                f,
                "the last cue ends at {last_cue:.0} s but the video is {video:.0} s long"
            ),
        }
    }
}

/// A subtitle ready to save: SRT bytes, as received when they were SRT already (the encoding
/// is left as it is; Jellyfin detects it).
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub bytes: Vec<u8>,
    pub cues: usize,
    pub last_cue_end: f64,
}

/// Checks `raw` and turns WebVTT into SRT. `video_secs` is the video's length when known.
pub fn check(raw: &[u8], video_secs: Option<f64>) -> Result<Checked, Rejection> {
    let raw = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw);
    let text = String::from_utf8_lossy(raw);
    let bytes = if text.trim_start().starts_with("WEBVTT") {
        vtt_to_srt(&text).into_bytes()
    } else {
        raw.to_vec()
    };
    let (cues, last) = cue_times(&String::from_utf8_lossy(&bytes));
    if cues == 0 {
        return Err(Rejection::NoCues);
    }
    if let Some(video) = video_secs.filter(|v| *v > 0.0)
        && last > video + timing_allowance_secs(video)
    {
        return Err(Rejection::TooLong {
            last_cue: last,
            video,
        });
    }
    Ok(Checked {
        bytes,
        cues,
        last_cue_end: last,
    })
}

/// The number of cues and the latest end time in seconds.
pub fn cue_times(text: &str) -> (usize, f64) {
    let mut cues = 0;
    let mut last = 0.0f64;
    for line in text.lines() {
        let Some((start, end)) = line.split_once("-->") else {
            continue;
        };
        let end = end.split_whitespace().next().unwrap_or("");
        if let (Some(_), Some(e)) = (parse_time(start.trim()), parse_time(end)) {
            cues += 1;
            last = last.max(e);
        }
    }
    (cues, last)
}

/// `01:02:03,456`, `01:02:03.456` or `02:03.456` in seconds.
fn parse_time(s: &str) -> Option<f64> {
    let (clock, millis) = s.split_once([',', '.'])?;
    let millis: f64 = millis.trim().parse().ok()?;
    let parts: Vec<f64> = clock
        .split(':')
        .map(|p| p.trim().parse::<f64>().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        [m, s] => m * 60.0 + s,
        _ => return None,
    };
    Some(secs + millis / 1000.0)
}

/// WebVTT to SRT: numbered cues, comma before the milliseconds, hours always written,
/// cue settings, notes and styles dropped.
pub fn vtt_to_srt(text: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let mut lines = block.lines().skip_while(|l| !l.contains("-->"));
        let Some(timing) = lines.next() else {
            continue;
        };
        let Some((start, end)) = timing.split_once("-->") else {
            continue;
        };
        let end = end.split_whitespace().next().unwrap_or("");
        let (Some(s), Some(e)) = (parse_time(start.trim()), parse_time(end)) else {
            continue;
        };
        n += 1;
        out.push_str(&format!("{n}\n{} --> {}\n", srt_time(s), srt_time(e)));
        for l in lines {
            out.push_str(l);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn srt_time(secs: f64) -> String {
    let total_ms = (secs * 1000.0).round() as u64;
    let (h, rest) = (total_ms / 3_600_000, total_ms % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    format!("{h:02}:{m:02}:{:02},{:03}", rest / 1000, rest % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello.\r\n\r\n2\r\n01:39:58,000 --> 01:40:00,250\r\nGoodbye.\r\n";

    #[test]
    fn reads_cues_and_last_end() {
        let (n, last) = cue_times(SRT);
        assert_eq!(n, 2);
        assert!((last - 6000.25).abs() < 1e-6);
    }

    #[test]
    fn accepts_a_subtitle_that_fits_the_video() {
        let c = check(SRT.as_bytes(), Some(6100.0)).unwrap();
        assert_eq!(c.cues, 2);
        assert_eq!(c.bytes, SRT.as_bytes());
        // within the allowance: 6000 s video, 10% is 600 s
        assert!(check(SRT.as_bytes(), Some(5600.0)).is_ok());
    }

    #[test]
    fn rejects_a_subtitle_far_beyond_the_video() {
        // a 45-minute episode with a 100-minute subtitle: another cut or another film
        let err = check(SRT.as_bytes(), Some(2700.0)).unwrap_err();
        assert!(matches!(err, Rejection::TooLong { .. }));
        assert!(err.to_string().contains("2700"));
    }

    #[test]
    fn unknown_length_skips_the_timing_check() {
        assert!(check(SRT.as_bytes(), None).is_ok());
    }

    #[test]
    fn rejects_something_that_is_not_a_subtitle() {
        assert_eq!(check(b"<html>Not found</html>", None), Err(Rejection::NoCues));
    }

    #[test]
    fn short_videos_get_two_minutes() {
        assert_eq!(timing_allowance_secs(300.0), 120.0);
        assert_eq!(timing_allowance_secs(7200.0), 720.0);
    }

    #[test]
    fn webvtt_becomes_srt() {
        let vtt = "\u{feff}WEBVTT\n\nNOTE made by hand\n\n00:01.000 --> 00:02.000 align:start\nOne\n\ncue-2\n01:00:00.500 --> 01:00:03.000\nTwo\nlines\n";
        let c = check(vtt.as_bytes(), None).unwrap();
        assert_eq!(
            String::from_utf8(c.bytes).unwrap(),
            "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n2\n01:00:00,500 --> 01:00:03,000\nTwo\nlines\n\n"
        );
    }

    #[test]
    fn other_encodings_are_kept_as_they_are() {
        let latin1 = b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xe9\n";
        assert_eq!(check(latin1, None).unwrap().bytes, latin1.to_vec());
    }
}
