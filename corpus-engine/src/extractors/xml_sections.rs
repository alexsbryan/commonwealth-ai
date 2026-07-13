// SPDX-License-Identifier: AGPL-3.0-or-later
//! Section-aware XML extractor.
//!
//! Walks a directory of `.xml` files, streams each through `quick-xml`,
//! and emits one [`ExtractedDoc`] per element whose local-name matches
//! the configured `element` field. Namespace-agnostic: only the local
//! name is compared, so a recipe pinned to `element = "section"` works
//! against USLM 1.x (`xmlns="http://xml.house.gov/schemas/uslm/1.0"`)
//! AND USLM 2.0 (`xmlns="http://schemas.gpo.gov/xml/uslm"`) without
//! recipe-side awareness of the transition.
//!
//! Content is the inner text of the matched element with all child
//! tags stripped (whitespace-normalised). Title is read off the
//! configured attribute (e.g. `identifier` on USLM `<section>` yields
//! titles like `/us/usc/t15/s1`) — `None` if the attribute is absent
//! or `title_attr` is unset.
//!
//! Designed for the United States Code (govinfo USCODE collection) but
//! shape-only — any sectioned XML works.
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use quick_xml::reader::Reader as XmlReader;

use super::{slug, ExtractedDoc, Extractor};
use crate::error::{Error, Result};

pub struct XmlSectionsExtractor {
    /// Local-name of the element type to emit one doc per. Compared
    /// case-sensitively against `e.local_name()` so USLM v1's
    /// `<section>` and v2's `<section>` both match.
    pub element: String,
    /// Optional attribute (local-name) read off the matched element
    /// and used as the doc title. e.g. `Some("identifier".into())`
    /// produces USLM titles like `/us/usc/t15/s1`.
    pub title_attr: Option<String>,
}

impl Extractor for XmlSectionsExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let files = collect_xml_files(source_path)?;
        Ok(Box::new(XmlSectionsIterator {
            files: files.into(),
            element: self.element.clone(),
            title_attr: self.title_attr.clone(),
            pending: VecDeque::new(),
        }))
    }
}

struct XmlSectionsIterator {
    files: VecDeque<PathBuf>,
    element: String,
    title_attr: Option<String>,
    pending: VecDeque<ExtractedDoc>,
}

impl Iterator for XmlSectionsIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(doc) = self.pending.pop_front() {
                return Some(Ok(doc));
            }
            let path = self.files.pop_front()?;
            match extract_sections(&path, &self.element, self.title_attr.as_deref()) {
                Ok(docs) => {
                    for d in docs {
                        self.pending.push_back(d);
                    }
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

fn extract_sections(
    path: &Path,
    element: &str,
    title_attr: Option<&str>,
) -> Result<Vec<ExtractedDoc>> {
    let file =
        File::open(path).map_err(|e| Error::Extraction(format!("open {}: {e}", path.display())))?;
    let mut reader = XmlReader::from_reader(BufReader::new(file));
    reader.config_mut().trim_text(false);

    let element_bytes = element.as_bytes();
    let title_attr_bytes = title_attr.map(|s| s.as_bytes());

    let mut docs = Vec::new();
    let mut depth: i32 = 0;
    let mut buffer = String::new();
    let mut title: Option<String> = None;
    let mut buf = Vec::new();
    let mut emit_count: usize = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                let is_root_match = depth == 0 && local.as_ref() == element_bytes;
                if is_root_match {
                    depth = 1;
                    buffer.clear();
                    title = None;
                    if let Some(attr_name) = title_attr_bytes {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == attr_name {
                                if let Ok(v) = attr.unescape_value() {
                                    title = Some(v.into_owned());
                                }
                                break;
                            }
                        }
                    }
                } else if depth > 0 {
                    depth += 1;
                }
            }
            Ok(Event::End(e)) if depth > 0 => {
                depth -= 1;
                let local = e.local_name();
                if depth == 0 && local.as_ref() == element_bytes {
                    let content = normalise_whitespace(&buffer);
                    if !content.is_empty() {
                        emit_count += 1;
                        let source_id = derive_source_id(path, title.as_deref(), emit_count);
                        docs.push(ExtractedDoc {
                            title: title.clone(),
                            content,
                            url: None,
                            source_id,
                            metadata: None,
                            source_file: path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_string()),
                            embed_text: None,
                        });
                    }
                }
            }
            Ok(Event::Text(t)) if depth > 0 => {
                if let Ok(s) = t.unescape() {
                    buffer.push_str(&s);
                    buffer.push(' ');
                }
            }
            Ok(Event::CData(t)) if depth > 0 => {
                let s = String::from_utf8_lossy(t.as_ref());
                buffer.push_str(&s);
                buffer.push(' ');
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(Error::Extraction(format!(
                    "xml parse error in {}: {e}",
                    path.display()
                )))
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(docs)
}

