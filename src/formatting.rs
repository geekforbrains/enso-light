//! Slack owns Markdown rendering and platform limits, independently of providers.

use anyhow::{Result, ensure};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MARKDOWN_CHARS: usize = 12_000;
const TABLE_CHARS: usize = 10_000;

/// Render before delivery so each chunk can be persisted independently.
pub fn messages(text: &str, plain: bool) -> Result<Vec<Value>> {
    ensure!(text.len() <= MAX_INPUT_BYTES, "Slack reply exceeds 1 MiB");
    if !plain {
        return markdown_messages(text);
    }
    let text = if text.trim().is_empty() {
        "(No text returned.)"
    } else {
        text
    };
    let chars: Vec<char> = text.chars().collect();
    Ok(chars
        .chunks(12_000)
        .map(|part| payload(&part.iter().collect::<String>(), json!([])))
        .collect())
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Style {
    bold: usize,
    italic: usize,
    strike: usize,
    code: bool,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Span {
    text: String,
    style: Style,
    link: Option<String>,
}

#[derive(Default)]
struct Inline {
    style: Style,
    links: Vec<String>,
}

impl Inline {
    fn event(&mut self, event: Event<'_>, spans: &mut Vec<Span>) {
        match event {
            Event::Start(Tag::Strong) => self.style.bold += 1,
            Event::End(TagEnd::Strong) => self.style.bold -= 1,
            Event::Start(Tag::Emphasis) => self.style.italic += 1,
            Event::End(TagEnd::Emphasis) => self.style.italic -= 1,
            Event::Start(Tag::Strikethrough) => self.style.strike += 1,
            Event::End(TagEnd::Strikethrough) => self.style.strike -= 1,
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                let destination = dest_url.into_string();
                if local_link(&destination) && !self.links.iter().any(|url| local_link(url)) {
                    self.push(spans, &destination, true);
                }
                self.links.push(destination);
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                self.links.pop();
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push(spans, &text, false);
            }
            Event::Code(text) => self.push(spans, &text, true),
            Event::SoftBreak | Event::HardBreak => self.push(spans, "\n", false),
            Event::TaskListMarker(checked) => {
                self.push(spans, if checked { "☑ " } else { "☐ " }, false);
            }
            _ => {}
        }
    }

    fn push(&self, spans: &mut Vec<Span>, text: &str, code: bool) {
        if self.links.iter().any(|url| local_link(url)) {
            return;
        }
        let mut style = self.style.clone();
        style.code = code;
        let span = Span {
            text: text.to_owned(),
            style,
            link: self.links.last().cloned(),
        };
        if let Some(last) = spans.last_mut()
            && last.style == span.style
            && last.link == span.link
        {
            last.text.push_str(text);
        } else {
            spans.push(span);
        }
    }
}

fn local_link(url: &str) -> bool {
    !url.contains(':') || url.starts_with("file:")
}

#[derive(Default)]
struct List {
    next: Option<u64>,
    marker: String,
    started: bool,
}

#[derive(Default)]
struct Prefix {
    first: String,
    rest: String,
}

fn prefix(quotes: usize, lists: &mut [List], heading: Option<usize>) -> Result<Prefix> {
    let quote = "> ".repeat(quotes);
    let mut first = quote.clone();
    let mut rest = quote;
    for list in lists {
        if list.started {
            first.push_str(&" ".repeat(list.marker.len()));
        } else {
            first.push_str(&list.marker);
            list.started = true;
        }
        rest.push_str(&" ".repeat(list.marker.len()));
    }
    if let Some(level) = heading {
        first.push_str(&"#".repeat(level));
        first.push(' ');
    }
    ensure!(first.len() < 1000, "Slack Markdown nesting is too deep");
    Ok(Prefix { first, rest })
}

/// Ordered chat.postMessage fragments without destinations. Every fragment has
/// a complete plain-text notification/accessibility fallback. Oversized inputs
/// fail before delivery admission; no content is silently truncated.
fn markdown_messages(markdown: &str) -> Result<Vec<Value>> {
    ensure!(
        markdown.len() <= MAX_INPUT_BYTES,
        "Slack reply exceeds 1 MiB"
    );
    if markdown.trim().is_empty() {
        return Ok(vec![payload("(No text returned.)", json!([]))]);
    }
    let mut output = Output::default();
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Parser::new_ext(markdown, options);
    let mut inline = Inline::default();
    let mut spans = Vec::new();
    let mut lists: Vec<List> = Vec::new();
    let mut quotes = 0;
    let mut heading = None;
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => heading = Some(level as usize),
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => {
                output.prose(&mut spans, quotes, &mut lists, heading)?;
                heading = None;
            }
            Event::Start(Tag::List(next)) => {
                output.prose(&mut spans, quotes, &mut lists, None)?;
                lists.push(List {
                    next,
                    ..List::default()
                });
            }
            Event::Start(Tag::Item) => {
                if let Some(list) = lists.last_mut() {
                    list.marker = list.next.map_or_else(|| "- ".into(), |n| format!("{n}. "));
                    list.next = list.next.map(|n| n.saturating_add(1));
                    list.started = false;
                }
            }
            Event::End(TagEnd::Item) => {
                output.prose(&mut spans, quotes, &mut lists, None)?;
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => quotes += 1,
            Event::End(TagEnd::BlockQuote(_)) => quotes -= 1,
            Event::Start(Tag::CodeBlock(kind)) => {
                output.prose(&mut spans, quotes, &mut lists, None)?;
                let mut code = String::new();
                for event in events.by_ref() {
                    match event {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(text) => code.push_str(&text),
                        _ => {}
                    }
                }
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                output.code(&code, &language, prefix(quotes, &mut lists, None)?)?;
            }
            Event::Start(Tag::Table(alignments)) => {
                output.prose(&mut spans, quotes, &mut lists, None)?;
                let mut rows = Vec::new();
                let mut row = Vec::new();
                let mut cell = Vec::new();
                let mut cell_inline = Inline::default();
                for event in events.by_ref() {
                    match event {
                        Event::End(TagEnd::Table) => break,
                        Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut cell)),
                        Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                            rows.push(std::mem::take(&mut row))
                        }
                        event => cell_inline.event(event, &mut cell),
                    }
                }
                output.table(&rows, &alignments)?;
            }
            Event::Rule => output.markdown("---".into(), "────".into()),
            event => inline.event(event, &mut spans),
        }
    }
    output.prose(&mut spans, quotes, &mut lists, None)?;
    output.flush();
    if output.messages.is_empty() {
        output
            .messages
            .push(payload("(No text returned.)", json!([])));
    }
    Ok(output.messages)
}

