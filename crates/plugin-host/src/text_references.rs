//! Single-pass scanner for literal text and `${namespace.name}` references.
//! Callers supply their own namespace and authority policy.

#[derive(Debug, PartialEq, Eq)]
pub enum Part<'a> {
    Literal(&'a str),
    Reference { name: &'a str, token: &'a str },
    Unclosed(&'a str),
}

/// Scan without allocating or examining replacement values
pub fn parts(mut rest: &str) -> impl Iterator<Item = Part<'_>> {
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match rest.find("${") {
            Some(0) => match rest[2..].find('}') {
                Some(end) => {
                    let (token, next) = rest.split_at(end + 3);
                    rest = next;
                    Some(Part::Reference {
                        name: &token[2..token.len() - 1],
                        token,
                    })
                }
                None => Some(Part::Unclosed(std::mem::take(&mut rest))),
            },
            Some(start) => {
                let (literal, next) = rest.split_at(start);
                rest = next;
                Some(Part::Literal(literal))
            }
            None => Some(Part::Literal(std::mem::take(&mut rest))),
        }
    })
}

/// Replace recognized complete references; preserve others verbatim
pub fn expand(
    source: &str,
    limit: usize,
    mut replace: impl FnMut(&str) -> Result<Option<String>, String>,
) -> Result<String, String> {
    let mut output = String::with_capacity(source.len().min(limit));
    let mut append = |value: &str| {
        if value.len() > limit.saturating_sub(output.len()) {
            Err("Expanded text exceeds the size limit".to_owned())
        } else {
            output.push_str(value);
            Ok(())
        }
    };
    for part in parts(source) {
        match part {
            Part::Literal(text) | Part::Unclosed(text) => append(text)?,
            Part::Reference { name, token } => {
                let replacement = replace(name)?;
                append(replacement.as_deref().unwrap_or(token))?;
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_once_and_preserves_unhandled_tokens_and_utf8() {
        let text = expand(
            "😀 ${demo.now}${demo.missing} ${literal} ${unfinished",
            100,
            |name| {
                Ok(match name {
                    "demo.now" => Some("${demo.missing}".into()),
                    "demo.missing" => Some(String::new()),
                    _ => None,
                })
            },
        )
        .unwrap();
        assert_eq!(text, "😀 ${demo.missing} ${literal} ${unfinished");
        assert!(expand("😀", 3, |_| Ok(None)).is_err());
    }
}
