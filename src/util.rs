//! Convenient utility methods, mostly for printing `syntect` data structures
//! prettily to the terminal.
use highlighting::{Style, StyleModifier};
use std::fmt::Write;
use std::ops::Range;
#[cfg(feature = "parsing")]
use parsing::ScopeStackOp;

/// Formats the styled fragments using 24-bit colour
/// terminal escape codes. Meant for debugging and testing.
/// It's currently fairly inefficient in its use of escape codes.
///
/// Note that this does not currently ever un-set the colour so that
/// the end of a line will also get highlighted with the background.
/// This means if you might want to use `println!("\x1b[0m");` after
/// to clear the colouring.
///
/// If `bg` is true then the background is also set
pub fn as_24_bit_terminal_escaped(v: &[(Style, &str)], bg: bool) -> String {
    let mut s: String = String::new();
    for &(ref style, text) in v.iter() {
        if bg {
            write!(s,
                   "\x1b[48;2;{};{};{}m",
                   style.background.r,
                   style.background.g,
                   style.background.b)
                .unwrap();
        }
        write!(s,
               "\x1b[38;2;{};{};{}m{}",
               style.foreground.r,
               style.foreground.g,
               style.foreground.b,
               text)
            .unwrap();
    }
    // s.push_str("\x1b[0m");
    s
}

/// Print out the various push and pop operations in a vector
/// with visual alignment to the line. Obviously for debugging.
#[cfg(feature = "parsing")]
pub fn debug_print_ops(line: &str, ops: &[(usize, ScopeStackOp)]) {
    for &(i, ref op) in ops.iter() {
        println!("{}", line.trim_right());
        print!("{: <1$}", "", i);
        match *op {
            ScopeStackOp::Push(s) => {
                println!("^ +{}", s);
            }
            ScopeStackOp::Pop(count) => {
                println!("^ pop {}", count);
            }
            ScopeStackOp::Clear(amount) => {
                println!("^ clear {:?}", amount);
            }
            ScopeStackOp::Restore => println!("^ restore"),
            ScopeStackOp::Noop => println!("noop"),
        }
    }
}


/// An iterator over the lines of a string, including the line endings.
///
/// This is similar to the standard library's `lines` method on `str`, except
/// that the yielded lines include the trailing newline character(s).
///
/// You can use it if you're parsing/highlighting some text that you have as a
/// string. With this, you can use the "newlines" variant of syntax definitions,
/// which is recommended.
///
/// # Examples
///
/// ```
/// use syntect::util::LinesWithEndings;
///
/// let mut lines = LinesWithEndings::from("foo\nbar\nbaz");
///
/// assert_eq!(Some("foo\n"), lines.next());
/// assert_eq!(Some("bar\n"), lines.next());
/// assert_eq!(Some("baz"), lines.next());
///
/// assert_eq!(None, lines.next());
/// ```
pub struct LinesWithEndings<'a> {
    input: &'a str,
}

impl<'a> LinesWithEndings<'a> {
    pub fn from(input: &'a str) -> LinesWithEndings<'a> {
        LinesWithEndings { input }
    }
}

impl<'a> Iterator for LinesWithEndings<'a> {
    type Item = &'a str;

    #[inline]
    fn next(&mut self) -> Option<&'a str> {
        if self.input.is_empty() {
            return None;
        }
        let split = self.input
            .find('\n')
            .map(|i| i + 1)
            .unwrap_or_else(|| self.input.len());
        let (line, rest) = self.input.split_at(split);
        self.input = rest;
        Some(line)
    }
}

