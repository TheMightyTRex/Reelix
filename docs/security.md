# Reelix threat model and security review

*Last reviewed 2026-10-31 (M10).*

## What Reelix protects

- **Credentials and secrets**: users' passwords (argon2id hashes), session tokens and API keys (stored as SHA-256), provider secrets (indexer API keys, tracker passwords/cookies, download-client and media-server credentials, notification tokens; AES-GCM with `secret.key`), and the Plex tokens of linked users.
- **The library and the files on disk**: Reelix can move, rename, hardlink and delete media, and reads/writes its database, `config.toml`, `secret.key` and backups.
- **Private-tracker accounts**: session cookies and passkeys that would get an account banned if leaked.
- **Availability**: a NAS-class host that shouldn't be pushed out of memory by one client.

## Who might attack it

| Actor | Can reach | Typical goal |
|---|---|---|
| Unauthenticated network client | Login, setup (refused once a user exists), health summary, static UI | Guess passwords, exhaust memory, find a bypass |
| Authenticated request-only user | Discover, requests, issues, own profile | Read other users' data, escalate, learn secrets from error text |
| Manager (Manage library) | Library, search/grab, queue, manual import, *arr shims, Torznab feed | Reach files or settings reserved for admins |
| Malicious indexer / tracker / metadata server | Responses to Reelix's requests, redirects, download links | Capture cookies/keys, exhaust memory, exploit parsers |
| Malicious download content | Release names, `.torrent`/`.nzb`, EPUB/audio files Reelix imports | Crash parsers, zip bombs, path tricks |
| Malicious indexer definition (YAML catalog) | Templates, selectors, regexes executed by the engine | ReDoS, credential exfiltration |

Reelix runs on a LAN or behind a reverse proxy, possibly exposed to the internet. Admins are trusted (they can set scripts, paths and URLs). The host OS and the reverse proxy are out of scope.

## Controls

- **Authentication**: argon2id with a dummy verify for unknown users and at most 4 concurrent hashes; login rate limiting per IP and username; 256-bit random session tokens and API keys stored hashed; `HttpOnly; SameSite=Lax` cookies, `Secure` behind HTTPS; a CSRF header required for cookie-authenticated writes.
- **External logins**: OIDC uses PKCE, a nonce, and a `state` bound to the starting browser by a sealed cookie; Plex PIN exchanges only work from the browser that created the PIN; post-login redirects are relative, printable-ASCII paths.
- **Authorization**: every route checks a permission; request, issue and content-request access checks ownership; the *arr compatibility APIs require Manage library, and ADMIN for profile, definition and custom-format writes, like the native API.
- **Secrets**: provider secrets are encrypted at rest and masked in API responses; HTTP error messages strip URLs (API keys travel in query strings) before they reach health checks, logs or Torznab errors; request logs redact `apikey`/`token`/`passkey` query values; health check details are only shown to managers.
- **Outbound requests (SSRF)**: indexer, notification, list and media-server URLs pass the address guard (private ranges blocked unless allowed); private-tracker cookies and definition headers are only sent to the tracker's own hosts, never to a download link or redirect elsewhere; Torznab download links are sealed (purpose-tagged, expire after 30 days) so the proxy can't be pointed at arbitrary URLs.
- **Files**: imports, moves and deletes go through a path guard that canonicalises paths against configured roots; manual import outside those roots requires the path to resolve inside the download's own folder; naming tokens strip `..`, separators and control characters; backup/log names are restricted to safe names and restore writes fixed entries only.
- **Untrusted input**: response bodies are capped (32 MB pages, 64 MB downloads, 8 MB EPUB metadata); the parsers (release names, Newznab/Torznab XML, rTorrent XML-RPC, YAML definitions, templates, selectors, dates, EPUB OPF) have never-panic property tests and libFuzzer targets in `fuzz/`; definition regexes use `regex` (linear time) with a bounded `fancy-regex` fallback.
- **Output**: Torznab XML is escaped; the UI never renders server text as HTML; the CSP blocks inline scripts.

## Review findings (2026-10-31)

| # | Severity | Finding | Status |
|---|---|---|---|
| H1 | High | Tracker cookies/headers sent to any host a download URL or redirect named | Fixed: sent only to the tracker's hosts |
| H2 | High | Indexer API keys leaked through error text in health checks and Torznab errors | Fixed: URLs stripped from HTTP errors; health details need Manage library |
| H3 | High | Manual import `starts_with` check bypassable with `..` | Fixed: both paths canonicalised |
| M1 | Medium | *arr shims allowed settings writes with Manage library | Fixed: ADMIN required, job payloads hidden from non-admins |
| M2 | Medium | Plex PIN / OIDC state not bound to the browser | Fixed: sealed flow cookies |
| M3 | Medium | Parallel logins could exhaust memory via argon2 | Fixed: hashing concurrency capped |
| M4 | Medium | Unbounded remote response and EPUB entry reads | Fixed: size caps |
| M5 | Medium | OIDC return path open redirect via control characters; panic on non-ASCII | Fixed |
| L1 | Low | Address guard: DNS rebinding window, a few special IPv6/benchmark ranges not blocked | Open |
| L2 | Low | Manage users can grant non-admin permissions they don't hold | Fixed: only permissions the actor holds |
| L3 | Low | Widening seasons on a pending request skips the quota check | Fixed: added seasons count against the quota |
| L4 | Low | Rate limit keyed on the proxy's IP behind a reverse proxy; own password change keeps other sessions; no WebSocket Origin check | Open |
| L5 | Low | API keys in debug request logs | Fixed |
| L6 | Low | Sealed Torznab links had no expiry or purpose separation | Fixed |
| L7 | Low | Torrent/NZB fetch for releases without an indexer bypasses the address guard | Open |

Open items are tracked for M11; none is reachable without an authenticated account or a malicious server the admin added.

## Fuzzing

Stable property tests run with `cargo test` (`tests/fuzz_props.rs` in reelix-parser, reelix-indexers, reelix-downloaders and reelix-books). For longer runs:

```sh
cargo install cargo-fuzz
cd fuzz && cargo +nightly fuzz run release_name -- -max_total_time=600
# targets: release_name, newznab_xml, indexer_definition, definition_template,
#          selector_and_dates, xmlrpc, epub_opf
```