#[derive(Clone, PartialEq, Eq)]
struct Wrapper {
    open: String,
    close: String,
    fallback_suffix: String,
}

impl Wrapper {
    fn style(marker: &str) -> Self {
        Self {
            open: marker.into(),
            close: marker.into(),
            fallback_suffix: String::new(),
        }
    }
}

fn wrappers(span: &Span) -> Result<Vec<Wrapper>> {
    let mut wrappers = Vec::new();
    for (enabled, delimiter) in [
        (span.style.bold > 0, "**"),
        (span.style.italic > 0, "_"),
        (span.style.strike > 0, "~~"),
    ] {
        if enabled {
            wrappers.push(Wrapper::style(delimiter));
        }
    }
    if let Some(url) = &span.link {
        ensure!(
            url.chars().count() <= 3000,
            "Slack link exceeds 3000 characters"
        );
        let escaped = url
            .replace('\\', "%5C")
            .replace('<', "%3C")
            .replace('>', "%3E")
            .replace('\n', "%0A")
            .replace('\r', "%0D");
        wrappers.push(Wrapper {
            open: "[".into(),
            close: format!("](<{escaped}>)"),
            fallback_suffix: format!(" ({url})"),
        });
    }
    if span.style.code {
        let fence = "`".repeat(longest_backticks(&span.text) + 1);
        ensure!(
            fence.len() < 1000,
            "Slack inline code delimiter is too long"
        );
        wrappers.push(Wrapper {
            open: format!(
                "{fence}{}",
                if span.text.chars().all(char::is_whitespace) {
                    ""
                } else {
                    " "
                }
            ),
            close: format!(
                "{}{fence}",
                if span.text.chars().all(char::is_whitespace) {
                    ""
                } else {
                    " "
                }
            ),
            fallback_suffix: String::new(),
        });
    }
    Ok(wrappers)
}

fn longest_backticks(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn escape(c: char) -> String {
    match c {
        '&' => "&amp;".into(),
        '<' => "&lt;".into(),
        '>' => "&gt;".into(),
        '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
        | '!' | '|' | '~' => format!("\\{c}"),
        _ => c.to_string(),
    }
}

/// Close and reopen inline containers at chunk boundaries. Neither an escape,
/// a code delimiter nor a link can be cut in half by a platform length limit.
struct InlineWriter<'a> {
    output: &'a mut Output,
    prefix: Prefix,
    markdown: String,
    plain: String,
    chars: usize,
    active: Vec<Wrapper>,
    has_content: bool,
}

