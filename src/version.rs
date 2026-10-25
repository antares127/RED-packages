use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Any,
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone)]
pub struct Constraint {
    pub name: String,
    pub op: Op,
    pub version: String,
}

impl Constraint {
    pub fn parse(input: &str) -> Constraint {
        let s = input.trim();
        // Longest operators first.
        for (needle, op) in [
            (">=", Op::Ge),
            ("<=", Op::Le),
            ("==", Op::Eq),
            ("=", Op::Eq),
            (">", Op::Gt),
            ("<", Op::Lt),
        ] {
            if let Some(pos) = s.find(needle) {
                let name = s[..pos].trim().to_string();
                let version = s[pos + needle.len()..].trim().to_string();
                if !name.is_empty() {
                    return Constraint { name, op, version };
                }
            }
        }
        Constraint {
            name: s.to_string(),
            op: Op::Any,
            version: String::new(),
        }
    }

    pub fn matches_version(&self, candidate: &str) -> bool {
        if self.op == Op::Any || self.version.is_empty() {
            return true;
        }
        let ord = version_cmp(candidate, &self.version);
        match self.op {
            Op::Any => true,
            Op::Eq => ord == Ordering::Equal,
            Op::Gt => ord == Ordering::Greater,
            Op::Ge => ord != Ordering::Less,
            Op::Lt => ord == Ordering::Less,
            Op::Le => ord != Ordering::Greater,
        }
    }
}

fn split_epoch(version: &str) -> (u64, &str) {
    if let Some((epoch, rest)) = version.split_once(':') {
        if let Ok(n) = epoch.parse::<u64>() {
            return (n, rest);
        }
    }
    (0, version)
}

fn tokenize(s: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut current_is_digit: Option<bool> = None;
    for (idx, byte) in bytes.iter().enumerate() {
        let is_digit = byte.is_ascii_digit();
        match current_is_digit {
            None => current_is_digit = Some(is_digit),
            Some(prev) if prev != is_digit => {
                out.push((prev, &s[start..idx]));
                start = idx;
                current_is_digit = Some(is_digit);
            }
            _ => {}
        }
    }
    if let Some(kind) = current_is_digit {
        if start < s.len() {
            out.push((kind, &s[start..]));
        }
    }
    // Separator-only runs (".", "-", "_", "~"...) carry no ordering weight.
    out.retain(|(_, part)| part.chars().any(|c| c.is_ascii_alphanumeric()));
    out
}

fn cmp_numeric(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    match a.len().cmp(&b.len()) {
        Ordering::Equal => a.cmp(b),
        other => other,
    }
}

/// Generic version comparison used by RED dependencies.
/// Numeric segments compare numerically, alpha segments lexically, and
/// numeric segments sort after alpha segments (so 1.0.1 > 1.0alpha).
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let (a_epoch, a) = split_epoch(a.trim());
    let (b_epoch, b) = split_epoch(b.trim());
    match a_epoch.cmp(&b_epoch) {
        Ordering::Equal => {}
        other => return other,
    }

    let a_tokens = tokenize(a);
    let b_tokens = tokenize(b);
    let mut i = 0;
    loop {
        match (a_tokens.get(i), b_tokens.get(i)) {
            (None, None) => return Ordering::Equal,
            (None, Some((b_digit, _))) => {
                return if *b_digit {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (Some((a_digit, _)), None) => {
                return if *a_digit {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (Some((a_digit, a_part)), Some((b_digit, b_part))) => {
                let ord = match (a_digit, b_digit) {
                    (true, true) => cmp_numeric(a_part, b_part),
                    (false, false) => a_part.cmp(b_part),
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(version_cmp("1.0", "1.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.2", "1.10"), Ordering::Less);
        assert_eq!(version_cmp("2.0", "1.99"), Ordering::Greater);
        assert_eq!(version_cmp("1.0.1", "1.0"), Ordering::Greater);
        assert_eq!(version_cmp("1.0", "1.0alpha"), Ordering::Greater);
        assert_eq!(version_cmp("1:1.0", "2.0"), Ordering::Greater);
    }

    #[test]
    fn parses_constraints() {
        let c = Constraint::parse("foo>=1.2");
        assert_eq!(c.name, "foo");
        assert_eq!(c.op, Op::Ge);
        assert!(c.matches_version("1.3"));
        assert!(!c.matches_version("1.1"));

        let bare = Constraint::parse("bar");
        assert_eq!(bare.op, Op::Any);
        assert!(bare.matches_version("0"));
    }
}
