use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use sovereign_core::error::{Error, Result};
use sovereign_core::types::DocumentChunk;

use super::{chunk_and_wrap, CorpusParser};

pub struct OpenAlexParser {
    corpus_id: String,
}

impl OpenAlexParser {
    pub fn new(corpus_id: &str) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
        }
    }
}

impl CorpusParser for OpenAlexParser {
    fn parse(&self, source_path: &Path) -> Result<Box<dyn Iterator<Item = Result<DocumentChunk>>>> {
        let file = File::open(source_path).map_err(|e| {
            Error::Storage(format!("Failed to open {}: {e}", source_path.display()))
        })?;

        let reader: Box<dyn BufRead> =
            if source_path.extension().and_then(|e| e.to_str()) == Some("gz") {
                Box::new(BufReader::new(flate2::read::GzDecoder::new(file)))
            } else {
                Box::new(BufReader::new(file))
            };

        Ok(Box::new(OpenAlexIterator {
            lines: reader.lines(),
            corpus_id: self.corpus_id.clone(),
            pending: VecDeque::new(),
            chunk_counter: 0,
        }))
    }
}

struct OpenAlexIterator {
    lines: std::io::Lines<Box<dyn BufRead>>,
    corpus_id: String,
    pending: VecDeque<DocumentChunk>,
    chunk_counter: usize,
}

impl Iterator for OpenAlexIterator {
    type Item = Result<DocumentChunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.pending.pop_front() {
                return Some(Ok(chunk));
            }
            let line = match self.lines.next()? {
                Ok(l) => l,
                Err(e) => return Some(Err(Error::Storage(format!("Read error: {e}")))),
            };
            if line.trim().is_empty() {
                continue;
            }
            let work: OpenAlexWork = match serde_json::from_str(&line) {
                Ok(w) => w,
                Err(_) => continue, // skip malformed lines
            };
            if let Some(chunks) = format_work(&self.corpus_id, &work, &mut self.chunk_counter) {
                self.pending = chunks.into();
                continue;
            }
        }
    }
}

#[derive(Deserialize)]
struct OpenAlexWork {
    id: Option<String>,
    title: Option<String>,
    publication_year: Option<i32>,
    doi: Option<String>,
    cited_by_count: Option<i32>,
    abstract_inverted_index: Option<serde_json::Value>,
    authorships: Option<Vec<Authorship>>,
}

#[derive(Deserialize)]
struct Authorship {
    author: Option<AuthorInfo>,
}

#[derive(Deserialize)]
struct AuthorInfo {
    display_name: Option<String>,
}

fn format_work(
    corpus_id: &str,
    work: &OpenAlexWork,
    chunk_counter: &mut usize,
) -> Option<Vec<DocumentChunk>> {
    // Filter: year >= 2010 and has abstract.
    let year = work.publication_year?;
    if year < 2010 {
        return None;
    }
    let abstract_text = work
        .abstract_inverted_index
        .as_ref()
        .and_then(reconstruct_abstract)?;
    if abstract_text.is_empty() {
        return None;
    }

    let title = work.title.as_deref().unwrap_or("Untitled");
    let authors = work
        .authorships
        .as_ref()
        .map(|a| {
            a.iter()
                .filter_map(|a| a.author.as_ref()?.display_name.as_deref())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let doi = work.doi.as_deref().unwrap_or("");
    let cited_by = work.cited_by_count.unwrap_or(0);

    let mut content = format!("OpenAlex: {title}\n");
    if !authors.is_empty() {
        content.push_str(&format!("Authors: {authors}\n"));
    }
    content.push_str(&format!("Year: {year} | Cited by: {cited_by}"));
    if !doi.is_empty() {
        content.push_str(&format!(" | DOI: {doi}"));
    }
    content.push_str(&format!("\n\n{abstract_text}"));

    let source = work
        .id
        .as_deref()
        .unwrap_or(title)
        .replace("https://openalex.org/", "");
    Some(chunk_and_wrap(corpus_id, &source, &content, chunk_counter))
}

fn reconstruct_abstract(inverted_index: &serde_json::Value) -> Option<String> {
    let obj = inverted_index.as_object()?;
    let mut words: Vec<(usize, &str)> = Vec::new();
    for (word, positions) in obj {
        if let Some(arr) = positions.as_array() {
            for pos in arr {
                if let Some(idx) = pos.as_u64() {
                    words.push((idx as usize, word.as_str()));
                }
            }
        }
    }
    if words.is_empty() {
        return None;
    }
    words.sort_by_key(|(idx, _)| *idx);
    let text: String = words.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_jsonl_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("works.jsonl");
        let mut f = File::create(&file_path).unwrap();

        // Valid record with abstract.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W123","title":"Test Paper","publication_year":2020,"doi":"10.1234/test","cited_by_count":42,"abstract_inverted_index":{{"This":[0],"is":[1],"a":[2],"test":[3],"abstract.":[4]}},"authorships":[{{"author":{{"display_name":"Jane Doe"}}}}]}}"#
        )
        .unwrap();

        // Record too old — should be filtered.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W456","title":"Old Paper","publication_year":2005,"abstract_inverted_index":{{"old":[0]}},"authorships":[]}}"#
        )
        .unwrap();

        // Record without abstract — should be filtered.
        writeln!(
            f,
            r#"{{"id":"https://openalex.org/W789","title":"No Abstract","publication_year":2022,"authorships":[]}}"#
        )
        .unwrap();

        let parser = OpenAlexParser::new("openalex");
        let chunks: Vec<_> = parser
            .parse(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Test Paper"));
        assert!(chunks[0].content.contains("Jane Doe"));
        assert!(chunks[0].content.contains("This is a test abstract."));
        assert!(chunks[0].content.contains("2020"));
    }

    #[test]
    fn reconstruct_abstract_works() {
        let idx = serde_json::json!({
            "Machine": [0],
            "learning": [1],
            "is": [2],
            "great.": [3]
        });
        let text = reconstruct_abstract(&idx).unwrap();
        assert_eq!(text, "Machine learning is great.");
    }
}
