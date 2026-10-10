//! The XML index of Windows image files (`install.wim`, `install.esd`,
//! `boot.wim`), which names each image and records its Windows version,
//! architecture, and languages. Upstream Rufus reads it through wimlib to
//! choose the Windows User Experience options and editions; the index is
//! stored uncompressed, so only the header and that one resource are read.

use std::io;

/// `MSWIM\0\0\0`.
const MAGIC: &[u8; 8] = b"MSWIM\0\0\0";
pub const HEADER_BYTES: usize = 208;
/// `RESHDR_FLAG_COMPRESSED`.
const COMPRESSED: u8 = 0x04;
/// Real indexes are tens of kilobytes; anything far larger is not trusted.
const MAX_XML_BYTES: u64 = 4 * 1024 * 1024;

/// Where the XML index lives, from the 208-byte WIM header.
pub fn xml_location(header: &[u8]) -> io::Result<(u64, usize)> {
    if header.len() < HEADER_BYTES || &header[..8] != MAGIC {
        return Err(invalid("not a WIM file"));
    }
    // The XML resource header sits at offset 72: a 7-byte stored size, one
    // flags byte, then the 64-bit offset and the original size.
    let reshdr = &header[72..96];
    let mut size = [0u8; 8];
    size[..7].copy_from_slice(&reshdr[..7]);
    let size = u64::from_le_bytes(size);
    let flags = reshdr[7];
    let offset = u64::from_le_bytes(reshdr[8..16].try_into().expect("8 bytes"));
    if flags & COMPRESSED != 0 {
        return Err(invalid("the WIM index is compressed"));
    }
    if size == 0 || size > MAX_XML_BYTES {
        return Err(invalid("the WIM index has an implausible size"));
    }
    Ok((offset, size as usize))
}

/// Decode the UTF-16LE index.
pub fn decode_xml(bytes: &[u8]) -> io::Result<String> {
    let bytes = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    if bytes.len() % 2 != 0 {
        return Err(invalid("the WIM index is not UTF-16"));
    }
    String::from_utf16(
        &bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| invalid("the WIM index is not UTF-16"))
}

/// A header-and-index-only WIM, enough for [`xml_location`] and [`images`].
#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub fn fixture(xml: &str) -> Vec<u8> {
    let mut index = vec![0xff, 0xfe];
    index.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    let mut wim = vec![0u8; HEADER_BYTES];
    wim[..8].copy_from_slice(MAGIC);
    wim[8..12].copy_from_slice(&(HEADER_BYTES as u32).to_le_bytes());
    wim[72..79].copy_from_slice(&(index.len() as u64).to_le_bytes()[..7]);
    wim[80..88].copy_from_slice(&(HEADER_BYTES as u64).to_le_bytes());
    wim[88..96].copy_from_slice(&(index.len() as u64).to_le_bytes());
    wim.extend(index);
    wim
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WimImage {
    /// The 1-based image index that `/IMAGE/INDEX` refers to.
    pub index: u32,
    pub name: String,
    /// `DISPLAYNAME`, falling back to `DESCRIPTION` on unofficial images.
    pub display_name: String,
    pub arch: Option<u32>,
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub languages: Vec<String>,
}