impl<'a> InlineWriter<'a> {
    fn new(output: &'a mut Output, prefix: Prefix) -> Self {
        Self {
            output,
            prefix,
            markdown: String::new(),
            plain: String::new(),
            chars: 0,
            active: Vec::new(),
            has_content: false,
        }
    }

    fn append(&mut self, value: &str) {
        self.markdown.push_str(value);
        self.chars += value.chars().count();
    }

    fn transition(&mut self, next: Vec<Wrapper>) {
        let shared = self
            .active
            .iter()
            .zip(&next)
            .take_while(|(a, b)| a == b)
            .count();
        let closed: Vec<_> = self.active[shared..].iter().rev().cloned().collect();
        for wrapper in closed {
            self.append(&wrapper.close);
            self.plain.push_str(&wrapper.fallback_suffix);
        }
        for wrapper in &next[shared..] {
            self.append(&wrapper.open);
        }
        self.active = next;
    }

    fn flush(&mut self) {
        if self.has_content {
            self.transition(Vec::new());
            self.output.markdown(
                std::mem::take(&mut self.markdown),
                std::mem::take(&mut self.plain),
            );
        } else {
            self.markdown.clear();
            self.plain.clear();
            self.active.clear();
        }
        self.chars = 0;
        self.has_content = false;
    }

