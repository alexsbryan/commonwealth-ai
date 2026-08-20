// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test fixtures for the `sec_facts` modules.
//!
//! Two filers on purpose. `store()` is Apple-shaped — the one the bars
//! were measured against. `other_store()` is deliberately unlike it in
//! every field (entity, ticker, CIK, form, fiscal calendar, concepts,
//! and a store with NO unmapped tags and no consolidated-only limit), so
//! a test can prove a derivation generalizes rather than having been
//! written for one company.

use super::SecFactStore;

pub(crate) fn store() -> SecFactStore {
    serde_json::from_value(serde_json::json!({
        "schema": 1,
        "entity": "Apple Inc.",
        "ticker": "AAPL",
        "cik": "0000320193",
        "as_of": {
            "form": "10-K",
            "accession": "0000320193-25-000079",
            "filed": "2025-10-31",
            "latest_period_end": "2025-09-27"
        },
        "concepts": {
            "revenue": {
                "label": "Total revenue (net sales)",
                "kind": "duration",
                "ask_terms": ["revenue", "net sales", "sales"],
                "facts": [
                    {"value": 391035000000.0, "unit": "USD",
                     "start": "2023-10-01", "end": "2024-09-28", "fiscal_year": 2024,
                     "tag": "us-gaap:RevenueFromContractWithCustomerExcludingAssessedTax",
                     "accession": "0000320193-24-000123", "form": "10-K", "filed": "2024-11-01"},
                    {"value": 416161000000.0, "unit": "USD",
                     "start": "2024-09-29", "end": "2025-09-27", "fiscal_year": 2025,
                     "tag": "us-gaap:RevenueFromContractWithCustomerExcludingAssessedTax",
                     "accession": "0000320193-25-000079", "form": "10-K", "filed": "2025-10-31"}
                ]
            },
            "gross_profit": {
                "label": "Gross profit (gross margin)",
                "kind": "duration",
                "ask_terms": ["gross profit", "gross margin"],
                "facts": [
                    {"value": 195201000000.0, "unit": "USD",
                     "start": "2024-09-29", "end": "2025-09-27", "fiscal_year": 2025,
                     "tag": "us-gaap:GrossProfit",
                     "accession": "0000320193-25-000079", "form": "10-K", "filed": "2025-10-31"}
                ]
            },
            "advertising_expense": {
                "label": "Advertising expense",
                "kind": "duration",
                "facts": [
                    {"value": 1800000000.0, "unit": "USD",
                     "start": "2014-09-28", "end": "2015-09-26", "fiscal_year": 2015,
                     "tag": "us-gaap:AdvertisingExpense",
                     "accession": "0000320193-15-000106", "form": "10-K", "filed": "2015-10-28"}
                ]
            },
            "total_assets": {
                "label": "Total assets",
                "kind": "instant",
                "facts": [
                    {"value": 359241000000.0, "unit": "USD",
                     "start": null, "end": "2025-09-27", "fiscal_year": 2025,
                     "tag": "us-gaap:Assets",
                     "accession": "0000320193-25-000079", "form": "10-K", "filed": "2025-10-31"}
                ]
            }
        },
        "coverage": {
            "filer_tags_total": 503,
            "covered_tags": 24,
            "unmapped_tags": 479,
            "consolidated_only": true
        }
    }))
    .expect("fixture parses")
}

/// A SECOND filer, nothing like Apple: different entity, ticker, CIK,
/// form, fiscal calendar, concepts, and a store with NO unmapped tags
/// and no consolidated-only limit. Used to prove the card generalizes
/// with zero new copy (§7.7(3)).
pub(crate) fn other_store() -> SecFactStore {
    serde_json::from_value(serde_json::json!({
        "schema": 1,
        "entity": "Contoso Pharmaceuticals PLC",
        "ticker": "CTSO",
        "cik": "0000999999",
        "as_of": {
            "form": "20-F",
            "accession": "0000999999-24-000001",
            "filed": "2024-03-15",
            "latest_period_end": "2023-12-31"
        },
        "concepts": {
            "research_and_development": {
                "label": "Research and development expense",
                "kind": "duration",
                "facts": [
                    {"value": 1200000.0, "unit": "USD",
                     "start": "2022-01-01", "end": "2022-12-31", "fiscal_year": 2022,
                     "tag": "us-gaap:ResearchAndDevelopmentExpense",
                     "accession": "0000999999-23-000001", "form": "20-F", "filed": "2023-03-15"},
                    {"value": 1500000.0, "unit": "USD",
                     "start": "2023-01-01", "end": "2023-12-31", "fiscal_year": 2023,
                     "tag": "us-gaap:ResearchAndDevelopmentExpense",
                     "accession": "0000999999-24-000001", "form": "20-F", "filed": "2024-03-15"}
                ]
            }
        },
        "coverage": {
            "filer_tags_total": 12,
            "covered_tags": 12,
            "unmapped_tags": 0,
            "consolidated_only": false
        }
    }))
    .expect("fixture parses")
}
