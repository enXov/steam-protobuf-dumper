use once_cell::sync::Lazy;
use regex::Regex;

/// Compiled regex for validating proto file names.
/// Matches: alphanumeric, underscores, forward/backslashes, dots — ending in `.proto`.
static PROTO_FILENAME_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-zA-Z_0-9\\/.]+\.proto$").expect("valid regex"));

/// Scans raw binary data for embedded protobuf `FileDescriptorProto` blobs.
///
/// The scanner looks for the protobuf wire-format marker `0x0A` (field 1,
/// length-delimited) followed by a length byte and a string ending in `.proto`.
/// When found, it invokes the callback with the candidate name and byte slice.
///
/// The callback returns `Some(bytes_consumed)` if parsing succeeded, or `None`
/// to advance by 1 byte.
///
/// This is a direct translation of the C# `ExecutableScanner.ScanFile()`.
pub fn scan_file<F>(data: &[u8], mut callback: F)
where
    F: FnMut(&str, &[u8]) -> Option<usize>,
{
    const MARKER_START: u8 = 0x0A;
    const MARKER_LENGTH: usize = 2;

    let mut i = 0;

    while i < data.len().saturating_sub(1) {
        // Find next marker byte
        match data[i..].iter().position(|&b| b == MARKER_START) {
            Some(offset) => i += offset,
            None => break,
        }

        if i >= data.len() - 1 {
            break;
        }

        let expected_length = data[i + 1] as usize;

        if i + MARKER_LENGTH + expected_length > data.len() {
            i += 1;
            continue;
        }

        // Extract the potential proto name as ASCII
        let name_bytes = &data[i + MARKER_LENGTH..i + MARKER_LENGTH + expected_length];
        let proto_name = if let Ok(s) = std::str::from_utf8(name_bytes) {
            s
        } else {
            i += 1;
            continue;
        };

        // Must end with .proto
        if !proto_name.ends_with(".proto") {
            i += 1;
            continue;
        }

        // Must match the valid proto filename pattern
        if !PROTO_FILENAME_REGEX.is_match(proto_name) {
            log::debug!("Skipping potentially valid '{}'", proto_name);
            i += 1;
            continue;
        }

        // Hand off to the callback for parsing
        let buffer = &data[i..];
        match callback(proto_name, buffer) {
            Some(bytes_consumed) => {
                i += bytes_consumed.max(1);
            }
            None => {
                i += 1;
            }
        }
    }
}
