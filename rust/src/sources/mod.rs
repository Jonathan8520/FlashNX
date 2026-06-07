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
