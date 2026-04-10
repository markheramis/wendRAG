/**
 * Structural extraction strategies that split document content into titled
 * sections suitable for downstream chunking.
 */

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

pub(super) const TEXT_WINDOW_SIZE: usize = 1000;

/**
 * Splits Markdown content into (optional title, body) sections along H1-H3
 * heading boundaries.
 */
pub(super) fn extract_markdown_sections(text: &str) -> Vec<(Option<String>, String)> {
    let parser = Parser::new(text);

    let mut sections: Vec<(Option<String>, String)> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body = String::new();
    let mut in_heading = false;
    let mut heading_text = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1 | HeadingLevel::H2 | HeadingLevel::H3,
                ..
            }) => {
                if !current_body.trim().is_empty() || current_title.is_some() {
                    sections.push((current_title.take(), std::mem::take(&mut current_body)));
                }
                in_heading = true;
                heading_text.clear();
            }
            Event::End(TagEnd::Heading(_)) if in_heading => {
                in_heading = false;
                current_title = Some(heading_text.trim().to_string());
            }
            Event::Text(t) | Event::Code(t) => {
                if in_heading {
                    heading_text.push_str(&t);
                } else {
                    current_body.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_heading {
                    heading_text.push(' ');
                } else {
                    current_body.push('\n');
                }
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                current_body.push_str("\n\n");
            }
            Event::Start(Tag::Item) => {
                current_body.push_str("- ");
            }
            Event::End(TagEnd::Item) => {
                current_body.push('\n');
            }
            _ => {}
        }
    }

    if !current_body.trim().is_empty() || current_title.is_some() {
        sections.push((current_title, current_body));
    }

    sections
}

/**
 * Splits plain-text content into paragraph-merged sections that respect the
 * target window size, each returned as (None, body).
 */
pub(super) fn extract_text_sections(text: &str) -> Vec<(Option<String>, String)> {
    let paragraphs: Vec<&str> = text.split("\n\n").collect();

    let mut merged: Vec<String> = Vec::new();
    let mut buf = String::new();

    for para in &paragraphs {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if buf.len() + para.len() + 2 > TEXT_WINDOW_SIZE && !buf.is_empty() {
            merged.push(std::mem::take(&mut buf));
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(para);
    }
    if !buf.is_empty() {
        merged.push(buf);
    }

    merged.into_iter().map(|s| (None, s)).collect()
}