    fn span(&mut self, span: &Span) -> Result<()> {
        let styled = wrappers(span)?;
        for c in span.text.chars() {
            // Whitespace outside emphasis keeps each continuation valid even
            // when the platform limit falls directly after a space.
            let next = if c.is_whitespace() && !span.style.code && span.link.is_none() {
                Vec::new()
            } else {
                styled.clone()
            };
            let closes: usize = next.iter().map(|w| w.close.chars().count()).sum();
            let value = if span.style.code {
                c.to_string()
            } else {
                escape(c)
            };
            let value = if c == '\n' {
                format!("{value}{}", self.prefix.rest)
            } else {
                value
            };
            let shared = self
                .active
                .iter()
                .zip(&next)
                .take_while(|(a, b)| a == b)
                .count();
            let transition: usize = self.active[shared..]
                .iter()
                .map(|w| w.close.chars().count())
                .sum::<usize>()
                + next[shared..]
                    .iter()
                    .map(|w| w.open.chars().count())
                    .sum::<usize>();
            if self.chars + transition + closes + value.chars().count() > MARKDOWN_CHARS {
                self.flush();
            }
            if self.markdown.is_empty() {
                let first = self.prefix.first.clone();
                self.append(&first);
                self.plain.push_str(&first);
            }
            self.transition(next.clone());
            ensure!(
                self.chars + closes + value.chars().count() <= MARKDOWN_CHARS,
                "Slack Markdown container exceeds the message limit"
            );
            self.append(&value);
            self.plain.push(c);
            self.has_content = true;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Output {
    messages: Vec<Value>,
    markdown: String,
    plain: String,
    markdown_chars: usize,
    parts: usize,
}

fn payload(text: &str, blocks: Value) -> Value {
    let mut payload = json!({"text": text, "mrkdwn": false, "parse": "none", "link_names": false,
        "unfurl_links": false, "unfurl_media": false});
    if blocks.as_array().is_some_and(|blocks| !blocks.is_empty()) {
        payload["blocks"] = blocks;
    }
    payload
}

impl Output {
    fn flush(&mut self) {
        if self.markdown.is_empty() {
            return;
        }
        self.messages.push(payload(
            &self.plain,
            json!([{"type":"markdown", "text":std::mem::take(&mut self.markdown)}]),
        ));
        self.plain.clear();
        self.markdown_chars = 0;
        self.parts = 0;
    }

    fn markdown(&mut self, markdown: String, plain: String) {
        let size = markdown.chars().count();
        // Native Markdown can expand into multiple native blocks. Bound the
        // number of independently authored paragraphs as well as source length.
        if self.markdown_chars + size + 2 > MARKDOWN_CHARS || self.parts == 30 {
            self.flush();
        }
        if !self.markdown.is_empty() {
            self.markdown.push_str("\n\n");
            self.plain.push_str("\n\n");
            self.markdown_chars += 2;
        }
        self.markdown.push_str(&markdown);
        self.plain.push_str(&plain);
        self.markdown_chars += size;
        self.parts += 1;
    }

    fn prose(
        &mut self,
        spans: &mut Vec<Span>,
        quotes: usize,
        lists: &mut [List],
        heading: Option<usize>,
    ) -> Result<()> {
        if spans.is_empty() {
            return Ok(());
        }
        let prefix = prefix(quotes, lists, heading)?;
        let mut writer = InlineWriter::new(self, prefix);
        for span in spans.drain(..) {
            writer.span(&span)?;
        }
        writer.flush();
        Ok(())
    }

    fn code(&mut self, code: &str, language: &str, prefix: Prefix) -> Result<()> {
        ensure!(
            language.len() <= 100
                && language
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')),
            "Slack code language must be a short language identifier"
        );
        let fence = "`".repeat(longest_backticks(code).max(2) + 1);
        ensure!(fence.len() < 1000, "Slack code delimiter is too long");
        let open = format!("{}{fence}{language}\n{}", prefix.first, prefix.rest);
        let close = format!("\n{}{fence}", prefix.rest);
        let overhead = open.chars().count() + close.chars().count();
        let capacity = MARKDOWN_CHARS - overhead;
        let mut chars = code.chars().peekable();
        loop {
            let mut part = String::new();
            let mut plain = String::new();
            let mut size = 0;
            while let Some(c) = chars.peek() {
                let needed = 1 + if *c == '\n' { prefix.rest.len() } else { 0 };
                if size + needed > capacity {
                    break;
                }
                let c = chars.next().unwrap();
                part.push(c);
                plain.push(c);
                if c == '\n' {
                    part.push_str(&prefix.rest);
                }
                size += needed;
            }
            let ending = if plain.ends_with('\n') {
                fence.clone()
            } else {
                close.clone()
            };
            self.markdown(format!("{open}{part}{ending}"), plain);
            if chars.peek().is_none() {
                break;
            }
        }
        Ok(())
    }

    fn table(&mut self, rows: &[Vec<Vec<Span>>], alignments: &[Alignment]) -> Result<()> {
        ensure!(
            alignments.len() <= 20,
            "Slack tables support at most 20 columns; attach wider data as a file"
        );
        let Some(header) = rows.first() else {
            return Ok(());
        };
        let row_chars = |row: &Vec<Vec<Span>>| {
            row.iter()
                .map(|cell| {
                    cell.iter()
                        .map(|span| span.text.chars().count())
                        .sum::<usize>()
                        .max(1)
                })
                .sum::<usize>()
        };
        let header_chars = row_chars(header);
        ensure!(
            header_chars <= TABLE_CHARS,
            "Slack table header exceeds 10000 characters"
        );
        let mut next = 1;
        self.flush();
        loop {
            let mut selected = vec![header];
            let mut count = header_chars;
            while let Some(row) = rows.get(next) {
                let size = row_chars(row);
                ensure!(
                    header_chars + size <= TABLE_CHARS,
                    "Slack table row with its header exceeds 10000 characters; attach the data as a file"
                );
                if selected.len() == 100 || count + size > TABLE_CHARS {
                    break;
                }
                selected.push(row);
                count += size;
                next += 1;
            }
            let fallback = selected
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.iter().map(plain_span).collect::<String>())
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(
                fallback.chars().count() <= 39_000,
                "Slack table fallback exceeds 39000 characters; shorten cell links or attach the data as a file"
            );
            let native: Vec<_> = selected
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    row.iter()
                        .map(|cell| native_cell(cell, index == 0))
                        .collect::<Vec<_>>()
                })
                .collect();
            let columns: Vec<_> = alignments.iter().map(|alignment| json!({"align":match alignment {Alignment::Center=>"center", Alignment::Right=>"right", _=>"left"}, "is_wrapped":true})).collect();
            self.messages.push(payload(
                &fallback,
                json!([{"type":"table", "rows":native, "column_settings":columns}]),
            ));
            if next == rows.len() {
                break;
            }
        }
        Ok(())
    }
}

fn plain_span(span: &Span) -> String {
    span.link
        .as_ref()
        .map_or_else(|| span.text.clone(), |url| format!("{} ({url})", span.text))
}

