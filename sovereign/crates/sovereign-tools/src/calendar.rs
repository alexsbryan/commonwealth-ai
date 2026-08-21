// SPDX-License-Identifier: AGPL-3.0-or-later

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

/// CalDAV calendar tool for reading and creating events.
pub struct CalendarTool {
    caldav_url: String,
    username: String,
    password: String,
    client: reqwest::Client,
}

impl CalendarTool {
    pub fn new(caldav_url: &str, username: &str, password: &str) -> Self {
        Self {
            caldav_url: caldav_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

impl CalendarTool {
    /// Bind this tool's state to its `calendar` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("calendar", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `calendar`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<StepOutput> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("Missing 'action' parameter".to_string()))?;

        match action {
            "list" => {
                // PROPFIND to list events.
                let response = self
                    .client
                    .request(
                        reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
                        &self.caldav_url,
                    )
                    .basic_auth(&self.username, Some(&self.password))
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml")
                    .body(
                        r#"<?xml version="1.0"?>
                        <propfind xmlns="DAV:">
                            <prop><displayname/><getcontenttype/></prop>
                        </propfind>"#,
                    )
                    .send()
                    .await
                    .map_err(|e| Error::Execution(format!("CalDAV request failed: {e}")))?;

                let text = response.text().await.map_err(|e| {
                    Error::Execution(format!("Failed to read CalDAV response: {e}"))
                })?;

                Ok(StepOutput::Text(text))
            }
            "create" => {
                let summary = params
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::InvalidInput("Missing 'summary' for create".to_string())
                    })?;

                let start = params
                    .get("start")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::InvalidInput("Missing 'start' for create".to_string()))?;

                let end = params
                    .get("end")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::InvalidInput("Missing 'end' for create".to_string()))?;

                let uid = uuid::Uuid::new_v4();
                let ical = format!(
                    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n\
                     UID:{uid}\r\nDTSTART:{start}\r\nDTEND:{end}\r\n\
                     SUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR"
                );

                let event_url = format!("{}/{uid}.ics", self.caldav_url);
                let response = self
                    .client
                    .put(&event_url)
                    .basic_auth(&self.username, Some(&self.password))
                    .header("Content-Type", "text/calendar")
                    .body(ical)
                    .send()
                    .await
                    .map_err(|e| Error::Execution(format!("CalDAV create failed: {e}")))?;

                if response.status().is_success() || response.status().as_u16() == 201 {
                    Ok(StepOutput::Text(format!(
                        "Event created: {summary} ({start} to {end})"
                    )))
                } else {
                    Err(Error::Execution(format!(
                        "CalDAV returned {}",
                        response.status()
                    )))
                }
            }
            _ => Err(Error::InvalidInput(format!("Unknown action: {action}"))),
        }
    }
}
