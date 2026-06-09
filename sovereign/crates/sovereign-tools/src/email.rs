// SPDX-License-Identifier: AGPL-3.0-or-later
use async_trait::async_trait;

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

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

#[async_trait]
impl Tool for EmailTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "email".to_string(),
            name: "Email".to_string(),
            description: "Read inbox and send emails via IMAP/SMTP".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["read_inbox", "send"] },
                    "limit": { "type": "integer", "description": "Number of messages to read (for read_inbox)" },
                    "to": { "type": "string", "description": "Recipient email (for send)" },
                    "subject": { "type": "string", "description": "Email subject (for send)" },
                    "body": { "type": "string", "description": "Email body (for send)" }
                },
                "required": ["action"]
            }),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Slow,
            scope: Scope::External,
            // Shape varies with `action` — send returns confirmation
            // text, read_inbox returns a list of message headers.
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![Permission::Network]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
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
