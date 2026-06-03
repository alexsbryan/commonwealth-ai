use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use crate::error::{Error, Result};
use super::{slug, strip_html, ExtractedDoc, Extractor};

// ═══════════════════════════════════════════════════════════════
// MediaWiki XML Extractor
// ═══════════════════════════════════════════════════════════════

/// Extracts articles from MediaWiki XML dump files (e.g., Wikipedia).
pub struct MediawikiExtractor {
    /// Namespace IDs to include (default: [0] for main namespace).
    pub namespace_filter: Vec<u32>,
    /// Whether to skip redirect pages.
    pub skip_redirects: bool,
    /// Decompression mode: Some("bzip2") or None for plain XML.
    pub decompress: Option<String>,
}

impl MediawikiExtractor {
    pub fn new() -> Self {
        Self {
            namespace_filter: vec![0],
            skip_redirects: true,
            decompress: None,
        }
    }

    pub fn with_decompress(mut self, decompress: Option<String>) -> Self {
        self.decompress = decompress;
        self
    }
}

impl Default for MediawikiExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor for MediawikiExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let file = File::open(source_path)
            .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", source_path.display())))?;

        let is_bz2 = self.decompress.as_deref() == Some("bzip2")
            || (source_path
                .extension()
                .and_then(|e| e.to_str()) == Some("bz2"));

        let reader: Box<dyn std::io::Read + Send> = if is_bz2 {
            Box::new(bzip2::read::BzDecoder::new(BufReader::new(file)))
        } else {
            Box::new(BufReader::new(file))
        };

        let xml_reader = XmlReader::from_reader(BufReader::new(reader));

        Ok(Box::new(WikiDumpIterator {
            reader: xml_reader,
            buf: Vec::new(),
            namespace_filter: self.namespace_filter.clone(),
            skip_redirects: self.skip_redirects,
            pending: VecDeque::new(),
            // Page state machine.
            in_page: false,
            current_tag: String::new(),
            current_title: String::new(),
            current_ns: String::new(),
            current_text: String::new(),
        }))
    }
}

struct WikiDumpIterator {
    reader: XmlReader<BufReader<Box<dyn std::io::Read + Send>>>,
    buf: Vec<u8>,
    namespace_filter: Vec<u32>,
    skip_redirects: bool,
    pending: VecDeque<ExtractedDoc>,
    // State machine for tracking position within <page> elements.
    in_page: bool,
    current_tag: String,
    current_title: String,
    current_ns: String,
    current_text: String,
}

