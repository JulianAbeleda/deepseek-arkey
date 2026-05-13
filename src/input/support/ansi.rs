use std::iter::Peekable;

use crate::terminal_width::char_display_width;

pub(crate) fn visible_suffix(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut tokens = Vec::new();
    let mut visible = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(sequence) = take_ansi_sequence(ch, &mut chars) {
            tokens.push(VisibleToken::Ansi(sequence));
            continue;
        }
        let width = char_display_width(ch);
        visible += width;
        tokens.push(VisibleToken::Char(ch, width));
    }

    if visible <= width {
        return text.to_string();
    }

    let mut suffix_width = 0usize;
    let mut start = None;
    for (index, token) in tokens.iter().enumerate().rev() {
        if let VisibleToken::Char(_, char_width) = token {
            if suffix_width + char_width > width {
                break;
            }
            suffix_width += char_width;
            start = Some(index);
        }
    }

    let Some(start) = start else {
        return String::new();
    };

    let mut out = String::new();
    if let Some(sequence) = active_sgr_before(&tokens, start) {
        out.push_str(&sequence);
    }
    for token in &tokens[start..] {
        match token {
            VisibleToken::Ansi(sequence) => out.push_str(sequence),
            VisibleToken::Char(ch, _) => out.push(*ch),
        }
    }
    out
}

enum VisibleToken {
    Ansi(String),
    Char(char, usize),
}

pub(crate) fn take_ansi_sequence<I>(ch: char, chars: &mut Peekable<I>) -> Option<String>
where
    I: Iterator<Item = char>,
{
    if ch != '\x1b' || chars.peek() != Some(&'[') {
        return None;
    }

    let mut sequence = String::from(ch);
    sequence.push(chars.next()?);
    for code in chars.by_ref() {
        sequence.push(code);
        if ('@'..='~').contains(&code) {
            break;
        }
    }
    Some(sequence)
}

fn active_sgr_before(tokens: &[VisibleToken], end: usize) -> Option<String> {
    let mut active = None;
    for token in &tokens[..end] {
        let VisibleToken::Ansi(sequence) = token else {
            continue;
        };
        if !sequence.ends_with('m') {
            continue;
        }
        if is_sgr_reset(sequence) {
            active = None;
        } else {
            active = Some(sequence.clone());
        }
    }
    active
}

fn is_sgr_reset(sequence: &str) -> bool {
    let Some(codes) = sequence
        .strip_prefix("\x1b[")
        .and_then(|rest| rest.strip_suffix('m'))
    else {
        return false;
    };
    if codes.is_empty() {
        return true;
    }

    let codes = codes.split(';').collect::<Vec<_>>();
    let mut index = 0;
    while index < codes.len() {
        match codes[index] {
            "" | "0" => return true,
            "38" | "48" | "58" if codes.get(index + 1) == Some(&"2") => index += 5,
            "38" | "48" | "58" if codes.get(index + 1) == Some(&"5") => index += 3,
            _ => index += 1,
        }
    }
    false
}
