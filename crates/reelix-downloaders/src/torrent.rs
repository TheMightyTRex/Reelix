//! Torrent helpers: info-hash from `.torrent` bytes or magnet links, and resolving
//! download URLs (which may redirect to a magnet) into a concrete torrent source.

use sha1::{Digest, Sha1};

use crate::ClientError;
use crate::model::ReleasePayload;

/// A torrent ready to hand to a client, with its lowercase hex info hash.
#[derive(Debug, Clone, PartialEq)]
pub enum TorrentSource {
    File {
        bytes: Vec<u8>,
        name: String,
        hash: String,
    },
    Magnet {
        uri: String,
        hash: String,
    },
}

impl TorrentSource {
    pub fn hash(&self) -> &str {
        match self {
            Self::File { hash, .. } | Self::Magnet { hash, .. } => hash,
        }
    }
}

/// Computes the v1 info hash (SHA-1 of the bencoded `info` dictionary), lowercase hex.
pub fn info_hash(torrent: &[u8]) -> Result<String, ClientError> {
    let bad = |m: &str| ClientError::InvalidPayload(format!("invalid torrent file: {m}"));
    if torrent.first() != Some(&b'd') {
        return Err(bad("not a bencoded dictionary"));
    }
    let mut pos = 1;
    while pos < torrent.len() && torrent[pos] != b'e' {
        let (key, after_key) = read_bytes(torrent, pos).ok_or_else(|| bad("bad key"))?;
        let end = skip_value(torrent, after_key, 0).ok_or_else(|| bad("bad value"))?;
        if key == b"info" {
            let digest = Sha1::digest(&torrent[after_key..end]);
            return Ok(hex_lower(&digest));
        }
        pos = end;
    }
    Err(bad("missing info dictionary"))
}