impl Iterator for WikiDumpIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }
            match self.read_next_page() {
                Ok(true) => continue,
                Ok(false) => return None, // EOF
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl WikiDumpIterator {
    /// Read XML events until a complete page is processed or EOF.
    fn read_next_page(&mut self) -> Result<bool> {
        loop {
            self.buf.clear();
            let event = self
                .reader
                .read_event_into(&mut self.buf)
                .map_err(|e| Error::Extraction(format!("XML parse error: {e}")))?;

            match event {
                Event::Start(ref e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if name == "page" {
                        self.in_page = true;
                        self.current_title.clear();
                        self.current_ns.clear();
                        self.current_text.clear();
                    }
                    if self.in_page {
                        self.current_tag = name;
                    }
                }
                Event::Text(ref e) if self.in_page => {
                    let text = e
                        .unescape()
                        .map_err(|e| Error::Extraction(format!("XML unescape error: {e}")))?;
                    match self.current_tag.as_str() {
                        "title" => self.current_title.push_str(&text),
                        "ns" => self.current_ns.push_str(&text),
                        "text" => self.current_text.push_str(&text),
                        _ => {}
                    }
                }
                Event::End(ref e) => {
                    let name = e.name();
                    if name.as_ref() == b"page" && self.in_page {
                        self.in_page = false;
                        self.process_page();
                        return Ok(true);
                    }
                    if self.in_page {
                        self.current_tag.clear();
                    }
                }
                Event::Eof => return Ok(false),
                _ => {}
            }
        }
    }

    fn process_page(&mut self) {
        // Check namespace filter.
        let ns: u32 = self.current_ns.trim().parse().unwrap_or(u32::MAX);
        if !self.namespace_filter.contains(&ns) {
            return;
        }
        // Skip redirects.
        if self.skip_redirects {
            let trimmed = self.current_text.trim_start();
            if trimmed.starts_with("#REDIRECT") || trimmed.starts_with("#redirect") {
                return;
            }
        }

        let title = self.current_title.trim().to_string();
        if title.is_empty() {
            return;
        }

        let cleaned = strip_mediawiki(&self.current_text);
        if cleaned.trim().is_empty() {
            return;
        }

        // Split by sections and emit a document for each section.
        let sections = split_sections(&cleaned);
        for (section_name, section_text) in sections {
            let text = section_text.trim();
            if text.is_empty() {
                continue;
            }

            let doc_title = if section_name.is_empty() {
                title.clone()
            } else {
                format!("{title} > {section_name}")
            };

            self.pending.push_back(ExtractedDoc {
                title: Some(doc_title),
                content: text.to_string(),
                url: None,
                source_id: slug(&title),
                metadata: None,
                source_file: None,
                embed_text: None,
            });
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// StackExchange XML Extractor
// ═══════════════════════════════════════════════════════════════

use crate::recipe::SeMode;

/// Extracts Q&A content from StackExchange XML data dumps.
///
/// See [`crate::recipe::ExtractorConfig::StackExchangeXml`] for the
/// full contract — in short, two extraction shapes:
///
/// - [`SeMode::AnswerOnly`] (legacy): one doc per high-score answer
///   with the question inlined. Streams lazily — each answer is
///   emitted as soon as its row is parsed.
/// - [`SeMode::QuestionWithAnswers`]: one doc per question, grouping
///   the top `max_answers_per_question` answers as numbered
///   "Approaches". Buffers questions and answers in memory through
///   one full pass over the XML, then emits grouped docs at EOF. The
///   per-question doc carries an `embed_text` breadth summary
///   (question title + first sentence of each answer) so the vector
///   embedding captures the trade-off space without overflowing the
///   embedding model's context window. Pair with the `passthrough`
///   chunker.
pub struct StackExchangeExtractor {
    /// Minimum answer score to include (applies in both modes).
    pub min_score: i32,
    /// Extraction shape — see [`SeMode`].
    pub mode: SeMode,
    /// Maximum answers grouped per question in [`SeMode::QuestionWithAnswers`].
    /// Ignored in [`SeMode::AnswerOnly`].
    pub max_answers_per_question: usize,
    /// Reject answers shorter than this many characters.
    pub min_answer_length: usize,
    /// Skip questions whose `ClosedDate` is non-empty (duplicates,
    /// off-topic, opinion-based per SO moderation).
    pub exclude_closed: bool,
    /// Optional restriction to questions tagged with at least one
    /// listed tag. Tags are matched case-insensitively.
    pub tag_filter: Option<Vec<String>>,
}

impl StackExchangeExtractor {
    pub fn new(min_score: i32) -> Self {
        Self {
            min_score,
            mode: SeMode::AnswerOnly,
            max_answers_per_question: 5,
            min_answer_length: 0,
            exclude_closed: true,
            tag_filter: None,
        }
    }
}

impl Default for StackExchangeExtractor {
    fn default() -> Self {
        Self::new(3)
    }
}

impl Extractor for StackExchangeExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = find_posts_files(source_path)?;
        match self.mode {
            SeMode::AnswerOnly => Ok(Box::new(StackExchangeIterator {
                files: files.into(),
                min_score: self.min_score,
                current: None,
                pending: VecDeque::new(),
            })),
            SeMode::QuestionWithAnswers => Ok(Box::new(QuestionWithAnswersIterator::new(
                files,
                self.min_score,
                self.max_answers_per_question.max(1),
                self.min_answer_length,
                self.exclude_closed,
                self.tag_filter.clone(),
            ))),
        }
    }
}

fn find_posts_files(path: &Path) -> Result<Vec<PathBuf>> {
    if path.is_file() {
        // Single-file mode. .7z files (Internet Archive SE dumps) get
        // extracted into a sibling dir on first encounter; other
        // single-file inputs are passed through (e.g. an already-
        // extracted Posts.xml).
        if is_seven_zip(path) {
            let extract_dir = ensure_seven_zip_extracted(path)?;
            return find_posts_files(&extract_dir);
        }
        return Ok(vec![path.to_path_buf()]);
    }

    // Directory mode. Auto-extract any .7z entries at the top level
    // before scanning, then walk the directory for community subdirs
    // each containing a `Posts.xml`.
    let entries = std::fs::read_dir(path)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;
    let mut top_entries = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Extraction(format!("Directory entry error: {e}")))?;
        top_entries.push(entry.path());
    }
    for p in &top_entries {
        if p.is_file() && is_seven_zip(p) {
            ensure_seven_zip_extracted(p)?;
        }
    }

    // Re-scan after extraction in case new dirs were created.
    let mut files = Vec::new();
    let entries = std::fs::read_dir(path)
        .map_err(|e| Error::Extraction(format!("Failed to read {}: {e}", path.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Extraction(format!("Directory entry error: {e}")))?;
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

/// True iff `path` looks like a 7z archive by extension. The
/// extension check is sufficient — we only invoke
/// `ensure_seven_zip_extracted` after seeing it, which validates
/// magic bytes by attempting decompression.
fn is_seven_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("7z"))
        .unwrap_or(false)
}

/// Extract `archive_path` into a sibling directory named after the
/// archive's stem, returning that directory.  Idempotent: if the
/// target dir already exists with a `Posts.xml` inside, extraction is
/// skipped.  Also creates a sentinel (`.extracted`) at the end of
/// the run so a future check has a fast positive answer even if the
/// archive layout doesn't put Posts.xml at the root.
fn ensure_seven_zip_extracted(archive_path: &Path) -> Result<PathBuf> {
    let stem = archive_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            Error::Extraction(format!(
                "7z archive path has no usable stem: {}",
                archive_path.display()
            ))
        })?;
    let parent = archive_path.parent().ok_or_else(|| {
        Error::Extraction(format!(
            "7z archive path has no parent: {}",
            archive_path.display()
        ))
    })?;
    let extract_dir = parent.join(stem);
    let sentinel = extract_dir.join(".extracted");

    if extract_dir.is_dir() && (sentinel.is_file() || extract_dir.join("Posts.xml").is_file()) {
        return Ok(extract_dir);
    }

    std::fs::create_dir_all(&extract_dir).map_err(|e| {
        Error::Extraction(format!(
            "create 7z extract dir {}: {e}",
            extract_dir.display()
        ))
    })?;
    tracing::info!(
        archive = %archive_path.display(),
        dest = %extract_dir.display(),
        "stackexchange_xml: decompressing 7z archive (one-time per install)"
    );
    sevenz_rust2::decompress_file(archive_path, &extract_dir).map_err(|e| {
        Error::Extraction(format!(
            "decompress 7z {} → {}: {e}",
            archive_path.display(),
            extract_dir.display()
        ))
    })?;
    let _ = std::fs::write(&sentinel, b"sevenz_rust2 ok\n");
    Ok(extract_dir)
}

struct StackExchangeIterator {
    files: VecDeque<PathBuf>,
    min_score: i32,
    current: Option<CommunityParser>,
    pending: VecDeque<ExtractedDoc>,
}

struct CommunityParser {
    reader: XmlReader<BufReader<File>>,
    buf: Vec<u8>,
    community: String,
    /// Maps question ID -> (title, body_text).
    questions: HashMap<u64, (String, String)>,
}

