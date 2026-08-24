//! **Good-net-citizen policy** for every outbound HTTP request.
//!
//! kaleidotron browses other people's servers — arbitrary sites via the HTTP browser, plus a dozen
//! free APIs and archives. This module is the etiquette layer, applied at the single choke point
//! ([`crate::cache::http_get`]) so no source can bypass it.
//!
//! What it enforces, and why each one is standard practice:
//!
//! * **`robots.txt` ([RFC 9309])** — the web's opt-out mechanism. Fetched once per host, cached for
//!   the session, and consulted before every request. `Disallow` rules for `*` (and for our own
//!   token) are honoured. A missing or unreadable `robots.txt` means *allowed* — that's what the
//!   RFC specifies, not a reason to refuse.
//! * **Per-host rate limiting** — a minimum gap between requests to the *same* host, so a grid of
//!   thumbnails or a recursive crawl can't hammer one server. Different hosts are independent, so
//!   this costs nothing when browsing several places at once.
//! * **`Crawl-delay`** — a non-standard but widely honoured `robots.txt` directive. When a host
//!   asks for a longer gap than our default, we use theirs.
//! * **`429` / `503` + `Retry-After`** — the server explicitly saying "slow down". We record the
//!   requested cooldown and hold off on that host until it passes.
//! * **An honest, identifying `User-Agent`** with a contact URL (set in [`crate::cache`]), so an
//!   operator seeing us in their logs can tell what we are and where to complain.
//!
//! Not implemented deliberately: we never ignore `robots.txt` for "just one file", and there is no
//! setting to disable this — a politeness layer that can be switched off is one that will be.
//!
//! **One principled exception — the integrated sources ([`API_HOSTS`]).** RFC 9309 is the *robots*
//! exclusion protocol: it governs "automatic clients known as **crawlers**" that discover and index
//! content by following links. A first-party client making a *user-initiated* request to a
//! **documented API or an intended file-download** is not a crawler — the API is the product, meant
//! to be called programmatically. Many sites `Disallow` their API / download paths purely to keep
//! *crawlers* out (Wikimedia `Disallow: /w/`, Iconify `Disallow: /`, ModArchive `/download.php`,
//! Lospec `/palette-list/*.gpl$`, Openverse `/v1/…` next to anti-AI-scraper `GPTBot`/`CCBot`
//! blocks); enforcing those against the app's own documented clients just makes the feature silently
//! return nothing. So for the specific hosts this app integrates as a source, we **skip the robots
//! path block only** — every other courtesy (per-host rate limit, `Crawl-delay`, `429`/`Retry-After`,
//! the honest `User-Agent`) still applies, which is the part that actually protects the server from
//! load. This is NOT a general opt-out: it's a fixed, code-level allowlist. The **HTTP browser**
//! (arbitrary user URLs — genuine crawling of sites we have no relationship with) is deliberately
//! excluded, so it stays fully robots-gated.
//!
//! [RFC 9309]: https://www.rfc-editor.org/rfc/rfc9309.html

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Default minimum gap between two requests to the same host.
pub const MIN_HOST_INTERVAL: Duration = Duration::from_millis(500);
/// Ceiling on a host-requested `Crawl-delay` / `Retry-After`, so a hostile or mistaken value can't
/// wedge the UI for minutes. Beyond this we simply give up on the request instead of sleeping.
pub const MAX_WAIT: Duration = Duration::from_secs(5);
/// The product token we match `robots.txt` groups against.
pub const UA_TOKEN: &str = "kaleidotron";

