use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, strip_html, CorpusParser};

pub struct StackExchangeParser {
    corpus_id: String,
    min_score: i32,
}

impl StackExchangeParser {
    pub fn new(corpus_id: &str, min_score: i32) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
            min_score,
        }
    }
}

impl CorpusParser for StackExchangeParser {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let files = find_posts_files(source_path)?;
        Ok(Box::new(StackExchangeIterator {
            files: files.into(),
            corpus_id: self.corpus_id.clone(),
            min_score: self.min_score,
            current: None,
            pending: VecDeque::new(),
            chunk_counter: 0,
        }))
    }
}

fn find_posts_files(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files = Vec::new();
    // Look for Posts.xml in subdirectories (one per community).
    let entries = std::fs::read_dir(path)
        .map_err(|e| Error::Storage(format!("Failed to read {}: {e}", path.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Storage(format!("Directory entry error: {e}")))?;
        let p = entry.path();
        if p.is_dir() {
            let posts = p.join("Posts.xml");
            if posts.is_file() {
                files.push(posts);
            }
        } else if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("Posts.xml"))
        {
            files.push(p);
        }
    }
    // Also check direct Posts.xml in the given path.
    let direct = path.join("Posts.xml");
    if direct.is_file() && !files.contains(&direct) {
        files.push(direct);
    }
    files.sort();
    Ok(files)
}

struct StackExchangeIterator {
    files: VecDeque<PathBuf>,
    corpus_id: String,
    min_score: i32,
    current: Option<CommunityParser>,
    pending: VecDeque<DocumentChunk>,
    chunk_counter: usize,
}

struct CommunityParser {
    reader: XmlReader<BufReader<File>>,
    buf: Vec<u8>,
    community: String,
    /// Maps question ID -> (title, body_text).
    questions: HashMap<u64, (String, String)>,
}

impl Iterator for StackExchangeIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }

            // Try to read next row from the current community file.
            if let Some(ref mut cp) = self.current {
                match read_next_answer(cp, self.min_score) {
                    Ok(Some((title, question_body, answer_body, score))) => {
                        let content = format!(
                            "Stack Exchange ({community}): {title}\n\n\
                             Q: {question_body}\n\n\
                             A (score {score}): {answer_body}",
                            community = cp.community,
                        );
                        let source = format!("{}:{}", cp.community, self.chunk_counter);
                        let chunks = chunk_and_wrap(
                            &self.corpus_id,
                            &source,
                            &content,
                            &mut self.chunk_counter,
                        );
                        self.pending = chunks.into();
                        continue;
                    }
                    Ok(None) => {
                        // EOF for this community file.
                        self.current = None;
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // Open the next community file.
            let path = self.files.pop_front()?;
            let community = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            match File::open(&path) {
                Ok(file) => {
                    let reader = XmlReader::from_reader(BufReader::new(file));
                    self.current = Some(CommunityParser {
                        reader,
                        buf: Vec::new(),
                        community,
                        questions: HashMap::new(),
                    });
                }
                Err(e) => {
                    return Some(Err(Error::Storage(format!(
                        "Failed to open {}: {e}",
                        path.display()
                    ))));
                }
            }
        }
    }
}

/// Read the next high-scoring answer from the XML.
/// Returns (question_title, question_body, answer_body, score).
fn read_next_answer(
    cp: &mut CommunityParser,
    min_score: i32,
) -> Result<Option<(String, String, String, i32)>> {
    loop {
        cp.buf.clear();
        let event = cp
            .reader
            .read_event_into(&mut cp.buf)
            .map_err(|e| Error::Storage(format!("XML parse error: {e}")))?;

        match event {
            Event::Empty(ref e) | Event::Start(ref e) if e.name().as_ref() == b"row" => {
                let attrs = parse_row_attrs(e)?;
                let post_type = attrs.get("PostTypeId").and_then(|s| s.parse::<u8>().ok());

                match post_type {
                    Some(1) => {
                        // Question: store for later lookup.
                        if let Some(id_str) = attrs.get("Id") {
                            if let Ok(id) = id_str.parse::<u64>() {
                                let title = attrs.get("Title").cloned().unwrap_or_default();
                                let body =
                                    attrs.get("Body").map(|b| strip_html(b)).unwrap_or_default();
                                cp.questions.insert(id, (title, body));
                            }
                        }
                    }
                    Some(2) => {
                        // Answer: check score and yield if above threshold.
                        let score = attrs
                            .get("Score")
                            .and_then(|s| s.parse::<i32>().ok())
                            .unwrap_or(0);
                        if score < min_score {
                            continue;
                        }
                        let parent_id = attrs.get("ParentId").and_then(|s| s.parse::<u64>().ok());
                        let (title, q_body) = parent_id
                            .and_then(|pid| cp.questions.get(&pid))
                            .cloned()
                            .unwrap_or_else(|| ("Unknown Question".to_string(), String::new()));
                        let a_body = attrs.get("Body").map(|b| strip_html(b)).unwrap_or_default();
                        return Ok(Some((title, q_body, a_body, score)));
                    }
                    _ => {}
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

fn parse_row_attrs(e: &quick_xml::events::BytesStart<'_>) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| Error::Storage(format!("XML attribute error: {e}")))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let val = attr
            .unescape_value()
            .map_err(|e| Error::Storage(format!("XML unescape error: {e}")))?
            .to_string();
        map.insert(key, val);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_test_xml(dir: &Path) -> PathBuf {
        let posts_path = dir.join("Posts.xml");
        let mut f = File::create(&posts_path).unwrap();
        write!(
            f,
            r#"<?xml version="1.0" encoding="utf-8"?>
<posts>
  <row Id="1" PostTypeId="1" Score="10" Title="How to sort a list?" Body="&lt;p&gt;How do I sort a list in Python?&lt;/p&gt;" />
  <row Id="2" PostTypeId="2" ParentId="1" Score="15" Body="&lt;p&gt;Use sorted() or list.sort().&lt;/p&gt;" />
  <row Id="3" PostTypeId="2" ParentId="1" Score="1" Body="&lt;p&gt;Just Google it.&lt;/p&gt;" />
  <row Id="4" PostTypeId="1" Score="5" Title="What is a monad?" Body="&lt;p&gt;Can someone explain monads?&lt;/p&gt;" />
  <row Id="5" PostTypeId="2" ParentId="4" Score="8" Body="&lt;p&gt;A monad is a monoid in the category of endofunctors.&lt;/p&gt;" />
</posts>"#
        )
        .unwrap();
        posts_path
    }

    #[test]
    fn parse_filters_low_score() {
        let dir = tempfile::tempdir().unwrap();
        let posts_path = make_test_xml(dir.path());

        let parser = StackExchangeParser::new("stackexchange", 3);
        let chunks: Vec<_> = parser
            .parse(&posts_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have 2 answers (score 15 and score 8), not the score 1 answer.
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].content.contains("sorted()"));
        assert!(chunks[1].content.contains("monad"));
        // Score 1 answer should be filtered.
        for c in &chunks {
            assert!(!c.content.contains("Just Google it"));
        }
    }

    #[test]
    fn parse_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_test_xml(&community_dir);

        let parser = StackExchangeParser::new("stackexchange", 3);
        let chunks: Vec<_> = parser
            .parse(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(chunks.len(), 2);
    }
}
