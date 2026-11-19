use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tok {
    Word(String),
    Pipe,
    Bang,
    LParen,
    RParen,
}

pub fn tokenize(input: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            '!' | '-' if is_operator_pos(&out) || (i > 0 && chars[i - 1].is_whitespace()) => {
                out.push(Tok::Bang);
                i += 1;
            }
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(Error::Query("unterminated quote".into()));
                }
                i += 1;
                out.push(Tok::Word(s));
            }
            _ => {
                let mut s = String::new();
                while i < chars.len() {
                    let ch = chars[i];
                    if ch.is_whitespace() || matches!(ch, '|' | '(' | ')') {
                        break;
                    }
                    if (ch == '!' || ch == '-')
                        && (is_operator_pos(&out)
                            || (i > 0 && chars[i - 1].is_whitespace()))
                        && s.is_empty()
                    {
                        break;
                    }
                    if ch == '\\' && i + 1 < chars.len() {
                        i += 1;
                        s.push(chars[i]);
                        i += 1;
                    } else {
                        s.push(ch);
                        i += 1;
                    }
                }
                if !s.is_empty() {
                    out.push(Tok::Word(s));
                }
            }
        }
    }
    Ok(out)
}

fn is_operator_pos(out: &[Tok]) -> bool {
    matches!(
        out.last(),
        None | Some(Tok::Pipe) | Some(Tok::LParen) | Some(Tok::Bang)
    )
}