fn native_cell(spans: &[Span], header: bool) -> Value {
    if spans.is_empty() {
        // Slack rejects an empty raw_text string. A space keeps the cell
        // visually blank without inventing a value or shifting columns.
        return json!({"type":"raw_text", "text":" "});
    }
    let elements: Vec<_> = spans
        .iter()
        .map(|span| {
            let mut element = if let Some(url) = &span.link {
                json!({"type":"link", "url":url, "text":span.text})
            } else {
                json!({"type":"text", "text":span.text})
            };
            let mut style = serde_json::Map::new();
            for (enabled, key) in [
                (header || span.style.bold > 0, "bold"),
                (span.style.italic > 0, "italic"),
                (span.style.strike > 0, "strike"),
                (span.style.code, "code"),
            ] {
                if enabled {
                    style.insert(key.into(), true.into());
                }
            }
            if !style.is_empty() {
                element["style"] = style.into();
            }
            element
        })
        .collect();
    json!({"type":"rich_text", "elements":[{"type":"rich_text_section", "elements":elements}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown_blocks(messages: &[Value]) -> String {
        messages
            .iter()
            .flat_map(|message| message["blocks"].as_array().into_iter().flatten())
            .filter(|block| block["type"] == "markdown")
            .map(|block| block["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn plain_text_chunks_preserve_unicode_without_markdown_or_mentions() {
        let text = format!("<@U123> **literal** {}", "界".repeat(25_000));
        let chunks = messages(&text, true).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(
            chunks
                .iter()
                .map(|part| part["text"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        for chunk in chunks {
            assert!(chunk.get("blocks").is_none());
            assert_eq!(chunk["mrkdwn"], false);
            assert_eq!(chunk["link_names"], false);
            assert!(chunk["text"].as_str().unwrap().chars().count() <= 12_000);
        }
    }

    #[test]
    fn markdown_has_plain_accessible_fallback_and_literal_mentions() {
        let messages = markdown_messages("# Report\n\n**Bold** and _italic_, ~~old~~, `a_*` and [docs](https://example.com).\n\n<@U123> <!channel> & <b>literal</b>").unwrap();
        let body = markdown_blocks(&messages);
        assert!(body.contains("# Report"));
        assert!(body.contains("**Bold**"));
        assert!(body.contains("_italic_"));
        assert!(body.contains("~~old~~"));
        assert!(body.contains("[docs](<https://example.com>)"));
        assert!(body.contains("&lt;@U123&gt;"));
        assert!(!body.contains("<@U123>"));
        assert!(
            messages[0]["text"]
                .as_str()
                .unwrap()
                .contains("docs (https://example.com)")
        );
        for message in messages {
            assert_eq!(message["mrkdwn"], false);
            assert_eq!(message["parse"], "none");
            assert_eq!(message["link_names"], false);
        }
    }

    #[test]
    fn nesting_lists_quotes_and_task_items_survive() {
        let body = markdown_blocks(&markdown_messages("> **hello**\n>\n> - parent\n>   - nested\n> - final\n\n7. seven\n8. eight\n\n- [x] done\n- [ ] pending").unwrap());
        assert!(body.contains("> **hello**"), "{body}");
        assert!(body.contains("> - parent"), "{body}");
        assert!(body.contains(">   - nested"), "{body}");
        assert!(body.contains("7. seven"), "{body}");
        assert!(body.contains("8. eight"), "{body}");
        assert!(body.contains("☑ done"), "{body}");
        assert!(body.contains("☐ pending"), "{body}");
    }

    #[test]
    fn long_emphasis_splits_into_balanced_markdown_without_losing_unicode() {
        let content = "界word ".repeat(8000);
        let messages = markdown_messages(&format!("**{content}end**")).unwrap();
        assert!(messages.len() > 2);
        let plain = messages
            .iter()
            .map(|m| m["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(plain, format!("{content}end"));
        for message in messages {
            let source = message["blocks"][0]["text"].as_str().unwrap();
            assert!(source.chars().count() <= MARKDOWN_CHARS);
            let parsed = Parser::new(source)
                .filter_map(|e| match e {
                    Event::Text(s) => Some(s.into_string()),
                    _ => None,
                })
                .collect::<String>();
            assert_eq!(parsed.trim(), message["text"].as_str().unwrap().trim());
        }
    }

    #[test]
    fn long_code_closes_and_reopens_fences_and_preserves_language_and_content() {
        let content = format!("{}\n``` inside code\n", "print('界')\n".repeat(4000));
        let messages = markdown_messages(&format!("````python\n{content}````\n")).unwrap();
        let plain = messages
            .iter()
            .map(|m| m["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(plain, content);
        assert!(messages.len() > 1);
        for message in messages {
            let source = message["blocks"][0]["text"].as_str().unwrap();
            assert!(source.chars().count() <= MARKDOWN_CHARS);
            let code_starts: Vec<_> = Parser::new(source)
                .filter_map(|e| match e {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                        Some(info.into_string())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(code_starts, ["python"]);
        }
    }

    #[test]
    fn tables_split_at_rows_repeat_header_and_preserve_cells() {
        let mut markdown = "| Name | Value |\n| :--- | ---: |\n".to_owned();
        for index in 0..210 {
            markdown.push_str(&format!("| row-{index} | **{index}** |\n"));
        }
        let messages = markdown_messages(&markdown).unwrap();
        assert_eq!(messages.len(), 3);
        let mut data_rows = 0;
        for message in messages {
            let table = &message["blocks"][0];
            let rows = table["rows"].as_array().unwrap();
            assert!(rows.len() <= 100);
            assert_eq!(rows[0][0]["elements"][0]["elements"][0]["text"], "Name");
            assert_eq!(table["column_settings"][1]["align"], "right");
            data_rows += rows.len() - 1;
            assert!(
                message["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Name | Value\n")
            );
        }
        assert_eq!(data_rows, 210);
    }

    #[test]
    fn blank_table_cells_preserve_positions_and_plain_fallback() {
        let messages = markdown_messages(
            "| Name | Color | |\n| --- | --- | --- |\n| first | | filled |\n| second | blue | |\n",
        )
        .unwrap();
        assert_eq!(messages.len(), 1);
        let rows = &messages[0]["blocks"][0]["rows"];
        for (row, column) in [(0, 2), (1, 1), (2, 2)] {
            assert_eq!(rows[row][column], json!({"type":"raw_text","text":" "}));
        }
        assert_eq!(rows[1][2]["elements"][0]["elements"][0]["text"], "filled");
        assert_eq!(
            messages[0]["text"],
            "Name | Color | \nfirst |  | filled\nsecond | blue | "
        );
    }

    #[test]
    fn blank_cells_count_toward_native_table_limits() {
        let source = format!(
            "| H | |\n| --- | --- |\n| {} | |\n| x | |\n",
            "界".repeat(9996)
        );
        let messages = markdown_messages(&source).unwrap();
        assert_eq!(messages.len(), 2);
        for message in messages {
            assert!(message["text"].as_str().unwrap().chars().count() <= 10_100);
            assert!(message["blocks"][0]["rows"].as_array().unwrap().len() <= 100);
        }
    }

    #[test]
    fn table_cell_limits_split_without_truncation() {
        let cell = "界".repeat(3000);
        let messages = markdown_messages(&format!(
            "| H |\n| --- |\n| {cell} |\n| {cell} |\n| {cell} |\n| {cell} |\n"
        ))
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages
                .iter()
                .map(|m| m["text"].as_str().unwrap().matches('界').count())
                .sum::<usize>(),
            12000
        );
        let error = messages_for_oversized_table();
        assert!(error.to_string().contains("10000"));
    }

    fn messages_for_oversized_table() -> anyhow::Error {
        markdown_messages(&format!("| H |\n| --- |\n| {} |", "x".repeat(10000))).unwrap_err()
    }

    #[test]
    fn local_paths_are_visible_code_and_inline_delimiters_remain_literal() {
        let source = "**one _two_ three**, `  `, ``a`b``, [report](/tmp/report.csv).";
        let body = markdown_blocks(&markdown_messages(source).unwrap());
        let code: Vec<_> = Parser::new(&body)
            .filter_map(|event| match event {
                Event::Code(text) => Some(text.into_string()),
                _ => None,
            })
            .collect();
        assert_eq!(code, ["  ", "a`b", "/tmp/report.csv"]);
        assert!(!body.contains("[report]"));
        let visible = Parser::new(&body)
            .filter_map(|event| match event {
                Event::Text(text) | Event::Code(text) => Some(text.into_string()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(visible, "one two three,   , a`b, /tmp/report.csv.");
    }

    #[test]
    fn invalid_input_is_rejected_before_any_delivery() {
        assert!(markdown_messages(&"x".repeat(MAX_INPUT_BYTES + 1)).is_err());
        assert!(
            markdown_messages(&format!("[link](https://example.com/{})", "x".repeat(3000)))
                .is_err()
        );
        assert_eq!(
            markdown_messages("   ").unwrap()[0]["text"],
            "(No text returned.)"
        );
        assert_eq!(
            markdown_messages("[link]: https://example.com").unwrap()[0]["text"],
            "(No text returned.)"
        );
    }
}
