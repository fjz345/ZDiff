use std::ops::Range;

pub trait RawTokenTrait: Clone + AsRef<RawToken> + From<RawToken> + Send + Sync + 'static {}
impl<T> RawTokenTrait for T where T: Clone + AsRef<RawToken> + From<RawToken> + Send + Sync + 'static
{}

#[derive(Debug, PartialEq, Copy, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TokenKind {
    Unknown,
    Identifier,
    Number,
    String,
    Symbol,
    Whitespace,
    Tab,
    Comment,
    CommentStart,
    CommentEnd,
    Newline,
    Keyword,
    Preprocessor,
    Bom,
}

impl TokenKind {
    pub fn is_keyword(&self) -> bool {
        matches!(self, TokenKind::Keyword)
    }
    pub fn is_whitespace(&self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::Tab | TokenKind::Newline
        )
    }
    pub fn is_comment(&self) -> bool {
        matches!(
            self,
            TokenKind::Comment | TokenKind::CommentStart | TokenKind::CommentEnd
        )
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RawToken {
    pub kind: TokenKind,
    pub span: Range<usize>,
}

impl AsRef<RawToken> for RawToken {
    fn as_ref(&self) -> &RawToken {
        &self
    }
}

/// Greedy: identifier, number and blank runs merge; a line comment or directive is one token.
#[derive(Debug, Clone)]
pub struct Lexer<'a> {
    source: &'a str,
    cursor: usize,
    // The only state carried across tokens: string literals and line comments never span a line.
    in_block_comment: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            in_block_comment: false,
        }
    }

    fn rest(&self) -> &'a str {
        &self.source[self.cursor..]
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek(), None | Some('\r' | '\n'))
    }

    fn consume_bytes(&mut self, len: usize) {
        self.cursor += len;
        debug_assert!(self.source.is_char_boundary(self.cursor));
    }

    /// Consumes a literal at the cursor if one starts there.
    fn consume_literal(&mut self) -> bool {
        match literal_len(self.rest()) {
            Some(len) => {
                self.consume_bytes(len);
                true
            }
            None => false,
        }
    }

    fn peek(&self) -> Option<char> {
        match *self.source.as_bytes().get(self.cursor)? {
            b if b.is_ascii() => Some(b as char),
            _ => self.source[self.cursor..].chars().next(),
        }
    }

    fn consume(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.cursor += c.len_utf8();
        Some(c)
    }

    pub fn parse(&mut self) -> Vec<RawToken> {
        let mut tokens = Vec::with_capacity(self.rest().len() / 4);
        tokens.extend(self.by_ref());
        tokens
    }

    pub fn token_value(&self, token: &RawToken) -> &str {
        &self.source[token.span.clone()]
    }

    pub fn reconstruct_source(&self, tokens: &[RawToken]) -> String {
        tokens.iter().map(|t| self.token_value(t)).collect()
    }
}

const KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "become",
    "bool",
    "box",
    "break",
    "byte",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "crate",
    "default",
    "do",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "final",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "interface",
    "let",
    "loop",
    "macro",
    "macro_rules",
    "match",
    "mod",
    "move",
    "mut",
    "new",
    "override",
    "priv",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "switch",
    "throw",
    "trait",
    "true",
    "try",
    "type",
    "typeof",
    "union",
    "unsafe",
    "unsized",
    "use",
    "virtual",
    "where",
    "while",
    "yield",
];

// Bit n of entry b: some keyword is n bytes long and starts with byte b.
const KEYWORD_LENGTHS_BY_FIRST_BYTE: [u16; 128] = {
    let mut lengths = [0; 128];
    let mut i = 0;
    while i < KEYWORDS.len() {
        let keyword = KEYWORDS[i].as_bytes();
        lengths[keyword[0] as usize] |= 1 << keyword.len();
        i += 1;
    }
    lengths
};

fn may_be_keyword(word: &str) -> bool {
    let (first, len) = (word.as_bytes()[0], word.len());
    first.is_ascii() && len < 16 && KEYWORD_LENGTHS_BY_FIRST_BYTE[first as usize] & (1 << len) != 0
}

