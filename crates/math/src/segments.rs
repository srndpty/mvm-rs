//! 式から式への変形の、TeX の式の自動の分け方と、部分どうしの照合。
//!
//! 派生の値で Project には保存しない。規則を変えたら版を上げる（版は cache key に入る）。
//!
//! 分け方（[`SEGMENTER_VERSION`]）:
//! - 字句: `\` + 英字の並び、`\` + 1 文字、`%` から改行（を含む）までの comment、それ以外は 1 byte。
//! - 深さ: `{`・`\left`・`\begin` で 1 増え、`}`・`\right`・`\end` で 1 減る。
//! - 深さ 0 の次の字句の前後で分ける。関係子（`=` `<` `>` `\le` `\ge` `\leq` `\geq` `\ne` `\neq`
//!   `\approx` `\equiv`）は常に、二項演算子（`+` `-` `\pm` `\mp` `\cdot` `\times`）は直前が項のときだけ。
//! - `^`・`_`・`\not` の直後の字句は引数とみなし、分けない。
//! - 演算子の前後の空白は演算子の部分に含める。部分は空にならず、連結すると入力に戻る。
//!
//! 照合（[`MATCHING_VERSION`]）: 前後の空白を除いた key が等しいものを対応させ、同じ key が重複するときは
//! n 番目の出現どうしを対応させる。対応しない source は消え、target は現れる。

use std::collections::HashMap;

