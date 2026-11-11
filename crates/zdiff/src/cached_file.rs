use std::{io, ops::Range, path::Path};

use zcommon::hash::{Blake3Hash, hash_contents};

use crate::{
    lexer::{Lexer, RawTokenTrait},
    read_file_contents,
    universal_path::UniversalPath,
};

#[derive(Debug, Default, PartialEq)]
pub struct FileMetadata {
    pub line_starts: Vec<usize>,
}

impl FileMetadata {
    pub fn new(contents: &str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(contents.match_indices('\n').map(|(i, _)| i))
            .collect();
        Self { line_starts }
    }

    pub fn get_line_index(&self, byte_offset: usize) -> usize {
        log::trace!(
            "get_line_index(byte_offset: {})\n{:?}",
            byte_offset,
            &self.line_starts
        );
        match self.line_starts.binary_search(&byte_offset) {
            Ok(line) => line,
            Err(line) => line - 1,
        }
    }

    pub fn num_lines(&self) -> usize {
        self.line_starts.len()
    }
}

#[derive(Debug)]
pub struct CachedFile<T: RawTokenTrait> {
    pub path: UniversalPath,
    pub hash: Blake3Hash,
    pub contents: String,
    pub tokens: Vec<T>,
    pub metadata: FileMetadata,
}

impl<T: RawTokenTrait> PartialEq for CachedFile<T> {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.hash == other.hash && self.metadata == other.metadata
    }
}

impl<T: RawTokenTrait> Eq for CachedFile<T> {}

impl<T: RawTokenTrait> CachedFile<T> {
    pub fn read_content_span(&self, span: Range<usize>) -> &str {
        &self.contents[span]
    }

    pub fn approx_bytes(&self) -> usize {
        self.contents.len()
            + self.tokens.len() * size_of::<T>()
            + self.metadata.line_starts.len() * size_of::<usize>()
    }

    pub fn content_search(&self, query: &str) -> Vec<usize> {
        log::trace!("content_search: {}", query);
        if query.is_empty() {
            return vec![];
        }
        self.contents
            .match_indices(query)
            .map(|(offset, _)| self.metadata.get_line_index(offset))
            .collect()
    }
}

impl<T: RawTokenTrait> CachedFile<T> {
    pub fn new(display_path: UniversalPath, physical_path: impl AsRef<Path>) -> io::Result<Self> {
        let contents = read_file_contents(&physical_path)?;
        Ok(Self::from_contents(display_path, contents))
    }

    pub fn from_contents(display_path: UniversalPath, contents: String) -> Self {
        let hash = hash_contents(contents.as_bytes());
        Self::build(display_path, contents, hash)
    }

    fn build(display_path: UniversalPath, contents: String, hash: Blake3Hash) -> Self {
        let mut tokens = Vec::with_capacity(contents.len() / 4);
        tokens.extend(Lexer::new(&contents).map(T::from));
        let metadata = FileMetadata::new(&contents);

        Self {
            path: display_path,
            hash,
            contents,
            tokens,
            metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::RawToken;

    #[test]
    fn test_cached_file_reconstruct_token_modification() {
        let temp_file_1 = tempfile::NamedTempFile::new().unwrap();

        let s1 = "\t#define hello_there\n\t// Comment\n";
        std::fs::write(temp_file_1.path(), s1).unwrap();

        let display_path_1 = UniversalPath::from(temp_file_1.path().to_path_buf());
        let physical_path_1 = temp_file_1.path().to_path_buf();
        let cached_file = CachedFile::<RawToken>::new(display_path_1, physical_path_1)
            .expect("failed to create CachedFile");

        let tokens_unmodified = cached_file.tokens.clone();
        let mut tokens_modified = cached_file.tokens.clone();
        tokens_modified.remove(0);

        let reconstructed_unmodified = tokens_unmodified
            .iter()
            .map(|t| cached_file.read_content_span(t.span.clone()))
            .collect::<String>();
        let reconstructed_modified = tokens_modified
            .iter()
            .map(|t| cached_file.read_content_span(t.span.clone()))
            .collect::<String>();

        assert_eq!(reconstructed_unmodified, s1);
        assert_eq!(
            reconstructed_modified,
            "#define hello_there\n\t// Comment\n"
        );
    }

    #[test]
    fn from_contents_matches_a_file_read_from_disk() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let contents = "one\ntwo words\n";
        std::fs::write(temp_file.path(), contents).unwrap();
        let path = UniversalPath::from(temp_file.path().to_path_buf());

        let from_disk = CachedFile::<RawToken>::new(path.clone(), temp_file.path()).unwrap();
        let from_contents = CachedFile::<RawToken>::from_contents(path, contents.to_string());
        assert_eq!(from_contents, from_disk);
        assert_eq!(from_contents.tokens.len(), from_disk.tokens.len());
        assert_eq!(from_contents.contents, from_disk.contents);
    }
}
