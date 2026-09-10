use protobuf::descriptor::FileDescriptorProto;
use protobuf::Message;

/// Result of attempting to parse a protobuf candidate.
#[derive(Debug)]
pub enum CandidateResult {
    Ok,
    Invalid(String),
}

/// Collects successfully parsed `FileDescriptorProto` instances from binary data.
///
/// Translation of the C# `ProtobufCollector` class.
pub struct ProtobufCollector {
    pub candidates: Vec<FileDescriptorProto>,
}

impl ProtobufCollector {
    pub fn new() -> Self {
        Self {
            candidates: Vec::new(),
        }
    }

    /// Attempts to parse a protobuf `FileDescriptorProto` from the given byte slice.
    ///
    /// Returns `(result, bytes_consumed)`:
    /// - `result`: whether parsing succeeded or failed (with error message)
    /// - `bytes_consumed`: how many bytes were consumed from the input
    pub fn try_parse_candidate(&mut self, data: &[u8]) -> (CandidateResult, usize) {
        let bytes_consumed = match consume_one_message(data) {
            Some(n) if n > 0 => n,
            _ => {
                return (
                    CandidateResult::Invalid("No data was consumed".to_string()),
                    0,
                );
            }
        };

        let message_bytes = &data[..bytes_consumed];

        match FileDescriptorProto::parse_from_bytes(message_bytes) {
            Ok(candidate) => {
                self.candidates.push(candidate);
                (CandidateResult::Ok, bytes_consumed)
            }
            Err(e) => (CandidateResult::Invalid(e.to_string()), bytes_consumed),
        }
    }

    /// Checks if a candidate with the given name already exists.
    pub fn has_candidate(&self, name: &str) -> bool {
        self.candidates.iter().any(|c| c.name() == name)
    }
}

/// Reads the protobuf wire format to find the boundary of a single message.
///
/// Watches for field number 1 (the "name" field in `FileDescriptorProto`)
/// appearing a second time, which indicates we've hit the next concatenated
/// message. Returns the byte offset of the message boundary.
///
/// This is a direct translation of the C# `ConsumeOneMessage()` method which
/// uses `ProtoReader` to walk fields.
fn consume_one_message(data: &[u8]) -> Option<usize> {
    let mut pos = 0;
    let mut consumed_field_one = false;

    while pos < data.len() {
        let position_before_field = pos;

        // Read field tag (varint)
        let (tag, new_pos) = match read_varint(data, pos) {
            Some(v) => v,
            None => return Some(position_before_field),
        };

        if tag == 0 {
            return Some(position_before_field);
        }

        let field_number = tag >> 3;
        let wire_type = tag & 0x07;

        // Field 1 is the "name" field in FileDescriptorProto.
        // If we see it twice, we've hit the next message.
        if field_number == 1 {
            if consumed_field_one {
                return Some(position_before_field);
            }
            consumed_field_one = true;
        }

        // Skip the field value based on wire type
        pos = match skip_field(data, new_pos, wire_type as u8) {
            Some(p) => p,
            None => return Some(position_before_field),
        };
    }

    Some(pos)
}

/// Reads a varint from the data at the given position.
/// Returns `(value, new_position)` or `None` if the data is truncated.
fn read_varint(data: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;

    loop {
        if pos >= data.len() {
            return None;
        }

        let byte = data[pos];
        pos += 1;

        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            return Some((result, pos));
        }

        if shift >= 64 {
            return None;
        }
    }
}

/// Skips a field value in the wire format based on the wire type.
/// Returns the new position after skipping, or `None` if data is truncated.
fn skip_field(data: &[u8], pos: usize, wire_type: u8) -> Option<usize> {
    match wire_type {
        // Varint
        0 => {
            let (_, new_pos) = read_varint(data, pos)?;
            Some(new_pos)
        }
        // 64-bit
        1 => {
            if pos + 8 > data.len() {
                return None;
            }
            Some(pos + 8)
        }
        // Length-delimited
        2 => {
            let (length, new_pos) = read_varint(data, pos)?;
            let end = new_pos + length as usize;
            if end > data.len() {
                return None;
            }
            Some(end)
        }
        // Start group (deprecated, but we need to handle it)
        3 => {
            // Skip until end group
            let mut p = pos;
            loop {
                let (tag, new_p) = read_varint(data, p)?;
                let wt = (tag & 0x07) as u8;
                if wt == 4 {
                    // End group
                    return Some(new_p);
                }
                p = skip_field(data, new_p, wt)?;
            }
        }
        // End group
        4 => Some(pos),
        // 32-bit
        5 => {
            if pos + 4 > data.len() {
                return None;
            }
            Some(pos + 4)
        }
        _ => None,
    }
}
