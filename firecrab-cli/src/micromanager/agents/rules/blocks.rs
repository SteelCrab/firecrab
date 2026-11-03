//! Markdown blocks are the unit a user clicks: a heading, a list item, a paragraph or a fence.
//! Edits rebuild only the target lines; every other line keeps its bytes and line ending style.
use super::super::super::resource::Error;
use serde::Serialize;

/// Line range `[start, end)`, zero-based.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
}

/// Lines without their `\n` or `\r\n`; a final newline does not create an empty last line.
fn lines(text: &str) -> Vec<&str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() && text.len() <= 1 {
        return Vec::new();
    }
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

fn eol(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Up to three leading spaces, as Markdown allows before a block marker.
fn content(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len() - rest.len() <= 3).then_some(rest)
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn fence(line: &str) -> Option<&str> {
    let rest = content(line)?;
    ["```", "~~~"].into_iter().find(|f| rest.starts_with(f))
}

fn heading(line: &str) -> bool {
    content(line).is_some_and(|rest| {
        let hashes = rest.bytes().take_while(|b| *b == b'#').count();
        (1..=6).contains(&hashes)
            && rest[hashes..]
                .chars()
                .next()
                .is_none_or(char::is_whitespace)
    })
}

fn item(line: &str) -> bool {
    content(line).is_some_and(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let after = if (1..=9).contains(&digits)
            && matches!(rest.as_bytes().get(digits), Some(b'.' | b')'))
        {
            &rest[digits + 1..]
        } else if matches!(rest.as_bytes().first(), Some(b'-' | b'*' | b'+')) {
            &rest[1..]
        } else {
            return false;
        };
        after.chars().next().is_none_or(char::is_whitespace)
    })
}

pub fn split(text: &str) -> Vec<Block> {
    let lines = lines(text);
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if blank(lines[i]) {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if let Some(open) = fence(lines[start]) {
            while i < lines.len() && fence(lines[i]) != Some(open) {
                i += 1;
            }
            i = (i + 1).min(lines.len());
        } else if !heading(lines[start]) {
            while i < lines.len()
                && !blank(lines[i])
                && !heading(lines[i])
                && !item(lines[i])
                && fence(lines[i]).is_none()
            {
                i += 1;
            }
        }
        blocks.push(Block { start, end: i });
    }
    blocks
}

pub fn text_of(text: &str, block: Block) -> String {
    lines(text)[block.start..block.end].join("\n")
}

fn join(lines: &[&str], eol: &str, final_newline: bool) -> String {
    let mut out = lines.join(eol);
    if final_newline && !lines.is_empty() {
        out.push_str(eol);
    }
    out
}

fn new_lines(text: &str) -> Vec<&str> {
    text.trim_end_matches(['\n', '\r'])
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

pub fn replace(text: &str, index: usize, replacement: &str) -> Result<String, Error> {
    let block = *split(text)
        .get(index)
        .ok_or_else(|| Error::Invalid("That rule no longer exists. Reload the file.".into()))?;
    let lines = lines(text);
    let mut out: Vec<&str> = lines[..block.start].to_vec();
    out.extend(new_lines(replacement));
    out.extend_from_slice(&lines[block.end..]);
    Ok(join(&out, eol(text), text.ends_with('\n')))
}

/// Add a block at the end, separated by exactly one blank line.
pub fn append(text: &str, addition: &str) -> String {
    let mut out = lines(text);
    while out.last().is_some_and(|l| blank(l)) {
        out.pop();
    }
    if !out.is_empty() {
        out.push("");
    }
    out.extend(new_lines(addition));
    join(&out, eol(text), true)
}

#[cfg(test)]
mod tests;
