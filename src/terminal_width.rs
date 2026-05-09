use std::iter::Peekable;

pub(crate) fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in text.split_whitespace() {
        let word_width = display_width(word);
        if word_width > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            lines.extend(chunk_long_word(word, width));
            continue;
        }

        let separator = usize::from(!current.is_empty());
        if current_width + separator + word_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        current.push_str(word);
        current_width += word_width;
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn pad_display_width(text: &str, width: usize) -> String {
    let len = display_width(text);
    if len >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - len))
}

pub(crate) fn display_width(text: &str) -> usize {
    let mut len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if take_ansi_sequence(ch, &mut chars) {
            continue;
        }
        len += char_display_width(ch);
    }
    len
}

pub(crate) fn char_display_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else if is_wide_char(ch) {
        2
    } else {
        1
    }
}

fn chunk_long_word(word: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for ch in word.chars() {
        let ch_width = char_display_width(ch);
        if current_width + ch_width > width && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn take_ansi_sequence<I>(ch: char, chars: &mut Peekable<I>) -> bool
where
    I: Iterator<Item = char>,
{
    if ch != '\x1b' || chars.peek() != Some(&'[') {
        return false;
    }

    chars.next();
    for next in chars.by_ref() {
        if ('@'..='~').contains(&next) {
            break;
        }
    }
    true
}

fn is_wide_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
    )
}

#[cfg(test)]
mod tests {
    use super::{display_width, pad_display_width, wrap_plain_text};

    #[test]
    fn display_width_ignores_ansi_sequences() {
        assert_eq!(display_width("\x1b[36;1mwide\x1b[0m"), 4);
    }

    #[test]
    fn wraps_words_and_chunks_long_words() {
        assert_eq!(
            wrap_plain_text("this wraps cleanly", 10),
            ["this wraps", "cleanly"]
        );
        assert_eq!(wrap_plain_text("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn pads_to_display_width() {
        assert_eq!(pad_display_width("\x1b[1mx\x1b[0m", 3), "\x1b[1mx\x1b[0m  ");
    }
}