/// Byte length of the literal starting at `rest` (which starts with `"` or `'`), or None if no
/// literal starts there.
/// - `"` always starts one. Backslash escapes the next char, except a line break. A string with no
///   closing quote ends before the line break.
/// - `'` starts one only when it closes after exactly one char or one escape (`'x'`, `'\''`,
///   `'\x7f'`, `'\u{1F600}'`). Otherwise it is not a literal, so Rust lifetimes (`'a`) and
///   apostrophes in prose don't turn the rest of the line into a string.
fn literal_len(rest: &str) -> Option<usize> {
    let is_line_break = |c: char| c == '\r' || c == '\n';
    let mut chars = rest.char_indices();
    match chars.next()?.1 {
        '"' => {
            while let Some((i, c)) = chars.next() {
                match c {
                    _ if is_line_break(c) => return Some(i),
                    '"' => return Some(i + 1),
                    '\\' => {
                        if chars.clone().next().is_some_and(|(_, n)| !is_line_break(n)) {
                            chars.next();
                        }
                    }
                    _ => {}
                }
            }
            Some(rest.len())
        }
        '\'' => match chars.next()?.1 {
            '\'' => None,
            c if is_line_break(c) => None,
            '\\' => {
                if is_line_break(chars.next()?.1) {
                    return None;
                }
                // The tail of a hex, octal or unicode escape, then the closing quote.
                for (i, c) in chars {
                    match c {
                        '\'' => return Some(i + 1),
                        _ if c.is_ascii_hexdigit() || c == '{' || c == '}' => {}
                        _ => return None,
                    }
                }
                None
            }
            _ => {
                let (i, c) = chars.next()?;
                (c == '\'').then_some(i + 1)
            }
        },
        _ => None,
    }
}

impl Iterator for Lexer<'_> {
    type Item = RawToken;

    fn next(&mut self) -> Option<Self::Item> {
        self.peek()?;
        let start = self.cursor;
        let kind = if self.in_block_comment {
            self.next_block_comment_token()
        } else {
            self.next_code_token()
        };
        Some(RawToken {
            kind,
            span: start..self.cursor,
        })
    }
}