fn read_bytes(b: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    let colon = pos + b.get(pos..)?.iter().position(|&c| c == b':')?;
    let len: usize = std::str::from_utf8(&b[pos..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    let end = start.checked_add(len)?;
    Some((b.get(start..end)?, end))
}

/// Returns the index just past the value starting at `pos`.
fn skip_value(b: &[u8], pos: usize, depth: usize) -> Option<usize> {
    if depth > 64 {
        return None;
    }
    match *b.get(pos)? {
        b'i' => Some(pos + b.get(pos..)?.iter().position(|&c| c == b'e')? + 1),
        b'l' | b'd' => {
            let mut p = pos + 1;
            while *b.get(p)? != b'e' {
                p = skip_value(b, p, depth + 1)?;
            }
            Some(p + 1)
        }
        b'0'..=b'9' => read_bytes(b, pos).map(|(_, end)| end),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Extracts the info hash (`xt=urn:btih:`) from a magnet link, as lowercase hex.
/// Accepts 40-char hex and 32-char base32 forms.
pub fn magnet_hash(uri: &str) -> Result<String, ClientError> {
    let bad = || ClientError::InvalidPayload(format!("magnet link without a btih hash: {uri}"));
    let url = url::Url::parse(uri).map_err(|_| bad())?;
    if url.scheme() != "magnet" {
        return Err(bad());
    }
    for (k, v) in url.query_pairs() {
        if k != "xt" {
            continue;
        }
        let Some(h) = v
            .strip_prefix("urn:btih:")
            .or_else(|| v.strip_prefix("urn:BTIH:"))
        else {
            continue;
        };
        if h.len() == 40 && h.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(h.to_ascii_lowercase());
        }
        if h.len() == 32
            && let Some(raw) = base32_decode(h)
        {
            return Ok(hex_lower(&raw));
        }
    }
    Err(bad())
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(20);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Turns any torrent payload into a [`TorrentSource`]. URLs are fetched with `http`
/// (which must not follow redirects automatically) so a redirect to a magnet link works.
pub async fn resolve(
    http: &reqwest::Client,
    payload: &ReleasePayload,
    title: &str,
) -> Result<TorrentSource, ClientError> {
    match payload {
        ReleasePayload::TorrentFile { bytes, name } => Ok(TorrentSource::File {
            hash: info_hash(bytes)?,
            bytes: bytes.clone(),
            name: name.clone(),
        }),
        ReleasePayload::Magnet(uri) => Ok(TorrentSource::Magnet {
            hash: magnet_hash(uri)?,
            uri: uri.clone(),
        }),
        ReleasePayload::Url(u) => fetch(http, u, title).await,
        ReleasePayload::NzbFile { .. } => Err(ClientError::InvalidPayload(
            "an NZB cannot be sent to a torrent client".into(),
        )),
        ReleasePayload::Soulseek(_) => Err(ClientError::InvalidPayload(
            "a Soulseek release cannot be sent to a torrent client".into(),
        )),
    }
}

/// Downloads a `.torrent` (following up to 6 redirects, magnet hops included).
///
/// No SSRF guard here: callers must check the address of `url` first (the pipeline runs the
/// indexer `AddressGuard` before handing out a [`ReleasePayload::Url`]).
async fn fetch(http: &reqwest::Client, url: &str, title: &str) -> Result<TorrentSource, ClientError> {
    let mut current = url.to_owned();
    for _ in 0..6 {
        if current.starts_with("magnet:") {
            return Ok(TorrentSource::Magnet {
                hash: magnet_hash(&current)?,
                uri: current,
            });
        }
        let resp = http.get(&current).send().await?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ClientError::InvalidPayload("redirect without Location".into()))?;
            let base = url::Url::parse(&current).map_err(|e| ClientError::InvalidPayload(e.to_string()))?;
            current = if loc.starts_with("magnet:") {
                loc.to_owned()
            } else {
                base.join(loc)
                    .map_err(|e| ClientError::InvalidPayload(e.to_string()))?
                    .to_string()
            };
            continue;
        }
        if !status.is_success() {
            return Err(ClientError::InvalidPayload(format!(
                "fetching torrent failed: HTTP {status}"
            )));
        }
        let bytes = crate::common::read_limited(resp, crate::common::MAX_DOWNLOAD_BYTES).await?;
        return Ok(TorrentSource::File {
            hash: info_hash(&bytes)?,
            name: format!("{}.torrent", crate::common::sanitize_file_name(title)),
            bytes,
        });
    }
    Err(ClientError::InvalidPayload("too many redirects".into()))
}

/// A `reqwest` client that leaves redirects to [`resolve`].
pub fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// Builds a tiny but valid single-file torrent (tests and fixtures).
#[doc(hidden)]
pub fn sample_torrent(name: &str) -> Vec<u8> {
    let info = format!(
        "d6:lengthi1024e4:name{}:{}12:piece lengthi16384e6:pieces20:{}e",
        name.len(),
        name,
        "x".repeat(20)
    );
    let ann = "http://tracker.example/announce";
    format!("d8:announce{}:{ann}4:info{info}e", ann.len()).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_hash_hashes_info_dict() {
        let t = sample_torrent("Movie.2020.mkv");
        let h = info_hash(&t).unwrap();
        let start = t.windows(6).position(|w| w == b"4:info").unwrap() + 6;
        let want = hex_lower(&Sha1::digest(&t[start..t.len() - 1]));
        assert_eq!(h, want);
        assert_eq!(h.len(), 40);
        assert!(info_hash(b"garbage").is_err());
        assert!(info_hash(b"d3:fooi1ee").is_err());
    }

    #[test]
    fn magnet_hashes() {
        let hex = "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A&dn=x";
        assert_eq!(
            magnet_hash(hex).unwrap(),
            "c12fe1c06bba254a9dc9f519b335aa7c1367a88a"
        );
        // Same hash in base32.
        let b32 = "magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK";
        assert_eq!(
            magnet_hash(b32).unwrap(),
            "c12fe1c06bba254a9dc9f519b335aa7c1367a88a"
        );
        assert!(magnet_hash("magnet:?dn=nohash").is_err());
        assert!(magnet_hash("http://x").is_err());
    }
}
