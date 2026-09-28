//! Resolve stored media before sending a display command to an existing device app.

use extend_protocol::model::CommandRequest;

use crate::error::{AppError, AppResult};
use crate::state::{Auth, Shared};

pub(crate) const MAX_ATTACHMENTS: usize = 8;
pub(crate) const MAX_ATTACHMENT_BYTES: usize = 8 << 20;

/// The one media argument, as (argument index, inline flag, media kind, value).
fn media_arg(args: &[String]) -> Option<(usize, bool, &str, &str)> {
    if args.first().map(String::as_str) != Some("show") {
        return None;
    }
    let mut found = None;
    let mut i = 1;
    while i < args.len() {
        let token = &args[i];
        if token == "--" {
            break;
        }
        let (flag, inline) = token
            .split_once('=')
            .map_or((token.as_str(), None), |(f, v)| (f, Some(v)));
        if matches!(flag, "--image" | "--video" | "--url" | "--text") {
            if found.is_some() {
                return None;
            }
            let value = inline.or_else(|| args.get(i + 1).map(String::as_str))?;
            found = Some((
                if inline.is_some() { i } else { i + 1 },
                inline.is_some(),
                &flag[2..],
                value,
            ));
            if inline.is_none() {
                i += 1;
            }
        }
        i += 1;
    }
    found.filter(|(_, _, kind, _)| matches!(*kind, "image" | "video"))
}

pub(crate) async fn resolve(
    state: &Shared,
    auth: &Auth,
    req: &mut CommandRequest,
    attachment_bytes: usize,
) -> AppResult<()> {
    if req.command.trim() != "display" {
        return Ok(());
    }
    let Some((index, inline, kind, value)) = media_arg(&req.args) else {
        return Ok(());
    };
    if value.starts_with("attachment:") || req.attachments.iter().any(|a| a.name == value) {
        return Ok(());
    }
    let attachment = super::files::display_attachment(
        state,
        auth,
        value,
        kind,
        MAX_ATTACHMENT_BYTES.saturating_sub(attachment_bytes),
    )
    .await?;
    if let Some(attachment) = attachment {
        if req.attachments.len() == MAX_ATTACHMENTS {
            return Err(AppError::invalid(
                "Attachments are limited to 8 files and 8 MiB in total.",
            ));
        }
        let value = format!("attachment:{}", attachment.name);
        req.args[index] = if inline { format!("--{kind}={value}") } else { value };
        req.attachments.push(attachment);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_flags_respect_values_and_the_end_of_options() {
        let args = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            media_arg(&args(&["show", "--image=file:abc"])),
            Some((1, true, "image", "file:abc"))
        );
        assert_eq!(
            media_arg(&args(&["show", "--video", "file:abc"])),
            Some((2, false, "video", "file:abc"))
        );
        for tokens in [
            &["clear", "--image", "file:abc"][..],
            &["show", "--", "--image", "file:abc"],
            &["show", "--text", "--image=file:abc"],
            &["show", "--image", "file:abc", "--url", "https://example.test"],
            &["show", "--image"],
        ] {
            assert!(media_arg(&args(tokens)).is_none(), "{tokens:?}");
        }
    }
}
