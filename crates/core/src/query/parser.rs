use crate::error::{Error, Result};
use crate::model::QueryOptions;

use super::lexer::{tokenize, Tok};
use super::filters::{self, DateField, DateFilter, SizeFilter};

#[derive(Clone, Debug)]
pub enum FilterSpec {
    Size(SizeFilter),
    Date(DateField, DateFilter),
    Ext(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    Contains,
    StartsWith,
    EndsWith,
    Exact,
    Wildcard,
    Regex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KindFilter {
    Any,
    File,
    Dir,
}

#[derive(Clone, Debug)]
pub struct Term {
    pub text: String,
    pub mode: MatchMode,
    pub field: Field,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub kind: KindFilter,
    pub filter: Option<FilterSpec>,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Term(Term),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    opts: QueryOptions,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut parts = vec![self.parse_and()?];
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.next();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::Or(parts)
        })
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut parts = Vec::new();
        loop {
            match self.peek() {
                None | Some(Tok::Pipe) | Some(Tok::RParen) => break,
                _ => parts.push(self.parse_unary()?),
            }
        }
        Ok(if parts.is_empty() {
            Expr::Term(Term {
                text: String::new(),
                mode: MatchMode::Contains,
                field: Field::Name,
                case_sensitive: self.opts.case_sensitive,
                whole_word: self.opts.whole_word,
                kind: KindFilter::Any,
                filter: None,
            })
        } else if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::And(parts)
        })
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if matches!(self.peek(), Some(Tok::Bang)) {
            self.next();
            Ok(Expr::Not(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    return Err(Error::Query("missing closing parenthesis".into()));
                }
                self.next();
                Ok(e)
            }
            Some(Tok::Word(w)) => Ok(Expr::Term(make_term(&w, self.opts)?)),
            other => Err(Error::Query(format!("unexpected token: {other:?}"))),
        }
    }
}

fn make_term(word: &str, opts: QueryOptions) -> Result<Term> {
    let mut text = word.to_string();
    let mut mode = if opts.regex {
        MatchMode::Regex
    } else {
        MatchMode::Contains
    };
    let mut field = if opts.path { Field::Path } else { Field::Name };
    let mut case_sensitive = opts.case_sensitive;
    let mut whole_word = opts.whole_word;
    let mut kind = KindFilter::Any;
    let mut filter = None;

    loop {
        let (prefix, rest) = match text.split_once(':') {
            Some(v) => v,
            None => break,
        };
        let p = prefix.to_ascii_lowercase();
        match p.as_str() {
            "startwith" | "sw" => {
                mode = MatchMode::StartsWith;
                text = rest.to_string();
            }
            "endwith" | "ew" => {
                mode = MatchMode::EndsWith;
                text = rest.to_string();
            }
            "exact" | "eq" => {
                mode = MatchMode::Exact;
                text = rest.to_string();
            }
            "wildcards" | "wildcard" => {
                mode = MatchMode::Wildcard;
                text = rest.to_string();
            }
            "nowildcards" => {
                if mode == MatchMode::Wildcard {
                    mode = MatchMode::Contains;
                }
                text = rest.to_string();
            }
            "re" | "regex" => {
                mode = MatchMode::Regex;
                text = rest.to_string();
            }
            "path" => {
                field = Field::Path;
                text = rest.to_string();
            }
            "name" => {
                field = Field::Name;
                text = rest.to_string();
            }
            "file" => {
                kind = KindFilter::File;
                text = rest.to_string();
            }
            "folder" | "dir" => {
                kind = KindFilter::Dir;
                text = rest.to_string();
            }
            "case" => {
                case_sensitive = true;
                text = rest.to_string();
            }
            "nocase" => {
                case_sensitive = false;
                text = rest.to_string();
            }
            "wholeword" | "ww" => {
                whole_word = true;
                text = rest.to_string();
            }
            "size" | "s" => {
                filter = Some(FilterSpec::Size(
                    filters::parse_size(rest).map_err(|e| Error::Query(e.to_string()))?,
                ));
                text = String::new();
            }
            "dm" | "datemodified" => {
                filter = Some(FilterSpec::Date(
                    DateField::Modified,
                    filters::parse_date(rest).map_err(|e| Error::Query(e.to_string()))?,
                ));
                text = String::new();
            }
            "dc" | "datecreated" => {
                filter = Some(FilterSpec::Date(
                    DateField::Created,
                    filters::parse_date(rest).map_err(|e| Error::Query(e.to_string()))?,
                ));
                text = String::new();
            }
            "da" | "dateaccessed" => {
                filter = Some(FilterSpec::Date(
                    DateField::Accessed,
                    filters::parse_date(rest).map_err(|e| Error::Query(e.to_string()))?,
                ));
                text = String::new();
            }
            "ext" | "extension" => {
                filter = Some(FilterSpec::Ext(filters::parse_ext(rest)));
                text = String::new();
            }
            "content" => {
                return Err(Error::Query("'content:' is not supported in v1".into()));
            }
            _ => break,
        }
    }

    if mode == MatchMode::Contains && (text.contains('*') || text.contains('?')) {
        mode = MatchMode::Wildcard;
    }

    Ok(Term {
        text,
        mode,
        field,
        case_sensitive,
        whole_word,
        kind,
        filter,
    })
}

pub fn parse(query: &str, opts: QueryOptions) -> Result<Expr> {
    let toks = tokenize(query)?;
    let mut p = Parser { toks, pos: 0, opts };
    if p.toks.is_empty() {
        return Ok(Expr::Term(Term {
            text: String::new(),
            mode: MatchMode::Contains,
            field: Field::Name,
            case_sensitive: opts.case_sensitive,
            whole_word: opts.whole_word,
            kind: KindFilter::Any,
            filter: None,
        }));
    }
    let e = p.parse_or()?;
    if p.pos != p.toks.len() {
        return Err(Error::Query("trailing tokens".into()));
    }
    Ok(e)
}
