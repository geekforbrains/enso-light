//! Slack renders standard Markdown itself. Enso keeps mention syntax inert and
//! splits replies to fit Slack's per-message limit.

use anyhow::{Context, Result, ensure};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag};
use serde_json::{Value, json};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Slack's limit for all `markdown` block text in one message.
const LIMIT: usize = 12_000;

/// Ordered chat.postMessage payloads, each persisted and delivered independently.
/// Markdown goes in a `markdown` block; the raw chunk is the notification fallback.
/// Blank text yields no payloads.
pub fn messages(text: &str, plain: bool) -> Result<Vec<Value>> {
    ensure!(text.len() <= MAX_INPUT_BYTES, "Slack reply exceeds 1 MiB");
    Ok(chunks(text, plain)
        .into_iter()
        .map(|chunk| {
            let blocks = (!plain).then(|| json!([{"type": "markdown", "text": escape(&chunk)}]));
            payload(&chunk, blocks)
        })
        .collect())
}

/// A caller-authored Block Kit message, sent as given with `text` as its
/// notification fallback. Accepts a blocks array or `{"blocks": [...]}`.
pub fn blocks(text: &str, json: &str) -> Result<Value> {
    ensure!(
        !text.trim().is_empty(),
        "Blocks need message text as their notification fallback"
    );
    let mut value: Value = serde_json::from_str(json).context("Blocks are not valid JSON")?;
    let blocks = if value.is_object() {
        value["blocks"].take()
    } else {
        value
    };
    ensure!(
        blocks.as_array().is_some_and(|blocks| !blocks.is_empty()),
        "Blocks must be a non-empty JSON array"
    );
    Ok(payload(text, Some(blocks)))
}

fn payload(text: &str, blocks: Option<Value>) -> Value {
    let mut payload = json!({"text": text, "mrkdwn": false, "parse": "none", "link_names": false,
        "unfurl_links": false, "unfurl_media": false});
    if let Some(blocks) = blocks {
        payload["blocks"] = blocks;
    }
    payload
}

/// Entity-escape `&` and `<` outside code and autolinks, so Slack shows them
/// literally and never parses `<@U…>` or `<!channel>` as a mention. Each chunk
/// is parsed on its own, as Slack will parse it.
fn escape(markdown: &str) -> String {
    let mut literal = Parser::new_ext(markdown, Options::ENABLE_TABLES)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Code(_)
            | Event::Start(
                Tag::CodeBlock(_)
                | Tag::Link {
                    link_type: LinkType::Autolink | LinkType::Email,
                    ..
                },
            ) => Some(range),
            _ => None,
        })
        .peekable();
    let mut escaped = String::with_capacity(markdown.len());
    for (index, c) in markdown.char_indices() {
        while literal.next_if(|range| range.end <= index).is_some() {}
        match c {
            _ if literal.peek().is_some_and(|range| range.start <= index) => escaped.push(c),
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[derive(Clone, Copy)]
enum Kind {
    Fence,
    Table,
    Other,
}

/// Raw chunks that fit Slack's limit once escaped. Splits fall between top-level
/// blocks; an oversized block splits by line, then by character, repeating its
/// code fence or table header so every chunk renders on its own.
fn chunks(text: &str, plain: bool) -> Vec<String> {
    let mut starts = vec![(0, Kind::Other)];
    if !plain {
        let mut depth = 0usize;
        for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
            if depth == 0 && !matches!(event, Event::End(_)) {
                let kind = match event {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => Kind::Fence,
                    Event::Start(Tag::Table(_)) => Kind::Table,
                    _ => Kind::Other,
                };
                let line = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
                starts.push((line, kind));
            }
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth -= 1,
                _ => {}
            }
        }
    }
    let mut chunks = Chunks::default();
    for (index, &(start, kind)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(text.len(), |next| next.0);
        chunks.block(&text[start..end], kind);
    }
    chunks.flush();
    chunks.done
}

/// Escaped size in UTF-16 units: an upper bound on what Slack counts.
fn cost(text: &str) -> usize {
    text.encode_utf16().count() + 4 * text.matches('&').count() + 3 * text.matches('<').count()
}

#[derive(Default)]
struct Chunks {
    done: Vec<String>,
    current: String,
    cost: usize,
}

impl Chunks {
    fn push(&mut self, text: &str) {
        self.current.push_str(text);
        self.cost += cost(text);
    }

    fn flush(&mut self) {
        let chunk = std::mem::take(&mut self.current);
        if !chunk.trim().is_empty() {
            self.done.push(chunk);
        }
        self.cost = 0;
    }

    fn block(&mut self, text: &str, kind: Kind) {
        if self.cost + cost(text) > LIMIT {
            self.flush();
        }
        if self.cost + cost(text) <= LIMIT {
            return self.push(text);
        }
        let lines = match kind {
            Kind::Fence => 1,
            Kind::Table => 2,
            Kind::Other => 0,
        };
        let mut head: String = text.split_inclusive('\n').take(lines).collect();
        let mut close = String::new();
        if let Kind::Fence = kind {
            let fence = head.trim_start();
            let marker = fence.chars().next().unwrap_or('`');
            close = fence.chars().take_while(|&c| c == marker).collect();
        }
        if cost(&head) + cost(&close) > LIMIT / 10 {
            // A pathological fence or header splits like text instead of repeating.
            head.clear();
            close.clear();
        }
        self.push(&head);
        for line in text[head.len()..].split_inclusive('\n') {
            if !self.append(line, &head, &close) {
                for c in line.chars() {
                    self.append(c.encode_utf8(&mut [0; 4]), &head, &close);
                }
            }
        }
    }

