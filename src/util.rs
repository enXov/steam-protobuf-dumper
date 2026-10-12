/// Escapes a string into a protobuf-compatible string literal with surrounding quotes.
///
/// Translates special characters into their escape sequences, matching the output
/// format of the original C# `Util.ToLiteral()`. Control characters without a named
/// escape are written as octal escapes.
pub fn to_literal(input: &str) -> String {
    let mut literal = String::with_capacity(input.len() + 2);
    literal.push('"');

    for c in input.chars() {
        match c {
            '\x07' => literal.push_str("\\a"),
            '\x08' => literal.push_str("\\b"),
            '\x0C' => literal.push_str("\\f"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '\x0B' => literal.push_str("\\v"),
            '\\' => literal.push_str("\\\\"),
            '"' => literal.push_str("\\\""),
            _ if (c as u32) < 0x20 || c as u32 == 0x7F => append_octal(&mut literal, c as u32),
            _ => literal.push(c),
        }
    }

    literal.push('"');
    literal
}

/// Escapes a byte sequence into a protobuf-compatible string literal with
/// surrounding quotes.
///
/// Non-printable and non-ASCII bytes are written as octal escapes, matching the
/// output format of the original C# `Util.ToLiteral(byte[])`.
pub fn to_literal_bytes(input: &[u8]) -> String {
    let mut literal = String::with_capacity(input.len() + 2);
    literal.push('"');

    for &b in input {
        if b == b'\\' || b == b'"' {
            literal.push('\\');
            literal.push(b as char);
        } else if !(0x20..0x7F).contains(&b) {
            append_octal(&mut literal, b as u32);
        } else {
            literal.push(b as char);
        }
    }

    literal.push('"');
    literal
}

/// Formats a double as a protobuf literal, matching the original C#
/// `Util.ToLiteral(double)`: infinities and NaN are written as `inf`, `-inf`
/// and `nan`, everything else with invariant-culture formatting.
pub fn to_literal_f64(value: f64) -> String {
    if value == f64::INFINITY {
        return "inf".to_string();
    }
    if value == f64::NEG_INFINITY {
        return "-inf".to_string();
    }
    if value.is_nan() {
        return "nan".to_string();
    }

    format!("{value}")
}

/// Formats a float as a protobuf literal, matching the original C#
/// `Util.ToLiteral(float)`.
pub fn to_literal_f32(value: f32) -> String {
    if !value.is_finite() {
        return to_literal_f64(value as f64);
    }

    format!("{value}")
}

fn append_octal(literal: &mut String, c: u32) {
    literal.push('\\');
    literal.push_str(&format!("{c:03o}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_string() {
        assert_eq!(to_literal("hello"), "\"hello\"");
    }

    #[test]
    fn test_escape_sequences() {
        assert_eq!(to_literal("a\nb"), "\"a\\nb\"");
        assert_eq!(to_literal("a\tb"), "\"a\\tb\"");
        assert_eq!(to_literal("a\\b"), "\"a\\\\b\"");
        assert_eq!(to_literal("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(to_literal(""), "\"\"");
    }

    #[test]
    fn test_control_characters_use_octal_escapes() {
        assert_eq!(to_literal("a\x01b"), "\"a\\001b\"");
        assert_eq!(to_literal("\x7F"), "\"\\177\"");
        assert_eq!(to_literal("\x1B[0m"), "\"\\033[0m\"");
    }

    #[test]
    fn test_bytes_literal() {
        assert_eq!(to_literal_bytes(b"abc"), "\"abc\"");
        assert_eq!(to_literal_bytes(&[0x00, 0xFF, b'a']), "\"\\000\\377a\"");
        assert_eq!(to_literal_bytes(b"a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(to_literal_bytes(&[0x7F]), "\"\\177\"");
        assert_eq!(to_literal_bytes(&[]), "\"\"");
    }

    #[test]
    fn test_double_literals() {
        assert_eq!(to_literal_f64(1.5), "1.5");
        assert_eq!(to_literal_f64(f64::INFINITY), "inf");
        assert_eq!(to_literal_f64(f64::NEG_INFINITY), "-inf");
        assert_eq!(to_literal_f64(f64::NAN), "nan");
    }

    #[test]
    fn test_float_literals() {
        assert_eq!(to_literal_f32(2.5), "2.5");
        assert_eq!(to_literal_f32(f32::INFINITY), "inf");
        assert_eq!(to_literal_f32(f32::NEG_INFINITY), "-inf");
        assert_eq!(to_literal_f32(f32::NAN), "nan");
    }
}
