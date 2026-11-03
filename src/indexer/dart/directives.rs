//! A lexical inventory. Resolution belongs to SCIP, never this scanner.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Import,
    Export,
    Part,
    PartOf,
    NamedPartOf,
}
#[derive(Debug)]
pub struct Directive {
    pub kind: Kind,
    pub uri: Option<String>,
    pub line: usize,
    pub conditional: bool,
}
#[derive(Debug)]
enum Token {
    Word(String),
    Literal(Option<String>),
    Punct(char),
}

fn string(source: &str, at: &mut usize, nesting: usize) -> Result<Option<String>, ()> {
    let raw = source.as_bytes()[*at] == b'r';
    *at += usize::from(raw);
    let quote = source.as_bytes()[*at];
    let count = if source.as_bytes().get(*at..*at + 3) == Some(&[quote; 3]) {
        3
    } else {
        1
    };
    *at += count;
    let mut value = String::new();
    let mut opaque = false;
    while *at < source.len() {
        if source
            .as_bytes()
            .get(*at..*at + count)
            .is_some_and(|s| s.iter().all(|b| *b == quote))
        {
            *at += count;
            return Ok((!opaque).then_some(value));
        }
        let c = source[*at..].chars().next().ok_or(())?;
        *at += c.len_utf8();
        if !raw && c == '$' {
            opaque = true;
            if source.as_bytes().get(*at) == Some(&b'{') {
                *at += 1;
                let mut depth = 1;
                while depth > 0 {
                    match token(source, at, nesting + 1)?.ok_or(())? {
                        Token::Punct('{') => depth += 1,
                        Token::Punct('}') => depth -= 1,
                        _ => {}
                    }
                }
            }
        } else if !raw && c == '\\' {
            let c = source[*at..].chars().next().ok_or(())?;
            *at += c.len_utf8();
            value.push(match c {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'v' => '\u{b}',
                '\\' | '\'' | '"' | '$' => c,
                'x' | 'u' => {
                    let braces = c == 'u' && source.as_bytes().get(*at) == Some(&b'{');
                    let start = *at + usize::from(braces);
                    let end = if braces {
                        start + source[start..].find('}').ok_or(())?
                    } else {
                        start + if c == 'x' { 2 } else { 4 }
                    };
                    let hex = source.get(start..end).ok_or(())?;
                    *at = end + usize::from(braces);
                    let mut code = u32::from_str_radix(hex, 16).map_err(|_| ())?;
                    if (0xd800..=0xdbff).contains(&code)
                        && source[*at..].starts_with("\\u")
                        && let Some(low) = source
                            .get(*at + 2..*at + 6)
                            .and_then(|s| u32::from_str_radix(s, 16).ok())
                            .filter(|low| (0xdc00..=0xdfff).contains(low))
                    {
                        code = 0x10000 + ((code - 0xd800) << 10) + low - 0xdc00;
                        *at += 6;
                    }
                    if (0xd800..=0xdfff).contains(&code) {
                        // Unpaired UTF-16 units are valid in body strings, but
                        // cannot be a Rust URI string. Keep them opaque.
                        opaque = true;
                        '\u{fffd}'
                    } else {
                        char::from_u32(code).ok_or(())?
                    }
                }
                _ => return Err(()),
            });
        } else {
            if count == 1 && matches!(c, '\n' | '\r') {
                return Err(());
            }
            value.push(c);
        }
    }
    Err(())
}

fn token(source: &str, at: &mut usize, nesting: usize) -> Result<Option<Token>, ()> {
    if nesting > 64 {
        return Err(());
    }
    loop {
        let Some(c) = source.get(*at..).and_then(|s| s.chars().next()) else {
            return Ok(None);
        };
        if c.is_whitespace() {
            *at += c.len_utf8();
            continue;
        }
        if source[*at..].starts_with("//") {
            *at += source[*at..].find('\n').unwrap_or(source.len() - *at);
            continue;
        }
        if source[*at..].starts_with("/*") {
            *at += 2;
            let mut depth = 1;
            while depth > 0 {
                if *at >= source.len() {
                    return Err(());
                }
                if source[*at..].starts_with("/*") {
                    depth += 1;
                    *at += 2;
                } else if source[*at..].starts_with("*/") {
                    depth -= 1;
                    *at += 2;
                } else {
                    *at += source[*at..].chars().next().ok_or(())?.len_utf8();
                }
            }
            continue;
        }
        if matches!(c, '\'' | '"')
            || (c == 'r'
                && source
                    .as_bytes()
                    .get(*at + 1)
                    .is_some_and(|b| matches!(b, b'\'' | b'"')))
        {
            return string(source, at, nesting).map(|s| Some(Token::Literal(s)));
        }
        if c.is_alphabetic() || matches!(c, '_' | '$') {
            let start = *at;
            *at += c.len_utf8();
            while let Some(c) = source[*at..].chars().next() {
                if !c.is_alphanumeric() && !matches!(c, '_' | '$') {
                    break;
                }
                *at += c.len_utf8();
            }
            return Ok(Some(Token::Word(source[start..*at].into())));
        }
        *at += c.len_utf8();
        return Ok(Some(Token::Punct(c)));
    }
}

