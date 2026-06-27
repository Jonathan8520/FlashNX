//! Multi-source import + Flashpoint cover lookup.
//!
//! FlashNX stays CONTENT-NEUTRAL: the IMPORTER tab is a user-driven pipe
//! (paste an archive.org URL or a direct `.swf` URL), and Flashpoint is used
//! ONLY as a metadata/cover source for games the user already owns — never as
//! a downloadable game catalog. `net.rs` owns the HTTPS transport; this layer
//! adds the per-source URL parsing and JSON shapes on top of `net::http_get`.

pub mod flashpoint;
pub mod gamezip;

/// How a user-pasted import string is routed. The IMPORTER tab classifies the
/// input and dispatches to the matching flow. Deliberately small — FlashNX
/// never browses or downloads a bundled catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// archive.org item/details/download URL, or a bare item-id.
    ArchiveOrg,
    /// A direct `.swf` URL on any host.
    DirectUrl,
}

/// Classify a user-pasted import string so the IMPORTER tab can route it.
/// archive.org URLs and bare item-ids → `ArchiveOrg`; anything else (a full
/// URL, typically ending in `.swf`) → `DirectUrl`.
pub fn classify(input: &str) -> SourceKind {
    let t = input.trim();
    let lower = t.to_ascii_lowercase();
    // Wayback Machine snapshots (web.archive.org/web/<ts>/<url>) are a DIRECT
    // file fetch, not an archive.org item. They contain "archive.org", so this
    // must precede the item check below or they'd wrongly hit the metadata list.
    if lower.contains("web.archive.org") {
        return SourceKind::DirectUrl;
    }
    if lower.contains("archive.org") {
        return SourceKind::ArchiveOrg;
    }
    // A bare token with no scheme and no slash, not itself a `.swf`, is treated
    // as an archive.org item-id (back-compat with the original import flow).
    if !t.contains("://") && !t.contains('/') && !lower.ends_with(".swf") {
        return SourceKind::ArchiveOrg;
    }
    SourceKind::DirectUrl
}

/// Rewrite a Wayback Machine URL to its RAW-content form by inserting the `id_`
/// modifier after the timestamp: `/web/<ts>/<url>` -> `/web/<ts>id_/<url>`.
/// Without it Wayback serves an HTML page wrapper instead of the original file,
/// so a `.swf` import downloads markup instead of the movie. No-op for
/// non-Wayback URLs and ones that already carry a modifier (`id_`, `im_`, ...).
pub fn wayback_raw(url: &str) -> std::string::String {
    if !url.to_ascii_lowercase().contains("web.archive.org") {
        return url.to_string();
    }
    const MARK: &str = "/web/";
    if let Some(pos) = url.find(MARK) {
        let after = pos + MARK.len();
        let rest = &url[after..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        // A modifier (id_/im_/...) sits between the digits and the next '/';
        // its absence (rest jumps straight to '/') means we must insert `id_`.
        if digits > 0 && rest[digits..].starts_with('/') {
            return std::format!(
                "{}{}id_{}",
                &url[..after],
                &rest[..digits],
                &rest[digits..]
            );
        }
    }
    url.to_string()
}
