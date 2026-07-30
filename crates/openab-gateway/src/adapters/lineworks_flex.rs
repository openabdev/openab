//! Markdown → LINE WORKS flexible-template (flex) rendering.
//!
//! LINE WORKS delivers bot replies as plain text by default, so agent
//! markdown (headings, code fences, lists, emphasis) renders literally. The
//! flexible template shares the LINE Flex Message JSON spec, which is enough
//! to approximate markdown: bold/size for headings, a shaded box for code
//! fences, spans for inline emphasis.
//!
//! Renderer contract:
//! - `markdown_to_flex` returns `None` when the text has no markdown markers
//!   (plain text is cheaper and lands identically) or when the produced
//!   bubble would exceed the platform size limits — callers then fall back
//!   to the plain-text path. Dispatch also falls back on any API rejection,
//!   so flex is strictly best-effort.

use serde_json::{json, Value};

/// Conservative ceiling on the serialized bubble size. The platform rejects
/// oversized flex payloads; staying well under keeps the fallback rare.
const MAX_FLEX_BYTES: usize = 20_000;
/// Ceiling on body components — deep bubbles degrade on mobile clients.
const MAX_COMPONENTS: usize = 120;
/// altText is capped at 400 chars by the API.
const ALT_TEXT_MAX: usize = 400;

/// Ordered-list line prefix: 1–3 leading digits followed by ". " (matches
/// GitHub-flavored markdown lists like "1. x" and "10. x").
fn ordered_list_prefix(t: &str) -> bool {
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    (1..=3).contains(&digits) && t[digits..].starts_with(". ")
}

/// Quick scan: does this text contain markdown the flex renderer improves?
fn has_markdown(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with('#') && t.chars().take_while(|c| *c == '#').count() <= 6
            || t.starts_with("```")
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("> ")
            || ordered_list_prefix(t)
    }) || text.contains("**")
        || text.contains('`')
}

/// Parse inline markdown (`**bold**`, `*italic*`, `` `code` ``) into flex
/// text spans. Unterminated markers render literally.
fn inline_spans(text: &str) -> Vec<Value> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = text;

    let flush = |plain: &mut String, spans: &mut Vec<Value>| {
        if !plain.is_empty() {
            spans.push(json!({"type": "span", "text": std::mem::take(plain)}));
        }
    };

    while !rest.is_empty() {
        let (marker, style): (&str, fn(&str) -> Value) = if rest.starts_with("**") {
            (
                "**",
                |t| json!({"type": "span", "text": t, "weight": "bold"}),
            )
        } else if rest.starts_with('`') {
            (
                "`",
                |t| json!({"type": "span", "text": t, "color": "#D63384"}),
            )
        } else if rest.starts_with('*') {
            (
                "*",
                |t| json!({"type": "span", "text": t, "style": "italic"}),
            )
        } else {
            let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
            plain.push_str(&rest[..ch_len]);
            rest = &rest[ch_len..];
            continue;
        };
        match rest[marker.len()..].find(marker) {
            Some(end) if end > 0 => {
                flush(&mut plain, &mut spans);
                spans.push(style(&rest[marker.len()..marker.len() + end]));
                rest = &rest[marker.len() + end + marker.len()..];
            }
            _ => {
                plain.push_str(marker);
                rest = &rest[marker.len()..];
            }
        }
    }
    flush(&mut plain, &mut spans);
    spans
}

fn text_component(line: &str, size: &str) -> Value {
    let spans = inline_spans(line);
    // A single unstyled span collapses to a plain text component.
    if spans.len() == 1
        && spans[0].get("weight").is_none()
        && spans[0].get("color").is_none()
        && spans[0].get("style").is_none()
    {
        return json!({"type": "text", "text": spans[0]["text"], "wrap": true, "size": size});
    }
    json!({"type": "text", "wrap": true, "size": size, "contents": spans, "text": " "})
}

fn code_box(lines: &[&str]) -> Value {
    let texts: Vec<Value> = lines
        .iter()
        .map(|l| {
            let shown = if l.is_empty() { " " } else { *l };
            json!({"type": "text", "text": shown, "wrap": true, "size": "xs", "color": "#333333"})
        })
        .collect();
    json!({
        "type": "box",
        "layout": "vertical",
        "backgroundColor": "#F5F5F5",
        "cornerRadius": "6px",
        "paddingAll": "8px",
        "margin": "sm",
        "contents": texts
    })
}

