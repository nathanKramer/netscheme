//! Turns source text into `Value`s (the "parser" of a classic browser).

use super::value::Value;

pub fn read_all(src: &str) -> Result<Vec<Value>, String> {
    let mut reader = Reader { chars: src.chars().collect(), pos: 0, line: 1, depth: 0 };
    let mut forms = Vec::new();
    loop {
        reader.skip_whitespace();
        if reader.peek().is_none() {
            return Ok(forms);
        }
        forms.push(reader.read()?);
    }
}

struct Reader {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    /// How many lists we're inside, as reading them is recursive.
    depth: u32,
}

/// Deeper nesting than this is refused, rather than overflowing the stack.
const MAX_NESTING: u32 = 1_000;

impl Reader {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn err(&self, msg: &str) -> String {
        format!("line {}: {msg}", self.line)
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c == ';' {
                while self.next().is_some_and(|c| c != '\n') {}
            } else if c.is_whitespace() {
                self.next();
            } else {
                break;
            }
        }
    }

    fn read(&mut self) -> Result<Value, String> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some('(') => {
                self.next();
                if self.depth >= MAX_NESTING {
                    return Err(self.err(&format!("lists are nested more than {MAX_NESTING} deep")));
                }
                self.depth += 1;
                let list = self.read_list();
                self.depth -= 1;
                list
            }
            Some(')') => Err(self.err("unexpected ')'")),
            Some('\'') => self.read_prefixed("quote"),
            Some('`') => self.read_prefixed("quasiquote"),
            Some(',') if self.chars.get(self.pos + 1) == Some(&'@') => {
                self.next();
                self.read_prefixed("unquote-splicing")
            }
            Some(',') => self.read_prefixed("unquote"),
            Some('"') => self.read_string(),
            Some(_) => Ok(self.read_atom()),
        }
    }

    /// The rest of a list, after its `(`.
    fn read_list(&mut self) -> Result<Value, String> {
        let mut items = Vec::new();
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(self.err("unclosed '('")),
                Some(')') => {
                    self.next();
                    return Ok(Value::list(items));
                }
                Some(_) => items.push(self.read()?),
            }
        }
    }

    /// `'x` is `(quote x)`, `` `x `` is `(quasiquote x)`, and so on.
    fn read_prefixed(&mut self, form: &str) -> Result<Value, String> {
        self.next();
        if self.depth >= MAX_NESTING {
            return Err(self.err(&format!("quotes are nested more than {MAX_NESTING} deep")));
        }
        self.depth += 1;
        let quoted = self.read();
        self.depth -= 1;
        Ok(Value::list(vec![Value::sym(form), quoted?]))
    }

    fn read_string(&mut self) -> Result<Value, String> {
        self.next();
        let mut s = String::new();
        loop {
            match self.next() {
                None => return Err(self.err("unterminated string")),
                Some('"') => return Ok(Value::Str(s.into())),
                Some('\\') => match self.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some(c) => s.push(c),
                    None => return Err(self.err("unterminated string")),
                },
                Some(c) => s.push(c),
            }
        }
    }

    fn read_atom(&mut self) -> Value {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '(' | ')' | '"' | ';' | '`' | ',') {
                break;
            }
            s.push(c);
            self.next();
        }
        let looks_numeric = s.starts_with(|c: char| c.is_ascii_digit()) || (s.len() > 1 && s.starts_with(['-', '+', '.']));
        if looks_numeric && let Ok(n) = s.parse::<f64>() {
            return Value::Num(n);
        }
        match s.as_str() {
            "nil" => Value::Nil,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ if s.len() > 1 && s.starts_with(':') => Value::Keyword(s[1..].into()),
            _ => Value::Sym(s.into()),
        }
    }
}
