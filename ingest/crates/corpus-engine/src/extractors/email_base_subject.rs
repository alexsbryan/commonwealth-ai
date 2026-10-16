//! RFC 5256 §2.1 base subject: the subject with its reply and forward marks removed, the key a mailbox
//! with no In-Reply-To or References headers can declare as its thread. The extractor offers it as the
//! `base_subject` field; a recipe chooses it (`document = { thread = "base_subject" }`), code never does.

/// The base subject of `subject`, case-folded, or `None` when nothing is left (RFC 5256 does not thread
/// an empty base subject, so no two such messages share a thread).
pub(crate) fn base_subject(subject: &str) -> Option<String> {
    let mut s = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    loop {
        strip_trailers(&mut s);
        loop {
            let before = s.len();
            if let Some(rest) = leader(&s) {
                s = rest.to_string();
            }
            if let Some(rest) = blob(&s).filter(|rest| !rest.is_empty()) {
                s = rest.to_string();
            }
            if s.len() == before {
                break;
            }
        }
        match fwd_wrapped(&s) {
            Some(inner) => s = inner.to_string(),
            None => break,
        }
    }
    let s = s.trim().to_lowercase();
    (!s.is_empty()).then_some(s)
}

/// Step 2: trailing `(fwd)` marks and whitespace.
fn strip_trailers(s: &mut String) {
    loop {
        let t = s.trim_end();
        let fwd = t
            .get(t.len().saturating_sub(5)..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case("(fwd)"));
        let cut = if fwd { t.len() - 5 } else { t.len() };
        if cut == s.len() {
            return;
        }
        s.truncate(cut);
    }
}

/// Step 3: `subj-leader = (*subj-blob subj-refwd) / WSP`, the remainder after it.
fn leader(s: &str) -> Option<&str> {
    if let Some(rest) = s.strip_prefix(' ') {
        return Some(rest);
    }
    let mut rest = s;
    while let Some(after) = blob(rest) {
        rest = after;
    }
    refwd(rest)
}

/// `subj-refwd = ("re" / ("fw" ["d"])) *WSP [subj-blob] ":"`, the remainder after it.
fn refwd(s: &str) -> Option<&str> {
    let lower = s.get(..3).unwrap_or(s).to_ascii_lowercase();
    let word = ["fwd", "fw", "re"]
        .into_iter()
        .find(|w| lower.starts_with(w))?;
    let mut rest = s[word.len()..].trim_start_matches(' ');
    if let Some(after) = blob(rest) {
        rest = after;
    }
    rest.strip_prefix(':')
}

/// `subj-blob = "[" *BLOBCHAR "]" *WSP`, the remainder after it.
fn blob(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('[')?;
    let end = inner.find([']', '['])?;
    inner[end..]
        .strip_prefix(']')
        .map(|rest| rest.trim_start_matches(' '))
}

/// Step 6: `[fwd: ... ]` around the whole subject, its inside.
fn fwd_wrapped(s: &str) -> Option<&str> {
    let head = s.get(..5)?;
    (head.eq_ignore_ascii_case("[fwd:") && s.ends_with(']')).then(|| &s[5..s.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::base_subject;

    #[test]
    fn reply_and_forward_marks_leave_the_base_subject() {
        for (subject, base) in [
            (
                "RFP for Summit Energy - California sites",
                "rfp for summit energy - california sites",
            ),
            (
                "RE: RFP for Summit Energy - California sites",
                "rfp for summit energy - california sites",
            ),
            ("Re: FW: Re:  Pricing   update", "pricing update"),
            ("Fwd: [list] Re: Pricing update (fwd)", "pricing update"),
            ("[fwd: Re: Pricing update]", "pricing update"),
            ("Re[2]: Pricing update", "pricing update"),
            ("[list-name] Pricing update", "pricing update"),
        ] {
            assert_eq!(base_subject(subject).as_deref(), Some(base), "{subject}");
        }
    }

    #[test]
    fn words_that_only_begin_like_a_mark_are_kept() {
        // "Request" starts with "re" and "Fwiw" with "fw"; neither is followed by ':' so neither is a mark.
        assert_eq!(
            base_subject("Request for Information").as_deref(),
            Some("request for information")
        );
        assert_eq!(
            base_subject("Fwiw: numbers").as_deref(),
            Some("fwiw: numbers")
        );
        assert_eq!(
            base_subject("[only a blob]").as_deref(),
            Some("[only a blob]")
        );
    }

    #[test]
    fn an_empty_base_subject_threads_nothing() {
        for subject in ["", "   ", "Re:", "RE: FW:", "(fwd)"] {
            assert_eq!(base_subject(subject), None, "{subject:?}");
        }
    }
}