/// Every `<IMAGE>` of the index, in order.
pub fn images(xml: &str) -> Vec<WimImage> {
    let mut images = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<IMAGE") {
        let after = &rest[start + "<IMAGE".len()..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let attributes = &after[..open_end];
        let body_start = &after[open_end + 1..];
        let Some(close) = body_start.find("</IMAGE>") else {
            break;
        };
        let body = &body_start[..close];
        rest = &body_start[close + "</IMAGE>".len()..];
        // `<IMAGE` also prefixes nothing else in the schema, but be exact.
        if !attributes.is_empty() && !attributes.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(index) = attribute(attributes, "INDEX").and_then(|value| value.parse().ok())
        else {
            continue;
        };
        let number = |tag| element(body, tag).and_then(|value| value.trim().parse().ok());
        let name = element(body, "NAME").unwrap_or_default();
        let display_name = element(body, "DISPLAYNAME")
            .or_else(|| element(body, "DESCRIPTION"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| name.clone());
        let languages = element_raw(body, "LANGUAGES")
            .map(|block| elements(block, "LANGUAGE"))
            .unwrap_or_default();
        images.push(WimImage {
            index,
            name,
            display_name,
            arch: number("ARCH"),
            major: number("MAJOR").unwrap_or(0),
            minor: number("MINOR").unwrap_or(0),
            build: number("BUILD").unwrap_or(0),
            languages,
        });
    }
    images
}

fn attribute(attributes: &str, name: &str) -> Option<String> {
    let at = attributes.find(&format!("{name}=\""))? + name.len() + 2;
    let value = &attributes[at..];
    Some(unescape(&value[..value.find('"')?]))
}

fn element_raw<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{tag}>"))?;
    Some(&body[start..start + end])
}

fn element(body: &str, tag: &str) -> Option<String> {
    element_raw(body, tag).map(|value| unescape(value.trim()))
}

fn elements(body: &str, tag: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = body;
    let close = format!("</{tag}>");
    while let Some(value) = element_raw(rest, tag) {
        found.push(unescape(value.trim()));
        let Some(end) = rest.find(&close) else { break };
        rest = &rest[end + close.len()..];
    }
    found
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"<WIM><TOTALBYTES>1</TOTALBYTES>
<IMAGE INDEX="1"><NAME>Windows 11 Home</NAME><DESCRIPTION>Windows 11 Home</DESCRIPTION>
<WINDOWS><ARCH>9</ARCH><LANGUAGES><LANGUAGE>en-US</LANGUAGE><DEFAULT>en-US</DEFAULT></LANGUAGES>
<VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD><SPBUILD>1742</SPBUILD></VERSION></WINDOWS>
<DISPLAYNAME>Windows 11 Home</DISPLAYNAME></IMAGE>
<IMAGE INDEX="6"><NAME>Windows 11 Pro</NAME><DESCRIPTION>Pro &amp; more</DESCRIPTION>
<WINDOWS><ARCH>12</ARCH><LANGUAGES><LANGUAGE>de-DE</LANGUAGE><LANGUAGE>fr-FR</LANGUAGE></LANGUAGES>
<VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26200</BUILD></VERSION></WINDOWS></IMAGE></WIM>"#;

    #[test]
    fn reads_every_image_with_version_and_languages() {
        let images = images(INDEX);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].display_name, "Windows 11 Home");
        assert_eq!(images[0].arch, Some(9));
        assert_eq!(
            (images[0].major, images[0].minor, images[0].build),
            (10, 0, 26100)
        );
        assert_eq!(images[0].languages, ["en-US"]);
        assert_eq!(images[1].index, 6);
        assert_eq!(images[1].display_name, "Pro & more");
        assert_eq!(images[1].languages, ["de-DE", "fr-FR"]);
        assert_eq!(images[1].build, 26200);
    }

    #[test]
    fn header_points_at_an_uncompressed_index() {
        let mut header = vec![0u8; HEADER_BYTES];
        header[..8].copy_from_slice(MAGIC);
        header[72..79].copy_from_slice(&[0x10, 0x27, 0, 0, 0, 0, 0]);
        header[80..88].copy_from_slice(&123_456u64.to_le_bytes());
        assert_eq!(
            xml_location(&header).expect("index location"),
            (123_456, 10_000)
        );
        header[79] = COMPRESSED;
        assert!(xml_location(&header).is_err());
        header[0] = b'X';
        assert!(xml_location(&header).is_err());
    }

    #[test]
    fn decodes_utf16_with_bom() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend("<WIM/>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_xml(&bytes).expect("decoded index"), "<WIM/>");
    }
}
