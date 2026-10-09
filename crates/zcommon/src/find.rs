use std::{fmt, ops::Range};

use regex::{Regex, RegexBuilder};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindQuery {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindError {
    EmptyQuery,
    InvalidRegex(String),
}

impl fmt::Display for FindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FindError::EmptyQuery => write!(f, "Empty query"),
            FindError::InvalidRegex(message) => write!(f, "Invalid regex: {message}"),
        }
    }
}

pub struct Matcher {
    kind: MatcherKind,
    whole_word: bool,
}

enum MatcherKind {
    Plain {
        needle: String,
        case_sensitive: bool,
    },
    Regex(Regex),
}

impl Matcher {
    pub fn new(query: &FindQuery) -> Result<Self, FindError> {
        if query.text.is_empty() {
            return Err(FindError::EmptyQuery);
        }
        let kind = if query.regex {
            let regex = RegexBuilder::new(&query.text)
                .case_insensitive(!query.case_sensitive)
                .build()
                .map_err(|e| FindError::InvalidRegex(e.to_string()))?;
            MatcherKind::Regex(regex)
        } else {
            MatcherKind::Plain {
                needle: query.text.clone(),
                case_sensitive: query.case_sensitive,
            }
        };
        Ok(Self {
            kind,
            whole_word: query.whole_word,
        })
    }

    /// Byte ranges of every match in `text`, in order. Plain matches may overlap; regex matches
    /// don't, and empty regex matches are skipped. Lazy, so a caller can stop early.
    pub fn find_iter<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut from = 0;
        std::iter::from_fn(move || {
            loop {
                if from > text.len() {
                    return None;
                }
                let found = match &self.kind {
                    MatcherKind::Plain {
                        needle,
                        case_sensitive,
                    } => find_occurrence(text, needle, *case_sensitive, from)?,
                    MatcherKind::Regex(regex) => regex.find_at(text, from)?.range(),
                };
                let accepted =
                    !found.is_empty() && (!self.whole_word || is_whole_word(text, found.clone()));
                // A rejected regex match is retried one char on: a word may start inside it.
                from = match &self.kind {
                    MatcherKind::Regex(_) if accepted => found.end,
                    _ => found.start + char_len_at(text, found.start),
                };
                if accepted {
                    return Some(found);
                }
            }
        })
    }
}

fn char_len_at(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(1, char::len_utf8)
}

fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True unless the match cuts an identifier: an identifier char on both sides of its start or
/// of its end.
fn is_whole_word(text: &str, range: Range<usize>) -> bool {
    let cuts = |before: Option<char>, after: Option<char>| {
        before.is_some_and(is_identifier_char) && after.is_some_and(is_identifier_char)
    };
    let matched = &text[range.clone()];
    !cuts(
        text[..range.start].chars().next_back(),
        matched.chars().next(),
    ) && !cuts(
        matched.chars().next_back(),
        text[range.end..].chars().next(),
    )
}

/// Byte ranges of every occurrence of `needle` in `text`, in order. Matches may overlap: "aa"
/// occurs twice in "aaa". Lazy, so a caller can stop early.
pub fn find_occurrences<'t>(
    text: &'t str,
    needle: &'t str,
    case_sensitive: bool,
) -> impl Iterator<Item = Range<usize>> + 't {
    assert!(!needle.is_empty(), "an empty needle matches everywhere");
    let mut from = 0;
    std::iter::from_fn(move || {
        let range = find_occurrence(text, needle, case_sensitive, from)?;
        // The next match may start inside this one, but only on a char boundary.
        from = range.start + char_len_at(text, range.start);
        Some(range)
    })
}

fn find_occurrence(
    text: &str,
    needle: &str,
    case_sensitive: bool,
    from: usize,
) -> Option<Range<usize>> {
    if case_sensitive {
        let start = from + text[from..].find(needle)?;
        Some(start..start + needle.len())
    } else {
        text[from..].char_indices().find_map(|(offset, _)| {
            let start = from + offset;
            let len = match_len_ignoring_case(&text[start..], needle)?;
            Some(start..start + len)
        })
    }
}

