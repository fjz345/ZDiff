use std::{collections::HashMap, ops::Range};

use crate::{
    lexer::{RawTokenTrait, TokenKind},
    myers::split_lines,
};

const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedScope {
    pub header: usize,
    pub end: usize,
    pub header_text: Range<usize>,
    parent: Option<usize>,
}

impl NestedScope {
    fn holds(&self, line: usize) -> bool {
        self.header < line && line < self.end
    }
}

pub fn nested_scopes<T: RawTokenTrait>(tokens: &[T], text: &str) -> Vec<NestedScope> {
    let lines = split_lines(tokens);
    let spans = match brace_spans(tokens, text, &lines) {
        Some(spans) if !spans.is_empty() => spans,
        _ => indent_spans(tokens, text, &lines),
    };
    let line_text = |line: usize| {
        let range = &lines[line];
        let start = tokens[range.start].as_ref().span.start;
        let end = range
            .clone()
            .rev()
            .find(|&i| tokens[i].as_ref().kind != TokenKind::Newline)
            .map_or(start, |i| tokens[i].as_ref().span.end);
        start..end
    };
    with_parents(spans, line_text)
}

pub fn enclosing(scopes: &[NestedScope], line: usize) -> Vec<&NestedScope> {
    let mut at = scopes.partition_point(|s| s.header < line).checked_sub(1);
    let mut held = Vec::new();
    while let Some(index) = at {
        let scope = &scopes[index];
        if scope.holds(line) {
            held.push(scope);
        }
        at = scope.parent;
    }
    held.reverse();
    held.dedup_by_key(|scope| scope.header);
    held
}

// Sorted by header, outer before inner, so the last scope opened above a line descends from
// the innermost one holding it.
fn with_parents(
    mut spans: Vec<(usize, usize)>,
    line_text: impl Fn(usize) -> Range<usize>,
) -> Vec<NestedScope> {
    spans.sort_by_key(|&(header, end)| (header, std::cmp::Reverse(end)));
    let mut scopes: Vec<NestedScope> = Vec::with_capacity(spans.len());
    let mut open: Vec<usize> = Vec::new();
    for (header, end) in spans {
        while open
            .last()
            .is_some_and(|&outer| end > scopes[outer].end || header < scopes[outer].header)
        {
            open.pop();
        }
        open.push(scopes.len());
        scopes.push(NestedScope {
            header,
            end,
            header_text: line_text(header),
            parent: open.len().checked_sub(2).map(|i| open[i]),
        });
    }
    scopes
}

fn first_code_token<T: RawTokenTrait>(tokens: &[T], line: &Range<usize>) -> Option<usize> {
    line.clone()
        .find(|&i| !tokens[i].as_ref().kind.is_whitespace())
}

fn is_symbol<T: RawTokenTrait>(tokens: &[T], text: &str, index: usize, symbols: &[&str]) -> bool {
    let token = tokens[index].as_ref();
    token.kind == TokenKind::Symbol && symbols.contains(&&text[token.span.clone()])
}

fn brace_spans<T: RawTokenTrait>(
    tokens: &[T],
    text: &str,
    lines: &[Range<usize>],
) -> Option<Vec<(usize, usize)>> {
    let line_of = |i: usize| lines.partition_point(|l| l.end <= i);
    let starts_with = |line: usize, symbols: &[&str]| {
        first_code_token(tokens, &lines[line]).is_some_and(|i| is_symbol(tokens, text, i, symbols))
    };
    let mut opened_on: HashMap<usize, usize> = HashMap::new();
    let header_of = |brace: usize, opened_on: &HashMap<usize, usize>| {
        let mut line = line_of(brace);
        let alone = first_code_token(tokens, &lines[line]) == Some(brace)
            && lines[line]
                .clone()
                .filter(|&i| i > brace)
                .all(|i| tokens[i].as_ref().kind.is_whitespace());
        if alone
            && line > 0
            && first_code_token(tokens, &lines[line - 1]).is_some()
            && !starts_with(line - 1, &["}"])
        {
            line -= 1;
        }
        while let Some(&up) = opened_on.get(&line) {
            line = up;
        }
        line
    };

    let (mut braces, mut brackets, mut spans) = (Vec::new(), Vec::new(), Vec::new());
    for (i, token) in tokens.iter().enumerate() {
        if token.as_ref().kind != TokenKind::Symbol {
            continue;
        }
        match &text[token.as_ref().span.clone()] {
            "{" => braces.push(i),
            "(" | "[" => brackets.push(i),
            ")" | "]" => {
                if let Some(open) = brackets.pop() {
                    let line = line_of(i);
                    if first_code_token(tokens, &lines[line]) == Some(i) && line_of(open) < line {
                        opened_on.insert(line, line_of(open));
                    }
                }
            }
            "}" => {
                let open = braces.pop()?;
                let close = line_of(i);
                if line_of(open) < close {
                    spans.push((header_of(open, &opened_on), close + 1));
                }
            }
            _ => {}
        }
    }
    braces.is_empty().then_some(spans)
}