/// Hosts of the **integrated web sources** this app is a documented first-party client of, where the
/// robots.txt *path* block is skipped (see the module docs). Rate-limiting / Crawl-delay / 429
/// handling still apply — only the crawler-oriented path block is lifted. Every one of these serves
/// a documented API or an intended user-download whose robots.txt `Disallow` targets bulk crawlers,
/// not a desktop client fetching one thing on a user's action — and enforcing it just makes the
/// feature silently return nothing. A listed host matches its exact host or any subdomain.
///
/// NB the **HTTP browser** (arbitrary user-entered URLs — the one place we genuinely *are* crawling
/// a site we have no relationship with) is deliberately NOT here, so it stays fully robots-gated.
pub const API_HOSTS: &[&str] = &[
    "api.openverse.org",     // image + audio + GIF search (JSON API; Disallow: /v1/…)
    "commons.wikimedia.org", // vector (SVG) search — the MediaWiki API (Disallow: /w/)
    "upload.wikimedia.org",  // Wikimedia file + thumbnail downloads
    "api.iconify.design",    // icon search (JSON API; Disallow: /)
    "lospec.com",            // palette browser + .gpl/.png downloads (Disallow: /palette-list/*.gpl$…)
    "modarchive.org",        // tracker-module search + downloads (covers api.modarchive.org)
];

/// The bare host of an origin like `https://api.openverse.org` → `api.openverse.org`.
fn origin_host(origin: &str) -> &str {
    origin.split_once("://").map(|(_, h)| h).unwrap_or(origin)
}

