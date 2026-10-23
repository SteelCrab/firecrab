use crate::model::escape;
use std::collections::BTreeMap;

fn safe_url(url: &str) -> bool {
    url.starts_with("https://")
        && url.len() > 8
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>\"'".contains(c))
}
fn link(label: &str, url: &str) -> String {
    if safe_url(url) {
        format!(
            "<a href=\"{}\" target=\"_blank\" rel=\"noreferrer noopener\">{}</a>",
            escape(url),
            escape(label)
        )
    } else {
        escape(label)
    }
}
fn inline(mut text: &str, refs: &BTreeMap<&str, &str>) -> String {
    let mut out = String::new();
    while !text.is_empty() {
        let token = if let Some(rest) = text.strip_prefix('`') {
            rest.find('`')
                .map(|end| (end + 2, format!("<code>{}</code>", escape(&rest[..end]))))
        } else if let Some(rest) = text.strip_prefix("**") {
            rest.find("**").map(|end| {
                (
                    end + 4,
                    format!("<strong>{}</strong>", escape(&rest[..end])),
                )
            })
        } else if let Some(rest) = text.strip_prefix('[') {
            rest.find(']').and_then(|end| {
                let label = &rest[..end];
                let tail = &rest[end + 1..];
                if let Some(url) = tail.strip_prefix('(') {
                    url.find(')')
                        .map(|close| (end + close + 4, link(label, &url[..close])))
                } else {
                    refs.get(label).map(|url| (end + 2, link(label, url)))
                }
            })
        } else {
            None
        };
        if let Some((length, html)) = token {
            out.push_str(&html);
            text = &text[length..];
        } else {
            let ch = text.chars().next().unwrap();
            out.push_str(&escape(&ch.to_string()));
            text = &text[ch.len_utf8()..];
        }
    }
    out
}
fn flush(text: &mut String, list: bool, refs: &BTreeMap<&str, &str>, out: &mut String) {
    if text.is_empty() {
        return;
    }
    let tag = if list { "li" } else { "p" };
    out.push_str(&format!("<{tag}>{}</{tag}>", inline(text, refs)));
    text.clear();
}
/// Render the same small Markdown subset as the former dashboard. Raw HTML is text.
pub fn render(markdown: &str) -> String {
    let normalized = markdown.replace("\r\n", "\n").replace('\r', "\n");
    let mut refs = BTreeMap::new();
    let mut lines = Vec::new();
    for line in normalized.lines() {
        if let Some((label, url)) = line
            .strip_prefix('[')
            .and_then(|line| line.split_once("]:"))
        {
            if safe_url(url.trim()) {
                refs.insert(label, url.trim());
            }
        } else {
            lines.push(line);
        }
    }
    let mut out = String::new();
    let mut paragraph = String::new();
    let mut list = false;
    let mut code: Option<Vec<&str>> = None;
    for raw in lines {
        let line = raw.trim();
        if let Some(block) = &mut code {
            if line.starts_with("```") {
                out.push_str(&format!(
                    "<pre><code>{}</code></pre>",
                    escape(&block.join("\n"))
                ));
                code = None;
            } else {
                block.push(raw);
            }
            continue;
        }
        let heading = line.strip_prefix('#').and_then(|rest| {
            let text = rest.trim_start_matches('#');
            text.strip_prefix(' ')
        });
        let item = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "));
        if line.is_empty() || line.starts_with("```") || heading.is_some() || item.is_some() {
            flush(&mut paragraph, list, &refs, &mut out);
            if item.is_none() && list {
                out.push_str("</ul>");
                list = false;
            }
            if line.starts_with("```") {
                code = Some(Vec::new());
            } else if let Some(heading) = heading {
                out.push_str(&format!("<h4>{}</h4>", escape(heading)));
            } else if let Some(item) = item {
                if !list {
                    out.push_str("<ul>");
                    list = true;
                }
                paragraph.push_str(item);
            }
        } else {
            if !paragraph.is_empty() {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
        }
    }
    if let Some(code) = code {
        out.push_str(&format!(
            "<pre><code>{}</code></pre>",
            escape(&code.join("\n"))
        ));
    }
    flush(&mut paragraph, list, &refs, &mut out);
    if list {
        out.push_str("</ul>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notes_keep_wrapped_items_code_and_reference_links() {
        let result = render(
            "### Added\n\n- **bold** and `code` [#12]\n  wrapped line\n\n```sh\nfirecrab update --apply\n```\n\n[#12]: https://example.com/12",
        );
        assert!(result.contains("<h4>Added</h4>"));
        assert!(result.contains("<strong>bold</strong> and <code>code</code>"));
        assert!(result.contains(">#12</a> wrapped line</li>"));
        assert!(result.contains("<pre><code>firecrab update --apply</code></pre>"));
    }
    #[test]
    fn untrusted_html_links_and_unicode_stay_safe() {
        let result = render(
            "한글 <img src=x onerror=alert(1)> [click](javascript:alert(1)) [x](https://example.com/\"x) [#missing]",
        );
        assert!(!result.contains("<img"));
        assert!(!result.contains("<a "));
        assert!(result.contains("한글"));
        assert!(result.contains("[#missing]"));
    }
}
