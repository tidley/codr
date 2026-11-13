//! Phone display adaptation only. Native history and delivery identities stay unchanged.
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use std::{borrow::Cow, ops::Range};
use unicode_width::UnicodeWidthStr;

/// Use labelled rows when columns would compete for a phone bubble's width.
pub(crate) fn readable_tables(text: &str) -> Cow<'_, str> {
    if !text.contains('|') {
        return Cow::Borrowed(text);
    }
    let mut depth = 0usize;
    let mut table: Option<Table> = None;
    let mut output = String::new();
    let mut copied = 0;
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        let code_span = matches!(&event, Event::Code(_));
        match event {
            Event::Start(tag) => {
                match tag {
                    Tag::Table(_) if depth == 0 => {
                        table = Some(Table {
                            source: text,
                            range,
                            rows: Vec::new(),
                        });
                    }
                    Tag::TableHead | Tag::TableRow => {
                        if let Some(table) = &mut table {
                            table.rows.push(Vec::new());
                        }
                    }
                    Tag::TableCell => {
                        if let Some(row) = table.as_mut().and_then(|table| table.rows.last_mut()) {
                            row.push(Cell {
                                range,
                                code: Vec::new(),
                                visible_width: 0,
                            });
                        }
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if tag == TagEnd::Table
                    && let Some(mut table) = table.take()
                {
                    table.range.end = range.end;
                    if table.is_wide() {
                        output.push_str(&text[copied..table.range.start]);
                        table.write(&mut output);
                        copied = table.range.end;
                        // Retain the existing blank line after the table, without adding a third.
                        if text[copied..].starts_with(['\n', '\r']) {
                            output.pop();
                        }
                    }
                }
            }
            Event::Code(value) | Event::Text(value) => {
                if let Some(cell) = table
                    .as_mut()
                    .and_then(|table| table.rows.last_mut())
                    .and_then(|row| row.last_mut())
                {
                    cell.visible_width = cell
                        .visible_width
                        .saturating_add(UnicodeWidthStr::width(value.as_ref()));
                    // Only code spans have table-specific escapes inside literal content.
                    if code_span {
                        cell.code.push(range);
                    }
                }
            }
            _ => {}
        }
    }
    if copied == 0 {
        Cow::Borrowed(text)
    } else {
        output.push_str(&text[copied..]);
        Cow::Owned(inline_references(&output))
    }
}

/// Reference definitions may land in a different transport message after conversion.
/// Make links self-contained while retaining their original inline label Markdown.
fn inline_references(text: &str) -> String {
    let mut output = String::new();
    let mut copied = 0;
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        let Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            ..
        }) = event
        else {
            continue;
        };
        let raw = &text[range.clone()];
        let end = match link_type {
            LinkType::Shortcut => raw.len() - 1,
            LinkType::Collapsed => raw[..raw.len() - 2].trim_end().len() - 1,
            LinkType::Reference => {
                let Some((open, _)) = raw.char_indices().rev().find(|(at, ch)| {
                    *ch == '['
                        && raw[..*at]
                            .bytes()
                            .rev()
                            .take_while(|byte| *byte == b'\\')
                            .count()
                            % 2
                            == 0
                }) else {
                    continue;
                };
                raw[..open].trim_end().len() - 1
            }
            _ => continue,
        };
        output.push_str(&text[copied..range.start]);
        output.push('[');
        output.push_str(&raw[1..end]);
        output.push_str("](<");
        output.push_str(
            &dest_url
                .replace('&', "&amp;")
                .replace('\\', "\\\\")
                .replace('<', "\\<")
                .replace('>', "\\>"),
        );
        output.push('>');
        if !title.is_empty() {
            output.push_str(" \"");
            output.push_str(
                &title
                    .replace('&', "&amp;")
                    .replace('\\', "\\\\")
                    .replace('"', "\\\""),
            );
            output.push('"');
        }
        output.push(')');
        copied = range.end;
    }
    output.push_str(&text[copied..]);
    output
}

struct Table<'a> {
    source: &'a str,
    range: Range<usize>,
    rows: Vec<Vec<Cell>>,
}

struct Cell {
    range: Range<usize>,
    code: Vec<Range<usize>>,
    visible_width: usize,
}

impl Cell {
    fn markdown<'a>(&self, source: &'a str) -> Cow<'a, str> {
        if !self
            .code
            .iter()
            .any(|range| source[range.clone()].contains("\\|"))
        {
            return Cow::Borrowed(source[self.range.clone()].trim());
        }
        let mut output = String::new();
        let mut copied = self.range.start;
        for range in &self.code {
            output.push_str(&source[copied..range.start]);
            // GFM removes the table escape even inside a code span. Outside a table,
            // the same backslash would become visible literal code.
            output.push_str(&source[range.clone()].replace("\\|", "|"));
            copied = range.end;
        }
        output.push_str(&source[copied..self.range.end]);
        Cow::Owned(output.trim().to_owned())
    }
}

impl Table<'_> {
    fn is_wide(&self) -> bool {
        let Some(header) = self.rows.first() else {
            return false;
        };
        self.rows.len() > 1
            && (header.len() >= 4
                || (0..header.len())
                    .map(|column| {
                        self.rows
                            .iter()
                            .filter_map(|row| row.get(column))
                            .map(|cell| cell.visible_width)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(3)
                    })
                    .sum::<usize>()
                    > 80)
    }

    fn write(&self, output: &mut String) {
        use std::fmt::Write;
        let header = &self.rows[0];
        for row in &self.rows[1..] {
            for (column, label) in header.iter().enumerate() {
                let label = label.markdown(self.source);
                if column == 0 {
                    output.push_str("### ");
                } else {
                    output.push_str("- ");
                }
                if label.is_empty() {
                    write!(output, "Column {}", column + 1).unwrap();
                } else {
                    output.push_str(&label);
                }
                output.push_str(": ");
                let value = row
                    .get(column)
                    .map(|cell| cell.markdown(self.source))
                    .unwrap_or_default();
                // A trailing hash run has closing-heading meaning only after conversion.
                let without_hashes = value.trim_end_matches('#');
                if column == 0
                    && (without_hashes.is_empty() || without_hashes.ends_with(char::is_whitespace))
                    && without_hashes.len() < value.len()
                {
                    output.push_str(without_hashes);
                    for _ in without_hashes.len()..value.len() {
                        output.push_str("\\#");
                    }
                } else {
                    output.push_str(&value);
                }
                output.push('\n');
            }
            output.push('\n');
        }
    }
}
