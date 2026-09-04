// SPDX-License-Identifier: AGPL-3.0-or-later
//! The four tools, and the corpora they run over.
//!
//! - `corpus_list` — what is served, with the one fact a caller needs before
//!   searching: whether the vector leg is live for that corpus.
//! - `corpus_search` — tier 1: cited chunks from `CorpusIndex::search` (the
//!   same LanceDB + Tantivy hybrid every sovereign surface uses).
//! - `atoms_lookup` — tier 1.5: the atoms an enrichment PRODUCED, read from
//!   `atlas/atoms.json` through `corpus_engine_vocab::atoms::AtomsFile`.
//!   That the artifact crosses the seam is the point; the atom-grounded
//!   ranking does not, and this module does not pretend to.
//! - `corpus_ontology` — what the corpus DECLARED, from `atlas/ontology.json`
//!   via `corpus_engine_vocab::ontology::OntologyPolicies`.
//!
//! Absence is reported, never defaulted (§18.3): a corpus with no atlas says
//! so by path; a corpus whose index width differs from the endpoint's is
//! served full-text only and says so at boot and in `corpus_list`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use corpus_engine::{CorpusEngine, CorpusIndex, EmbedFn, ScoredChunk};
use corpus_engine_vocab::atoms::{AtomEnvelope, AtomsFile};
use corpus_engine_vocab::ontology::OntologyPolicies;
use serde_json::{json, Value};

use crate::host::HostProfile;

pub struct ToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub structured: Option<Value>,
}

struct Served {
    id: String,
    index: CorpusIndex,
    dims: usize,
    /// The index width matches the endpoint's, so the vector leg runs.
    vector: bool,
}

pub struct Server {
    embed: EmbedFn,
    indexes_dir: PathBuf,
    served: Vec<Served>,
    default_limit: usize,
    profile: HostProfile,
}

impl Server {
    pub async fn open(
        recipes_dir: PathBuf,
        indexes_dir: PathBuf,
        embed: EmbedFn,
        corpora: Vec<String>,
        default_limit: usize,
        profile: HostProfile,
    ) -> Result<Self> {
        let engine = CorpusEngine::new(recipes_dir, indexes_dir.clone(), embed.clone());
        let ids: Vec<String> = if corpora.is_empty() {
            engine
                .installed_indexes()
                .await?
                .into_iter()
                .map(|i| i.corpus_id)
                .collect()
        } else {
            corpora
        };
        let mut served = Vec::new();
        for id in ids {
            let index = match engine.open_index_for_corpus(&id).await {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("corpus-mcp: corpus `{id}`: cannot open ({e}) — not served");
                    continue;
                }
            };
            let dims = index.info().await?.embedding_dimensions;
            let vector = dims == profile.embed_dims;
            if !vector {
                eprintln!(
                    "corpus-mcp: corpus `{id}`: index is {dims}-d but `{}` returns {}-d — \
                     vector search DISABLED for it, full-text only",
                    profile.embed_model, profile.embed_dims
                );
            }
            served.push(Served {
                id,
                index,
                dims,
                vector,
            });
        }
        if served.is_empty() {
            bail!(
                "no searchable corpus: installed indexes live under {}; name one with \
                 --corpus <id>",
                indexes_dir.display()
            );
        }
        eprintln!(
            "corpus-mcp: serving {}: {}",
            if served.len() == 1 {
                "1 corpus"
            } else {
                "corpora"
            },
            served
                .iter()
                .map(|s| format!("{}{}", s.id, if s.vector { "" } else { " (fts-only)" }))
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(Self {
            embed,
            indexes_dir,
            served,
            default_limit,
            profile,
        })
    }

    pub fn instructions(&self) -> String {
        format!(
            "corpus-mcp serves {} corpus(es) from a local corpus-engine index through `{}` \
             ({}). `corpus_search` returns cited chunks; `atoms_lookup` and \
             `corpus_ontology` read what enrichment produced and declared. Ranking by atlas \
             atoms is not part of this host.",
            self.served.len(),
            self.profile.base_url,
            self.profile.kind.label()
        )
    }

