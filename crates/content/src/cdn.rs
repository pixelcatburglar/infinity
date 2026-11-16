//! Content CDN. The public gateway answers flatbuffer content requests
//! (content/client/flatbuffer/provider.py) with a checkpoint URL that the client
//! fetches over plain HTTP on port 80; the image server serves `/content/<name>`.
//! The content itself is what the retail server published for this build
//! (crate::retail).

/// The published content and its version, by CDN name.
pub fn content(name: &str) -> Option<(&'static [u8], (u32, u32, u32))> {
    crate::retail::cdn(name).map(|f| (f.data, f.version))
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}