/// Is this origin one of the documented APIs we call as a first-party client (so the robots.txt
/// path block doesn't apply)? Matches the exact host or a subdomain of a listed host.
pub fn is_documented_api(origin: &str) -> bool {
    let host = origin_host(origin);
    API_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// What a host's `robots.txt` tells us.
#[derive(Clone, Debug, Default)]
pub struct Robots {
    /// Path prefixes we must not fetch.
    pub disallow: Vec<String>,
    /// Prefixes explicitly re-allowed (longest-match wins over `disallow`, per RFC 9309).
    pub allow: Vec<String>,
    /// Host-requested gap between requests, if any.
    pub crawl_delay: Option<Duration>,
}

impl Robots {
    /// Is `path` (the URL path, e.g. `/pub/modules/`) fetchable?
    ///
    /// RFC 9309: the **most specific** (longest) matching rule wins, and `Allow` beats `Disallow`
    /// on an equal-length match — so a site can carve an exception out of a broad block.
    pub fn allows(&self, path: &str) -> bool {
        let best = |rules: &[String]| -> Option<usize> {
            rules
                .iter()
                .filter(|r| path.starts_with(r.as_str()))
                .map(|r| r.len())
                .max()
        };
        match (best(&self.disallow), best(&self.allow)) {
            (Some(d), Some(a)) => a >= d,
            (Some(_), None) => false,
            _ => true,
        }
    }
}

/// Parse a `robots.txt` body, keeping only the groups that apply to us: `User-agent: *` and any
/// group naming our token. Other agents' groups are skipped entirely.
pub fn parse_robots(body: &str) -> Robots {
    let mut out = Robots::default();
    // Are we inside a group that applies to us? Consecutive `User-agent:` lines share one group.
    let (mut applies, mut in_group_header) = (false, false);
    for raw in body.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "user-agent" => {
                // A new group starts after any rule line; stacked agents extend the current one.
                if !in_group_header {
                    applies = false;
                    in_group_header = true;
                }
                let ua = v.to_ascii_lowercase();
                if ua == "*" || ua.contains(UA_TOKEN) {
                    applies = true;
                }
            }
            "disallow" | "allow" | "crawl-delay" => {
                in_group_header = false;
                if !applies {
                    continue;
                }
                match k.as_str() {
                    // An empty `Disallow:` means "nothing is disallowed" — not "block everything".
                    "disallow" if !v.is_empty() => out.disallow.push(v.to_string()),
                    "allow" if !v.is_empty() => out.allow.push(v.to_string()),
                    "crawl-delay" => {
                        if let Ok(secs) = v.parse::<f64>() {
                            if secs > 0.0 {
                                out.crawl_delay = Some(Duration::from_secs_f64(secs.min(60.0)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {} // Sitemap, Host, … — not our concern
        }
    }
    out
}

/// Split a URL into `(scheme_host, path)` — e.g. `("https://x.org", "/a/b")`.
pub fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Some((format!("{scheme}://{host}"), path.to_string()))
}

#[derive(Default)]
struct HostState {
    robots: Option<Robots>,
    last: Option<Instant>,
    /// Set by a `429`/`503`; no request to this host until it passes.
    cooldown_until: Option<Instant>,
}

fn hosts() -> &'static Mutex<HashMap<String, HostState>> {
    static H: OnceLock<Mutex<HashMap<String, HostState>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fetch a host's `robots.txt` directly — deliberately NOT through `cache::http_get`, which would
/// recurse straight back into this module.
fn fetch_robots(origin: &str) -> Robots {
    let url = format!("{origin}/robots.txt");
    match ureq::get(&url)
        .set("User-Agent", crate::cache::USER_AGENT)
        .timeout(Duration::from_secs(10))
        .call()
    {
        // A 4xx (usually 404 = no robots.txt) means unrestricted, per RFC 9309.
        Ok(r) => r.into_string().map(|b| parse_robots(&b)).unwrap_or_default(),
        Err(_) => Robots::default(),
    }
}

/// Gate an outbound request: refuse it if `robots.txt` disallows the path, otherwise sleep just
/// long enough to respect the per-host interval. Returns `Err` with a human reason when the request
/// must not proceed.
///
/// Called on worker threads only — the UI thread never performs network I/O.
pub fn before_request(url: &str) -> Result<(), String> {
    let Some((origin, path)) = split_url(url) else {
        return Ok(()); // not http(s) — nothing to police
    };

    // Load robots.txt once per host. Done outside the lock: the fetch can take seconds and must not
    // block every other host's requests behind this one.
    let known = {
        let map = hosts().lock().map_err(|_| "lock")?;
        map.get(&origin).map(|h| h.robots.is_some()).unwrap_or(false)
    };
    if !known {
        let r = fetch_robots(&origin);
        let mut map = hosts().lock().map_err(|_| "lock")?;
        map.entry(origin.clone()).or_default().robots = Some(r);
    }

    // Decide whether we may fetch, and how long to wait first.
    let wait = {
        let mut map = hosts().lock().map_err(|_| "lock")?;
        let st = map.entry(origin.clone()).or_default();
        // Honour robots.txt path blocks — EXCEPT for the documented APIs we're a first-party client
        // of (a crawler-exclusion rule shouldn't forbid a documented API call; see module docs).
        if !is_documented_api(&origin) {
            if let Some(r) = &st.robots {
                if !r.allows(&path) {
                    return Err(format!("blocked by robots.txt: {path}"));
                }
            }
        }
        let now = Instant::now();
        if let Some(until) = st.cooldown_until {
            if until > now {
                let left = until - now;
                if left > MAX_WAIT {
                    return Err("host asked us to back off; try again shortly".into());
                }
                st.last = Some(now + left);
                left
            } else {
                st.cooldown_until = None;
                Duration::ZERO
            }
        } else {
            let gap = st
                .robots
                .as_ref()
                .and_then(|r| r.crawl_delay)
                .unwrap_or(MIN_HOST_INTERVAL)
                .min(MAX_WAIT);
            let wait = st
                .last
                .map(|t| gap.saturating_sub(now.duration_since(t)))
                .unwrap_or(Duration::ZERO);
            // Reserve our slot *before* releasing the lock, so concurrent workers queue up behind
            // each other instead of all seeing the same stale `last` and firing at once.
            st.last = Some(now + wait);
            wait
        }
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    Ok(())
}

/// Record a `429`/`503` response: hold off on this host until `Retry-After` (or a default) passes.
pub fn note_throttled(url: &str, retry_after: Option<&str>) {
    let Some((origin, _)) = split_url(url) else { return };
    let secs = retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 300);
    if let Ok(mut map) = hosts().lock() {
        map.entry(origin).or_default().cooldown_until = Some(Instant::now() + Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_groups_that_apply_to_us_only() {
        let r = parse_robots(
            "User-agent: EvilBot\nDisallow: /\n\n\
             User-agent: *\nDisallow: /private\nDisallow: /tmp\nAllow: /private/ok\nCrawl-delay: 2\n",
        );
        // The EvilBot group must NOT leak into ours.
        assert_eq!(r.disallow, vec!["/private", "/tmp"]);
        assert_eq!(r.allow, vec!["/private/ok"]);
        assert_eq!(r.crawl_delay, Some(Duration::from_secs(2)));
    }

    #[test]
    fn longest_match_wins_and_allow_beats_disallow() {
        let r = parse_robots("User-agent: *\nDisallow: /private\nAllow: /private/ok\n");
        assert!(!r.allows("/private/secret"));
        assert!(r.allows("/private/ok/file.png"), "explicit Allow carves an exception");
        assert!(r.allows("/public/x"));
    }

    #[test]
    fn absent_or_empty_rules_mean_allowed() {
        assert!(Robots::default().allows("/anything"));
        // An empty `Disallow:` is the documented way to say "everything is permitted".
        let r = parse_robots("User-agent: *\nDisallow:\n");
        assert!(r.disallow.is_empty());
        assert!(r.allows("/anything"));
        // Comments and blank lines are ignored.
        let r = parse_robots("# hello\n\nUser-agent: *\nDisallow: /x  # trailing\n");
        assert_eq!(r.disallow, vec!["/x"]);
    }

    #[test]
    fn our_own_token_is_matched() {
        let r = parse_robots("User-agent: kaleidotron\nDisallow: /nope\n");
        assert!(!r.allows("/nope"));
    }

    #[test]
    fn stacked_user_agents_share_a_group() {
        // `User-agent: a` + `User-agent: *` on consecutive lines = one group covering both.
        let r = parse_robots("User-agent: SomeBot\nUser-agent: *\nDisallow: /shared\n");
        assert!(!r.allows("/shared"));
    }

    #[test]
    fn documented_apis_are_exempt_from_robots_path_block() {
        // Every integrated source host (and its subdomains) is exempt.
        assert!(is_documented_api("https://api.openverse.org")); // /v1/images/ + /v1/audio/
        assert!(is_documented_api("https://cdn.api.openverse.org")); // subdomain
        assert!(is_documented_api("https://commons.wikimedia.org")); // vector search (/w/api.php)
        assert!(is_documented_api("https://upload.wikimedia.org")); // Wikimedia files
        assert!(is_documented_api("https://api.iconify.design")); // icon search
        assert!(is_documented_api("https://lospec.com")); // palette downloads
        assert!(is_documented_api("https://modarchive.org")); // module downloads
        assert!(is_documented_api("https://api.modarchive.org")); // subdomain of modarchive.org
        // Unrelated hosts and spoofs are NOT exempt (the HTTP browser stays robots-gated).
        assert!(!is_documented_api("https://openverse.org")); // the site, not the API host
        assert!(!is_documented_api("https://api.openverse.org.evil.com")); // suffix spoof
        assert!(!is_documented_api("https://en.wikipedia.org")); // not a listed source host
        assert!(!is_documented_api("https://example.org"));
    }

    #[test]
    fn splits_urls() {
        assert_eq!(
            split_url("https://x.org/a/b?q=1"),
            Some(("https://x.org".into(), "/a/b?q=1".into()))
        );
        assert_eq!(split_url("https://x.org"), Some(("https://x.org".into(), "/".into())));
        assert_eq!(split_url("not a url"), None);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    #[test]
    #[ignore = "hits the live network"]
    fn reads_real_robots_txt() {
        for origin in ["https://modarchive.org", "https://www.qb64phoenix.com"] {
            let r = fetch_robots(origin);
            eprintln!(
                "{origin}: {} disallow, {} allow, crawl_delay={:?}",
                r.disallow.len(), r.allow.len(), r.crawl_delay
            );
            eprintln!("   /  allowed = {}", r.allows("/"));
        }
    }
}
