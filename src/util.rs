/// Escapes a string into a protobuf-compatible string literal with surrounding quotes.
///
/// Translates special characters into their escape sequences, matching the output
/// format of the original C# `Util.ToLiteral()`.
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
            _ => literal.push(c),
        }
    }

    literal.push('"');
    literal
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
}
