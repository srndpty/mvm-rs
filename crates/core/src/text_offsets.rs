//! UTF-8 の byte offset と UTF-16 の code unit offset の変換。書記素ではなく符号の境界で数える。
//!
//! UTF-16 の offset は数式の部分式指定など、外部（LaTeX 側の範囲）とのやり取りで使う。

/// byte offset を UTF-16 の code unit offset にする。文字の途中・終端外なら `None`。
pub fn utf8_to_utf16_offset(text: &str, byte_offset: usize) -> Option<usize> {
    let prefix = text.get(..byte_offset)?;
    Some(prefix.encode_utf16().count())
}

/// UTF-16 の code unit offset を byte offset にする。surrogate pair の途中・終端外なら `None`。
pub fn utf16_to_utf8_offset(text: &str, unit_offset: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, character) in text.char_indices() {
        if units == unit_offset {
            return Some(byte);
        }
        units += character.len_utf16();
    }
    (units == unit_offset).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_boundaries_both_ways() {
        // "aé日😀": byte 境界 0,1,3,6,10 / unit 境界 0,1,2,3,5（手計算）。
        let text = "a\u{e9}\u{65e5}\u{1f600}";
        for (byte, unit) in [(0, 0), (1, 1), (3, 2), (6, 3), (10, 5)] {
            assert_eq!(utf8_to_utf16_offset(text, byte), Some(unit));
            assert_eq!(utf16_to_utf8_offset(text, unit), Some(byte));
        }
    }

    #[test]
    fn rejects_offsets_inside_a_character_or_past_the_end() {
        assert_eq!(utf8_to_utf16_offset("日本", 1), None);
        assert_eq!(utf8_to_utf16_offset("日本", 7), None);
        // surrogate pair の間。
        assert_eq!(utf16_to_utf8_offset("a\u{1f600}", 2), None);
        assert_eq!(utf16_to_utf8_offset("a", 2), None);
        assert_eq!(utf16_to_utf8_offset("", 0), Some(0));
    }
}