/// Split a highlighted line at a byte index in the line into a before and
/// after component. It's just a helper that does the somewhat tricky logic
/// including splitting a span if the index lies on a boundary.
///
/// This can be used to extract a chunk of the line out for special treatment
/// like wrapping it in an HTML tag for extra styling.
///
/// Generic for testing purposes and fancier use cases, but intended for use with
/// the `Vec<(Style, &str)>` returned by `highlight` methods. Look at the source
/// code for `modify_range` for an example usage.
pub fn split_at<'a, A: Clone>(mut v: &[(A, &'a str)], mut split_i: usize) -> (Vec<(A, &'a str)>, Vec<(A, &'a str)>) {
    // Consume all tokens before the split
    let mut before = Vec::new();
    for tok in v {
        if tok.1.len() > split_i {
            break;
        }
        before.push(tok.clone());
        split_i -= tok.1.len();
    }
    v = &v[before.len()..];

    let mut after = Vec::new();
    // If necessary, split the token the split falls inside
    if v.len() > 0 && split_i > 0 {
        let (sa, sb) = v[0].1.split_at(split_i);
        before.push((v[0].0.clone(), sa));
        after.push((v[0].0.clone(), sb));
        v = &v[1..];
    }

    after.extend_from_slice(v);

    return (before, after);
}

/// Modify part of a highlighted line using a style modifier, useful for highlighting sections of a line.
///
/// # Examples
///
/// ```
/// use syntect::util::modify_range;
/// use syntect::highlighting::{Style, StyleModifier, FontStyle};
///
/// let plain = Style::default();
/// let boldmod = StyleModifier { foreground: None, background: None, font_style: Some(FontStyle::BOLD) };
/// let bold = plain.apply(boldmod);
///
/// let l = &[(plain, "abc"), (plain, "def"), (plain, "ghi")];
/// let l2 = modify_range(l, 1..6, boldmod);
/// assert_eq!(l2, &[(plain, "a"), (bold, "bc"), (bold, "def"), (plain, "ghi")]);
/// ```
pub fn modify_range<'a>(v: &[(Style, &'a str)], r: Range<usize>, modifier: StyleModifier) -> Vec<(Style, &'a str)> {
    let (mut result, in_and_after) = split_at(v, r.start);
    let (inside, mut after) = split_at(&in_and_after, r.end - r.start);

    result.extend(inside.iter().map(|(style, s)| { (style.apply(modifier), *s)}));
    result.append(&mut after);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lines_with_endings() {
        fn lines(s: &str) -> Vec<&str> {
            LinesWithEndings::from(s).collect()
        }

        assert!(lines("").is_empty());
        assert_eq!(lines("f"), vec!["f"]);
        assert_eq!(lines("foo"), vec!["foo"]);
        assert_eq!(lines("foo\n"), vec!["foo\n"]);
        assert_eq!(lines("foo\nbar"), vec!["foo\n", "bar"]);
        assert_eq!(lines("foo\nbar\n"), vec!["foo\n", "bar\n"]);
        assert_eq!(lines("foo\r\nbar"), vec!["foo\r\n", "bar"]);
        assert_eq!(lines("foo\r\nbar\r\n"), vec!["foo\r\n", "bar\r\n"]);
        assert_eq!(lines("\nfoo"), vec!["\n", "foo"]);
        assert_eq!(lines("\n\n\n"), vec!["\n", "\n", "\n"]);
    }

    #[test]
    fn test_split_at() {
        let l: &[(u8, &str)] = &[];
        let (before, after) = split_at(l, 0); // empty
        assert_eq!((&before[..], &after[..]), (&[][..],&[][..]));

        let l = &[(0u8, "abc"), (1u8, "def"), (2u8, "ghi")];

        let (before, after) = split_at(l, 0); // at start
        assert_eq!((&before[..], &after[..]), (&[][..],&[(0u8, "abc"), (1u8, "def"), (2u8, "ghi")][..]));

        let (before, after) = split_at(l, 4); // inside token
        assert_eq!((&before[..], &after[..]), (&[(0u8, "abc"), (1u8, "d")][..],&[(1u8, "ef"), (2u8, "ghi")][..]));

        let (before, after) = split_at(l, 3); // between tokens
        assert_eq!((&before[..], &after[..]), (&[(0u8, "abc")][..],&[(1u8, "def"), (2u8, "ghi")][..]));

        let (before, after) = split_at(l, 9); // just after last token
        assert_eq!((&before[..], &after[..]), (&[(0u8, "abc"), (1u8, "def"), (2u8, "ghi")][..], &[][..]));

        let (before, after) = split_at(l, 10); // out of bounds
        assert_eq!((&before[..], &after[..]), (&[(0u8, "abc"), (1u8, "def"), (2u8, "ghi")][..], &[][..]));
    }
}
