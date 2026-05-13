mod ansi;
mod edit;
mod layout;
mod render;
mod terminal;
mod visible;

#[allow(unused_imports)]
pub(crate) use ansi::{take_ansi_sequence, visible_suffix};
#[cfg(test)]
pub(crate) use edit::buffer_prefix;
pub(crate) use edit::{
    byte_index, char_len, insert_at, insert_str_at, next_word_cursor, previous_word_cursor,
    remove_at, remove_before, remove_previous_word,
};
#[allow(unused_imports)]
pub(crate) use layout::{
    clear_dock_rows, clear_rows_above_dock, clear_transient_rows, dock_row, output_row,
    parse_forced_terminal_size, reset_output_scroll_region, set_output_scroll_region,
    terminal_rows, terminal_width, transcript_view_height,
};
#[allow(unused_imports)]
pub(crate) use render::{
    compose_rendered_dock_rows, muted_dock_help, newline, progress_panel_rows, prompt_echo_block,
    prompt_echo_block_lines, prompt_echo_marker, prompt_echo_plain_blank, render_dock_lines,
    render_line, style_prompt_echo, style_prompt_echo_with_color, submitted_prompt_echo,
    submitted_prompt_echo_with_options, write_raw_lines,
};
#[allow(unused_imports)]
pub(crate) use terminal::{is_key_press_or_repeat, keyboard_enhancement_flags, RawModeGuard};
#[allow(unused_imports)]
pub(crate) use visible::{compose_dock_rows, truncate_display_text, visible_len, ComposedDockRows};