/// Measured in `text`: a char and its other case may differ in byte length (KELVIN SIGN, 'k').
fn match_len_ignoring_case(text: &str, needle: &str) -> Option<usize> {
    let mut text_chars = text.char_indices();
    for n in needle.chars() {
        let (_, t) = text_chars.next()?;
        let equal = if t.is_ascii() && n.is_ascii() {
            t.eq_ignore_ascii_case(&n)
        } else {
            t.to_lowercase().eq(n.to_lowercase())
        };
        if !equal {
            return None;
        }
    }
    Some(text_chars.next().map_or(text.len(), |(end, _)| end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> FindQuery {
        FindQuery {
            text: text.to_owned(),
            ..Default::default()
        }
    }

    fn find(query: FindQuery, text: &str) -> Vec<Range<usize>> {
        Matcher::new(&query).unwrap().find_iter(text).collect()
    }

    fn case_sensitive(text: &str) -> FindQuery {
        FindQuery {
            case_sensitive: true,
            ..query(text)
        }
    }

    fn whole_word(text: &str) -> FindQuery {
        FindQuery {
            whole_word: true,
            ..query(text)
        }
    }

    fn regex(text: &str) -> FindQuery {
        FindQuery {
            regex: true,
            ..query(text)
        }
    }

    #[test]
    fn plain_matching_is_case_insensitive_by_default() {
        assert_eq!(
            find(query("value"), "Value value VALUE vAlUe"),
            vec![0..5, 6..11, 12..17, 18..23]
        );
        assert!(find(query("value"), "val").is_empty());
    }

    #[test]
    fn case_sensitive_plain_matching_matches_only_the_exact_casing() {
        assert_eq!(
            find(case_sensitive("value"), "Value value VALUE"),
            vec![6..11]
        );
        assert_eq!(
            find(case_sensitive("Value"), "Value value VALUE"),
            vec![0..5]
        );
    }

    #[test]
    fn multi_byte_text_gives_byte_ranges_of_the_text() {
        assert_eq!(find(query("ÉCOLE"), "École école"), vec![0..6, 7..13]);
        assert_eq!(find(case_sensitive("€😀"), "é€😀é€😀"), vec![2..9, 11..18]);
        let text = "\u{212A}ey key";
        assert_eq!(find(query("key"), text), vec![0..5, 6..9]);
        assert_eq!(find(regex("key"), text), vec![0..5, 6..9]);
    }

    #[test]
    fn plain_matches_overlap_on_char_boundaries() {
        assert_eq!(find(query("aa"), "aAaA"), vec![0..2, 1..3, 2..4]);
        assert_eq!(find(case_sensitive("aa"), "aaaa"), vec![0..2, 1..3, 2..4]);
        assert_eq!(find(query("éé"), "ÉéÉ"), vec![0..4, 2..6]);
        assert_eq!(
            find(case_sensitive("日本日"), "日本日本日"),
            vec![0..9, 6..15]
        );
    }

    #[test]
    fn whole_word_needs_identifier_boundaries() {
        assert_eq!(
            find(whole_word("id"), "id width _id id_x xid (id) id"),
            vec![0..2, 23..25, 27..29]
        );
        assert_eq!(find(query("id"), "width _id"), vec![1..3, 7..9]);
        assert_eq!(find(whole_word("id"), "éid id"), vec![5..7]);
    }

    #[test]
    fn whole_word_accepts_a_match_that_ends_or_starts_on_punctuation() {
        assert_eq!(find(whole_word("foo("), "foo(x) xfoo(x)"), vec![0..4]);
        assert_eq!(find(whole_word(".x"), "a.x a.xy"), vec![1..3]);
    }

    #[test]
    fn whole_word_keeps_overlapping_plain_matches_that_are_words() {
        assert_eq!(find(whole_word("a a"), "a a a"), vec![0..3, 2..5]);
    }

    #[test]
    fn regex_matches_are_non_overlapping_and_follow_the_case_flag() {
        assert_eq!(find(regex(r"\d+"), "a1 22 333"), vec![1..2, 3..5, 6..9]);
        assert_eq!(find(regex("aa"), "aaaa"), vec![0..2, 2..4]);
        assert_eq!(find(regex("v[a-z]+"), "Value value"), vec![0..5, 6..11]);
        assert_eq!(
            find(
                FindQuery {
                    case_sensitive: true,
                    ..regex("v[a-z]+")
                },
                "Value value"
            ),
            vec![6..11]
        );
    }

    #[test]
    fn regex_metacharacters_are_literal_without_the_regex_flag() {
        assert_eq!(find(query("a.c"), "abc a.c"), vec![4..7]);
        assert!(Matcher::new(&query("(")).is_ok());
    }

    #[test]
    fn whole_word_regex_retries_inside_a_rejected_match() {
        let whole_word_regex = |pattern: &str| FindQuery {
            whole_word: true,
            ..regex(pattern)
        };
        assert_eq!(find(whole_word_regex("a b|b"), "xa b"), vec![3..4]);
        assert_eq!(find(whole_word_regex(r"\d+"), "a12 34 5b"), vec![4..6]);
    }

    #[test]
    fn empty_regex_matches_are_dropped() {
        assert!(find(regex("a*"), "bbb").is_empty());
        assert_eq!(find(regex("a*"), "baab"), vec![1..3]);
        assert!(find(regex("^"), "abc").is_empty());
        assert_eq!(find(regex(".*"), "abc"), vec![0..3]);
    }

    #[test]
    fn a_regex_that_would_span_lines_matches_nothing_on_split_lines() {
        let text = "let a\nb = 1;";
        let matcher = Matcher::new(&regex(r"a\s+b")).unwrap();
        assert_eq!(matcher.find_iter(text).collect::<Vec<_>>(), vec![4..7]);
        for line in text.lines() {
            assert!(matcher.find_iter(line).next().is_none());
        }
        let lines: Vec<Vec<_>> = text
            .lines()
            .map(|line| {
                Matcher::new(&regex(r"\w+"))
                    .unwrap()
                    .find_iter(line)
                    .collect()
            })
            .collect();
        assert_eq!(lines, vec![vec![0..3, 4..5], vec![0..1, 4..5]]);
    }

    #[test]
    fn an_invalid_regex_is_an_error_with_a_message() {
        let Err(FindError::InvalidRegex(message)) = Matcher::new(&regex("(a")) else {
            panic!("expected an invalid regex error");
        };
        assert!(!message.is_empty());
        assert!(
            FindError::InvalidRegex(message)
                .to_string()
                .starts_with("Invalid regex")
        );
    }

    #[test]
    fn an_empty_query_is_an_error() {
        for query in [query(""), regex(""), whole_word("")] {
            assert!(matches!(Matcher::new(&query), Err(FindError::EmptyQuery)));
        }
        assert_eq!(FindError::EmptyQuery.to_string(), "Empty query");
        assert_eq!(find(case_sensitive(" "), "a b  c"), vec![1..2, 3..4, 4..5]);
    }
}