    /// Add part of an oversized block, starting a new chunk when needed.
    fn append(&mut self, part: &str, head: &str, close: &str) -> bool {
        let reserve = if close.is_empty() { 0 } else { cost(close) + 1 };
        let fits = |chunks: &Self| chunks.cost + cost(part) + reserve <= LIMIT;
        if !fits(self) && self.current.len() > head.len() {
            if !close.is_empty() {
                if !self.current.ends_with('\n') {
                    self.current.push('\n');
                }
                self.current.push_str(close);
            }
            self.flush();
            self.push(head);
        }
        if !fits(self) {
            return false;
        }
        self.push(part);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(messages: &[Value]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message["blocks"][0]["text"].as_str().unwrap())
            .collect()
    }

    fn fallback(messages: &[Value]) -> String {
        messages
            .iter()
            .map(|message| message["text"].as_str().unwrap())
            .collect()
    }

    fn assert_sendable(messages: &[Value]) {
        for message in messages {
            for (key, value) in [
                ("mrkdwn", json!(false)),
                ("parse", json!("none")),
                ("link_names", json!(false)),
                ("unfurl_links", json!(false)),
                ("unfurl_media", json!(false)),
            ] {
                assert_eq!(message[key], value);
            }
            let text = message["blocks"][0]["text"]
                .as_str()
                .unwrap_or_else(|| message["text"].as_str().unwrap());
            assert!(text.encode_utf16().count() <= LIMIT);
        }
    }

    #[test]
    fn markdown_passes_through_with_mentions_inert_outside_code() {
        let source = "# Report\n\n**Bold**, [docs](https://example.com?a=1&b=2), <https://example.com>.\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n- [x] done\n\n<@U123> <!channel> <b>x</b> a & b `<!here> &amp;`\n\n```html\n<!DOCTYPE html>\n```\n";
        let messages = messages(source, false).unwrap();
        assert_eq!(messages.len(), 1);
        assert_sendable(&messages);
        assert_eq!(fallback(&messages), source);
        assert_eq!(
            markdown(&messages)[0],
            "# Report\n\n**Bold**, [docs](https://example.com?a=1&amp;b=2), <https://example.com>.\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n- [x] done\n\n&lt;@U123> &lt;!channel> &lt;b>x&lt;/b> a &amp; b `<!here> &amp;`\n\n```html\n<!DOCTYPE html>\n```\n"
        );
    }

    #[test]
    fn plain_text_is_unformatted_and_complete() {
        let text = format!("<@U123> **literal** {}", "界".repeat(25_000));
        let messages = messages(&text, true).unwrap();
        assert_eq!(messages.len(), 3);
        assert_sendable(&messages);
        assert_eq!(fallback(&messages), text);
        assert!(
            messages
                .iter()
                .all(|message| message.get("blocks").is_none())
        );
    }

    #[test]
    fn long_replies_split_between_blocks() {
        let source = format!("{}\n\n", "word ".repeat(199)).repeat(30);
        let messages = messages(&source, false).unwrap();
        assert_eq!(messages.len(), 3);
        assert_sendable(&messages);
        assert_eq!(fallback(&messages), source);
        for text in markdown(&messages) {
            assert!(text.starts_with("word ") && text.ends_with("\n\n"));
        }
    }

    #[test]
    fn oversized_code_block_reopens_its_fence_and_language() {
        let code = "if a < b && c { print('<!here>') }\n".repeat(1_000);
        let messages = messages(&format!("Intro\n\n````python\n{code}````\n"), false).unwrap();
        assert!(messages.len() > 3);
        assert_sendable(&messages);
        assert_eq!(markdown(&messages)[0], "Intro\n\n");
        let mut content = String::new();
        for text in &markdown(&messages)[1..] {
            let mut fences = Vec::new();
            for event in Parser::new(text) {
                match event {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                        fences.push(info.into_string())
                    }
                    Event::Text(text) => content.push_str(&text),
                    _ => {}
                }
            }
            assert_eq!(fences, ["python"]);
        }
        assert_eq!(content, code);
    }

    #[test]
    fn oversized_table_repeats_its_header() {
        let header = "| Name | Value |\n| :--- | ---: |\n";
        let mut source = header.to_owned();
        for index in 0..1_000 {
            source.push_str(&format!("| row-{index} | {index} |\n"));
        }
        let messages = messages(&source, false).unwrap();
        assert!(messages.len() > 1);
        assert_sendable(&messages);
        let mut rows = 0;
        for text in markdown(&messages) {
            assert!(text.starts_with(header));
            rows += text.lines().count() - 2;
        }
        assert_eq!(rows, 1_000);
    }

    #[test]
    fn long_lines_split_by_character_and_escaping_counts_toward_the_limit() {
        let source = "a<b&c😀".repeat(5_000);
        let messages = messages(&source, false).unwrap();
        assert!(messages.len() > 5);
        assert_sendable(&messages);
        assert_eq!(fallback(&messages), source);
    }

    #[test]
    fn blank_and_oversized_input() {
        assert!(messages(" \n", false).unwrap().is_empty());
        assert!(messages(&"x".repeat(MAX_INPUT_BYTES + 1), false).is_err());
    }

    #[test]
    fn blocks_are_sent_as_given_with_fallback_text() {
        let table = json!([{"type": "table", "rows": [[{"type": "raw_text", "text": "A"}]]}]);
        for source in [table.to_string(), json!({"blocks": table}).to_string()] {
            let payload = blocks("Summary", &source).unwrap();
            assert_eq!(payload["blocks"], table);
            assert_eq!(payload["text"], "Summary");
            assert_sendable(&[payload]);
        }
        assert!(blocks(" ", &table.to_string()).is_err());
        assert!(blocks("Summary", "[]").is_err());
        assert!(blocks("Summary", r#"{"type": "section"}"#).is_err());
    }
}
