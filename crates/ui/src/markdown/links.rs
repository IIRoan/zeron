//! Original content and validated navigation are deliberately separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub label: String,
    pub original: String,
    pub navigation: Result<String, &'static str>,
}
impl LinkTarget {
    pub fn new(label: &str, original: &str) -> Self {
        Self {
            label: label.into(),
            original: original.into(),
            navigation: crate::browser::model::transcript_address(original),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
    /// Normal click or keyboard activation opens a web URL in the default browser.
    Primary,
    /// Open a resolved workspace file in the editor. Web URLs still open externally.
    Internal,
    External,
    Copy,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkOutcome {
    Internal,
    External(String),
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkActivation {
    pub target: LinkTarget,
    pub action: LinkAction,
    pub source_session: Option<String>,
}
impl LinkActivation {
    pub fn web_outcome(&self) -> LinkOutcome {
        if self.action == LinkAction::Copy {
            return LinkOutcome::Rejected;
        }
        match &self.target.navigation {
            Ok(url) => LinkOutcome::External(url.clone()),
            Err(_) => LinkOutcome::Rejected,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_web_activation_leaves_through_the_external_opener() {
        for url in [
            "https://github.com/acme/repo/pull/12",
            "https://example.com/docs",
            "http://localhost:3000/",
        ] {
            for action in [
                LinkAction::Primary,
                LinkAction::Internal,
                LinkAction::External,
            ] {
                let activation = LinkActivation {
                    target: LinkTarget::new("link", url),
                    action,
                    source_session: None,
                };
                assert_eq!(activation.web_outcome(), LinkOutcome::External(url.into()));
            }
        }
    }

    #[test]
    fn rejected_targets_never_fall_back() {
        for url in [
            "javascript:alert(1)",
            "data:text/plain,hi",
            "mailto:a@b.com",
            "https://user:pass@example.com",
            "https://example.com/\npath",
            "\nhttps://example.com",
            "https://",
            "example.com",
            "https:///path",
            "https://example.com/%GG",
        ] {
            let a = LinkActivation {
                target: LinkTarget::new("safe label", url),
                action: LinkAction::Internal,
                source_session: Some("chat".into()),
            };
            assert_eq!(a.web_outcome(), LinkOutcome::Rejected, "{url}");
        }
    }
    #[test]
    fn labels_and_original_destinations_survive_normalization() {
        let t = LinkTarget::new("Documentation", "HTTPS://EXAMPLE.COM");
        assert_eq!(t.label, "Documentation");
        assert_eq!(t.original, "HTTPS://EXAMPLE.COM");
        assert_eq!(t.navigation, Ok("https://example.com/".into()));
    }
    #[test]
    fn markdown_and_autolinks_share_validation() {
        let tree = super::super::parser::parse_full(
            "[label](https://example.com) and https://example.org/path",
        );
        let super::super::parser::Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!("paragraph")
        };
        let urls: Vec<_> = runs.iter().filter_map(|r| r.style.link.as_ref()).collect();
        assert_eq!(urls.len(), 2);
        assert!(
            urls.iter()
                .all(|u| LinkTarget::new("", u).navigation.is_ok())
        );
    }
}
