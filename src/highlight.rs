//! Syntax highlighting for code in text inputs: `(textarea :code :netscheme)`.

use std::ops::Range;

use crate::{lisp::Value, style::Palette};

/// A kind of token that's drawn in its own colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Token {
    Comment,
    String,
    /// `:name`.
    Keyword,
    /// Numbers, `true`, `false` and `nil`.
    Constant,
    /// The symbol a list starts with: what it calls.
    Head,
}

impl Token {
    pub fn color(self, palette: &Palette) -> crate::style::Color {
        match self {
            Token::Comment => palette.syntax_comment,
            Token::String => palette.syntax_string,
            Token::Keyword => palette.syntax_keyword,
            Token::Constant => palette.syntax_constant,
            Token::Head => palette.syntax_head,
        }
    }
}

/// Finds the tokens to colour in some code, as ranges of its characters.
pub type Tokenizer = fn(&[char]) -> Vec<(Range<usize>, Token)>;

/// The tokenizer for the language an element's `:code` attribute names, if
/// the browser can highlight it.
pub fn language(code: Option<&Value>) -> Option<Tokenizer> {
    match code {
        Some(Value::Keyword(name) | Value::Str(name)) if &**name == "netscheme" => Some(netscheme),
        _ => None,
    }
}

/// The tokens to colour in Netscheme code, as ranges of `chars`. It reads as
/// the reader does, but never fails: code being typed is often unfinished.
pub fn netscheme(chars: &[char]) -> Vec<(Range<usize>, Token)> {
    let mut tokens = Vec::new();
    let mut i = 0;
    // Whether the next atom starts a list.
    let mut head = false;
    while i < chars.len() {
        let start = i;
        match chars[i] {
            ';' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                tokens.push((start..i, Token::Comment));
                continue;
            }
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i = (i + 1).min(chars.len());
                tokens.push((start..i, Token::String));
                head = false;
                continue;
            }
            '(' => head = true,
            c if c.is_whitespace() => {}
            ')' | '\'' | '`' | ',' | '@' => head = false,
            _ => {
                while i < chars.len() && !(chars[i].is_whitespace() || matches!(chars[i], '(' | ')' | '"' | ';' | '`' | ',')) {
                    i += 1;
                }
                let atom: String = chars[start..i].iter().collect();
                let numeric = atom.starts_with(|c: char| c.is_ascii_digit()) || (atom.len() > 1 && atom.starts_with(['-', '+', '.']));
                let token = match atom.as_str() {
                    "nil" | "true" | "false" => Some(Token::Constant),
                    _ if numeric && atom.parse::<f64>().is_ok() => Some(Token::Constant),
                    _ if atom.len() > 1 && atom.starts_with(':') => Some(Token::Keyword),
                    _ if head => Some(Token::Head),
                    _ => None,
                };
                tokens.extend(token.map(|token| (start..i, token)));
                head = false;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(code: &str) -> Vec<(String, Token)> {
        let chars: Vec<char> = code.chars().collect();
        netscheme(&chars).into_iter().map(|(range, token)| (chars[range].iter().collect(), token)).collect()
    }

    #[test]
    fn colours_netscheme() {
        let code = "(define (f x) ; doubles\n  (p :size 12 \"x \\\"y\\\"\" nil -1 x))";
        let s = |text: &str, token| (text.to_string(), token);
        assert_eq!(
            tokens(code),
            vec![
                s("define", Token::Head),
                s("f", Token::Head),
                s("; doubles", Token::Comment),
                s("p", Token::Head),
                s(":size", Token::Keyword),
                s("12", Token::Constant),
                s("\"x \\\"y\\\"\"", Token::String),
                s("nil", Token::Constant),
                s("-1", Token::Constant),
            ]
        );
    }

    #[test]
    fn unfinished_code_still_colours() {
        assert_eq!(tokens("(p \"unterminated"), vec![("p".into(), Token::Head), ("\"unterminated".into(), Token::String)]);
        assert_eq!(tokens("\"ends in a backslash\\"), vec![("\"ends in a backslash\\".into(), Token::String)]);
        assert_eq!(tokens("'(a b)"), vec![("a".into(), Token::Head)], "quoted lists start with a head too");
    }
}
