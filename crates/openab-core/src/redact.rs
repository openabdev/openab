//! Rendering session-bearing identifiers in logs without handing out the session.
//!
//! An ACP session is addressed two ways and both carry the same uuid: the session id is
//! `sess_<uuid>` and the channel id is `acp_<uuid>`. Either one is enough to resume — `sess_` is
//! taken directly by resume, and `acp_` differs from it only by prefix — so both are credentials,
//! and a redaction that covers one of them covers nothing.
//!
//! Ids also travel embedded: a pool key is `<platform>:<channel_id>`, so scanning for a field
//! named `channel` misses it entirely. Redaction here matches on the VALUE's shape, which is why
//! it can be applied to a composite without the caller taking it apart.
//!
//! Applying this to a non-ACP identifier is a no-op, deliberately. A Discord or Slack channel id
//! is public and operators grep for it, and because the function leaves it untouched, a caller
//! that cannot tell whether a given id will ever be ACP does not have to find out — applying it
//! is free and removes the question. That is cheaper than an investigation and safer than a guess.

/// Hash any `acp_`/`sess_` prefixed segment of `s`, leaving everything else exactly as it was.
///
/// Segments are split on `:` so a `<platform>:<channel_id>` pool key redacts its id half and keeps
/// the platform readable.
///
/// The same uuid tags identically whichever prefix carried it, so one session reads as one tag
/// across every log line that mentions it — that correlation is the only reason to keep an
/// identifier in a log at all.
pub fn redact_session_ids(s: &str) -> String {
    s.split(':')
        .map(|seg| {
            match seg
                .strip_prefix("acp_")
                .or_else(|| seg.strip_prefix("sess_"))
            {
                Some(uuid) if !uuid.is_empty() => hash_tag(uuid),
                _ => seg.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn hash_tag(uuid: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(uuid.as_bytes());
    let short: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("#{short}")
}

#[cfg(test)]
mod tests {
    use super::redact_session_ids;

    /// A table, not a single vector — the branch structure is what needs pinning.
    ///
    /// One example only proves the hash. The predicate is the part most likely to move: this
    /// function exists because a redaction covering `acp_` but not `sess_` was shipped, and that
    /// edit changes which inputs hash without changing the output for any `acp_` input. A single
    /// `acp_...` vector cannot see it.
    #[test]
    fn both_encodings_hash_alike_and_everything_else_passes_through() {
        let u = "00000000-0000-0000-0000-000000000000";
        let tag = redact_session_ids(&format!("acp_{u}"));
        assert!(
            tag.starts_with('#') && tag.len() == 9,
            "expected #<8hex>, got {tag}"
        );
        assert_eq!(
            redact_session_ids(&format!("sess_{u}")),
            tag,
            "both encodings carry the SAME uuid, so they must produce the same tag or one session \
             reads as two"
        );
        assert_eq!(
            redact_session_ids(&format!("acp:acp_{u}")),
            format!("acp:{tag}"),
            "a <platform>:<id> pool key must redact the id half and keep the platform greppable"
        );
        assert_eq!(
            redact_session_ids("1234567890"),
            "1234567890",
            "public ids stay greppable"
        );
        assert_eq!(
            redact_session_ids("-"),
            "-",
            "the no-session sentinel is not a session"
        );
        assert_eq!(redact_session_ids(""), "", "empty in, empty out");
        assert_eq!(
            redact_session_ids("acp_"),
            "acp_",
            "a bare prefix carries no uuid to hide"
        );
        assert_eq!(
            redact_session_ids("discord:1234567890"),
            "discord:1234567890",
            "a non-ACP composite is untouched, which is why applying this blindly is safe"
        );
    }
}