pub fn literal(source: &str) -> Option<String> {
    let mut at = 0;
    let Token::Literal(value) = token(source, &mut at, 0).ok()?? else {
        return None;
    };
    (at == source.len()).then_some(value).flatten()
}

pub fn read(source: &str) -> Result<Vec<Directive>, ()> {
    let (mut at, mut line) = (0, 1);
    let mut tokens = Vec::new();
    while at < source.len() {
        let start = at;
        let next = token(source, &mut at, 0)?;
        line += source[start..at].bytes().filter(|b| *b == b'\n').count();
        if let Some(next) = next {
            tokens.push((next, line));
        }
    }
    let (mut at, mut depth) = (0, 0i32);
    let mut out = Vec::new();
    while at < tokens.len() {
        let (next, line) = &tokens[at];
        match next {
            Token::Punct('{') => depth += 1,
            Token::Punct('}') => depth -= 1,
            _ => {}
        }
        if depth != 0 {
            at += 1;
            continue;
        }
        let kind = match next {
            Token::Word(s) if s == "import" => Kind::Import,
            Token::Word(s) if s == "export" => Kind::Export,
            Token::Word(s) if s == "part" => Kind::Part,
            _ => {
                at += 1;
                continue;
            }
        };
        let start = at;
        at += 1;
        let kind = if kind == Kind::Part
            && matches!(tokens.get(at), Some((Token::Word(s), _)) if s == "of")
        {
            at += 1;
            Kind::PartOf
        } else {
            kind
        };
        if kind == Kind::Part && !matches!(tokens.get(at), Some((Token::Literal(_), _))) {
            continue;
        }
        let uri = match tokens.get(at) {
            Some((Token::Literal(s), _)) => s.clone(),
            _ => None,
        };
        let name_start = at;
        let uri_line = tokens.get(at).map_or(*line, |(_, line)| *line);
        while at < tokens.len() && !matches!(tokens[at].0, Token::Punct(';')) {
            at += 1;
        }
        if at == tokens.len() {
            return Err(());
        }
        let kind = if kind == Kind::PartOf
            && (at - name_start) % 2 == 1
            && tokens[name_start..at]
                .iter()
                .enumerate()
                .all(|(i, (token, _))| {
                    if i % 2 == 0 {
                        matches!(token, Token::Word(_))
                    } else {
                        matches!(token, Token::Punct('.'))
                    }
                }) {
            Kind::NamedPartOf
        } else {
            kind
        };
        let conditional = tokens[start..at]
            .iter()
            .any(|(t, _)| matches!(t, Token::Word(s) if s == "if"));
        out.push(Directive {
            kind,
            uri,
            line: uri_line,
            conditional,
        });
        at += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventories_directives_not_comments_or_body_strings() {
        let source = "/* import 'no.dart'; /* nested */ */\nimport r'package:foo/a.dart' deferred as x; export \"b.dart\" if (dart.library.io) 'c.dart';\npart of example.library;\nvoid main() { print(\"${f('import x;')}\"); }";
        let directives = read(source).unwrap();
        assert_eq!(directives.len(), 3);
        assert_eq!(directives[0].uri.as_deref(), Some("package:foo/a.dart"));
        assert_eq!(directives[0].line, 2);
        assert!(directives[1].conditional);
        assert_eq!(directives[2].kind, Kind::NamedPartOf);
        assert_eq!(read("part of '${library}';").unwrap()[0].kind, Kind::PartOf);
    }
    #[test]
    fn decodes_literals_and_rejects_unfinished_input() {
        assert_eq!(literal("r'''a.dart'''"), Some("a.dart".into()));
        assert_eq!(literal("'a\\u002edart'"), Some("a.dart".into()));
        assert_eq!(literal("'\\uD83D\\uDE00.dart'"), Some("😀.dart".into()));
        assert!(read("import 'unfinished").is_err());
        assert!(read("/* unfinished").is_err());
        let mut nested = "'x'".to_string();
        for _ in 0..70 {
            nested = format!("\"${{{nested}}}\"");
        }
        assert!(read(&format!("final value = {nested};")).is_err());
    }
}