pub const SEGMENTER_VERSION: &str = "mvm-tex-segments/1";
pub const MATCHING_VERSION: &str = "mvm-tex-match/1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TexSegment {
    /// 入力の連続した一部（空白を含む）。
    pub text: String,
    /// 照合に使う値（前後の ASCII 空白を除いたもの）。
    pub key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentPair {
    pub source: usize,
    pub target: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SegmentMatching {
    /// source の番号の昇順。target の番号は昇順とは限らない（項の並べ替え）。
    pub pairs: Vec<SegmentPair>,
    pub unmatched_source: Vec<usize>,
    pub unmatched_target: Vec<usize>,
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// 前後の ASCII 空白を除いた照合の key。
pub fn segment_key(text: &str) -> String {
    text.trim_matches(|character: char| matches!(character, ' ' | '\t' | '\n' | '\r'))
        .to_owned()
}

/// `begin` の字句の終わり。
fn token_end(source: &[u8], begin: usize) -> usize {
    let size = source.len();
    if source[begin] == b'%' {
        let mut end = begin + 1;
        while end < size && source[end] != b'\n' {
            end += 1;
        }
        return if end < size { end + 1 } else { end };
    }
    if source[begin] != b'\\' {
        // UTF-8 の文字は 1 文字を 1 字句にする（文字の途中で分けない）。
        let mut end = begin + 1;
        while end < size && (source[end] & 0xC0) == 0x80 {
            end += 1;
        }
        return end;
    }
    let mut end = begin + 1;
    if end >= size {
        return end;
    }
    if !source[end].is_ascii_alphabetic() {
        end += 1;
        while end < size && (source[end] & 0xC0) == 0x80 {
            end += 1;
        }
        return end;
    }
    while end < size && source[end].is_ascii_alphabetic() {
        end += 1;
    }
    end
}

const RELATIONS: [&str; 11] = [
    "=", "<", ">", "\\le", "\\ge", "\\leq", "\\geq", "\\ne", "\\neq", "\\approx", "\\equiv",
];
const BINARIES: [&str; 6] = ["+", "-", "\\pm", "\\mp", "\\cdot", "\\times"];
const OPENERS: [&str; 3] = ["{", "\\left", "\\begin"];
const CLOSERS: [&str; 3] = ["}", "\\right", "\\end"];
const TAKES_ARGUMENT: [&str; 3] = ["^", "_", "\\not"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Previous {
    Start,
    Operator,
    Open,
    Term,
}

pub fn segment_tex(source: &str) -> Vec<TexSegment> {
    let bytes = source.as_bytes();
    let size = bytes.len();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0i64;
    let mut previous = Previous::Start;
    let mut argument_pending = false;
    let mut begin = 0;
    while begin < size {
        let end = token_end(bytes, begin);
        let token = &source[begin..end];
        let token_begin = begin;
        begin = end;
        let first = bytes[token_begin];
        if first == b'%' || is_space(first) {
            continue;
        }
        if OPENERS.contains(&token) {
            depth += 1;
            argument_pending = false;
            continue;
        }
        if CLOSERS.contains(&token) {
            depth -= 1;
            previous = Previous::Term;
            continue;
        }
        if depth != 0 {
            continue;
        }
        if argument_pending {
            argument_pending = false;
            previous = Previous::Term;
            continue;
        }
        if TAKES_ARGUMENT.contains(&token) {
            argument_pending = true;
            previous = Previous::Term;
            continue;
        }
        if RELATIONS.contains(&token) || (BINARIES.contains(&token) && previous == Previous::Term) {
            cuts.push((token_begin, end));
            previous = Previous::Operator;
        } else if token == "(" || token == "[" {
            previous = Previous::Open;
        } else {
            previous = Previous::Term;
        }
    }

    let mut segments = Vec::new();
    let mut push = |from: usize, to: usize| {
        if from < to {
            let text = source[from..to].to_owned();
            let key = segment_key(&text);
            segments.push(TexSegment { text, key });
        }
    };
    let mut position = 0;
    for (cut_begin, cut_end) in cuts {
        // 演算子の前後の空白を演算子の部分へ寄せる（前の演算子が取った分は越えない）。
        let mut from = cut_begin;
        while from > position && is_space(bytes[from - 1]) {
            from -= 1;
        }
        let mut to = cut_end;
        while to < size && is_space(bytes[to]) {
            to += 1;
        }
        push(position, from);
        push(from, to);
        position = to;
    }
    push(position, size);
    segments
}

pub fn match_segments(source: &[TexSegment], target: &[TexSegment]) -> SegmentMatching {
    let mut occurrences: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, segment) in target.iter().enumerate() {
        occurrences
            .entry(segment.key.as_str())
            .or_default()
            .push(index);
    }
    let mut matching = SegmentMatching::default();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut used = vec![false; target.len()];
    for (index, segment) in source.iter().enumerate() {
        let nth = seen.entry(segment.key.as_str()).or_insert(0);
        let found = occurrences
            .get(segment.key.as_str())
            .and_then(|list| list.get(*nth))
            .copied();
        *nth += 1;
        match found {
            Some(target_index) => {
                matching.pairs.push(SegmentPair {
                    source: index,
                    target: target_index,
                });
                used[target_index] = true;
            }
            None => matching.unmatched_source.push(index),
        }
    }
    matching.unmatched_target = used
        .iter()
        .enumerate()
        .filter(|(_, used)| !**used)
        .map(|(index, _)| index)
        .collect();
    matching
}

#[cfg(test)]
mod tests {
    use super::*;

    const E1: &str = "x^2 + \\frac{b}{a}x = -\\frac{c}{a}";
    const E2: &str = "x^2 + \\frac{b}{a}x + \\left(\\frac{b}{2a}\\right)^2 = -\\frac{c}{a} + \\left(\\frac{b}{2a}\\right)^2";
    const E3: &str = "\\left(x + \\frac{b}{2a}\\right)^2 = \\frac{b^2 - 4ac}{4a^2}";
    const D1: &str = "x + x = 2x";
    const D2: &str = "x + x + x = 3x";

    /// 期待値は C++ 版の試験（tests/core/test_math_transform.cpp）から写した、手で書いた分け方。
    fn expect(source: &str, pieces: &[(&str, &str)]) {
        let got = segment_tex(source);
        let want: Vec<TexSegment> = pieces
            .iter()
            .map(|(text, key)| TexSegment {
                text: (*text).to_owned(),
                key: (*key).to_owned(),
            })
            .collect();
        assert_eq!(got, want, "{source}");
        let joined: String = got.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(joined, source, "連結すると入力に戻らない");
    }

    #[test]
    fn golden_segments() {
        expect(
            E1,
            &[
                ("x^2", "x^2"),
                (" + ", "+"),
                ("\\frac{b}{a}x", "\\frac{b}{a}x"),
                (" = ", "="),
                ("-\\frac{c}{a}", "-\\frac{c}{a}"),
            ],
        );
        expect(
            E3,
            &[
                (
                    "\\left(x + \\frac{b}{2a}\\right)^2",
                    "\\left(x + \\frac{b}{2a}\\right)^2",
                ),
                (" = ", "="),
                ("\\frac{b^2 - 4ac}{4a^2}", "\\frac{b^2 - 4ac}{4a^2}"),
            ],
        );
        expect(
            D2,
            &[
                ("x", "x"),
                (" + ", "+"),
                ("x", "x"),
                (" + ", "+"),
                ("x", "x"),
                (" = ", "="),
                ("3x", "3x"),
            ],
        );
        expect(
            "\\boxed{x^2 + y^2 = r^2}",
            &[("\\boxed{x^2 + y^2 = r^2}", "\\boxed{x^2 + y^2 = r^2}")],
        );
    }

    #[test]
    fn segment_rules() {
        expect("", &[]);
        expect("   ", &[("   ", "")]);
        expect("x=y", &[("x", "x"), ("=", "="), ("y", "y")]);
        expect(" = x", &[(" = ", "="), ("x", "x")]);
        expect("x =", &[("x", "x"), (" =", "=")]);
        expect("  x  ", &[("  x  ", "x")]);
        expect("-x + y", &[("-x", "-x"), (" + ", "+"), ("y", "y")]);
        expect("a + + b", &[("a", "a"), (" + ", "+"), ("+ b", "+ b")]);
        expect(
            "(-a + b) = c",
            &[
                ("(-a", "(-a"),
                (" + ", "+"),
                ("b)", "b)"),
                (" = ", "="),
                ("c", "c"),
            ],
        );
        expect("x^-1 + y", &[("x^-1", "x^-1"), (" + ", "+"), ("y", "y")]);
        expect("x_= y", &[("x_= y", "x_= y")]);
        expect(
            "x^ {2} + y",
            &[("x^ {2}", "x^ {2}"), (" + ", "+"), ("y", "y")],
        );
        expect(
            "a \\not= b = c",
            &[("a \\not= b", "a \\not= b"), (" = ", "="), ("c", "c")],
        );
        expect(
            "a \\le b \\leq c \\left( d \\right)",
            &[
                ("a", "a"),
                (" \\le ", "\\le"),
                ("b", "b"),
                (" \\leq ", "\\leq"),
                ("c \\left( d \\right)", "c \\left( d \\right)"),
            ],
        );
        expect(
            "x \\pm y \\cdot z \\times w - v",
            &[
                ("x", "x"),
                (" \\pm ", "\\pm"),
                ("y", "y"),
                (" \\cdot ", "\\cdot"),
                ("z", "z"),
                (" \\times ", "\\times"),
                ("w", "w"),
                (" - ", "-"),
                ("v", "v"),
            ],
        );
        expect(
            "\\{a = b\\}",
            &[("\\{a", "\\{a"), (" = ", "="), ("b\\}", "b\\}")],
        );
        expect(
            "\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}",
            &[(
                "\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}",
                "\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}",
            )],
        );
        expect(
            "x = %c = d\n y",
            &[("x", "x"), (" = ", "="), ("%c = d\n y", "%c = d\n y")],
        );
        expect(
            "x = 50\\% + y",
            &[
                ("x", "x"),
                (" = ", "="),
                ("50\\%", "50\\%"),
                (" + ", "+"),
                ("y", "y"),
            ],
        );
        expect("x = y\\", &[("x", "x"), (" = ", "="), ("y\\", "y\\")]);
        expect("a } = b", &[("a } = b", "a } = b")]);
        expect("a { = b", &[("a { = b", "a { = b")]);
        expect("α = β", &[("α", "α"), (" = ", "="), ("β", "β")]);
        expect("x\t=\r\ny", &[("x", "x"), ("\t=\r\n", "="), ("y", "y")]);
    }

    fn matching(source: &str, target: &str) -> SegmentMatching {
        match_segments(&segment_tex(source), &segment_tex(target))
    }

    fn pairs(list: &[(usize, usize)]) -> Vec<SegmentPair> {
        list.iter()
            .map(|(source, target)| SegmentPair {
                source: *source,
                target: *target,
            })
            .collect()
    }

    #[test]
    fn golden_matching() {
        let got = matching(E1, E2);
        assert_eq!(got.pairs, pairs(&[(0, 0), (1, 1), (2, 2), (3, 5), (4, 6)]));
        assert!(got.unmatched_source.is_empty());
        assert_eq!(got.unmatched_target, vec![3, 4, 7, 8]);

        let got = matching(E2, E3);
        assert_eq!(got.pairs, pairs(&[(5, 1)]));
        assert_eq!(got.unmatched_source, vec![0, 1, 2, 3, 4, 6, 7, 8]);
        assert_eq!(got.unmatched_target, vec![0, 2]);

        let got = matching(D1, D2);
        assert_eq!(got.pairs, pairs(&[(0, 0), (1, 1), (2, 2), (3, 5)]));
        assert_eq!(got.unmatched_source, vec![4]);
        assert_eq!(got.unmatched_target, vec![3, 4, 6]);

        let got = matching("a + b", "b + a");
        assert_eq!(got.pairs, pairs(&[(0, 2), (1, 1), (2, 0)]));
        let got = matching("x+y", "x + y");
        assert_eq!(got.pairs, pairs(&[(0, 0), (1, 1), (2, 2)]));
        let got = matching("a", "b");
        assert!(got.pairs.is_empty());
        assert_eq!(
            (got.unmatched_source, got.unmatched_target),
            (vec![0], vec![0])
        );
        assert_eq!(matching("", "").pairs, Vec::new());
    }
}