fn indent_spans<T: RawTokenTrait>(
    tokens: &[T],
    text: &str,
    lines: &[Range<usize>],
) -> Vec<(usize, usize)> {
    let code_lines: Vec<(usize, usize, bool)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let first = first_code_token(tokens, line)?;
            let kind = tokens[first].as_ref().kind;
            if kind.is_comment() || kind == TokenKind::Preprocessor {
                return None;
            }
            let indent = text
                [tokens[line.start].as_ref().span.start..tokens[first].as_ref().span.start]
                .chars()
                .fold(0, |width, c| match c {
                    '\t' => (width / TAB_WIDTH + 1) * TAB_WIDTH,
                    _ => width + 1,
                });
            let continues = is_symbol(tokens, text, first, &["}", ")", "]"]);
            Some((index, indent, continues))
        })
        .collect();

    let mut spans = Vec::new();
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (k, &(line, indent, continues)) in code_lines.iter().enumerate() {
        if continues {
            continue;
        }
        while open.last().is_some_and(|&(_, outer)| outer >= indent) {
            let (header, _) = open.pop().expect("checked above");
            spans.push((header, code_lines[k - 1].0 + 1));
        }
        if code_lines
            .get(k + 1)
            .is_some_and(|&(_, next, _)| next > indent)
        {
            open.push((line, indent));
        }
    }
    let end = code_lines.last().map_or(0, |&(line, _, _)| line + 1);
    spans.extend(open.into_iter().map(|(header, _)| (header, end)));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn headers(text: &str, line: usize) -> Vec<usize> {
        let scopes = nested_scopes(&Lexer::new(text).parse(), text);
        enclosing(&scopes, line)
            .iter()
            .map(|scope| scope.header)
            .collect()
    }

    #[test]
    fn a_header_text_is_its_line_without_the_line_break() {
        let text = "a {\r\n    fn f() {\r\n        x;\r\n    }\r\n}\r\n";
        let scopes = nested_scopes(&Lexer::new(text).parse(), text);
        let texts: Vec<&str> = enclosing(&scopes, 2)
            .iter()
            .map(|scope| &text[scope.header_text.clone()])
            .collect();
        assert_eq!(texts, ["a {", "    fn f() {"]);
    }

    #[test]
    fn scopes_opened_on_one_line_count_once() {
        let text = "fn f() { if x {\n    y();\n} }\n";
        assert_eq!(headers(text, 1), [0]);
    }

    const RUST: &str = "\
use x;
impl Parser {
    fn parse(&mut self) {
        match token {
            Brace => {
                depth += 1;
            }
        }
    }

    fn other() {}
}
";

    #[test]
    fn a_line_lists_its_enclosing_brace_headers_outermost_first() {
        assert_eq!(headers(RUST, 5), [1, 2, 3, 4]);
        assert_eq!(headers(RUST, 4), [1, 2, 3]);
        assert_eq!(headers(RUST, 0), Vec::<usize>::new());
    }

    #[test]
    fn a_closing_brace_line_belongs_to_its_scope() {
        assert_eq!(headers(RUST, 6), [1, 2, 3, 4]);
        assert_eq!(headers(RUST, 8), [1, 2]);
        assert_eq!(headers(RUST, 11), [1]);
        assert_eq!(headers(RUST, 12), Vec::<usize>::new());
    }

    #[test]
    fn a_scope_on_one_line_encloses_nothing() {
        assert_eq!(headers(RUST, 10), [1]);
        assert_eq!(headers(RUST, 9), [1]);
    }

    #[test]
    fn an_allman_brace_is_headed_by_the_line_above() {
        let text = "void f()\n{\n    if (x)\n    {\n        y();\n    }\n}\n";
        assert_eq!(headers(text, 4), [0, 2]);
        assert_eq!(headers(text, 1), [0]);
    }

    #[test]
    fn a_multi_line_signature_is_headed_by_its_first_line() {
        let text = "fn f(\n    a: u32,\n) -> u32 {\n    a\n}\n";
        assert_eq!(headers(text, 3), [0]);
        let allman = "void f(\n    int a\n)\n{\n    g();\n}\n";
        assert_eq!(headers(allman, 4), [0]);
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_count() {
        let text = "fn a() {\n    s = \"}\";\n    // }\n    b();\n}\n";
        assert_eq!(headers(text, 3), [0]);
    }

    const PYTHON: &str = "\
class A:
    def f(self):
        if x:
            y()

# comment at column 0
            z()

        w()

def g(
    a,
    b,
):
    return a
";

    #[test]
    fn without_braces_indentation_gives_the_scopes() {
        assert_eq!(headers(PYTHON, 3), [0, 1, 2]);
        assert_eq!(headers(PYTHON, 8), [0, 1]);
        assert_eq!(headers(PYTHON, 10), Vec::<usize>::new());
    }

    #[test]
    fn blank_and_comment_lines_do_not_end_an_indented_scope() {
        assert_eq!(headers(PYTHON, 6), [0, 1, 2]);
        assert_eq!(headers(PYTHON, 4), [0, 1, 2]);
    }

    #[test]
    fn a_line_starting_with_a_closer_continues_the_indented_scope() {
        assert_eq!(headers(PYTHON, 14), [10]);
        assert_eq!(headers(PYTHON, 12), [10]);
    }

    #[test]
    fn unbalanced_braces_fall_back_to_indentation() {
        let text = "fn a() {\n    b();\n    c();\n";
        assert_eq!(headers(text, 2), [0]);
    }

    #[test]
    fn only_one_line_brace_scopes_fall_back_to_indentation() {
        let text = "def f():\n    d = {'a': 1}\n    g()\n";
        assert_eq!(headers(text, 2), [0]);
    }
}
