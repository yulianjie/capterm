use anyhow::{Result, ensure};

pub(crate) const MEBIBYTE: u64 = 1024 * 1024;
pub(crate) const PREVIEW_BYTES: u64 = MEBIBYTE;
pub(crate) const TEXT_PROBE_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TextEncoding {
    #[default]
    Utf8,
    Gbk,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
        }
    }

    fn bom_bytes(self) -> Option<&'static [u8]> {
        match self {
            Self::Utf8 => Some(b"\xef\xbb\xbf"),
            Self::Gbk => None,
            Self::Utf16Le => Some(b"\xff\xfe"),
            Self::Utf16Be => Some(b"\xfe\xff"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct FileText {
    pub(crate) text: String,
    pub(crate) readonly: bool,
    pub(crate) truncated: bool,
    pub(crate) encoding: TextEncoding,
    pub(crate) size: Option<u64>,
    pub(crate) mtime: Option<u32>,
    pub(crate) bom: bool,
    pub(crate) edit_limit: u64,
}

pub(crate) enum SaveTextResult {
    Saved(FileText),
    Conflict(Option<FileText>),
}

pub(crate) fn strip_bom(data: &[u8], encoding: TextEncoding) -> (&[u8], bool) {
    match encoding.bom_bytes().and_then(|bom| data.strip_prefix(bom)) {
        Some(text) => (text, true),
        None => (data, false),
    }
}

pub(crate) fn decode_text(data: &[u8], encoding: TextEncoding, truncated: bool) -> Option<String> {
    match encoding {
        TextEncoding::Utf8 => match std::str::from_utf8(data) {
            Ok(text) => Some(text.into()),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&data[..error.valid_up_to()])
                    .ok()
                    .map(str::to_owned)
            }
            Err(_) => None,
        },
        TextEncoding::Gbk => encoding_rs::GBK
            .decode_without_bom_handling_and_without_replacement(data)
            .or_else(|| {
                truncated
                    .then(|| data.split_last())
                    .flatten()
                    .and_then(|(_, data)| {
                        encoding_rs::GBK.decode_without_bom_handling_and_without_replacement(data)
                    })
            })
            .map(|text| text.into_owned()),
        TextEncoding::Utf16Le => decode_utf16(data, truncated, u16::from_le_bytes),
        TextEncoding::Utf16Be => decode_utf16(data, truncated, u16::from_be_bytes),
    }
}

pub(crate) fn decode_text_buffer(
    mut data: Vec<u8>,
    encoding: TextEncoding,
    truncated: bool,
) -> Option<(String, bool)> {
    let (content, bom) = strip_bom(&data, encoding);
    if encoding != TextEncoding::Utf8 {
        return decode_text(content, encoding, truncated).map(|text| (text, bom));
    }

    if bom {
        let len = content.len();
        data.copy_within(3.., 0);
        data.truncate(len);
    }
    if truncated && data.capacity() > 2 * PREVIEW_BYTES as usize {
        data.shrink_to_fit();
    }
    match String::from_utf8(data) {
        Ok(text) => Some((text, bom)),
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            let mut data = error.into_bytes();
            data.truncate(valid);
            String::from_utf8(data).ok().map(|text| (text, bom))
        }
        Err(_) => None,
    }
}

fn decode_utf16(data: &[u8], truncated: bool, decode_unit: fn([u8; 2]) -> u16) -> Option<String> {
    let (units, remainder) = data.as_chunks::<2>();
    if !truncated && !remainder.is_empty() {
        return None;
    }
    let mut units = units.iter().copied().map(decode_unit);
    if truncated
        && units
            .clone()
            .next_back()
            .is_some_and(|unit| (0xd800..=0xdbff).contains(&unit))
    {
        let _ = units.next_back();
    }
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

pub(crate) fn encode_text(text: &str, encoding: TextEncoding, bom: bool) -> Result<Vec<u8>> {
    match encoding {
        TextEncoding::Utf8 if bom => {
            let mut bytes = b"\xef\xbb\xbf".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            Ok(bytes)
        }
        TextEncoding::Utf8 => Ok(text.as_bytes().to_vec()),
        TextEncoding::Gbk => {
            let (bytes, _, had_errors) = encoding_rs::GBK.encode(text);
            ensure!(!had_errors, "编辑内容包含 GBK 无法表示的字符");
            Ok(bytes.into_owned())
        }
        TextEncoding::Utf16Le => Ok(encode_utf16(text, bom, u16::to_le_bytes)),
        TextEncoding::Utf16Be => Ok(encode_utf16(text, bom, u16::to_be_bytes)),
    }
}

fn encode_utf16(text: &str, bom: bool, encode_unit: fn(u16) -> [u8; 2]) -> Vec<u8> {
    bom.then_some(0xfeff)
        .into_iter()
        .chain(text.encode_utf16())
        .flat_map(encode_unit)
        .collect()
}

pub(crate) fn preserve_line_endings(original: &str, edited: &str) -> String {
    let original_newlines = original.bytes().filter(|&byte| byte == b'\n').count();
    if original_newlines > 0 && original.matches("\r\n").count() == original_newlines {
        edited.replace("\r\n", "\n").replace('\n', "\r\n")
    } else if original_newlines == 0 && original.contains('\r') {
        edited.replace("\r\n", "\r").replace('\n', "\r")
    } else {
        edited.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_utf16_in_both_byte_orders() -> Result<()> {
        let text = "电📝\r\n";
        for encoding in [TextEncoding::Utf16Le, TextEncoding::Utf16Be] {
            let bytes = encode_text(text, encoding, true)?;
            let (data, bom) = strip_bom(&bytes, encoding);
            assert!(bom);
            assert_eq!(decode_text(data, encoding, false).as_deref(), Some(text));
        }
        Ok(())
    }

    #[test]
    fn preview_ignores_only_an_incomplete_final_utf16_character() -> Result<()> {
        let encoding = TextEncoding::Utf16Le;
        let bytes = encode_text("a📝", encoding, false)?;
        for end in 3..bytes.len() {
            assert_eq!(decode_text(&bytes[..end], encoding, false), None);
            assert_eq!(
                decode_text(&bytes[..end], encoding, true).as_deref(),
                Some("a")
            );
        }
        assert_eq!(decode_text(&[0, 0xdc], encoding, true), None);
        Ok(())
    }

    #[test]
    fn preserves_pure_cr_line_endings() {
        assert_eq!(preserve_line_endings("a\rb\r", "x\ny\r\nz\r"), "x\ry\rz\r");
    }
}