impl Iterator for StackExchangeIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }

            if let Some(ref mut cp) = self.current {
                match read_next_answer(cp, self.min_score) {
                    Ok(Some((title, question_body, answer_body, score, community))) => {
                        let content = format!(
                            "Q: {question_body}\n\nA (score {score}): {answer_body}",
                        );
                        let source_id = slug(&format!("{community}-{title}"));
                        self.pending.push_back(ExtractedDoc {
                            title: Some(title),
                            content,
                            url: None,
                            source_id,
                            metadata: Some(serde_json::json!({
                                "community": community,
                                "score": score,
                            })),
                            source_file: None,
                            embed_text: None,
                        });
                        continue;
                    }
                    Ok(None) => {
                        self.current = None;
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

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
                    return Some(Err(Error::Extraction(format!(
                        "Failed to open {}: {e}",
                        path.display()
                    ))));
                }
            }
        }
    }
}

/// Read the next high-scoring answer from the XML.
/// Returns (question_title, question_body, answer_body, score, community).
fn read_next_answer(
    cp: &mut CommunityParser,
    min_score: i32,
) -> Result<Option<(String, String, String, i32, String)>> {
    loop {
        cp.buf.clear();
        let event = cp
            .reader
            .read_event_into(&mut cp.buf)
            .map_err(|e| Error::Extraction(format!("XML parse error: {e}")))?;

        match event {
            Event::Empty(ref e) | Event::Start(ref e) if e.name().as_ref() == b"row" => {
                let attrs = parse_row_attrs(e)?;
                let post_type = attrs.get("PostTypeId").and_then(|s| s.parse::<u8>().ok());

                match post_type {
                    Some(1) => {
                        // Question: store for later lookup.
                        if let Some(id_str) = attrs.get("Id") {
                            if let Ok(id) = id_str.parse::<u64>() {
                                let title = attrs
                                    .get("Title")
                                    .cloned()
                                    .unwrap_or_default();
                                let body = attrs
                                    .get("Body")
                                    .map(|b| strip_html(b))
                                    .unwrap_or_default();
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
                        let parent_id = attrs
                            .get("ParentId")
                            .and_then(|s| s.parse::<u64>().ok());
                        let (title, q_body) = parent_id
                            .and_then(|pid| cp.questions.get(&pid))
                            .cloned()
                            .unwrap_or_else(|| ("Unknown Question".to_string(), String::new()));
                        let a_body = attrs
                            .get("Body")
                            .map(|b| strip_html(b))
                            .unwrap_or_default();
                        return Ok(Some((title, q_body, a_body, score, cp.community.clone())));
                    }
                    _ => {}
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

// ───────────────────────────────────────────────────────────────
// QuestionWithAnswers iterator
// ───────────────────────────────────────────────────────────────

/// Buffered question metadata captured during the XML scan.
struct QMeta {
    title: String,
    body: String,
    tags: Vec<String>,
    closed: bool,
    accepted_answer_id: Option<u64>,
    community: String,
}

/// Buffered answer metadata captured during the XML scan.
struct AMeta {
    body: String,
    score: i32,
    is_accepted: bool,
}

/// One pass over Posts.xml: collects every question and every
/// score-passing answer in memory, then emits one grouped doc per
/// surviving question at EOF. Memory cost is roughly proportional to
/// `(num_questions + num_passing_answers) * mean_post_size_bytes`,
/// which is manageable for the smaller SE sites and the multi-answer
/// SO subset that knowledge-density filtering targets. AnswerOnly
/// stays the right shape for the full SO breadth corpus.
struct QuestionWithAnswersIterator {
    files: VecDeque<PathBuf>,
    min_score: i32,
    max_answers_per_question: usize,
    min_answer_length: usize,
    exclude_closed: bool,
    tag_filter_lower: Option<Vec<String>>,
    /// Holds emit-ready docs once the current file's grouping pass has
    /// been done. Drained one-at-a-time so the ingest loop can yield
    /// to inference between docs.
    pending: VecDeque<ExtractedDoc>,
    /// Set once a file completes its grouping pass; advances to the
    /// next Posts.xml when the pending queue empties.
    file_done: bool,
}

impl QuestionWithAnswersIterator {
    fn new(
        files: Vec<PathBuf>,
        min_score: i32,
        max_answers_per_question: usize,
        min_answer_length: usize,
        exclude_closed: bool,
        tag_filter: Option<Vec<String>>,
    ) -> Self {
        let tag_filter_lower = tag_filter.map(|tags| {
            tags.into_iter()
                .map(|t| t.trim().to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        });
        Self {
            files: files.into(),
            min_score,
            max_answers_per_question,
            min_answer_length,
            exclude_closed,
            tag_filter_lower,
            pending: VecDeque::new(),
            file_done: true,
        }
    }

    fn process_next_file(&mut self) -> Result<bool> {
        let Some(path) = self.files.pop_front() else {
            return Ok(false);
        };
        let community = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let file = File::open(&path)
            .map_err(|e| Error::Extraction(format!("Failed to open {}: {e}", path.display())))?;
        let mut reader = XmlReader::from_reader(BufReader::new(file));
        let mut buf = Vec::new();

        let mut questions: HashMap<u64, QMeta> = HashMap::new();
        let mut answers: HashMap<u64, Vec<AMeta>> = HashMap::new();

        loop {
            buf.clear();
            let event = reader
                .read_event_into(&mut buf)
                .map_err(|e| Error::Extraction(format!("XML parse error: {e}")))?;
            match event {
                Event::Empty(ref e) | Event::Start(ref e) if e.name().as_ref() == b"row" => {
                    let attrs = parse_row_attrs(e)?;
                    let post_type = attrs.get("PostTypeId").and_then(|s| s.parse::<u8>().ok());
                    match post_type {
                        Some(1) => {
                            let Some(id) = attrs.get("Id").and_then(|s| s.parse::<u64>().ok())
                            else {
                                continue;
                            };
                            let title = attrs.get("Title").cloned().unwrap_or_default();
                            let body = attrs
                                .get("Body")
                                .map(|b| strip_html(b))
                                .unwrap_or_default();
                            let tags = parse_tags(attrs.get("Tags").map(|s| s.as_str()));
                            let closed = attrs
                                .get("ClosedDate")
                                .map(|s| !s.trim().is_empty())
                                .unwrap_or(false);
                            let accepted_answer_id = attrs
                                .get("AcceptedAnswerId")
                                .and_then(|s| s.parse::<u64>().ok());
                            questions.insert(
                                id,
                                QMeta {
                                    title,
                                    body,
                                    tags,
                                    closed,
                                    accepted_answer_id,
                                    community: community.clone(),
                                },
                            );
                        }
                        Some(2) => {
                            let score = attrs
                                .get("Score")
                                .and_then(|s| s.parse::<i32>().ok())
                                .unwrap_or(0);
                            if score < self.min_score {
                                continue;
                            }
                            let Some(parent_id) =
                                attrs.get("ParentId").and_then(|s| s.parse::<u64>().ok())
                            else {
                                continue;
                            };
                            let body = attrs
                                .get("Body")
                                .map(|b| strip_html(b))
                                .unwrap_or_default();
                            if body.chars().count() < self.min_answer_length {
                                continue;
                            }
                            let answer_id =
                                attrs.get("Id").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                            let is_accepted = questions
                                .get(&parent_id)
                                .and_then(|q| q.accepted_answer_id)
                                .map(|aid| aid == answer_id)
                                .unwrap_or(false);
                            answers.entry(parent_id).or_default().push(AMeta {
                                body,
                                score,
                                is_accepted,
                            });
                        }
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }

        for (qid, q) in questions {
            if self.exclude_closed && q.closed {
                continue;
            }
            if !self.tag_matches(&q.tags) {
                continue;
            }
            let Some(mut q_answers) = answers.remove(&qid) else {
                continue;
            };
            // Sort: accepted first, then score desc.
            q_answers.sort_by(|a, b| {
                b.is_accepted
                    .cmp(&a.is_accepted)
                    .then_with(|| b.score.cmp(&a.score))
            });
            q_answers.truncate(self.max_answers_per_question);
            if q_answers.is_empty() {
                continue;
            }
            let doc = build_grouped_doc(qid, &q, &q_answers);
            self.pending.push_back(doc);
        }
        Ok(true)
    }

    fn tag_matches(&self, tags: &[String]) -> bool {
        let Some(ref filter) = self.tag_filter_lower else {
            return true;
        };
        tags.iter()
            .any(|t| filter.iter().any(|f| f == &t.to_ascii_lowercase()))
    }
}

impl Iterator for QuestionWithAnswersIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }
            if !self.file_done && self.pending.is_empty() {
                self.file_done = true;
            }
            match self.process_next_file() {
                Ok(true) => {
                    self.file_done = false;
                    continue;
                }
                Ok(false) => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Parse a `<...><...>` tag string into a Vec of trimmed tag names.
/// Stack Exchange writes question tags as e.g.
/// `&lt;rust&gt;&lt;memory-management&gt;`.  After XML unescape that's
/// `<rust><memory-management>`.
fn parse_tags(raw: Option<&str>) -> Vec<String> {
    let Some(s) = raw else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for ch in s.chars() {
        match ch {
            '<' => {
                inside = true;
                current.clear();
            }
            '>' if inside => {
                inside = false;
                let t = current.trim().to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
            _ if inside => current.push(ch),
            _ => {}
        }
    }
    out
}

/// Build the grouped `ExtractedDoc` for a single question.
///
/// `content` is the structured "Question / Approach 1 / Approach 2"
/// body that goes into FTS (so keyword searches hit any term in any
/// answer). `embed_text` is the synthesized breadth summary
/// (question title + first sentence of each answer) — the vector
/// embedding text. Capping `embed_text` to ~200 tokens keeps the
/// embed model's context window honest.
///
/// `metadata` carries knowledge-density signals so the
/// `KnowledgeDensity` document filter can act on them post-extraction.
fn build_grouped_doc(qid: u64, q: &QMeta, answers: &[AMeta]) -> ExtractedDoc {
    let mut content = String::with_capacity(q.body.len() + answers.iter().map(|a| a.body.len()).sum::<usize>() + 256);
    content.push_str("Question: ");
    content.push_str(q.title.trim());
    content.push_str("\n\n");
    if !q.body.trim().is_empty() {
        content.push_str(q.body.trim());
        content.push_str("\n\n");
    }
    for (i, a) in answers.iter().enumerate() {
        let label = if a.is_accepted {
            format!("Approach {} (score: {}, accepted):", i + 1, a.score)
        } else {
            format!("Approach {} (score: {}):", i + 1, a.score)
        };
        content.push_str(&label);
        content.push('\n');
        content.push_str(a.body.trim());
        content.push_str("\n\n");
    }

    let mut embed = String::with_capacity(256);
    embed.push_str(q.title.trim());
    if !embed.ends_with('.') && !embed.ends_with('?') && !embed.is_empty() {
        embed.push('.');
    }
    for a in answers {
        let sentence = first_sentence_skipping_code(&a.body);
        if sentence.is_empty() {
            continue;
        }
        embed.push(' ');
        embed.push_str(&sentence);
        if !embed.ends_with('.') && !embed.ends_with('?') {
            embed.push('.');
        }
        // Cap to ~1000 chars so the embed model's 512-token context
        // window stays comfortable across tokenizer differences.
        if embed.len() >= 1000 {
            break;
        }
    }

    let metadata = serde_json::json!({
        "community": q.community,
        "question_id": qid,
        "tags": q.tags,
        "closed": q.closed,
        "answer_count": answers.len(),
        "max_answer_score": answers.iter().map(|a| a.score).max().unwrap_or(0),
        "min_answer_score": answers.iter().map(|a| a.score).min().unwrap_or(0),
        "min_answer_length": answers
            .iter()
            .map(|a| a.body.chars().count() as u64)
            .min()
            .unwrap_or(0),
        "se_mode": "question_with_answers",
    });

    ExtractedDoc {
        title: Some(q.title.clone()),
        content,
        url: None,
        source_id: slug(&format!("{}-{}", q.community, qid)),
        metadata: Some(metadata),
        source_file: None,
        embed_text: Some(embed),
    }
}

/// Take the first sentence of an answer body, skipping any leading
/// fenced code blocks. Stack Overflow culture front-loads the
/// "approach" claim before the code, so this picks up exactly the
/// summary text we want for the breadth-summary embedding.
fn first_sentence_skipping_code(body: &str) -> String {
    let mut text = body.trim();
    while text.starts_with("```") {
        if let Some(end) = text[3..].find("```") {
            text = text[3 + end + 3..].trim_start();
        } else {
            break;
        }
    }
    let mut out = String::new();
    for ch in text.chars() {
        if ch == '\n' && !out.is_empty() {
            break;
        }
        out.push(ch);
        if matches!(ch, '.' | '?' | '!') && out.len() > 30 {
            break;
        }
    }
    out.trim().to_string()
}

/// Parse attributes from a <row> element into a HashMap.
fn parse_row_attrs(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for attr in e.attributes() {
        let attr =
            attr.map_err(|e| Error::Extraction(format!("XML attribute error: {e}")))?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let val = attr
            .unescape_value()
            .map_err(|e| Error::Extraction(format!("XML unescape error: {e}")))?
            .to_string();
        map.insert(key, val);
    }
    Ok(map)
}

// ─── MediaWiki Markup Stripping ───────────────────────────────

/// Strip MediaWiki markup, producing plain text.
pub(crate) fn strip_mediawiki(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Templates: {{...}} -- remove entirely (may be nested).
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                skip_nested(&mut chars, '{', '}');
            }
            // Tables: {|...|} -- remove entirely.
            '{' if chars.peek() == Some(&'|') => {
                chars.next();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '{' && chars.peek() == Some(&'|') {
                        chars.next();
                        depth += 1;
                    } else if c == '|' && chars.peek() == Some(&'}') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
            }
            // Wikilinks: [[target|display]] -> display, or [[target]] -> target.
            '[' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut link_text = String::new();
                let mut depth = 1;
                while let Some(c) = chars.next() {
                    if c == '[' && chars.peek() == Some(&'[') {
                        chars.next();
                        depth += 1;
                        link_text.push_str("[[");
                    } else if c == ']' && chars.peek() == Some(&']') {
                        chars.next();
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        link_text.push_str("]]");
                    } else {
                        link_text.push(c);
                    }
                }
                // Use display text (after |), or the full link text.
                let display = link_text
                    .rsplit_once('|')
                    .map(|(_, d)| d)
                    .unwrap_or(&link_text);
                // Skip file/image links.
                if !link_text.starts_with("File:")
                    && !link_text.starts_with("Image:")
                    && !link_text.starts_with("Category:")
                {
                    result.push_str(display);
                }
            }
            // External links: [url text] -> text.
            '[' => {
                let mut link = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    link.push(c);
                }
                // Display text is everything after the first space.
                if let Some(pos) = link.find(' ') {
                    result.push_str(&link[pos + 1..]);
                }
            }
            // Bold/italic: '''text''' or ''text''.
            '\'' if chars.peek() == Some(&'\'') => {
                while chars.peek() == Some(&'\'') {
                    chars.next();
                }
            }
            // HTML-like tags: <ref>...</ref>, <nowiki>, etc.
            '<' => {
                let mut tag = String::new();
                let mut is_closing = false;
                for c in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                    tag.push(c);
                }
                if tag.starts_with('/') {
                    is_closing = true;
                    tag = tag[1..].to_string();
                }
                let tag_name = tag
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                // For ref, nowiki, gallery, etc. -- skip content until closing tag.
                if !is_closing
                    && !tag.ends_with('/')
                    && matches!(
                        tag_name.as_str(),
                        "ref" | "nowiki" | "gallery" | "math" | "source" | "syntaxhighlight" | "code"
                    )
                {
                    let close = format!("</{tag_name}>");
                    let mut buf = String::new();
                    for c in chars.by_ref() {
                        buf.push(c);
                        if buf.ends_with(&close) {
                            break;
                        }
                    }
                }
            }
            // Section headers: == Title == -> preserved as text.
            '=' if result.ends_with('\n') || result.is_empty() => {
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    result
}

/// Skip nested pairs, e.g., {{ ... {{ ... }} ... }}.
fn skip_nested(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, open: char, close: char) {
    let mut depth = 1;
    while let Some(c) = chars.next() {
        if c == open && chars.peek() == Some(&open) {
            chars.next();
            depth += 1;
        } else if c == close && chars.peek() == Some(&close) {
            chars.next();
            depth -= 1;
            if depth == 0 {
                return;
            }
        }
    }
}

/// Split article text by section headers (== ... ==).
/// Returns Vec of (section_name, section_text).
/// The lead section has an empty name.
pub(crate) fn split_sections(text: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_name = String::new();
    let mut current_text = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("==") && trimmed.ends_with("==") {
            // Save previous section.
            if !current_text.is_empty() || sections.is_empty() {
                sections.push((current_name, current_text));
            }
            current_name = trimmed.trim_matches('=').trim().to_string();
            current_text = String::new();
        } else {
            current_text.push_str(line);
            current_text.push('\n');
        }
    }
    // Save final section.
    if !current_text.is_empty() {
        sections.push((current_name, current_text));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ─── MediaWiki Tests ──────────────────────────────────────

    fn make_test_xml() -> String {
        r#"<mediawiki>
  <page>
    <title>Rust (programming language)</title>
    <ns>0</ns>
    <revision>
      <text>'''Rust''' is a [[programming language]] focused on safety and performance.

== History ==

Rust was originally designed by Graydon Hoare at [[Mozilla]].

== Features ==

Rust has a strong [[type system]] and [[ownership (computer science)|ownership]] model.

{{Infobox programming language
|name = Rust
}}
</text>
    </revision>
  </page>
  <page>
    <title>Talk:Rust</title>
    <ns>1</ns>
    <revision>
      <text>This is a talk page and should be skipped.</text>
    </revision>
  </page>
  <page>
    <title>Redirect Page</title>
    <ns>0</ns>
    <revision>
      <text>#REDIRECT [[Rust (programming language)]]</text>
    </revision>
  </page>
  <page>
    <title>Python (programming language)</title>
    <ns>0</ns>
    <revision>
      <text>'''Python''' is a high-level [[programming language]].

== Design philosophy ==

Python emphasizes code readability.
</text>
    </revision>
  </page>
</mediawiki>"#
            .to_string()
    }

    #[test]
    fn parse_xml_dump() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dump.xml");
        let mut f = File::create(&file_path).unwrap();
        f.write_all(make_test_xml().as_bytes()).unwrap();

        let extractor = MediawikiExtractor::new();
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have docs for Rust and Python, not Talk or Redirect.
        assert!(docs.len() >= 2);

        let all_titles: Vec<_> = docs
            .iter()
            .filter_map(|d| d.title.as_ref())
            .collect();
        let all_content: String = docs.iter().map(|d| d.content.clone()).collect();

        assert!(all_titles.iter().any(|t| t.contains("Rust")));
        assert!(all_titles.iter().any(|t| t.contains("Python")));
        assert!(!all_content.contains("Talk:Rust"));
        assert!(!all_content.contains("#REDIRECT"));
    }

    #[test]
    fn parse_bz2_dump() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dump.xml.bz2");
        let f = File::create(&file_path).unwrap();
        let mut encoder = bzip2::write::BzEncoder::new(f, bzip2::Compression::fast());
        encoder.write_all(make_test_xml().as_bytes()).unwrap();
        encoder.finish().unwrap();

        let extractor = MediawikiExtractor::new().with_decompress(Some("bzip2".to_string()));
        let docs: Vec<_> = extractor
            .extract(&file_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(docs.len() >= 2);
        let all_titles: Vec<_> = docs.iter().filter_map(|d| d.title.as_ref()).collect();
        assert!(all_titles.iter().any(|t| t.contains("Rust")));
    }

    #[test]
    fn strip_mediawiki_templates() {
        let text = "Before {{Infobox|name=Test}} after.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Before"));
        assert!(result.contains("after."));
        assert!(!result.contains("Infobox"));
    }

    #[test]
    fn strip_mediawiki_wikilinks() {
        let text = "A [[programming language]] and [[Rust (lang)|Rust]].";
        let result = strip_mediawiki(text);
        assert!(result.contains("programming language"));
        assert!(result.contains("Rust"));
        assert!(!result.contains("[["));
    }

    #[test]
    fn strip_mediawiki_bold_italic() {
        let text = "'''Bold''' and ''italic'' text.";
        let result = strip_mediawiki(text);
        assert!(result.contains("Bold"));
        assert!(result.contains("italic"));
        assert!(!result.contains("'''"));
    }

    #[test]
    fn split_sections_works() {
        let text = "Lead text.\n== History ==\nHistory text.\n== Features ==\nFeature text.\n";
        let sections = split_sections(text);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].0, "");
        assert!(sections[0].1.contains("Lead text"));
        assert_eq!(sections[1].0, "History");
        assert_eq!(sections[2].0, "Features");
    }

    // ─── StackExchange Tests ──────────────────────────────────

    fn make_se_test_xml(dir: &Path) -> PathBuf {
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
    fn se_parse_filters_low_score() {
        let dir = tempfile::tempdir().unwrap();
        let posts_path = make_se_test_xml(dir.path());

        let extractor = StackExchangeExtractor::new(3);
        let docs: Vec<_> = extractor
            .extract(&posts_path)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Should have 2 answers (score 15 and score 8), not the score 1 answer.
        assert_eq!(docs.len(), 2);
        assert!(docs[0].content.contains("sorted()"));
        assert!(docs[1].content.contains("monad"));
        for d in &docs {
            assert!(!d.content.contains("Just Google it"));
        }
    }

    #[test]
    fn se_parse_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_test_xml(&community_dir);

        let extractor = StackExchangeExtractor::new(3);
        let docs: Vec<_> = extractor
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(docs.len(), 2);
    }

    // ─── QuestionWithAnswers mode ─────────────────────────────────

    /// Multi-answer thread fixture: question 100 has three substantive
    /// answers (one accepted), question 200 has just one answer, and
    /// question 300 is closed. Tags are SE-style angle-wrapped.
    fn make_se_grouped_xml(dir: &Path) -> PathBuf {
        let posts_path = dir.join("Posts.xml");
        let mut f = File::create(&posts_path).unwrap();
        write!(
            f,
            r#"<?xml version="1.0" encoding="utf-8"?>
<posts>
  <row Id="100" PostTypeId="1" Score="40" Title="Best database for real-time leaderboard systems"
       Body="&lt;p&gt;Need to support millions of users.&lt;/p&gt;"
       AcceptedAnswerId="102"
       Tags="&lt;architecture&gt;&lt;database&gt;" />
  <row Id="101" PostTypeId="2" ParentId="100" Score="31"
       Body="&lt;p&gt;PostgreSQL with materialized views handles complex leaderboard queries well when you need joins. Use indexed views to keep computation cheap.&lt;/p&gt;" />
  <row Id="102" PostTypeId="2" ParentId="100" Score="47"
       Body="&lt;p&gt;Redis sorted sets give you O(log N) operations for ranking with ZADD and ZRANGE. Single-digit-millisecond latency at scale.&lt;/p&gt;" />
  <row Id="103" PostTypeId="2" ParentId="100" Score="18"
       Body="&lt;p&gt;DynamoDB offers managed scalability with consistent single-digit-millisecond latency for ranked reads.&lt;/p&gt;" />
  <row Id="104" PostTypeId="2" ParentId="100" Score="2"
       Body="&lt;p&gt;Just use SQLite.&lt;/p&gt;" />
  <row Id="200" PostTypeId="1" Score="5" Title="How to parse JSON in Python"
       Body="&lt;p&gt;Standard library?&lt;/p&gt;"
       Tags="&lt;python&gt;&lt;json&gt;" />
  <row Id="201" PostTypeId="2" ParentId="200" Score="50"
       Body="&lt;p&gt;Use json.loads() — it's in the stdlib and parses any valid JSON document into Python objects directly.&lt;/p&gt;" />
  <row Id="300" PostTypeId="1" Score="3" Title="Opinion-based question"
       Body="&lt;p&gt;What's the best language?&lt;/p&gt;"
       ClosedDate="2024-01-01T00:00:00.000"
       Tags="&lt;subjective&gt;" />
  <row Id="301" PostTypeId="2" ParentId="300" Score="10"
       Body="&lt;p&gt;Whichever you know best for the problem you have today.&lt;/p&gt;" />
  <row Id="302" PostTypeId="2" ParentId="300" Score="8"
       Body="&lt;p&gt;Depends on the platform and team experience entirely.&lt;/p&gt;" />
</posts>"#
        )
        .unwrap();
        posts_path
    }

    fn grouped_extractor() -> StackExchangeExtractor {
        StackExchangeExtractor {
            min_score: 3,
            mode: SeMode::QuestionWithAnswers,
            max_answers_per_question: 5,
            min_answer_length: 0,
            exclude_closed: true,
            tag_filter: None,
        }
    }

    #[test]
    fn se_grouped_emits_one_doc_per_question_with_answers() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let docs: Vec<_> = grouped_extractor()
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Question 100 (multi-answer, open) and 200 (single-answer,
        // open) survive; 300 is closed and gets dropped.  Question 100
        // is the multi-answer thread; 200 has a single answer above
        // min_score.
        let titles: Vec<_> = docs.iter().filter_map(|d| d.title.as_deref()).collect();
        assert!(titles.iter().any(|t| t.contains("leaderboard")));
        assert!(titles.iter().any(|t| t.contains("JSON")));
        assert!(!titles.iter().any(|t| t.contains("Opinion-based")));
    }

    #[test]
    fn se_grouped_orders_accepted_first_then_score() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let docs: Vec<_> = grouped_extractor()
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        let leaderboard = docs
            .iter()
            .find(|d| d.title.as_deref().unwrap_or("").contains("leaderboard"))
            .expect("leaderboard doc present");
        let body = &leaderboard.content;

        // Accepted answer (Redis, score 47, AcceptedAnswerId=102)
        // appears before the higher-text-position PostgreSQL answer
        // (score 31), and both appear before DynamoDB (score 18).
        let pg = body.find("PostgreSQL").expect("postgres present");
        let redis = body.find("Redis").expect("redis present");
        let dynamo = body.find("DynamoDB").expect("dynamo present");
        assert!(redis < pg, "accepted answer should be Approach 1");
        assert!(pg < dynamo, "answers should be sorted by score desc after accepted");

        // Score-2 answer is below min_score=3 and must not appear.
        assert!(!body.contains("Just use SQLite"));

        // The accepted-flag label should mark exactly one answer.
        let accepted_count = body.matches(", accepted)").count();
        assert_eq!(accepted_count, 1);
    }

    #[test]
    fn se_grouped_embed_text_is_breadth_summary() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let docs: Vec<_> = grouped_extractor()
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let leaderboard = docs
            .iter()
            .find(|d| d.title.as_deref().unwrap_or("").contains("leaderboard"))
            .expect("leaderboard doc present");

        let embed = leaderboard.embed_text.as_deref().expect("embed_text set");
        // Title appears.
        assert!(embed.contains("leaderboard"));
        // First sentence of each surviving answer appears (Redis,
        // PostgreSQL, DynamoDB).
        assert!(embed.contains("Redis"));
        assert!(embed.contains("PostgreSQL"));
        assert!(embed.contains("DynamoDB"));
        // Excluded score-2 answer does NOT.
        assert!(!embed.contains("SQLite"));
        // Stays compact — well under the 1000-char cap.
        assert!(embed.len() < 1000, "embed_text should be capped");
    }

    #[test]
    fn se_grouped_metadata_carries_density_signals() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let docs: Vec<_> = grouped_extractor()
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let leaderboard = docs
            .iter()
            .find(|d| d.title.as_deref().unwrap_or("").contains("leaderboard"))
            .unwrap();
        let meta = leaderboard.metadata.as_ref().unwrap();
        assert_eq!(meta["se_mode"], "question_with_answers");
        assert_eq!(meta["community"], "stackoverflow.com");
        assert_eq!(meta["closed"], false);
        // Three answers passed the min_score=3 floor (47/31/18).
        assert_eq!(meta["answer_count"], 3);
        assert_eq!(meta["max_answer_score"], 47);
        assert_eq!(meta["min_answer_score"], 18);
        let tags: Vec<&str> = meta["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(tags.contains(&"architecture"));
        assert!(tags.contains(&"database"));
    }

    #[test]
    fn se_grouped_drops_short_answers() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        // min_answer_length = 1000 — all our test answers are ~150 chars,
        // so every answer is rejected and no docs come out.
        let mut e = grouped_extractor();
        e.min_answer_length = 1000;
        let docs: Vec<_> = e
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(docs.is_empty(), "min_answer_length should reject everything");
    }

    #[test]
    fn se_grouped_respects_tag_filter() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let mut e = grouped_extractor();
        e.tag_filter = Some(vec!["architecture".into()]);
        let docs: Vec<_> = e
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        // Only the leaderboard question carries `architecture`.
        assert_eq!(docs.len(), 1);
        assert!(docs[0].title.as_deref().unwrap().contains("leaderboard"));
    }

    #[test]
    fn se_grouped_keeps_closed_when_exclude_closed_off() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let mut e = grouped_extractor();
        e.exclude_closed = false;
        let docs: Vec<_> = e
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let titles: Vec<_> = docs.iter().filter_map(|d| d.title.as_deref()).collect();
        assert!(titles.iter().any(|t| t.contains("Opinion-based")));
    }

    #[test]
    fn se_grouped_caps_at_max_answers_per_question() {
        let dir = tempfile::tempdir().unwrap();
        let community_dir = dir.path().join("stackoverflow.com");
        std::fs::create_dir(&community_dir).unwrap();
        make_se_grouped_xml(&community_dir);

        let mut e = grouped_extractor();
        e.max_answers_per_question = 2;
        let docs: Vec<_> = e
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        let leaderboard = docs
            .iter()
            .find(|d| d.title.as_deref().unwrap_or("").contains("leaderboard"))
            .unwrap();
        let approaches = leaderboard.content.matches("Approach ").count();
        assert_eq!(approaches, 2, "should keep top-2 answers when cap=2");
        // Lowest-score surviving answer (DynamoDB, score 18) must be
        // dropped under the cap.
        assert!(!leaderboard.content.contains("DynamoDB"));
    }

    #[test]
    fn parse_tags_handles_se_format() {
        assert_eq!(parse_tags(Some("<rust><memory-management>")), vec!["rust", "memory-management"]);
        assert_eq!(parse_tags(None), Vec::<String>::new());
        assert_eq!(parse_tags(Some("")), Vec::<String>::new());
    }

    #[test]
    fn first_sentence_skips_leading_code_block() {
        let body = "```python\nprint('hi')\n```\nUse the standard library to parse JSON. It's faster.";
        let s = first_sentence_skipping_code(body);
        assert!(s.starts_with("Use the standard library"));
    }

    // ─── 7z auto-extract ──────────────────────────────────────────

    #[test]
    fn is_seven_zip_recognises_common_extensions() {
        assert!(is_seven_zip(Path::new("foo.7z")));
        assert!(is_seven_zip(Path::new("FOO.7Z")));
        assert!(is_seven_zip(Path::new("a/b/c.7z")));
        assert!(!is_seven_zip(Path::new("foo.zip")));
        assert!(!is_seven_zip(Path::new("foo.tar.gz")));
        assert!(!is_seven_zip(Path::new("foo")));
    }

    #[test]
    fn ensure_seven_zip_extracted_is_idempotent_when_already_extracted() {
        // Pre-populate the destination so the fn must NOT try to
        // decompress (which would fail because no real archive exists
        // at the source path).
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("softwareengineering.stackexchange.com.7z");
        std::fs::write(&archive, b"not a real archive").unwrap();
        let extract = dir.path().join("softwareengineering.stackexchange.com");
        std::fs::create_dir_all(&extract).unwrap();
        std::fs::write(extract.join("Posts.xml"), b"<posts/>").unwrap();

        let returned = ensure_seven_zip_extracted(&archive).unwrap();
        assert_eq!(returned, extract);
    }

    /// Build a real 7z archive whose root entry is `Posts.xml`
    /// (matching the Internet Archive Stack Exchange dump layout —
    /// each per-community .7z holds the XML files at its root, not
    /// nested under a community subdir). Verify the SE extractor's
    /// `find_posts_files` auto-decompresses on first encounter and
    /// finds the posts file inside the resulting `<archive_stem>/`
    /// directory. End-to-end: download dir → archives → extracted
    /// Posts.xml → grouped docs.
    #[test]
    fn extractor_auto_extracts_seven_zip_archive_in_directory() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("bundle");
        std::fs::create_dir(&bundle).unwrap();

        // Source layout: a flat `Posts.xml` (no community subdir),
        // mirroring the IA archive root.
        let src_root = dir.path().join("src");
        std::fs::create_dir_all(&src_root).unwrap();
        std::fs::write(
            src_root.join("Posts.xml"),
            r#"<?xml version="1.0" encoding="utf-8"?>
<posts>
  <row Id="1" PostTypeId="1" Score="10" Title="Index design"
       Body="&lt;p&gt;Covering vs filtered indexes?&lt;/p&gt;"
       AcceptedAnswerId="2" Tags="&lt;index&gt;" />
  <row Id="2" PostTypeId="2" ParentId="1" Score="20"
       Body="&lt;p&gt;Filtered indexes save space when most rows fail the predicate; covering indexes save reads when the workload is read-heavy. Choose based on workload.&lt;/p&gt;" />
  <row Id="3" PostTypeId="2" ParentId="1" Score="12"
       Body="&lt;p&gt;Covering indexes give you index-only scans which are very fast at scale, even if they cost more disk.&lt;/p&gt;" />
</posts>"#,
        )
        .unwrap();

        // Build a real .7z whose archive root contains `Posts.xml`.
        // The encoder is enabled only in dev-dependencies (see Cargo.toml).
        let archive = bundle.join("dba.stackexchange.com.7z");
        sevenz_rust2::compress_to_path(&src_root, &archive).expect("compress fixture");
        // Cleanup source so the only thing under bundle/ is the archive.
        std::fs::remove_dir_all(&src_root).unwrap();

        let extractor = StackExchangeExtractor {
            min_score: 3,
            mode: SeMode::QuestionWithAnswers,
            max_answers_per_question: 5,
            min_answer_length: 0,
            exclude_closed: true,
            tag_filter: None,
        };
        let docs: Vec<_> = extractor
            .extract(&bundle)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // The 2-answer multi-perspective question survives.
        assert_eq!(docs.len(), 1);
        assert!(docs[0].title.as_deref().unwrap().contains("Index design"));
        // Sentinel was written so a re-run skips the decompress step.
        assert!(bundle.join("dba.stackexchange.com").join(".extracted").is_file());

        // Re-run is idempotent: the extracted dir + sentinel mean
        // ensure_seven_zip_extracted short-circuits; docs still come out.
        let docs2: Vec<_> = extractor
            .extract(&bundle)
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs2.len(), 1);
    }
}
