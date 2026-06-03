//! `acquire_source` / `make_extractor` / `make_chunker` — extracted out
//! of `engine::ingest`.
//!
//! The three factory methods that translate a recipe's `[acquire]` /
//! `[extract]` / `[chunk]` blocks into the trait objects the ingest
//! pipeline drives. Behaviour-preserving — same dispatch arms, same
//! error semantics, same trait surface.

use std::path::{Path, PathBuf};

use super::CorpusEngine;
use crate::acquirers::bulk_download::BulkDownloader;
use crate::acquirers::http_api::HttpApiAcquirer;
use crate::acquirers::huggingface::HuggingFaceDatasetAcquirer;
use crate::acquirers::local_file::LocalFileAcquirer;
use crate::chunkers::{self, Chunker};
use crate::error::{Error, Result};
use crate::extractors::{self, Extractor};
use crate::progress::ProgressCallback;
use crate::recipe::{AcquirerConfig, ChunkerConfig, ExtractorConfig, Recipe};

impl CorpusEngine {
    pub(crate) async fn acquire_source(
        &self,
        recipe: &Recipe,
        download_dir: &Path,
        progress: &Option<ProgressCallback>,
    ) -> Result<PathBuf> {
        match &recipe.acquire {
            AcquirerConfig::BulkDownload { url, urls, resume } => {
                // Interpolate `{name}` placeholders in URL(s) against
                // resolved recipe parameters so `urls = [".../USCODE-{year}-title15.zip"]`
                // can pick up a `[parameters.year]` install-time value.
                // Empty placeholder set is a clean no-op for legacy
                // recipes that never declare parameters.
                let bindings: std::collections::BTreeMap<String, String> = recipe
                    .resolved_parameters
                    .values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_interpolation()))
                    .collect();
                let render =
                    |tpl: &str| crate::acquirers::http_api::template::render_template(
                        tpl, "", &bindings,
                    );
                let url_rendered = url
                    .as_deref()
                    .map(render)
                    .transpose()?;
                let urls_rendered = urls
                    .as_ref()
                    .map(|v| v.iter().map(|u| render(u)).collect::<Result<Vec<_>>>())
                    .transpose()?;
                let downloader = match (url_rendered.as_deref(), urls_rendered.as_ref()) {
                    (Some(u), None) => BulkDownloader::new(u, *resume),
                    (None, Some(us)) if !us.is_empty() => {
                        BulkDownloader::with_urls(us.clone(), *resume)
                    }
                    (Some(_), Some(_)) => {
                        return Err(Error::Recipe(
                            "bulk_download: set exactly one of `url` or `urls`, not both".into(),
                        ))
                    }
                    _ => {
                        return Err(Error::Recipe(
                            "bulk_download: requires `url` or non-empty `urls`".into(),
                        ))
                    }
                };
                downloader
                    .download(download_dir, &recipe.corpus.id, progress)
                    .await
            }
            AcquirerConfig::LocalFile { path } => {
                let acq = LocalFileAcquirer::new(path);
                acq.acquire()
            }
            AcquirerConfig::HuggingFaceDataset { repo, subset, file_indices } => {
                let mut acq = HuggingFaceDatasetAcquirer::new(repo, subset.as_deref());
                if let Some(indices) = file_indices {
                    acq.file_indices = Some(indices.clone());
                }
                acq.download(download_dir, &recipe.corpus.id, progress).await
            }
            AcquirerConfig::WebCrawl { .. } => {
                Err(Error::Recipe("Web crawl acquirer not yet implemented".into()))
            }
            AcquirerConfig::HttpApi {
                base_url,
                requests,
                pagination,
                follow,
                rate_limit_per_second,
                user_agent,
                headers,
            } => {
                // The acquirer reads parameters off the recipe's
                // runtime-only `resolved_parameters` field, populated
                // by the CLI / desktop install path before ingest.
                // For installs through paths that don't (yet) thread
                // parameters, the field defaults to empty — recipes
                // that genuinely require parameters will fail in
                // `for_each_bindings` with a clear message.
                let acq = HttpApiAcquirer::new(
                    base_url.clone(),
                    requests.clone(),
                    pagination.clone(),
                    follow.clone(),
                    *rate_limit_per_second,
                    user_agent.clone(),
                    headers.clone(),
                    recipe.resolved_parameters.clone(),
                )?;
                acq.acquire(download_dir, &recipe.corpus.id, progress).await
            }
            AcquirerConfig::Custom { kind, params } => {
                let _ = progress; // Custom acquirers do not emit progress; see CustomAcquirerFn docs.
                let acquirer = self.custom_acquirer(kind).ok_or_else(|| {
                    Error::Recipe(format!(
                        "No custom acquirer registered for kind '{kind}'. \
                         Call CorpusEngine::register_acquirer before ingest."
                    ))
                })?;
                (acquirer)(params.clone(), download_dir.to_path_buf()).await
            }
        }
    }

    pub(crate) fn make_extractor(
        &self,
        config: &ExtractorConfig,
        corpus_id: &str,
    ) -> Box<dyn Extractor> {
        match config {
            ExtractorConfig::MediawikiXml {
                namespace_filter,
                skip_redirects,
                decompress,
            } => Box::new(extractors::xml::MediawikiExtractor {
                namespace_filter: namespace_filter.clone(),
                skip_redirects: *skip_redirects,
                decompress: decompress.clone(),
            }),
            ExtractorConfig::StackExchangeXml {
                min_score,
                mode,
                max_answers_per_question,
                min_answer_length,
                exclude_closed,
                tag_filter,
            } => {
                Box::new(extractors::xml::StackExchangeExtractor {
                    min_score: *min_score,
                    mode: *mode,
                    max_answers_per_question: *max_answers_per_question,
                    min_answer_length: *min_answer_length,
                    exclude_closed: *exclude_closed,
                    tag_filter: tag_filter.clone(),
                })
            }
            ExtractorConfig::Jsonl {
                content_field,
                title_field,
                filter,
                decompress,
            } => Box::new(extractors::json::JsonlExtractor {
                content_field: content_field.clone(),
                title_field: title_field.clone(),
                filter: filter.clone(),
                decompress: decompress.clone(),
            }),
            ExtractorConfig::Json {
                document_path,
                content_field,
                title_field,
                url_field,
                id_field,
            } => Box::new(extractors::json_api::JsonApiExtractor {
                document_path: document_path.clone(),
                content_field: content_field.clone(),
                title_field: title_field.clone(),
                url_field: url_field.clone(),
                id_field: id_field.clone(),
            }),
            ExtractorConfig::Html {
                content_selector,
                title_selector,
            } => Box::new(extractors::html::HtmlExtractor {
                content_selector: content_selector.clone(),
                title_selector: title_selector.clone(),
                label: String::new(),
            }),
            ExtractorConfig::HtmlSections {
                sections,
                fallback,
                title_selector: _title_selector,
            } => {
                // Recipe-level regex validation happens up front in
                // `sovereign recipe validate` (Phase 4a); reaching
                // this arm with a bad regex means the recipe was
                // installed without validation. Panic with the
                // section name so the operator sees the actual
                // pattern that's broken.
                Box::new(
                    extractors::html_sections::HtmlSectionsExtractor::new(
                        sections,
                        fallback.clone(),
                    )
                    .unwrap_or_else(|e| {
                        panic!("html_sections recipe failed to construct: {e}")
                    }),
                )
            }
            ExtractorConfig::Csv {
                content_column,
                title_column,
                delimiter,
            } => Box::new(extractors::csv::CsvExtractor {
                content_column: content_column.clone(),
                title_column: title_column.clone(),
                delimiter: delimiter.map(|c| c as u8),
            }),
            ExtractorConfig::GutenbergCatalog {} => {
                Box::new(extractors::gutenberg_catalog::GutenbergCatalogExtractor)
            }
            ExtractorConfig::WikipediaCatalog {} => {
                Box::new(extractors::wikipedia_catalog::WikipediaCatalogExtractor)
            }
            ExtractorConfig::WikipediaApiArticle {} => {
                Box::new(extractors::wikipedia_api_article::WikipediaApiArticleExtractor::default())
            }
            ExtractorConfig::Parquet {
                content_column,
                label_column,
                url_column,
                content_transform,
            } => Box::new(extractors::parquet::ParquetExtractor {
                content_column: content_column.clone(),
                label_column: label_column.clone(),
                url_column: url_column.clone(),
                content_transform: content_transform.clone(),
            }),
            ExtractorConfig::Plaintext {
                title_pattern,
                strip_boilerplate,
            } => Box::new(extractors::plaintext::PlaintextExtractor {
                title_pattern: title_pattern.clone(),
                strip_boilerplate: strip_boilerplate.clone(),
            }),
            ExtractorConfig::WikipediaStructured {
                title_column,
                url_column,
                controversy_patterns,
                factual_patterns,
                ..
            } => Box::new(
                extractors::wikipedia_structured::WikipediaStructuredExtractor {
                    title_column: title_column.clone(),
                    url_column: url_column.clone(),
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                },
            ),
            ExtractorConfig::WikipediaJsonl {
                controversy_patterns,
                factual_patterns,
                article_range,
                shard_indices,
            } => Box::new(
                extractors::wikipedia_jsonl::WikipediaJsonlExtractor {
                    controversy_patterns: controversy_patterns.clone(),
                    factual_patterns: factual_patterns.clone(),
                    article_range: *article_range,
                    shard_indices: shard_indices.clone(),
                },
            ),
            #[cfg(feature = "treesitter")]
            ExtractorConfig::Code {
                context_lines,
                max_lines_per_chunk,
            } => Box::new(extractors::code::CodeExtractor {
                context_lines: *context_lines,
                max_lines_per_chunk: *max_lines_per_chunk,
            }),
            #[cfg(not(feature = "treesitter"))]
            ExtractorConfig::Code { .. } => {
                // The recipe requested the `code` extractor but this
                // corpus-engine build doesn't include tree-sitter. Fail
                // loudly at recipe-load time, not silently at query time.
                panic!(
                    "corpus-engine was built without the `treesitter` feature — \
                     rebuild with `cargo build --features treesitter` to enable \
                     the `code` extractor"
                );
            }
            #[cfg(feature = "markdown")]
            ExtractorConfig::Markdown {} => {
                Box::new(extractors::markdown::MarkdownExtractor::new())
            }
            #[cfg(not(feature = "markdown"))]
            ExtractorConfig::Markdown {} => {
                panic!(
                    "corpus-engine was built without the `markdown` feature — \
                     rebuild with `cargo build --features markdown` to enable \
                     the section-aware markdown extractor"
                );
            }
            ExtractorConfig::AlignmentWorkspace {} => {
                Box::new(extractors::alignment_workspace::AlignmentWorkspaceExtractor)
            }
            ExtractorConfig::AnthropicExport {} => {
                Box::new(extractors::anthropic_export::AnthropicExportExtractor)
            }
            ExtractorConfig::XmlSections {
                element,
                title_attr,
            } => Box::new(extractors::xml_sections::XmlSectionsExtractor {
                element: element.clone(),
                title_attr: title_attr.clone(),
            }),
            ExtractorConfig::Custom { kind, extension, params: _params } => {
                let registered = self.custom_extractor(kind).unwrap_or_else(|| {
                    panic!(
                        "No custom extractor registered for kind '{kind}'. \
                         Call CorpusEngine::register_extractor before ingest \
                         (sovereign-tools registers \"pdf\" at daemon startup; \
                         bare-CLI flows need to register it explicitly)."
                    )
                });
                Box::new(extractors::custom_file::CustomFileExtractor {
                    extension: extension.clone(),
                    kind: kind.clone(),
                    extractor: registered,
                })
            }
            ExtractorConfig::DescribedAsset { max_bytes_per_asset } => {
                let asset_store = self.asset_store_for(corpus_id);
                let registry = self.asset_sub_extractors();
                let asset_atoms_sidecar = self
                    .index_dir()
                    .join(corpus_id)
                    .join("atlas")
                    .join("asset_atoms.jsonl");
                Box::new(
                    extractors::described_asset::DescribedAssetExtractor {
                        store: asset_store,
                        registry,
                        asset_atoms_sidecar,
                        max_bytes_per_asset: *max_bytes_per_asset,
                    },
                )
            }
            ExtractorConfig::Email {
                max_body_bytes,
                max_attachment_bytes,
            } => {
                let asset_store = self.asset_store_for(corpus_id);
                let registry = self.asset_sub_extractors();
                let atlas_dir = self.index_dir().join(corpus_id).join("atlas");
                let dispatch =
                    extractors::email_rfc5322::EmailAssetDispatch {
                        store: asset_store,
                        registry,
                        asset_atoms_sidecar: atlas_dir.join("asset_atoms.jsonl"),
                        asset_edges_sidecar: atlas_dir.join("asset_edges.jsonl"),
                    };
                Box::new(
                    extractors::email_rfc5322::EmailExtractor::new(
                        extractors::email_rfc5322::EmailExtractorConfig {
                            max_body_bytes: *max_body_bytes,
                            max_attachment_bytes: *max_attachment_bytes,
                        },
                    )
                    .with_asset_dispatch(dispatch),
                )
            }
        }
    }

    pub(crate) fn make_chunker(&self, config: &ChunkerConfig) -> Box<dyn Chunker> {
        match config {
            ChunkerConfig::Paragraph {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::paragraph::ParagraphChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Sentence { max_chars } => {
                Box::new(chunkers::sentence::SentenceChunker {
                    max_chars: *max_chars,
                })
            }
            ChunkerConfig::Fixed {
                max_chars,
                overlap_chars,
            } => Box::new(chunkers::fixed::FixedChunker {
                max_chars: *max_chars,
                overlap_chars: *overlap_chars,
            }),
            ChunkerConfig::Semantic { max_chars } => {
                Box::new(chunkers::semantic::SemanticChunker {
                    max_chars: *max_chars,
                })
            }
            ChunkerConfig::Passthrough => Box::new(chunkers::passthrough::PassthroughChunker),
            ChunkerConfig::PortalEventBullet { max_chars } => Box::new(
                chunkers::portal_event_bullet::PortalEventBulletChunker::new(*max_chars),
            ),
            ChunkerConfig::ThreadedTurns => {
                Box::new(chunkers::threaded_turns::ThreadedTurnsChunker::new())
            }
        }
    }
}
