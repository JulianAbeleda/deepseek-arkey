pub(crate) fn insert_at(buffer: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert(byte_index, ch);
    *cursor += 1;
}

pub(crate) fn insert_str_at(buffer: &mut String, cursor: &mut usize, text: &str) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert_str(byte_index, text);
    *cursor += char_len(text);
}

pub(crate) fn remove_before(buffer: &mut String, cursor: &mut usize) -> Option<char> {
    if *cursor == 0 {
        return None;
    }
    *cursor -= 1;
    remove_at(buffer, *cursor)
}

pub(crate) fn remove_at(buffer: &mut String, cursor: usize) -> Option<char> {
    if cursor >= char_len(buffer) {
        return None;
    }
    let start = byte_index(buffer, cursor);
    let ch = buffer[start..].chars().next()?;
    let end = start + ch.len_utf8();
    buffer.replace_range(start..end, "");
    Some(ch)
}

pub(crate) fn byte_index(buffer: &str, cursor: usize) -> usize {
    buffer
        .char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len())
}

pub(crate) fn char_len(buffer: &str) -> usize {
    buffer.chars().count()
}

#[cfg(test)]
pub(crate) fn buffer_prefix(buffer: &str, cursor: usize) -> String {
    buffer.chars().take(cursor).collect()
}

pub(crate) fn previous_word_cursor(buffer: &str, cursor: usize) -> usize {
    let chars = buffer.chars().collect::<Vec<_>>();
    let mut index = cursor.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

pub(crate) fn next_word_cursor(buffer: &str, cursor: usize) -> usize {
    let chars = buffer.chars().collect::<Vec<_>>();
    let mut index = cursor.min(chars.len());
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

pub(crate) fn remove_previous_word(buffer: &mut String, cursor: &mut usize) -> bool {
    let end = *cursor;
    let start = previous_word_cursor(buffer, end);
    if start == end {
        return false;
    }
    let start_byte = byte_index(buffer, start);
    let end_byte = byte_index(buffer, end);
    buffer.replace_range(start_byte..end_byte, "");
    *cursor = start;
    true
}
