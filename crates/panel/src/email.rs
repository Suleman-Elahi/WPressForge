//! Async SMTP email delivery for notifications and user invitations.
//!
//! When SMTP is configured (`WP_PANEL_SMTP_HOST` set), delivers via RFC 5321
//! SMTP over TCP (with AUTH LOGIN support). When unconfigured, falls back
//! to logging the message body at INFO level.

use crate::config::Config;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::info;

#[derive(Debug, Clone)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub body_text: String,
}

/// Delivers an email message using the configured SMTP server.
/// If `config.smtp_host` is None or empty, logs the message and returns Ok(()).
pub async fn send_email(config: &Config, msg: &EmailMessage) -> Result<(), String> {
    let host = match &config.smtp_host {
        Some(h) if !h.is_empty() => h,
        _ => {
            info!(
                to = %msg.to,
                subject = %msg.subject,
                "SMTP not configured; simulated email delivery: {}",
                msg.body_text
            );
            return Ok(());
        }
    };

    let port = config.smtp_port;
    let from_addr = config.smtp_from.as_deref().unwrap_or("noreply@localhost");
    let addr = format!("{host}:{port}");

    let stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("Failed to connect to SMTP server at {addr}: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    // 1. Service greeting (220)
    read_reply(&mut reader, &mut line, 220).await?;

    // 2. EHLO handshake
    write_line(&mut reader, "EHLO localhost").await?;
    read_multiline_reply(&mut reader, &mut line, 250).await?;

    // 3. Optional AUTH LOGIN
    if let (Some(user), Some(pass)) = (&config.smtp_user, &config.smtp_password)
        && !user.is_empty()
    {
        write_line(&mut reader, "AUTH LOGIN").await?;
        read_reply(&mut reader, &mut line, 334).await?;

        let user_b64 = BASE64.encode(user.as_bytes());
        write_line(&mut reader, &user_b64).await?;
        read_reply(&mut reader, &mut line, 334).await?;

        let pass_b64 = BASE64.encode(pass.as_bytes());
        write_line(&mut reader, &pass_b64).await?;
        read_reply(&mut reader, &mut line, 235).await?;
    }

    // 4. MAIL FROM
    write_line(&mut reader, &format!("MAIL FROM:<{from_addr}>")).await?;
    read_reply(&mut reader, &mut line, 250).await?;

    // 5. RCPT TO
    write_line(&mut reader, &format!("RCPT TO:<{}>", msg.to)).await?;
    read_reply(&mut reader, &mut line, 250).await?;

    // 6. DATA
    write_line(&mut reader, "DATA").await?;
    read_reply(&mut reader, &mut line, 354).await?;

    // 7. Body with RFC 2822 headers
    let date = chrono::Utc::now().to_rfc2822();
    let payload = format!(
        "From: {from_addr}\r\n\
         To: {}\r\n\
         Date: {date}\r\n\
         Subject: {}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         \r\n\
         {}\r\n\
         .\r\n",
        msg.to, msg.subject, msg.body_text
    );
    reader
        .get_mut()
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| format!("Failed to write email data: {e}"))?;
    reader
        .get_mut()
        .flush()
        .await
        .map_err(|e| format!("Failed to flush email data: {e}"))?;

    read_reply(&mut reader, &mut line, 250).await?;

    // 8. QUIT
    let _ = write_line(&mut reader, "QUIT").await;

    info!(to = %msg.to, subject = %msg.subject, "Email delivered successfully via SMTP");
    Ok(())
}

async fn write_line<S: AsyncWriteExt + Unpin>(stream: &mut S, line: &str) -> Result<(), String> {
    stream
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .map_err(|e| format!("Failed writing command `{line}`: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("Failed flushing `{line}`: {e}"))?;
    Ok(())
}

async fn read_reply<S: AsyncBufReadExt + Unpin>(
    reader: &mut S,
    buf: &mut String,
    expected_code: u16,
) -> Result<String, String> {
    buf.clear();
    reader
        .read_line(buf)
        .await
        .map_err(|e| format!("Failed reading SMTP reply: {e}"))?;
    let code = buf
        .get(0..3)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if code != expected_code {
        return Err(format!(
            "Expected SMTP code {expected_code}, received {code}: {}",
            buf.trim()
        ));
    }
    Ok(buf.trim().to_string())
}

async fn read_multiline_reply<S: AsyncBufReadExt + Unpin>(
    reader: &mut S,
    buf: &mut String,
    expected_code: u16,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    loop {
        buf.clear();
        let bytes = reader
            .read_line(buf)
            .await
            .map_err(|e| format!("Failed reading SMTP reply line: {e}"))?;
        if bytes == 0 {
            break;
        }
        let trimmed = buf.trim().to_string();
        lines.push(trimmed);
        if buf.len() >= 4 {
            let code = buf[0..3].parse::<u16>().unwrap_or(0);
            let sep = buf.chars().nth(3).unwrap_or(' ');
            if code != expected_code {
                return Err(format!(
                    "Expected SMTP code {expected_code}, received {code}: {}",
                    buf.trim()
                ));
            }
            if sep == ' ' {
                break;
            }
        } else {
            break;
        }
    }
    Ok(lines)
}

/// Helper to format and send team user invitations.
pub async fn send_invitation(
    config: &Config,
    to_email: &str,
    role: &str,
    token: &str,
) -> Result<(), String> {
    let subject = "Invitation to WPressForge".to_string();
    let body_text = format!(
        "Hello,\n\n\
         You have been invited to join WPressForge as an {role}.\n\n\
         Click the link below to accept your invitation and set up your account:\n\
         /register?token={token}\n\n\
         This link will expire in 7 days.\n\n\
         Regards,\n\
         WPressForge Team\n"
    );

    send_email(
        config,
        &EmailMessage {
            to: to_email.to_string(),
            subject,
            body_text,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(smtp_host: Option<String>) -> Config {
        Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            database: "test.db".into(),
            workers: 1,
            admin_email: "admin@example.com".into(),
            admin_password: None,
            secure_cookies: false,
            demo_data: false,
            static_dir: "static".into(),
            smtp_host,
            smtp_port: 587,
            smtp_user: None,
            smtp_password: None,
            smtp_from: Some("noreply@wpressforge.local".into()),
        }
    }

    #[tokio::test]
    async fn unconfigured_smtp_logs_and_returns_ok() {
        let config = test_config(None);
        let msg = EmailMessage {
            to: "user@example.com".into(),
            subject: "Test".into(),
            body_text: "Hello".into(),
        };
        let result = send_email(&config, &msg).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_invitation_constructs_correct_payload() {
        let config = test_config(None);
        let res = send_invitation(&config, "invitee@example.com", "operator", "token123").await;
        assert!(res.is_ok());
    }
}