    pub fn tool_list(&self) -> Value {
        let corpus_prop = json!({
            "type": "string",
            "description": format!("Corpus id. Served: {}.", self.served.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", "))
        });
        json!([
            {
                "name": "corpus_list",
                "description": "List the corpora this host serves, with index width and whether vector search is live for each, and whether an atlas (atoms.json) is present.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "corpus_search",
                "description": "Hybrid (vector + full-text) search over a corpus. Returns cited chunks: title, url, corpus, score, and the passage. Use for any question a document collection can answer.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The question or phrase to search for." },
                        "corpus": corpus_prop,
                        "limit": { "type": "integer", "description": format!("Top-K chunks (default {}).", self.default_limit) }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "atoms_lookup",
                "description": "Look up atlas atoms (Entity, Claim, Event, Question, Position, …) a corpus's enrichment produced, by kind and/or name substring. Reads atlas/atoms.json; says so if the corpus has none.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "corpus": corpus_prop,
                        "query": { "type": "string", "description": "Case-insensitive substring matched against each atom's display name and text." },
                        "kind": { "type": "string", "description": "Atom kind label: entity, event, state, relation, claim, question, configuration, argument, position, opposition, asset." },
                        "limit": { "type": "integer", "description": "Max atoms (default 20)." }
                    },
                    "required": ["corpus"]
                }
            },
            {
                "name": "corpus_ontology",
                "description": "What a corpus DECLARED for its enrichment: the ontology policies recorded in atlas/ontology.json (declared types per kind, vocabulary terms, guidance). Says so if none was recorded.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "corpus": corpus_prop },
                    "required": ["corpus"]
                }
            }
        ])
    }

    pub async fn call(&self, name: &str, args: &Value) -> Result<ToolOutcome> {
        match name {
            "corpus_list" => Ok(self.corpus_list()),
            "corpus_search" => self.corpus_search(args).await,
            "atoms_lookup" => Ok(self.atoms_lookup(args)),
            "corpus_ontology" => Ok(self.corpus_ontology(args)),
            other => Err(anyhow!("unknown tool: {other}")),
        }
    }

    fn corpus_list(&self) -> ToolOutcome {
        let rows: Vec<Value> = self
            .served
            .iter()
            .map(|s| {
                let atlas = self.atlas_path(&s.id, "atoms.json").exists();
                json!({
                    "corpus_id": s.id,
                    "embedding_dimensions": s.dims,
                    "vector_search": s.vector,
                    "has_atlas": atlas,
                })
            })
            .collect();
        let text = self
            .served
            .iter()
            .map(|s| {
                format!(
                    "{} — {}-d, {}{}",
                    s.id,
                    s.dims,
                    if s.vector {
                        "vector + full-text"
                    } else {
                        "full-text only (width mismatch)"
                    },
                    if self.atlas_path(&s.id, "atoms.json").exists() {
                        ", atlas present"
                    } else {
                        ", no atlas"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        ToolOutcome {
            text,
            is_error: false,
            structured: Some(json!({ "corpora": rows })),
        }
    }

    async fn corpus_search(&self, args: &Value) -> Result<ToolOutcome> {
        let Some(query) = args["query"].as_str().filter(|q| !q.trim().is_empty()) else {
            return Ok(refuse("corpus_search needs a non-empty `query`"));
        };
        let limit = args["limit"]
            .as_u64()
            .map(|l| l as usize)
            .unwrap_or(self.default_limit);
        let targets: Vec<&Served> = match args["corpus"].as_str() {
            Some(id) => match self.served.iter().find(|s| s.id == id) {
                Some(s) => vec![s],
                None => {
                    return Ok(refuse(&format!(
                        "corpus `{id}` is not served; served: {}",
                        self.served
                            .iter()
                            .map(|s| s.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )))
                }
            },
            None => self.served.iter().collect(),
        };

        // One embedding for the query, only if some target can use it.
        let embedding = if targets.iter().any(|t| t.vector) {
            (self.embed)(query).await?
        } else {
            Vec::new()
        };
        let mut hits: Vec<ScoredChunk> = Vec::new();
        for t in &targets {
            let emb: &[f32] = if t.vector { &embedding } else { &[] };
            let mut found = t.index.search(emb, query, limit).await?;
            hits.append(&mut found);
        }
        if targets.len() > 1 {
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit);
        }
        if hits.is_empty() {
            return Ok(ToolOutcome {
                text: format!("No chunks matched `{query}`."),
                is_error: false,
                structured: Some(json!({ "results": [] })),
            });
        }

        let mut text = String::new();
        let mut rows = Vec::with_capacity(hits.len());
        for (i, h) in hits.iter().enumerate() {
            let title = h.title.as_deref().unwrap_or("(untitled)");
            let url = h.url.as_deref().unwrap_or("-");
            text.push_str(&format!(
                "[{n}] {title} — {url} (corpus {corpus}, score {score:.4})\n{body}\n\n",
                n = i + 1,
                corpus = h.corpus_id,
                score = h.score,
                body = truncate(&h.content, 600),
            ));
            rows.push(json!({
                "rank": i + 1,
                "corpus_id": h.corpus_id,
                "title": h.title,
                "url": h.url,
                "score": h.score,
                "chunk_id": h.chunk_id,
                "source_doc_id": h.source_doc_id,
                "content": h.content,
            }));
        }
        Ok(ToolOutcome {
            text: text.trim_end().to_string(),
            is_error: false,
            structured: Some(json!({ "query": query, "results": rows })),
        })
    }

    fn atlas_path(&self, corpus: &str, file: &str) -> PathBuf {
        self.indexes_dir.join(corpus).join("atlas").join(file)
    }

    fn atoms_lookup(&self, args: &Value) -> ToolOutcome {
        let Some(corpus) = args["corpus"].as_str() else {
            return refuse("atoms_lookup needs `corpus`");
        };
        let path = self.atlas_path(corpus, "atoms.json");
        let file = match read_atoms(&path) {
            Ok(f) => f,
            Err(e) => return refuse(&e.to_string()),
        };
        let needle = args["query"].as_str().map(str::to_lowercase);
        let kind = args["kind"].as_str().map(str::to_lowercase);
        let limit = args["limit"].as_u64().map(|l| l as usize).unwrap_or(20);

        let total = file.atoms.len();
        let matched: Vec<&AtomEnvelope> = file
            .atoms
            .iter()
            .filter(|a| kind.as_deref().is_none_or(|k| a.atom_type().label() == k))
            .filter(|a| {
                needle.as_deref().is_none_or(|q| {
                    a.display_name(None).to_lowercase().contains(q)
                        || atom_text(a).to_lowercase().contains(q)
                })
            })
            .take(limit)
            .collect();
        let rows: Vec<Value> = matched
            .iter()
            .map(|a| {
                json!({
                    "id": a.id(),
                    "kind": a.atom_type().label(),
                    "name": a.display_name(Some(120)),
                    "text": truncate(atom_text(a), 400),
                    "evidence_chunks": a.evidence().iter().map(|c| c.chunk_id.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        let text = if matched.is_empty() {
            format!(
                "No atoms matched in `{corpus}` ({total} atoms in {}).",
                path.display()
            )
        } else {
            let mut t = format!(
                "{} of {total} atoms in `{corpus}` (schema {}):\n",
                matched.len(),
                file.schema_version
            );
            for a in &matched {
                t.push_str(&format!(
                    "- [{}] {} — {} ({})\n",
                    a.atom_type().label(),
                    a.display_name(Some(120)),
                    truncate(atom_text(a), 200),
                    a.id().as_str()
                ));
            }
            t
        };
        ToolOutcome {
            text: text.trim_end().to_string(),
            is_error: false,
            structured: Some(json!({ "corpus": corpus, "total_atoms": total, "atoms": rows })),
        }
    }

    fn corpus_ontology(&self, args: &Value) -> ToolOutcome {
        let Some(corpus) = args["corpus"].as_str() else {
            return refuse("corpus_ontology needs `corpus`");
        };
        let path = self.atlas_path(corpus, "ontology.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) => {
                return refuse(&format!(
                    "corpus `{corpus}` recorded no ontology at {} ({e}) — either its \
                     enrichment predates ontology.json or it was never built/pulled here",
                    path.display()
                ))
            }
        };
        let policies: OntologyPolicies = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(e) => {
                return refuse(&format!(
                    "{}: not an OntologyPolicies document: {e}",
                    path.display()
                ))
            }
        };
        let mut text = format!("Ontology declared by `{corpus}`:\n");
        if policies.shape.types.is_empty() {
            text.push_str("- declared types: none (prose-only / version-0 block)\n");
        } else {
            let mut by_kind: HashMap<String, Vec<&str>> = HashMap::new();
            for t in &policies.shape.types {
                by_kind
                    .entry(format!("{:?}", t.kind).to_lowercase())
                    .or_default()
                    .push(t.name.as_str());
            }
            let mut kinds: Vec<_> = by_kind.into_iter().collect();
            kinds.sort();
            for (k, names) in kinds {
                text.push_str(&format!("- {k} types: {}\n", names.join(", ")));
            }
        }
        let v = policies.vocabulary();
        text.push_str(&format!(
            "- vocabulary: concern=`{}` position=`{}` tension=`{}` absence=`{}` evidence=`{}`\n",
            v.canonical_concern_term,
            v.position_term,
            v.tension_term,
            v.absence_term,
            v.evidence_term
        ));
        if !policies.prose.guidance.trim().is_empty() {
            text.push_str(&format!(
                "- guidance: {}\n",
                truncate(policies.prose.guidance.trim(), 400)
            ));
        }
        let structured = serde_json::to_value(&policies).ok();
        ToolOutcome {
            text: text.trim_end().to_string(),
            is_error: false,
            structured,
        }
    }
}

fn read_atoms(path: &Path) -> Result<AtomsFile> {
    if !path.exists() {
        bail!(
            "no atlas at {} — this corpus's enrichment was not built or pulled on this machine",
            path.display()
        );
    }
    let file = std::fs::File::open(path)?;
    Ok(serde_json::from_reader(std::io::BufReader::new(file))?)
}

/// The prose an atom carries, by kind. Entity/Event/Configuration describe;
/// Claim/Question/Position state; the rest have only their name.
fn atom_text(a: &AtomEnvelope) -> &str {
    match a {
        AtomEnvelope::Entity(e) => &e.description,
        AtomEnvelope::Event(e) => &e.description,
        AtomEnvelope::Configuration(c) => &c.description,
        AtomEnvelope::Claim(c) => &c.content,
        AtomEnvelope::Question(q) => &q.content,
        AtomEnvelope::Position(p) => &p.content,
        _ => "",
    }
}

fn refuse(msg: &str) -> ToolOutcome {
    ToolOutcome {
        text: msg.to_string(),
        is_error: true,
        structured: None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>().trim_end())
    }
}
