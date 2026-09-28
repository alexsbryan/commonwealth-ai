// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh's model catalogue. `ModelInfo`, `ModelArchitecture` and
//! `ModelAvailability` live in `oicp_types::model_catalog` since
//! pb-mesh-exit-core; re-exported here so every historical path resolves.
pub use oicp_types::model_catalog::{ModelArchitecture, ModelAvailability, ModelInfo};

// The model-transfer wire lives in `oicp_types::model_transfer` (phase-b
// pb-serve-sheds-core); its historical path stays reachable here.
pub use oicp_types::model_transfer::{
    model_file_url, models_list_url, ModelFileInfo, ModelFileListing, MODELS_LIST_PATH,
    MODEL_FILE_ROUTE,
};
