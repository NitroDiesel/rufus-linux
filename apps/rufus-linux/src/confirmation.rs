//! Source details shared by the confirmation and its non-destructive UI fixture.

use rufus_core::plan::{ImageSource, ImageSourceKind};
use rufus_image::format_size;

pub fn source_details(source: &ImageSource) -> String {
    let disk_size = source.decompressed_size_bytes.or_else(|| {
        matches!(
            source.kind,
            ImageSourceKind::Raw | ImageSourceKind::IsoHybrid
        )
        .then_some(source.size_bytes)
    });
    format!(
        "Image: {}\nFile size: {}\nDisk size: {}",
        source.path.display(),
        format_size(source.size_bytes),
        disk_size
            .map(format_size)
            .unwrap_or_else(|| "Unknown; bounded by target capacity".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_bytes_are_never_presented_as_decoded_capacity() {
        let mut source = ImageSource {
            path: "/images/test.vhdx".into(),
            kind: ImageSourceKind::Vhdx,
            size_bytes: 8 * 1024 * 1024,
            decompressed_size_bytes: Some(16 * 1024 * 1024),
            sha256: None,
        };
        assert_eq!(
            source_details(&source),
            "Image: /images/test.vhdx\nFile size: 8.0 MB\nDisk size: 16.0 MB"
        );
        source.decompressed_size_bytes = None;
        for kind in [
            ImageSourceKind::Vhd,
            ImageSourceKind::Vhdx,
            ImageSourceKind::CompressedRaw,
        ] {
            source.kind = kind;
            assert!(source_details(&source).contains("Disk size: Unknown"));
        }
        for kind in [ImageSourceKind::Raw, ImageSourceKind::IsoHybrid] {
            source.kind = kind;
            assert!(source_details(&source).contains("Disk size: 8.0 MB"));
        }
    }
}
