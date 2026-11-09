const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|n| chunk.get(n).copied().unwrap_or(0));
        let bits = u32::from(a) << 16 | u32::from(b) << 8 | u32::from(c);
        for n in 0..4 {
            if n <= chunk.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * n) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The escape sequence that asks the terminal to put `text` on the system
/// clipboard.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_to_a_multiple_of_four() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_clipboard_sequence_carries_the_base64_of_the_text() {
        assert_eq!(
            osc52("fn main() {}\n"),
            "\x1b]52;c;Zm4gbWFpbigpIHt9Cg==\x07"
        );
    }

    #[test]
    fn multibyte_text_is_encoded_as_utf8() {
        assert_eq!(osc52("é"), "\x1b]52;c;w6k=\x07");
    }
}
