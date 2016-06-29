//! Convenient utility methods, mostly for printing `syntect` data structures
//! prettily to the terminal.
use highlighting::Style;
use std::fmt::Write;
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
            ScopeStackOp::Noop => println!("noop"),
        }
    }
}
