#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub links: Vec<String>,
    pub images: Vec<String>,
    pub scripts: Vec<String>,
    pub styles: Vec<String>,
}

impl Found {
    pub fn group(&self, name: &str) -> Option<&[String]> {
        match name {
            "links" => Some(&self.links),
            "images" => Some(&self.images),
            "scripts" => Some(&self.scripts),
            "styles" => Some(&self.styles),
            _ => None,
        }
    }

    pub fn groups() -> [&'static str; 4] {
        ["images", "links", "scripts", "styles"]
    }
}

fn group_of(tag: &str, attribute: &str, rel: Option<&str>) -> &'static str {
    match (tag, attribute) {
        ("img", _) | ("source", _) => "images",
        ("script", _) => "scripts",
        ("link", _) if rel.is_some_and(|rel| rel.contains("stylesheet")) => "styles",
        _ => "links",
    }
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        rest = &rest[at + name.len()..];

        if before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            continue;
        }
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(rest.len());
            return Some(&rest[..end]);
        }
        let rest = &rest[1..];
        let end = rest.find(quote)?;
        return Some(&rest[..end]);
    }
    None
}

pub fn absolute(base: &str, reference: &str) -> String {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return reference.to_string();
    }
    let scheme_end = base.find("://").map(|at| at + 3).unwrap_or(0);
    if let Some(rest) = reference.strip_prefix("//") {
        return format!("{}{rest}", &base[..scheme_end]);
    }
    let root_end = base[scheme_end..]
        .find('/')
        .map(|at| scheme_end + at)
        .unwrap_or(base.len());
    if reference.starts_with('/') {
        return format!("{}{reference}", &base[..root_end]);
    }
    let folder_end = base
        .rfind('/')
        .filter(|at| *at >= root_end)
        .unwrap_or(root_end);
    format!("{}/{reference}", &base[..folder_end])
}

pub fn scan(base: &str, html: &str) -> Found {
    let mut found = Found::default();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[..close];
        rest = &rest[close + 1..];

        let name: String = tag
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if name.is_empty() {
            continue;
        }
        let rel = attribute(tag, "rel").map(str::to_lowercase);
        for held in ["href", "src"] {
            let Some(reference) = attribute(tag, held) else {
                continue;
            };
            if reference.is_empty()
                || reference.starts_with('#')
                || reference.starts_with("javascript:")
                || reference.starts_with("data:")
                || reference.starts_with("mailto:")
            {
                continue;
            }
            let into = group_of(&name, held, rel.as_deref());
            let full = absolute(base, reference);
            let bucket = match into {
                "images" => &mut found.images,
                "scripts" => &mut found.scripts,
                "styles" => &mut found.styles,
                _ => &mut found.links,
            };
            if !bucket.contains(&full) {
                bucket.push(full);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"
        <html><head>
          <link rel="stylesheet" href="/style.css">
          <link rel="icon" href="favicon.ico">
          <script src="https://cdn.example.net/app.js"></script>
        </head><body>
          <a href="/about">About</a>
          <a href='contact.html'>Contact</a>
          <a href="#top">Top</a>
          <a href="mailto:x@example.org">Mail</a>
          <img src="//images.example.org/logo.png">
          <img src="data:image/png;base64,AAAA">
          <div data-href="/not-a-link"></div>
          <a href="/about">About again</a>
        </body></html>
    "##;

    #[test]
    fn each_reference_lands_in_the_group_its_tag_implies() {
        let found = scan("https://example.org/docs/index.html", PAGE);
        assert_eq!(found.styles, vec!["https://example.org/style.css"]);
        assert_eq!(found.scripts, vec!["https://cdn.example.net/app.js"]);
        assert_eq!(found.images, vec!["https://images.example.org/logo.png"]);
        assert_eq!(
            found.links,
            vec![
                "https://example.org/docs/favicon.ico",
                "https://example.org/about",
                "https://example.org/docs/contact.html",
            ]
        );
    }

    #[test]
    fn anchors_scripts_and_data_urls_are_left_out() {
        let found = scan("https://example.org/", PAGE);
        for held in found.links.iter().chain(found.images.iter()) {
            assert!(!held.contains('#'), "{held}");
            assert!(!held.starts_with("data:"), "{held}");
            assert!(!held.starts_with("mailto:"), "{held}");
        }
    }

    #[test]
    fn the_same_address_is_listed_once() {
        let found = scan("https://example.org/", PAGE);
        assert_eq!(
            found
                .links
                .iter()
                .filter(|held| held.ends_with("/about"))
                .count(),
            1
        );
    }

    #[test]
    fn an_attribute_is_not_matched_inside_a_longer_name() {
        let found = scan("https://example.org/", r#"<div data-href="/no"></div>"#);
        assert!(found.links.is_empty(), "{:?}", found.links);
    }

    #[test]
    fn a_reference_is_resolved_against_the_page_it_was_found_on() {
        let base = "https://example.org/docs/guide.html";
        assert_eq!(absolute(base, "https://other.org/x"), "https://other.org/x");
        assert_eq!(absolute(base, "//cdn.org/x"), "https://cdn.org/x");
        assert_eq!(absolute(base, "/x"), "https://example.org/x");
        assert_eq!(absolute(base, "x"), "https://example.org/docs/x");
        assert_eq!(
            absolute("https://example.org", "/x"),
            "https://example.org/x"
        );
    }
}
