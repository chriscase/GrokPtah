//! Pure merge rules for combine_queued_prompts (#157).
//! Lifted conceptually from upstream xai-prompt-queue/combine.rs.

/// Separator between original follow-ups in the joined model body.
pub const TEXT_SEPARATOR: &str = "\n\n";

/// Adapter-filled gate for one queue row.
#[derive(Debug, Clone, Copy)]
pub struct CombineGate<'a> {
    pub id: &'a str,
    /// Plain user prompt, not bash/command/cron.
    pub is_plain_prompt: bool,
    /// Synthetic / auto-wake origins never combine.
    pub is_synthetic: bool,
    /// Client-expanded skill payload.
    pub is_expanded_skill: bool,
    /// Bash command.
    pub is_bash: bool,
    /// Followers must have no images; front may keep its own.
    pub has_images: bool,
    pub text: &'a str,
}

pub fn can_merge_front(g: &CombineGate<'_>) -> bool {
    g.is_plain_prompt && !g.is_synthetic && !g.is_expanded_skill && !g.is_bash && !g.text.is_empty()
}

pub fn can_merge_follower(g: &CombineGate<'_>, skip_ids: &[&str]) -> bool {
    can_merge_front(g) && !g.has_images && !skip_ids.contains(&g.id)
}

/// Length of the mergeable prefix (including front). `1` means take front only.
pub fn combine_prefix_len<'a>(
    items: impl IntoIterator<Item = CombineGate<'a>>,
    skip_ids: &[&str],
) -> usize {
    let mut iter = items.into_iter();
    let Some(front) = iter.next() else {
        return 0;
    };
    if !can_merge_front(&front) {
        return 1;
    }
    let mut n = 1;
    for next in iter {
        if !can_merge_follower(&next, skip_ids) {
            break;
        }
        n += 1;
    }
    n
}

pub fn join_texts<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(TEXT_SEPARATOR)
}

#[inline]
#[allow(dead_code)]
pub fn is_combined(segs: &[String]) -> bool {
    segs.len() >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g<'a>(
        id: &'a str,
        text: &'a str,
        plain: bool,
        images: bool,
        synth: bool,
    ) -> CombineGate<'a> {
        CombineGate {
            id,
            is_plain_prompt: plain,
            is_synthetic: synth,
            is_expanded_skill: false,
            is_bash: false,
            has_images: images,
            text,
        }
    }

    #[test]
    fn merges_two_plain() {
        let items = [
            g("a", "one", true, false, false),
            g("b", "two", true, false, false),
        ];
        assert_eq!(combine_prefix_len(items, &[]), 2);
        assert_eq!(join_texts(["one", "two"]), "one\n\ntwo");
    }

    #[test]
    fn stops_on_bash() {
        let items = [
            g("a", "one", true, false, false),
            CombineGate {
                id: "b",
                is_plain_prompt: false,
                is_synthetic: false,
                is_expanded_skill: false,
                is_bash: true,
                has_images: false,
                text: "ls",
            },
        ];
        assert_eq!(combine_prefix_len(items, &[]), 1);
    }

    #[test]
    fn follower_with_images_breaks() {
        let items = [
            g("a", "one", true, true, false),
            g("b", "two", true, true, false),
        ];
        // front may have images; follower may not
        assert_eq!(combine_prefix_len(items, &[]), 1);
    }
}