fn normalise_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

fn derive_source_id(path: &Path, title: Option<&str>, ordinal: usize) -> String {
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        return slug(t);
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    slug(&format!("{stem}-{ordinal}"))
}

fn collect_xml_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::Extraction(format!("read_dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Extraction(format!("dir entry: {e}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("xml"))
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn extracts_uslm_v1_sections_by_local_name() {
        // USLM 1.x uses a custom namespace; the extractor must ignore
        // namespaces and match on local-name. Two sections in one file
        // → two docs.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "title15.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<uscDoc xmlns="http://xml.house.gov/schemas/uslm/1.0">
  <main>
    <title identifier="/us/usc/t15">
      <num value="15">Title 15.</num>
      <heading>Commerce and Trade</heading>
      <chapter identifier="/us/usc/t15/ch1">
        <section identifier="/us/usc/t15/s1">
          <num value="1">§1.</num>
          <heading>Trusts, etc., in restraint of trade illegal; penalty</heading>
          <content>
            <p>Every contract, combination in the form of trust or otherwise…</p>
          </content>
        </section>
        <section identifier="/us/usc/t15/s2">
          <num value="2">§2.</num>
          <heading>Monopolizing trade a felony; penalty</heading>
          <content>
            <p>Every person who shall monopolize, or attempt to monopolize…</p>
          </content>
        </section>
      </chapter>
    </title>
  </main>
</uscDoc>"#,
        );
        let ex = XmlSectionsExtractor {
            element: "section".into(),
            title_attr: Some("identifier".into()),
        };
        let docs: Vec<_> = ex
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].title.as_deref(), Some("/us/usc/t15/s1"));
        assert!(docs[0]
            .content
            .contains("Trusts, etc., in restraint of trade illegal"));
        assert!(docs[0].content.contains("Every contract, combination"));
        assert_eq!(docs[1].title.as_deref(), Some("/us/usc/t15/s2"));
    }

    #[test]
    fn extracts_uslm_v2_sections_under_different_namespace() {
        // USLM 2.0 uses a different namespace URL — local-name matching
        // shields the recipe from the v1→v2 transition.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "title.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<uscDoc xmlns="http://schemas.gpo.gov/xml/uslm">
  <main>
    <section identifier="/us/usc/t26/s1">
      <num>§1.</num>
      <heading>Tax imposed</heading>
      <content><p>There is hereby imposed on the taxable income…</p></content>
    </section>
  </main>
</uscDoc>"#,
        );
        let ex = XmlSectionsExtractor {
            element: "section".into(),
            title_attr: Some("identifier".into()),
        };
        let docs: Vec<_> = ex
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title.as_deref(), Some("/us/usc/t26/s1"));
        assert!(docs[0].content.contains("Tax imposed"));
    }

    #[test]
    fn skips_nested_section_elements() {
        // Some legal XML nests `<section>` under itself for sub-units.
        // The depth tracker should swallow inner Starts/Ends and only
        // emit one doc at the outer close.
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "n.xml",
            r#"<root>
              <section identifier="/a">
                <section identifier="/a/1"><p>sub one</p></section>
                <section identifier="/a/2"><p>sub two</p></section>
                <p>parent body</p>
              </section>
            </root>"#,
        );
        let ex = XmlSectionsExtractor {
            element: "section".into(),
            title_attr: Some("identifier".into()),
        };
        let docs: Vec<_> = ex
            .extract(dir.path())
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            docs.len(),
            1,
            "outer section is one doc; inner sections are merged into its content"
        );
        assert_eq!(docs[0].title.as_deref(), Some("/a"));
        let c = &docs[0].content;
        assert!(c.contains("sub one"));
        assert!(c.contains("sub two"));
        assert!(c.contains("parent body"));
    }
}