impl Lexer<'_> {
    /// Whitespace keeps its kinds (ignore-whitespace and row building depend on them) and
    /// everything else is Comment. Quotes, `//`, `/*` and `#` are plain text here.
    fn next_block_comment_token(&mut self) -> TokenKind {
        if let Some(kind) = self.lex_newline().or_else(|| self.lex_blank()) {
            return kind;
        }
        if self.rest().starts_with("*/") {
            self.consume_bytes(2);
            self.in_block_comment = false;
            return TokenKind::CommentEnd;
        }
        self.lex_word_or_symbol();
        TokenKind::Comment
    }

    fn lex_newline(&mut self) -> Option<TokenKind> {
        match self.peek()? {
            '\r' => {
                self.consume();
                if self.peek() == Some('\n') {
                    self.consume();
                }
                Some(TokenKind::Newline)
            }
            '\n' => {
                self.consume();
                Some(TokenKind::Newline)
            }
            _ => None,
        }
    }

    fn lex_blank(&mut self) -> Option<TokenKind> {
        match self.peek()? {
            '\t' => {
                self.consume();
                Some(TokenKind::Tab)
            }
            c if c.is_whitespace() => {
                self.consume();
                while self.peek().map_or(false, |next| {
                    next.is_whitespace() && next != '\n' && next != '\r'
                }) {
                    self.consume();
                }
                Some(TokenKind::Whitespace)
            }
            _ => None,
        }
    }

    fn next_code_token(&mut self) -> TokenKind {
        if let Some(kind) = self.lex_newline().or_else(|| self.lex_blank()) {
            return kind;
        }
        if self.cursor == 0 && self.rest().starts_with('\u{feff}') {
            self.consume();
            return TokenKind::Bom;
        }
        let c = self.peek().expect("next_code_token at end of source");
        match c {
            '/' if self.rest().starts_with("//") => {
                let rest = self.rest().as_bytes();
                let len = rest.iter().position(|&b| b == b'\r' || b == b'\n');
                self.consume_bytes(len.unwrap_or(rest.len()));
                TokenKind::Comment
            }
            '/' if self.rest().starts_with("/*") => {
                self.consume_bytes(2);
                self.in_block_comment = true;
                TokenKind::CommentStart
            }
            // A `*/` with no open comment: kept as CommentEnd, as before; it changes no state.
            '*' if self.rest().starts_with("*/") => {
                self.consume_bytes(2);
                TokenKind::CommentEnd
            }
            '"' | '\'' if self.consume_literal() => TokenKind::String,
            '#' => {
                self.consume();
                // The directive ends where a comment starts; literals are skipped so a `//`
                // inside one (`#define URL "http://x"`) doesn't end it.
                while !self.at_line_end()
                    && !self.rest().starts_with("//")
                    && !self.rest().starts_with("/*")
                {
                    if !self.consume_literal() {
                        self.consume();
                    }
                }
                TokenKind::Preprocessor
            }
            _ => self.lex_word_or_symbol(),
        }
    }

    fn lex_word_or_symbol(&mut self) -> TokenKind {
        let c = self.peek().expect("lex_word_or_symbol at end of source");
        match c {
            _ if c.is_alphabetic()
                || c == '_'
                || (c > '\x7f' && !c.is_control() && !c.is_whitespace()) =>
            {
                let start = self.cursor;
                self.consume();
                while self.peek().map_or(false, |next| {
                    next.is_alphanumeric()
                        || next == '_'
                        || (next > '\x7f' && !next.is_control() && !next.is_whitespace())
                }) {
                    self.consume();
                }

                let word = &self.source[start..self.cursor];
                if may_be_keyword(word) && KEYWORDS.binary_search(&word).is_ok() {
                    TokenKind::Keyword
                } else {
                    TokenKind::Identifier
                }
            }
            _ if c.is_digit(10) => {
                self.consume();
                while self.peek().map_or(false, |next| next.is_digit(10)) {
                    self.consume();
                }
                TokenKind::Number
            }
            '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '-' | '=' | '+' | '['
            | ']' | '{' | '}' | '|' | ';' | ':' | '\'' | '<' | '>' | ',' | '.' | '?' | '/' => {
                let next = self.source.as_bytes().get(self.cursor + 1).copied();
                let is_operator = matches!(
                    (c as u8, next),
                    (b'>', Some(b'>' | b'='))
                        | (b'<', Some(b'<' | b'='))
                        | (b'=' | b'!' | b'+' | b'-' | b'*' | b'/', Some(b'='))
                        | (b'&', Some(b'&'))
                        | (b'|', Some(b'|'))
                        | (b'-', Some(b'>'))
                        | (b':', Some(b':'))
                );
                self.consume_bytes(if is_operator { 2 } else { 1 });
                TokenKind::Symbol
            }
            _ => {
                self.consume();
                TokenKind::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_syntax() {
        let cases = [
            // Declarations
            ("C Decl", r#"void main(int argc, char *argv[])"#),
            ("Rust Decl", r#"fn main()"#),
            ("Python Decl", r#"def main(argc, argv):"#),
            // Definitions
            (
                "C Def",
                "void main(int argc, char *argv[])\n{\n    std::out << \"Hello, world!\" << std::endl;\n}",
            ),
            (
                "Rust Def",
                "fn main()\n{\n    println!(\"Hello, world!\");\n}\n",
            ),
            (
                "Python Def",
                "def main(argc, argv):\n    print(\"Hello, world!\")",
            ),
        ];

        for (name, src) in cases {
            let mut lexer = Lexer::new(src);
            let tokens = lexer.parse();

            assert!(
                !tokens.iter().any(|t| matches!(t.kind, TokenKind::Unknown)),
                "[{}] Lexer found unknown tokens: {:?}",
                name,
                tokens
                    .iter()
                    .filter(|t| matches!(t.kind, TokenKind::Unknown))
                    .map(|t| lexer.token_value(t))
                    .collect::<Vec<_>>()
            );

            let reconstructed = lexer.reconstruct_source(&tokens);
            assert_eq!(
                reconstructed, src,
                "[{}] Reconstruction mismatch!\nExpected: {:?}\nGot:      {:?}",
                name, src, reconstructed
            );
        }
    }

    #[test]
    fn test_greedy_operators_exhaustive() {
        let input = "x / / y // comment\nx /* block */ y";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.parse();

        // Verification Table:
        // Index | Token Kind | Value
        // -------------------------
        // 0     | Identifier   | "x"
        // 1     | Whitespace   | " "
        // 2     | Symbol       | "/"
        // 3     | Whitespace   | " "
        // 4     | Symbol       | "/"
        // 5     | Whitespace   | " "
        // 6     | Identifier   | "y"
        // 7     | Whitespace   | " "
        // 8     | Comment      | "// comment"
        // 9     | Whitespace   | "\n"
        // 10    | Identifier   | "x"
        // 11    | Whitespace   | " "
        // 12    | CommentStart | "/*"
        // 13    | Whitespace   | " "
        // 14    | Comment      | "block"
        // 15    | Whitespace   | " "
        // 16    | CommentEnd   | "*/"
        // 17    | Whitespace   | " "
        // 18    | Identifier   | "y"

        let expected = vec![
            (TokenKind::Identifier, "x"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Symbol, "/"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Symbol, "/"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Identifier, "y"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Comment, "// comment"),
            (TokenKind::Newline, "\n"),
            (TokenKind::Identifier, "x"),
            (TokenKind::Whitespace, " "),
            (TokenKind::CommentStart, "/*"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Comment, "block"),
            (TokenKind::Whitespace, " "),
            (TokenKind::CommentEnd, "*/"),
            (TokenKind::Whitespace, " "),
            (TokenKind::Identifier, "y"),
        ];

        assert_eq!(tokens.len(), expected.len(), "Token count mismatch.");

        println!("Tokens: {:#?}", tokens);

        for (i, (kind, value)) in expected.into_iter().enumerate() {
            assert_eq!(
                tokens[i].kind, kind,
                "Token[{}] kind mismatch. Expected {:?}, got {:?}",
                i, kind, tokens[i].kind
            );
            assert_eq!(
                lexer.token_value(&tokens[i]),
                value,
                "Token[{}] value mismatch. Expected {:?}, got {:?}",
                i,
                value,
                lexer.token_value(&tokens[i])
            );
        }
    }

    #[test]
    fn test_complex_strings() {
        let input = r#"" " "" "with symbols !@#" "unclosed"#;
        let mut lexer = Lexer::new(input);
        let tokens = lexer.parse();

        assert!(
            tokens.iter().any(|t| matches!(t.kind, TokenKind::String)),
            "Lexer failed to identify any String tokens in input: {}",
            input
        );

        let reconstructed = lexer.reconstruct_source(&tokens);
        assert_eq!(
            reconstructed, input,
            "String reconstruction failed. Original: {}, Got: {}",
            input, reconstructed
        );
    }

    #[test]
    fn test_numeric_boundaries() {
        let input = "123.456 789";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.parse();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Number,
            "Expected '123' to be Number, got {:?}",
            tokens[0].kind
        );
        assert_eq!(
            tokens[1].kind,
            TokenKind::Symbol,
            "Expected '.' to be Symbol, got {:?}",
            tokens[1].kind
        );
        assert_eq!(
            tokens[2].kind,
            TokenKind::Number,
            "Expected '456' to be Number, got {:?}",
            tokens[2].kind
        );
        assert_eq!(
            tokens[3].kind,
            TokenKind::Whitespace,
            "Expected ' ' to be Whitespace, got {:?}",
            tokens[3].kind
        );
        assert_eq!(
            tokens[4].kind,
            TokenKind::Number,
            "Expected '789' to be Number, got {:?}",
            tokens[4].kind
        );
    }

    #[test]
    fn test_unicode_and_whitespace() {
        let input = "let 🦀 = \"value\";\t\n ";
        let mut lexer = Lexer::new(input);
        let tokens = lexer.parse();

        for token in &tokens {
            assert!(
                !matches!(token.kind, TokenKind::Unknown),
                "Unknown token found: {:?} ('{}')",
                token,
                &input[token.span.clone()]
            );
        }

        assert_eq!(
            lexer.reconstruct_source(&tokens),
            input,
            "Unicode/Whitespace reconstruction failed."
        );
    }

    #[test]
    fn test_lex_idempotency() {
        let input = "fn main() { let x = 5; } // check";
        let mut lexer1 = Lexer::new(input);
        let tokens1 = lexer1.parse();

        let reconstructed = lexer1.reconstruct_source(&tokens1);
        let mut lexer2 = Lexer::new(&reconstructed);
        let tokens2 = lexer2.parse();

        assert_eq!(
            tokens1.len(),
            tokens2.len(),
            "Idempotency failed: different token counts. Original: {}, New: {}",
            tokens1.len(),
            tokens2.len()
        );

        for (i, (t1, t2)) in tokens1.iter().zip(tokens2.iter()).enumerate() {
            assert_eq!(
                t1.kind, t2.kind,
                "Token kind mismatch at index {}.\nToken 1: {:?}\nToken 2: {:?}",
                i, t1, t2
            );
            assert_eq!(
                t1.span, t2.span,
                "Token spawn mismatch at index {}.\nToken 1: {:?}\nToken 2: {:?}",
                i, t1, t2
            );
        }
    }

    #[test]
    fn test_lexer_newline_formats() {
        let unix_input = "a\nb";
        let win_input = "a\r\nb";

        // Test Unix Lexing
        let lex_unix = Lexer::new(unix_input);
        let tokens_unix: Vec<RawToken> = lex_unix.collect();

        // Expect: [Identifier("a"), Newline("\n"), Identifier("b")]
        assert_eq!(tokens_unix.len(), 3);
        assert_eq!(tokens_unix[1].kind, TokenKind::Newline);
        assert_eq!(tokens_unix[1].span.end - tokens_unix[1].span.start, 1);
        assert_eq!(&unix_input[tokens_unix[1].span.clone()], "\n");

        // Test Windows Lexing
        let lex_win = Lexer::new(win_input);
        let tokens_win: Vec<RawToken> = lex_win.collect();

        // Expect: [Identifier("a"), Newline("\r\n"), Identifier("b")]
        assert_eq!(tokens_win.len(), 3);
        assert_eq!(tokens_win[1].kind, TokenKind::Newline);
        assert_eq!(tokens_win[1].span.end - tokens_win[1].span.start, 2);
        assert_eq!(&win_input[tokens_win[1].span.clone()], "\r\n");
    }

    #[test]
    fn test_lexer_mixed_whitespace_and_newlines() {
        let input = " \n \r\n ";
        let tokens: Vec<RawToken> = Lexer::new(input).collect();

        dbg!(&tokens);

        // Should distinguish between Whitespace (spaces) and Newline
        assert_eq!(tokens[0].kind, TokenKind::Whitespace);
        assert_eq!(tokens[1].kind, TokenKind::Newline);
        assert_eq!(tokens[2].kind, TokenKind::Whitespace);
        assert_eq!(tokens[3].kind, TokenKind::Newline);
        assert_eq!(tokens[4].kind, TokenKind::Whitespace);
    }

    #[test]
    fn test_files_simple() {
        let cpp_file = r#"
    #include <iostream>

    // Main entry point
    int main() {
        std::cout << "Hello from C++" << std::endl;
        return 0;
    }
    "#;

        let rust_file = r#"
    fn main() {
        /* Macros use the ! symbol */
        println!("Hello from Rust");
    }
    "#;

        let python_file = r#"
    #!/usr/bin/env python3
    import os

    def main():
        print("Hello from Python")

    if __name__ == "__main__":
        main()
    "#;

        let files = [
            ("main.cpp", cpp_file),
            ("main.rs", rust_file),
            ("main.py", python_file),
        ];

        for (filename, content) in files {
            let mut lexer = Lexer::new(content);
            let tokens = lexer.parse();

            assert!(
                !tokens.iter().any(|t| matches!(t.kind, TokenKind::Unknown)),
                "[{}] Lexer failed to categorize some characters: {:?}",
                filename,
                tokens
                    .iter()
                    .filter(|t| matches!(t.kind, TokenKind::Unknown))
                    .map(|t| lexer.token_value(t))
                    .collect::<Vec<_>>()
            );

            let reconstructed = lexer.reconstruct_source(&tokens);
            assert_eq!(
                reconstructed, content,
                "[{}] Reconstruction failed. Content was modified during lexing.",
                filename
            );

            match filename {
                "main.cpp" => {
                    assert!(
                        tokens
                            .iter()
                            .any(|t| lexer.token_value(t) == "#include <iostream>")
                    );
                }
                "main.rs" => {
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "/*"));
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "*/"));
                }
                "main.py" => {
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "__name__"));
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn test_files_advanced() {
        let advanced_cpp = r#"
    #include <vector>
    #include <memory>
    #include <algorithm>

    /* * Advanced Template Meta-programming 
    * and modern C++ features.
    */
    namespace engine {
        template <typename T>
        class ResourceManager {
        private:
            std::vector<std::shared_ptr<T>> resources;
            size_t total_allocated = 0;

        public:
            ResourceManager() = default;

            auto add(T&& item) -> void {
                resources.push_back(std::make_shared<T>(std::move(item)));
                total_allocated += sizeof(T);
            }

            template <typename F>
            void for_each(F func) {
                std::for_each(resources.begin(), resources.end(), [&](auto& res) {
                    if (res != nullptr) {
                        func(*res);
                    }
                });
            }

            auto size() const { return resources.size(); }
        };
    }

    int main(int argc, char** argv) {
        engine::ResourceManager<int> manager;
        for(int i = 0; i < 100; ++i) {
            manager.add(i * 2);
        }
        // Check bits: 0xFF & 0b1010
        int bit_check = 0xAF >> 2;
        return bit_check > 0 ? 0 : 1;
    }
    "#;

        let advanced_rust = r#"
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A complex trait for asynchronous processing
    pub trait Processor {
        type Output;
        fn process(&self, data: &str) -> Self::Output;
    }

    #[derive(Debug, Clone)]
    struct Node<T> where T: Processor {
        id: u64,
        inner: Arc<Mutex<T>>,
        metadata: HashMap<String, String>,
    }

    impl<T> Node<T> where T: Processor {
        pub fn new(id: u64, p: T) -> Self {
            Self {
                id,
                inner: Arc::new(Mutex::new(p)),
                metadata: HashMap::new(),
            }
        }

        pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
            let lock = self.inner.lock().unwrap();
            let _ = lock.process("input_data");
            println!("Node {} finished processing.", self.id);
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn test_node() {
            let input = "x += 5; // increment";
            assert!(input.contains("+="));
        }
    }
    "#;

        let advanced_python = r#"
    import numpy as np
    import pandas as pd
    from datetime import datetime

    class DataPipeline:
        """
        Handles heavy data transformations
        """
        def __init__(self, name: str):
            self.name = name
            self.start_time = datetime.now()
            self._cache = {}

        @property
        def status(self) -> str:
            return f"Pipeline {self.name} started at {self.start_time}"

        def process_frame(self, df: pd.DataFrame) -> pd.DataFrame:
            # Complex filtering
            mask = (df['val'] > 0.5) & (df['category'] != 'ignore')
            df['transformed'] = df['val'].apply(lambda x: x ** 2 if x > 0 else -1)
            
            # Multiline string check
            query = """
            SELECT * FROM results
            WHERE score > 90
            AND status = 'PASS'
            """
            return df[mask]

    def run_simulation():
        data = np.random.rand(1000, 3)
        cols = ['a', 'b', 'c']
        df = pd.DataFrame(data, columns=cols)
        pipe = DataPipeline("Sim_01")
        print(pipe.status)
        return pipe.process_frame(df)

    if __name__ == "__main__":
        results = run_simulation()
        print(f"Processed {len(results)} rows.")
    "#;

        let workload = [
            ("Advanced C++", advanced_cpp),
            ("Advanced Rust", advanced_rust),
            ("Advanced Python", advanced_python),
        ];

        for (name, source) in workload {
            let mut lexer = Lexer::new(source);
            let tokens = lexer.parse();

            let unknown_tokens: Vec<_> = tokens
                .iter()
                .filter(|t| t.kind == TokenKind::Unknown)
                .map(|t| lexer.token_value(t))
                .collect();

            assert!(
                unknown_tokens.is_empty(),
                "[{}] Lexer failed on these characters: {:?}",
                name,
                unknown_tokens
            );

            let reconstructed = lexer.reconstruct_source(&tokens);
            assert_eq!(
                reconstructed, source,
                "[{}] RECONSTRUCTION FAILURE. Loss of data detected.",
                name
            );

            match name {
                "Advanced C++" => {
                    assert!(
                        tokens
                            .iter()
                            .any(|t| lexer.token_value(t) == "ResourceManager")
                    );
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == ">>")); // Shift or nested template
                }
                "Advanced Rust" => {
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "Processor"));
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "Box"));
                }
                "Advanced Python" => {
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "lambda"));
                    assert!(tokens.iter().any(|t| lexer.token_value(t) == "status"));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_files_simple_header() {
        let source = "\t#define hello_there
\t// Keyboard/Gamepad Navigation options
    bool        ConfigNavSwapGamepadButtons;    // = false
\tbool        ConfigNavMoveSetMousePos;       // = false
";

        let expected = [
            ("\t", TokenKind::Tab),
            ("#define hello_there", TokenKind::Preprocessor),
            ("\n", TokenKind::Newline),
            ("\t", TokenKind::Tab),
            ("// Keyboard/Gamepad Navigation options", TokenKind::Comment),
            ("\n", TokenKind::Newline),
            ("    ", TokenKind::Whitespace),
            ("bool", TokenKind::Keyword),
            ("        ", TokenKind::Whitespace),
            ("ConfigNavSwapGamepadButtons", TokenKind::Identifier),
            (";", TokenKind::Symbol),
            ("    ", TokenKind::Whitespace),
            ("// = false", TokenKind::Comment),
            ("\n", TokenKind::Newline),
            ("\t", TokenKind::Tab),
            ("bool", TokenKind::Keyword),
            ("        ", TokenKind::Whitespace),
            ("ConfigNavMoveSetMousePos", TokenKind::Identifier),
            (";", TokenKind::Symbol),
            ("       ", TokenKind::Whitespace),
            ("// = false", TokenKind::Comment),
            ("\n", TokenKind::Newline),
        ];

        let mut lexer = Lexer::new(source);
        let tokens = lexer.parse();

        let unknown: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Unknown)
            .map(|t| lexer.token_value(t))
            .collect();
        assert!(
            unknown.is_empty(),
            "Lexer produced Unknown tokens: {:?}",
            unknown
        );

        if tokens.len() != expected.len()
            || !tokens.iter().enumerate().all(|(i, t)| {
                let actual_val = lexer.token_value(t);
                i < expected.len() && (actual_val, t.kind) == expected[i]
            })
        {
            let mut report = String::new();
            report.push_str("\nTOKEN MATCH FAILURE\n");
            report.push_str(&format!(
                "{:<3} | {:<20} | {:<15} | {:<20} | {:<15}\n",
                "IDX", "ACTUAL VAL", "ACTUAL KIND", "EXPECTED VAL", "EXPECTED KIND"
            ));
            report.push_str(&"-".repeat(80));
            report.push('\n');

            let max_len = tokens.len().max(expected.len());
            for i in 0..max_len {
                let actual = tokens.get(i);
                let exp = expected.get(i);

                let a_val = actual
                    .map(|t| format!("{:?}", lexer.token_value(t)))
                    .unwrap_or_else(|| "MISSING".to_string());
                let a_kind = actual
                    .map(|t| format!("{:?}", t.kind))
                    .unwrap_or_else(|| "".to_string());

                let e_val = exp
                    .map(|(v, _)| format!("{:?}", v))
                    .unwrap_or_else(|| "EXTRA".to_string());
                let e_kind = exp
                    .map(|(_, k)| format!("{:?}", k))
                    .unwrap_or_else(|| "".to_string());

                let marker = if actual.is_some()
                    && exp.is_some()
                    && (lexer.token_value(actual.unwrap()), actual.unwrap().kind)
                        == (exp.unwrap().0, exp.unwrap().1)
                {
                    " "
                } else {
                    "!"
                };

                report.push_str(&format!(
                    "{:<3}{} | {:<20} | {:<15} | {:<20} | {:<15}\n",
                    i, marker, a_val, a_kind, e_val, e_kind
                ));
            }
            panic!("{}", report);
        }
    }

    fn lex(src: &str) -> Vec<RawToken> {
        let tokens = Lexer::new(src).parse();
        assert_tiles(src, &tokens);
        tokens
    }

    /// Reconstruction is exact, spans are contiguous and non-empty, and line breaks only ever
    /// appear in Newline tokens (rows, line metadata and revert's hunk_bytes rely on all three).
    fn assert_tiles(src: &str, tokens: &[RawToken]) {
        let mut cursor = 0;
        for t in tokens {
            assert_eq!(t.span.start, cursor, "gap or overlap at {t:?} in {src:?}");
            assert!(!t.span.is_empty(), "empty token {t:?} in {src:?}");
            let text = &src[t.span.clone()];
            if t.kind != TokenKind::Newline {
                assert!(
                    !text.contains(['\r', '\n']),
                    "line break inside {:?} token {text:?} in {src:?}",
                    t.kind
                );
            }
            cursor = t.span.end;
        }
        assert_eq!(cursor, src.len(), "tokens don't reach the end of {src:?}");
    }

    fn range_of(src: &str, part: &str) -> Range<usize> {
        let start = src
            .find(part)
            .unwrap_or_else(|| panic!("{part:?} not in {src:?}"));
        assert!(
            src[start + 1..].find(part).is_none(),
            "{part:?} is not unique in {src:?}"
        );
        start..start + part.len()
    }

    /// Each non-whitespace token inside one of `comments` has a comment kind and each one outside
    /// doesn't; no token straddles a comment edge. Each of `strings` is exactly one String token.
    fn check_comments_and_strings(src: &str, comments: &[&str], strings: &[&str]) {
        let comment_ranges: Vec<_> = comments.iter().map(|c| range_of(src, c)).collect();
        let string_ranges: Vec<_> = strings.iter().map(|s| range_of(src, s)).collect();
        let tokens = lex(src);
        let describe = |t: &RawToken| format!("{:?} {:?} in {src:?}", t.kind, &src[t.span.clone()]);
        for t in &tokens {
            let inside = comment_ranges
                .iter()
                .any(|r| r.start <= t.span.start && t.span.end <= r.end);
            let overlaps = comment_ranges
                .iter()
                .any(|r| t.span.start < r.end && r.start < t.span.end);
            assert!(
                inside || !overlaps,
                "{} straddles a comment edge",
                describe(t)
            );
            if t.kind.is_whitespace() {
                continue;
            }
            assert_eq!(t.kind.is_comment(), inside, "{}", describe(t));
        }
        for r in &string_ranges {
            assert!(
                tokens
                    .iter()
                    .any(|t| t.kind == TokenKind::String && t.span == *r),
                "no String token for {:?} in {src:?}: {tokens:?}",
                &src[r.clone()]
            );
        }
    }

    #[test]
    fn block_comment_spans_lines() {
        check_comments_and_strings("a /* if x\n   while */ b\n", &["/* if x\n   while */"], &[]);
        check_comments_and_strings(
            "a /* if x\r\n\t \"y\r\n*/ b\r\n",
            &["/* if x\r\n\t \"y\r\n*/"],
            &[],
        );
    }

    #[test]
    fn line_comment_runs_to_the_end_of_its_line() {
        check_comments_and_strings("x = 1; // return y\nz\n", &["// return y"], &[]);
        check_comments_and_strings("x = 1; // return y\r\nz", &["// return y"], &[]);
    }

    #[test]
    fn comment_markers_inside_strings_do_not_start_comments() {
        check_comments_and_strings(
            "s = \"// not\" + \"/* no\";\nt = '/' ;\n",
            &[],
            &["\"// not\"", "\"/* no\"", "'/'"],
        );
        check_comments_and_strings("c = '\"'; // yes \"\nd\n", &["// yes \""], &["'\"'"]);
    }

    #[test]
    fn escaped_quotes_do_not_end_literals() {
        check_comments_and_strings(
            "a = \"x\\\"// y\\\\\"; b // c\n",
            &["// c"],
            &["\"x\\\"// y\\\\\""],
        );
        check_comments_and_strings(
            "q = '\\''; // c\nr = '\\u{1F600}';\n",
            &["// c"],
            &["'\\''", "'\\u{1F600}'"],
        );
    }

    #[test]
    fn unterminated_string_ends_at_the_line_end() {
        check_comments_and_strings("a = \"abc // d\nb // c\n", &["// c"], &["\"abc // d"]);
        check_comments_and_strings("a = \"abc // d\r\nb // c", &["// c"], &["\"abc // d"]);
        // A trailing backslash must not swallow the line break, LF or CRLF.
        check_comments_and_strings("a = \"abc\\\r\nb // c\n", &["// c"], &["\"abc\\"]);
        check_comments_and_strings("a = \"abc\\\nb // c\n", &["// c"], &["\"abc\\"]);
        check_comments_and_strings("a = \"abc", &[], &["\"abc"]);
    }

    #[test]
    fn unterminated_block_comment_runs_to_eof() {
        check_comments_and_strings(
            "a /* x\nif y\n\"z\n// w\n",
            &["/* x\nif y\n\"z\n// w\n"],
            &[],
        );
        check_comments_and_strings("a /*", &["/*"], &[]);
    }

    #[test]
    fn keywords_inside_comments_are_not_keywords() {
        let src = "if a // if while\n/* return\nstruct */ else\n";
        check_comments_and_strings(src, &["// if while", "/* return\nstruct */"], &[]);
        let keywords: Vec<_> = lex(src)
            .iter()
            .filter(|t| t.kind == TokenKind::Keyword)
            .map(|t| &src[t.span.clone()])
            .collect();
        assert_eq!(keywords, ["if", "else"]);
    }

    #[test]
    fn a_bom_is_a_token_of_its_own_only_at_the_start_of_the_file() {
        let kinds = |src: &str| -> Vec<(TokenKind, String)> {
            lex(src)
                .iter()
                .map(|t| (t.kind, src[t.span.clone()].to_string()))
                .collect()
        };
        assert_eq!(
            kinds("\u{feff}if x"),
            [
                (TokenKind::Bom, "\u{feff}".into()),
                (TokenKind::Keyword, "if".into()),
                (TokenKind::Whitespace, " ".into()),
                (TokenKind::Identifier, "x".into()),
            ]
        );
        assert_eq!(kinds("\u{feff}"), [(TokenKind::Bom, "\u{feff}".into())]);
        assert_eq!(
            kinds("a\u{feff}b"),
            [(TokenKind::Identifier, "a\u{feff}b".into())]
        );
    }
}
