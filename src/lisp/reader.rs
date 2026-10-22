//! Turns source text into `Value`s (the "parser" of a classic browser).

use super::value::Value;

pub fn read_all(src: &str) -> Result<Vec<Value>, String> {
    let mut reader = Reader { chars: src.chars().collect(), pos: 0, line: 1 };
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
}

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
            Some(')') => Err(self.err("unexpected ')'")),
            Some('\'') => {
                self.next();
                Ok(Value::list(vec![Value::sym("quote"), self.read()?]))
            }
            Some('"') => self.read_string(),
            Some(_) => Ok(self.read_atom()),
        }
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
            if c.is_whitespace() || matches!(c, '(' | ')' | '"' | ';') {
                break;
            }
            s.push(c);
            self.next();
        }
        let looks_numeric = s.starts_with(|c: char| c.is_ascii_digit())
            || (s.len() > 1 && s.starts_with(['-', '+', '.']));
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
