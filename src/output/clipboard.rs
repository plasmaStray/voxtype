//! Clipboard-based text output
//!
//! Uses wl-copy to copy text to the Wayland clipboard.
//! This is the most reliable fallback as it works on all Wayland compositors.
//!
//! Under an X11 session this method is unavailable; the chain falls through
//! to `XclipOutput` (xclip/xsel) instead.
//!
//! Requires: wl-clipboard package installed

use super::session::{detect, DisplaySession};
use super::TextOutput;
use crate::error::OutputError;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Clipboard-based text output
/// MIME type passed to wl-copy for transcribed text. Without it wl-copy
/// guesses the type from the content, which costs ~10 ms per copy and
/// misfiles some text: a transcript starting with "From " is offered only as
/// `application/mbox`, so text fields refuse to paste it (#738). With a text
/// type, wl-copy also offers the `text/plain`, `UTF8_STRING`, `STRING` and
/// `TEXT` aliases that XWayland and older clients ask for.
pub(crate) const WL_COPY_TEXT_MIME: &str = "text/plain;charset=utf-8";

/// Copy text to the Wayland clipboard, optionally asking history managers to
/// omit the selection. Older wl-clipboard versions reject `--sensitive`; in
/// that case retry without the hint so output continues to work.
pub(crate) async fn copy_to_wayland_clipboard(
    text: &[u8],
    mark_sensitive: bool,
) -> Result<(), OutputError> {
    async fn copy(
        text: &[u8],
        mark_sensitive: bool,
    ) -> Result<std::process::ExitStatus, OutputError> {
        let mut command = Command::new("wl-copy");
        command.arg("--type").arg(WL_COPY_TEXT_MIME);
        if mark_sensitive {
            command.arg("--sensitive");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    OutputError::WlCopyNotFound
                } else {
                    OutputError::InjectionFailed(e.to_string())
                }
            })?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text)
                .await
                .map_err(|e| OutputError::InjectionFailed(e.to_string()))?;
        }
        child
            .wait()
            .await
            .map_err(|e| OutputError::InjectionFailed(e.to_string()))
    }

    let status = copy(text, mark_sensitive).await?;
    if status.success() {
        return Ok(());
    }
    if mark_sensitive {
        tracing::debug!("wl-copy rejected --sensitive; retrying without it");
        if copy(text, false).await?.success() {
            return Ok(());
        }
    }
    Err(OutputError::InjectionFailed(
        "wl-copy exited with error".to_string(),
    ))
}

pub struct ClipboardOutput {
    /// Text to append after transcription
    append_text: Option<String>,
    mark_sensitive: bool,
}

impl ClipboardOutput {
    /// Create a new clipboard output
    pub fn new(append_text: Option<String>, mark_sensitive: bool) -> Self {
        Self {
            append_text,
            mark_sensitive,
        }
    }
}

#[async_trait::async_trait]
impl TextOutput for ClipboardOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        if text.is_empty() {
            return Ok(());
        }

        // Prepare text with optional append
        let text = if let Some(ref append) = self.append_text {
            std::borrow::Cow::Owned(format!("{}{}", text, append))
        } else {
            std::borrow::Cow::Borrowed(text)
        };

        copy_to_wayland_clipboard(text.as_bytes(), self.mark_sensitive).await?;

        tracing::info!("Text copied to clipboard ({} chars)", text.len());
        Ok(())
    }

    async fn is_available(&self) -> bool {
        // wl-copy is a no-op without a Wayland compositor (the issue from
        // GitHub #346). Under X11, defer to xclip/xsel via XclipOutput.
        if detect() != DisplaySession::Wayland {
            tracing::debug!("clipboard (wl-copy) skipped: not a Wayland session");
            return false;
        }

        Command::new("which")
            .arg("wl-copy")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn name(&self) -> &'static str {
        "clipboard (wl-copy)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let output = ClipboardOutput::new(None, false);
        assert!(output.append_text.is_none());

        let output = ClipboardOutput::new(Some(" ".to_string()), false);
        assert_eq!(output.append_text, Some(" ".to_string()));
    }
}