/// Render markdown into `(altText, bubble)`. `None` = use plain text.
pub fn markdown_to_flex(text: &str) -> Option<(String, Value)> {
    if !has_markdown(text) {
        return None;
    }

    let mut components: Vec<Value> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            // Fenced code block: collect until the closing fence.
            let start = i + 1;
            let mut end = start;
            while end < lines.len() && !lines[end].trim_start().starts_with("```") {
                end += 1;
            }
            components.push(code_box(&lines[start..end]));
            i = if end < lines.len() { end + 1 } else { end };
            continue;
        }
        if trimmed.is_empty() {
            i += 1;
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            let title = trimmed[hashes + 1..].trim();
            let size = match hashes {
                1 => "xl",
                2 => "lg",
                _ => "md",
            };
            components.push(json!({
                "type": "text", "text": title, "weight": "bold",
                "size": size, "wrap": true, "margin": "md"
            }));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            components.push(text_component(&format!("• {item}"), "sm"));
        } else if ordered_list_prefix(trimmed) {
            components.push(text_component(trimmed, "sm"));
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            components.push(json!({
                "type": "text", "text": quote, "wrap": true,
                "size": "sm", "color": "#888888", "style": "italic"
            }));
        } else {
            components.push(text_component(trimmed, "sm"));
        }
        i += 1;
    }

    if components.is_empty() || components.len() > MAX_COMPONENTS {
        return None;
    }

    let bubble = json!({
        "type": "bubble",
        "body": {
            "type": "box",
            "layout": "vertical",
            "spacing": "sm",
            "contents": components
        }
    });
    if serde_json::to_string(&bubble)
        .map(|s| s.len())
        .unwrap_or(usize::MAX)
        > MAX_FLEX_BYTES
    {
        return None;
    }

    let mut alt: String = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if alt.chars().count() > ALT_TEXT_MAX {
        alt = alt.chars().take(ALT_TEXT_MAX - 1).collect::<String>() + "…";
    }
    Some((alt, bubble))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_returns_none() {
        assert!(markdown_to_flex("just a plain sentence").is_none());
        assert!(markdown_to_flex("多行\n純文字\n也一樣").is_none());
    }

    #[test]
    fn heading_renders_bold_sized() {
        let (_alt, bubble) = markdown_to_flex("# 標題\n內文").unwrap();
        let body = &bubble["body"]["contents"];
        assert_eq!(body[0]["text"], "標題");
        assert_eq!(body[0]["weight"], "bold");
        assert_eq!(body[0]["size"], "xl");
        assert_eq!(body[1]["text"], "內文");
    }

    #[test]
    fn code_fence_renders_shaded_box() {
        let (_alt, bubble) = markdown_to_flex("```\nlet x = 1;\nlet y = 2;\n```").unwrap();
        let boxed = &bubble["body"]["contents"][0];
        assert_eq!(boxed["type"], "box");
        assert_eq!(boxed["backgroundColor"], "#F5F5F5");
        assert_eq!(boxed["contents"][0]["text"], "let x = 1;");
        assert_eq!(boxed["contents"][1]["text"], "let y = 2;");
    }

    #[test]
    fn unclosed_code_fence_consumes_rest() {
        let (_alt, bubble) = markdown_to_flex("```\nno closing fence").unwrap();
        let boxed = &bubble["body"]["contents"][0];
        assert_eq!(boxed["contents"][0]["text"], "no closing fence");
    }

    #[test]
    fn list_items_get_bullets() {
        let (_alt, bubble) = markdown_to_flex("- 甲\n- 乙\n1. 丙").unwrap();
        let body = &bubble["body"]["contents"];
        assert_eq!(body[0]["text"], "• 甲");
        assert_eq!(body[1]["text"], "• 乙");
        assert_eq!(body[2]["text"], "1. 丙");
    }

    #[test]
    fn multi_digit_ordered_lists_render_rich() {
        // "10. item" must be detected as markdown and rendered as a list line.
        let (_alt, bubble) =
            markdown_to_flex("9. nine\n10. ten\n123. many").expect("multi-digit list is markdown");
        let body = &bubble["body"]["contents"];
        assert_eq!(body[0]["text"], "9. nine");
        assert_eq!(body[1]["text"], "10. ten");
        assert_eq!(body[2]["text"], "123. many");
        // 4+ digits is not treated as a list prefix (matches GFM practice).
        assert!(!ordered_list_prefix("1234. nope"));
        assert!(ordered_list_prefix("10. yes"));
        assert!(!ordered_list_prefix("10.no-space"));
    }

    #[test]
    fn inline_bold_and_code_become_spans() {
        let (_alt, bubble) = markdown_to_flex("這是 **重點** 和 `code` 測試\n- x").unwrap();
        let spans = &bubble["body"]["contents"][0]["contents"];
        assert_eq!(spans[0]["text"], "這是 ");
        assert_eq!(spans[1]["text"], "重點");
        assert_eq!(spans[1]["weight"], "bold");
        assert_eq!(spans[3]["text"], "code");
        assert_eq!(spans[3]["color"], "#D63384");
    }

    #[test]
    fn unterminated_markers_render_literally() {
        let spans = inline_spans("a ** b ` c");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["text"], "a ** b ` c");
    }

    #[test]
    fn alt_text_capped_at_400_chars() {
        let long = format!("# t\n{}", "字".repeat(1000));
        let (alt, _bubble) = markdown_to_flex(&long).unwrap();
        assert!(alt.chars().count() <= 400);
        assert!(alt.ends_with('…'));
    }

    #[test]
    fn oversized_input_falls_back() {
        // Hundreds of list items exceed the component ceiling → plain text.
        let big = (0..500)
            .map(|i| format!("- item {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(markdown_to_flex(&big).is_none());
    }
}
