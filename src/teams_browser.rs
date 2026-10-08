//! Narrow, platform-independent eligibility checks for Teams browser pages.

/// Chromium window classes are also used by unrelated Electron apps.
pub fn is_teams_browser_process(image_path: &str) -> bool {
    let executable = image_path.rsplit(['\\', '/']).next().unwrap_or_default();
    executable.eq_ignore_ascii_case("msedge.exe") || executable.eq_ignore_ascii_case("chrome.exe")
}

/// Check the document's actual HTTPS authority, never its title or URL path.
/// The caller discards the URL after this check; it must not be logged.
pub fn is_teams_web_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    [
        "teams.microsoft.com",
        "teams.cloud.microsoft",
        "teams.live.com",
    ]
    .iter()
    .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

/// Discover documents inside one browser window without entering their contents.
/// The children callback must return only immediate children of its argument,
/// not siblings from a filtered tree that might have omitted the window itself.
/// Unknown node types stop a branch rather than risking a walk into page content.
pub fn top_level_browser_documents<Node>(
    window: Node,
    mut children: impl FnMut(&Node) -> Vec<Node>,
    mut is_document: impl FnMut(&Node) -> Option<bool>,
) -> Vec<Node> {
    let mut documents = Vec::new();
    let mut pending = children(&window);
    while let Some(node) = pending.pop() {
        match is_document(&node) {
            Some(true) => documents.push(node),
            Some(false) => pending.extend(children(&node)),
            None => {}
        }
    }
    documents
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_and_chrome_processes_are_eligible() {
        for path in [
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Browser\CHROME.EXE",
            "msedge.exe",
        ] {
            assert!(is_teams_browser_process(path), "{path}");
        }
    }

    #[test]
    fn other_electron_apps_and_webviews_are_not_browsers() {
        for path in [
            "",
            r"C:\Apps\Code.exe",
            r"C:\Apps\msedgewebview2.exe",
            r"C:\Apps\ms-teams.exe",
            r"C:\Apps\msedge.exe.backup",
            r"C:\msedge.exe\unrelated.exe",
        ] {
            assert!(!is_teams_browser_process(path), "{path}");
        }
    }

    #[test]
    fn official_teams_https_documents_are_eligible() {
        for url in [
            "https://teams.microsoft.com",
            "https://teams.microsoft.com/v2/",
            "https://teams.cloud.microsoft/v2/",
            "https://teams.live.com/",
            "https://teams.microsoft.com:443/v2/",
            "https://teams.microsoft.com/?view=meeting#call",
            "HTTPS://TEAMS.CLOUD.MICROSOFT/v2/",
        ] {
            assert!(is_teams_web_url(url), "{url}");
        }
    }

    #[test]
    fn titles_and_non_https_pages_cannot_establish_teams_identity() {
        for url in [
            "",
            "Microsoft Teams",
            "teams.microsoft.com",
            "http://teams.microsoft.com/",
            "file:///teams.microsoft.com",
            "https://example.com/Microsoft-Teams",
            "https://example.com/?url=https://teams.microsoft.com/",
            "https://example.com/#https://teams.microsoft.com/",
        ] {
            assert!(!is_teams_web_url(url), "{url}");
        }
    }

    #[test]
    fn lookalike_hosts_and_userinfo_are_rejected() {
        for url in [
            "https://teams.microsoft.com.example.com/",
            "https://notteams.microsoft.com/",
            "https://example.com@teams.microsoft.com/",
            "https://teams.microsoft.com@example.com/",
            "https://teams.cloud.microsoft.example.com/",
            "https://teams.microsoft.com:444/",
        ] {
            assert!(!is_teams_web_url(url), "{url}");
        }
    }

    #[test]
    fn malformed_authorities_are_rejected() {
        for url in [
            " https://teams.microsoft.com/",
            "https:///teams.microsoft.com/",
            "https://teams.microsoft.com /",
            "https://teams.microsoft.com\n/",
            "https://teams.microsoft.com\\@example.com/",
            "https://teams.microsoft.com%2f.example.com/",
        ] {
            assert!(!is_teams_web_url(url), "{url}");
        }
    }

    #[test]
    fn document_traversal_stays_inside_the_selected_browser_window() {
        // Desktop 0 has browser window 1 and unrelated window 2. Only the
        // browser's document 4 is eligible; window 2's document 5 is not.
        let documents = top_level_browser_documents(
            1,
            |node| match node {
                0 => vec![1, 2],
                1 => vec![3],
                2 => vec![5],
                3 => vec![4],
                _ => panic!("must not enumerate a document's contents"),
            },
            |node| Some(matches!(node, 4 | 5)),
        );
        assert_eq!(documents, vec![4]);
    }

    #[test]
    fn document_traversal_stops_before_page_contents_and_nested_frames() {
        let documents = top_level_browser_documents(
            1,
            |node| match node {
                1 => vec![2, 3],
                2 => vec![4],
                3 => vec![5],
                _ => panic!("page contents must not be visited during discovery"),
            },
            |node| Some(matches!(node, 4 | 5)),
        );
        let mut documents = documents;
        documents.sort_unstable();
        assert_eq!(documents, vec![4, 5]);
    }

    #[test]
    fn unreadable_node_does_not_trigger_a_page_content_walk() {
        let documents = top_level_browser_documents(
            1,
            |node| match node {
                1 => vec![2, 3],
                _ => panic!("neither a document nor an unknown node may be expanded"),
            },
            |node| match node {
                2 => Some(true),
                3 => None,
                _ => Some(false),
            },
        );
        assert_eq!(documents, vec![2]);
    }
}
