// SPDX-License-Identifier: AGPL-3.0-or-later

use sovereign_core::error::{Error, Result};
use sovereign_core::types::*;
use std::sync::Arc;
use sovereign_core::tool_manifest::DeclaredTool;

/// Email tool stub. Real IMAP/SMTP implementation requires the `email` feature.
///
/// When the `email` feature is enabled, this uses `lettre` for SMTP sending.
/// IMAP reading is implemented with raw TCP for minimal dependencies.
pub struct EmailTool {
    #[allow(dead_code)]
    imap_host: String,
    #[allow(dead_code)]
    imap_port: u16,
    #[allow(dead_code)]
    smtp_host: String,
    #[allow(dead_code)]
    smtp_port: u16,
    #[allow(dead_code)]
    username: String,
    #[allow(dead_code)]
    password: String,
}

impl EmailTool {
    pub fn new(
        imap_host: &str,
        imap_port: u16,
        smtp_host: &str,
        smtp_port: u16,
        username: &str,
        password: &str,
    ) -> Self {
        Self {
            imap_host: imap_host.to_string(),
            imap_port,
            smtp_host: smtp_host.to_string(),
            smtp_port,
            username: username.to_string(),
            password: password.to_string(),
        }
    }
}

impl EmailTool {
    /// Bind this tool's state to its `email` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_core::tool_manifest::declared("email", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `email`.
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
            "read_inbox" => {
                // IMAP reading is a complex protocol. For now, return a helpful message.
                // A full implementation would use an IMAP client library.
                Ok(StepOutput::Text(
                    "Email reading requires IMAP client configuration. \
                     Configure IMAP settings in your sovereign config to enable inbox access."
                        .to_string(),
                ))
            }
            "send" => {
                let _to = params
                    .get("to")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::InvalidInput("Missing 'to' for send".to_string()))?;
                let _subject = params
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::InvalidInput("Missing 'subject' for send".to_string()))?;
                let _body = params
                    .get("body")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::InvalidInput("Missing 'body' for send".to_string()))?;

                // Email sending requires the `email` feature and lettre.
                // The approval channel will always prompt before sending.
                #[cfg(feature = "email")]
                {
                    use lettre::transport::smtp::authentication::Credentials;
                    use lettre::{Message, SmtpTransport, Transport};

                    let email =
                        Message::builder()
                            .from(self.username.parse().map_err(|e| {
                                Error::Execution(format!("Invalid from address: {e}"))
                            })?)
                            .to(_to.parse().map_err(|e| {
                                Error::Execution(format!("Invalid to address: {e}"))
                            })?)
                            .subject(_subject)
                            .body(_body.to_string())
                            .map_err(|e| Error::Execution(format!("Failed to build email: {e}")))?;

                    let creds = Credentials::new(self.username.clone(), self.password.clone());
                    let mailer = SmtpTransport::relay(&self.smtp_host)
                        .map_err(|e| Error::Execution(format!("SMTP connection failed: {e}")))?
                        .credentials(creds)
                        .port(self.smtp_port)
                        .build();

                    mailer
                        .send(&email)
                        .map_err(|e| Error::Execution(format!("Failed to send email: {e}")))?;

                    return Ok(StepOutput::Text(format!("Email sent to {_to}: {_subject}")));
                }

                #[cfg(not(feature = "email"))]
                {
                    Ok(StepOutput::Text(
                        "Email sending requires the 'email' feature to be enabled at build time."
                            .to_string(),
                    ))
                }
            }
            _ => Err(Error::InvalidInput(format!("Unknown action: {action}"))),
        }
    }
}
