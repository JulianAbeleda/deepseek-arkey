use super::{
    approval_panel_rows, buffer_prefix, compose_dock_rows, compose_rendered_dock_rows, insert_at,
    keyboard_enhancement_flags, next_slash_completion, next_word_cursor, output_row,
    parse_forced_terminal_size, previous_word_cursor, progress_panel_rows, prompt_echo_block_lines,
    remove_at, remove_before, remove_previous_word, slash_command_matches,
    slash_completion_panel_rows, submitted_prompt_echo_with_options, take_ansi_sequence,
    visible_len, visible_suffix, ApprovalChoice, ApprovalModal, DockedComposer, DOCK_RESERVED_ROWS,
};
use crossterm::event::KeyboardEnhancementFlags;

#[test]
fn keyboard_enhancement_flags_enable_modified_enter_reporting() {
    let flags = keyboard_enhancement_flags();
    assert!(flags.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    assert!(flags.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
    assert!(flags.contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES));
}

#[test]
fn key_event_filter_ignores_release_events() {
    assert!(super::is_key_press_or_repeat(
        crossterm::event::KeyEvent::new_with_kind(
            crossterm::event::KeyCode::Char('h'),
            crossterm::event::KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Press,
        )
    ));
    assert!(super::is_key_press_or_repeat(
        crossterm::event::KeyEvent::new_with_kind(
            crossterm::event::KeyCode::Char('h'),
            crossterm::event::KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Repeat,
        )
    ));
    assert!(!super::is_key_press_or_repeat(
        crossterm::event::KeyEvent::new_with_kind(
            crossterm::event::KeyCode::Char('h'),
            crossterm::event::KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Release,
        )
    ));
}

#[test]
fn edits_at_cursor() {
    let mut text = "hi".to_string();
    let mut cursor = 1;
    insert_at(&mut text, &mut cursor, 'e');
    assert_eq!(text, "hei");
    assert_eq!(cursor, 2);
    assert_eq!(remove_before(&mut text, &mut cursor), Some('e'));
    assert_eq!(text, "hi");
    assert_eq!(cursor, 1);
    assert_eq!(remove_at(&mut text, cursor), Some('i'));
    assert_eq!(text, "h");
}

#[test]
fn visible_len_ignores_ansi() {
    assert_eq!(visible_len("\x1b[36mdeepseek\x1b[0m › "), 11);
}

#[test]
fn dock_display_uses_visible_suffix_for_long_lines() {
    assert_eq!(visible_suffix("abcdef", 4), "cdef");
    assert_eq!(visible_suffix("ab界", 3), "b界");
    assert_eq!(buffer_prefix("hello", 3), "hel");
}

#[test]
fn dock_display_keeps_ansi_sequences_intact_at_narrow_widths() {
    let text = "\x1b[36;1mdeepseek\x1b[0m \x1b[38;2;122;162;247m[deepseek-v4-flash]\x1b[0m › draft";
    let suffix = visible_suffix(text, 10);

    assert_eq!(suffix, "\x1b[38;2;122;162;247mh]\x1b[0m › draft");
    assert_eq!(visible_len(&suffix), 10);
}

#[test]
fn dock_display_preserves_active_ansi_style_inside_suffix() {
    let suffix = visible_suffix("\x1b[31mabcdef\x1b[0m", 4);

    assert_eq!(suffix, "\x1b[31mcdef\x1b[0m");
    assert_eq!(visible_len(&suffix), 4);
}

#[test]
fn dock_display_does_not_treat_rgb_zero_as_ansi_reset() {
    let suffix = visible_suffix("\x1b[38;2;0;162;0mabcdef\x1b[0m", 4);

    assert_eq!(suffix, "\x1b[38;2;0;162;0mcdef\x1b[0m");
    assert_eq!(visible_len(&suffix), 4);
}

#[test]
fn word_motion_skips_whitespace_and_words() {
    let text = "one  two\nthree";

    assert_eq!(previous_word_cursor(text, 8), 5);
    assert_eq!(previous_word_cursor(text, 5), 0);
    assert_eq!(next_word_cursor(text, 0), 5);
    assert_eq!(next_word_cursor(text, 5), 9);
}

#[test]
fn deletes_previous_word() {
    let mut text = "one  two\nthree".to_string();
    let mut cursor = 8;

    assert!(remove_previous_word(&mut text, &mut cursor));
    assert_eq!(text, "one  \nthree");
    assert_eq!(cursor, 5);
}

#[test]
fn dock_composer_wraps_multiline_buffer_and_tracks_cursor() {
    let rows = compose_dock_rows("p> ", "alpha\nbeta", 8, 20);

    assert_eq!(rows.lines, vec!["p> alpha".to_string(), "beta".to_string()]);
    assert_eq!(rows.cursor_row, 1);
    assert_eq!(rows.cursor_col, 2);
    assert!(DOCK_RESERVED_ROWS >= rows.lines.len());
}

#[test]
fn dock_reserves_vertical_padding_rows() {
    let mut rows = compose_dock_rows("p> ", "draft", 5, 20);
    rows.lines.insert(0, String::new());
    rows.lines.push(String::new());
    rows.cursor_row += 1;

    assert_eq!(rows.lines.first().map(String::as_str), Some(""));
    assert_eq!(rows.lines.last().map(String::as_str), Some(""));
    assert_eq!(rows.cursor_row, 1);
    assert!(DOCK_RESERVED_ROWS >= rows.lines.len());
}

#[test]
fn tab_completes_unique_slash_command_prefix() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/sta".to_string();
    composer.cursor = 4;

    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/status");
    assert_eq!(composer.cursor, 7);
}

#[test]
fn repeated_tab_cycles_multiple_slash_matches() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/r".to_string();
    composer.cursor = 2;

    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/root");
    assert_eq!(composer.cursor, 5);
    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/runtime");
    assert_eq!(composer.cursor, 8);
    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/root");
}

#[test]
fn slash_completion_preserves_trailing_text_after_command_token() {
    let completion = next_slash_completion("/r path", 2, None, None).unwrap();
    let rest = &"/r path"[super::byte_index("/r path", completion.token_end)..];

    assert_eq!(completion.command, "/root");
    assert_eq!(rest, " path");
}

#[test]
fn editing_after_completion_resets_slash_cycle() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/r".to_string();
    composer.cursor = 2;

    assert!(composer.apply_slash_completion());
    assert!(composer.slash_completion_index.is_some());
    insert_at(&mut composer.buffer, &mut composer.cursor, 'x');
    composer.reset_slash_completion();

    assert_eq!(composer.buffer, "/rootx");
    assert_eq!(composer.slash_completion_index, None);
    assert_eq!(composer.slash_completion_prefix, None);
}

#[test]
fn slash_draft_renders_completion_panel_suggestions() {
    let panel = slash_completion_panel_rows("/r", None, 48);
    let plain = panel
        .iter()
        .map(|line| strip_ansi_for_test(line))
        .collect::<Vec<_>>();

    assert_eq!(plain.len(), 3);
    assert!(plain[0].starts_with("─"));
    assert!(plain[1].contains("/root"));
    assert!(plain[1].contains("Show or set active workspace root"));
    assert!(plain[2].contains("/runtime"));
    assert!(plain[2].contains("Show provider/debug runtime state"));
    assert_eq!(slash_command_matches("/sta"), vec!["/status"]);
}

#[test]
fn help_slash_command_completes_from_h_prefix() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/h".to_string();
    composer.cursor = 2;

    assert!(strip_ansi_for_test(&slash_completion_panel_rows("/h", None, 48)[1]).contains("/help"));
    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/help");
    assert_eq!(composer.cursor, 5);
}

#[test]
fn quit_alias_is_available_to_slash_completion() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/q".to_string();
    composer.cursor = 2;

    assert!(strip_ansi_for_test(&slash_completion_panel_rows("/q", None, 48)[1]).contains("/quit"));
    assert!(composer.apply_slash_completion());
    assert_eq!(composer.buffer, "/quit");
    assert_eq!(composer.cursor, 5);
}

#[test]
fn no_match_slash_draft_renders_panel_message() {
    let panel = slash_completion_panel_rows("/zzz", None, 32);

    assert_eq!(panel.len(), 2);
    assert!(strip_ansi_for_test(&panel[1]).contains("No slash command match"));
}

#[test]
fn slash_completion_panel_marks_selected_command() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "/r".to_string();
    composer.cursor = 2;

    assert!(composer.apply_slash_completion());
    let panel = slash_completion_panel_rows("/r", composer.slash_completion_index, 48);
    let plain = panel
        .iter()
        .map(|line| strip_ansi_for_test(line))
        .collect::<Vec<_>>();

    assert!(plain.iter().any(|line| line.starts_with("› /root")));
    assert!(!plain.iter().any(|line| line.starts_with("› /runtime")));

    assert!(composer.apply_slash_completion());
    let panel = slash_completion_panel_rows("/r", composer.slash_completion_index, 48);
    let plain = panel
        .iter()
        .map(|line| strip_ansi_for_test(line))
        .collect::<Vec<_>>();

    assert!(plain.iter().any(|line| line.starts_with("› /runtime")));
}

#[test]
fn completion_panel_does_not_alter_input_cursor_position() {
    let panel = slash_completion_panel_rows("/r", None, 20);
    let rows = compose_rendered_dock_rows("p> ", "/r", 2, 20, &panel, false);

    assert!(strip_ansi_for_test(&rows.lines[1]).starts_with("─"));
    let prompt_row = rows
        .lines
        .iter()
        .position(|line| strip_ansi_for_test(line).starts_with("p> /r"))
        .unwrap();
    assert_eq!(rows.cursor_row, prompt_row);
    assert_eq!(rows.cursor_col, 5);
    assert!(DOCK_RESERVED_ROWS >= rows.lines.len());
}

#[test]
fn completion_panel_rows_are_capped_to_dock_reservation() {
    let panel = slash_completion_panel_rows("/", None, 80);
    let rows = compose_rendered_dock_rows("p> ", "/", 1, 80, &panel, false);

    assert_eq!(rows.lines.len(), DOCK_RESERVED_ROWS);
    let prompt_row = rows
        .lines
        .iter()
        .position(|line| strip_ansi_for_test(line).starts_with("p> /"))
        .unwrap();
    assert_eq!(rows.cursor_row, prompt_row);
    assert_eq!(rows.cursor_col, 4);
}

#[test]
fn approval_modal_renders_inside_dock_without_input_row() {
    let modal = ApprovalModal::new(
            "run_shell".to_string(),
            "approval required: run_shell\ncwd: .\nreason: run tests\ncommand: cargo test --offline\nType yes run to approve, n to deny.\n".to_string(),
        );
    let panel = approval_panel_rows(&modal, 56);
    let rows = compose_rendered_dock_rows("p> ", "hidden", 6, 56, &panel, true);
    let plain = strip_ansi_for_test(&rows.lines.join("\n"));

    assert!(rows.lines.len() <= DOCK_RESERVED_ROWS);
    assert!(!plain.contains("p> hidden"));
    assert!(plain.contains("approval"));
    assert!(plain.contains("run_shell requires approval"));
    assert!(plain.contains("command: cargo test --offline"));
    assert!(plain.contains("→ [1] Approve once"));
    assert!(plain.contains("[2] Approve for this session"));
}

#[test]
fn approval_modal_selection_maps_to_session_choice() {
    let mut modal = ApprovalModal::new(
        "propose_patch".to_string(),
        "approval required: propose_patch\npath: src/input.rs\n".to_string(),
    );
    modal.move_selection(1);

    assert_eq!(modal.selected_choice(), ApprovalChoice::ApproveForSession);
}

#[test]
fn dock_active_rows_follow_rendered_rows() {
    let mut composer = DockedComposer::new("p> ".to_string());

    assert_eq!(composer.active_dock_rows(), 1);

    composer.rendered_dock_rows = 3;
    assert_eq!(composer.active_dock_rows(), 3);

    composer.rendered_dock_rows = DOCK_RESERVED_ROWS + 4;
    assert_eq!(composer.active_dock_rows(), DOCK_RESERVED_ROWS);
}

#[test]
fn output_region_expands_when_dock_reservation_shrinks() {
    assert!(output_row(3) > output_row(DOCK_RESERVED_ROWS));
}

#[test]
fn submitted_prompt_echo_uses_highlighted_prompt_block() {
    let echo = submitted_prompt_echo_with_options("inspect README", 40, true);
    let plain = strip_ansi_for_test(&echo);
    let lines = plain.lines().collect::<Vec<_>>();

    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], "");
    assert_eq!(lines[1], " ".repeat(40));
    assert!(lines[2].starts_with(">  inspect README"));
    assert_eq!(visible_len(lines[2]), 40);
    assert_eq!(lines[3], " ".repeat(40));
    assert_eq!(lines[4], "");
    assert!(echo.contains("48;2;40;42;54"));
    assert!(echo.contains("\x1b[1;38;2;187;154;247;48;2;40;42;54m"));

    let raw_lines = echo.lines().collect::<Vec<_>>();
    assert!(!raw_lines[1].contains("48;2;40;42;54"));
    assert!(raw_lines[2].contains("48;2;40;42;54"));
    assert!(!raw_lines[3].contains("48;2;40;42;54"));
}

#[test]
fn submitted_prompt_echo_prefixes_multiline_prompts() {
    let echo = submitted_prompt_echo_with_options("first line\nsecond line", 32, false);
    let plain = strip_ansi_for_test(&echo);

    assert!(plain.contains(">  first line"));
    assert!(plain.contains(">  second line"));
    assert!(!echo.contains("\x1b["));
    assert!(plain
        .lines()
        .all(|line| line.is_empty() || visible_len(line) == 32));
}

#[test]
fn prompt_echo_block_wraps_and_pads_content_rows() {
    let lines =
        prompt_echo_block_lines("this is a longer prompt that should wrap cleanly", 30, true);
    let plain = strip_ansi_for_test(&lines.join("\n"));

    assert!(lines.len() > 3);
    assert!(plain.contains(">  this is a longer prompt"));
    assert!(lines
        .iter()
        .all(|line| visible_len(&strip_ansi_for_test(line)) == 30));
    assert!(!lines[0].contains("48;2;40;42;54"));
    assert!(lines[1].contains("48;2;40;42;54"));
    assert!(!lines.last().unwrap().contains("48;2;40;42;54"));
}

#[test]
fn composer_stream_state_can_reset() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.buffer = "draft".to_string();
    composer.stream_buffer = "hello".to_string();
    assert_eq!(composer.stream_buffer, "hello");
    assert!(!composer.status_active);
    composer.status_active = true;
    composer.status_rows = 3;
    composer.reset_stream_state();
    assert!(composer.stream_buffer.is_empty());
    assert!(!composer.status_active);
    assert_eq!(composer.status_rows, 0);
    assert_eq!(composer.buffer, "draft");
}

#[test]
fn composer_status_state_is_consumed_before_rewrite() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.status_active = true;
    composer.status_rows = 2;
    assert!(composer.take_status_active());
    assert!(!composer.status_active);
    assert_eq!(composer.status_rows, 0);
    assert!(!composer.take_status_active());
}

#[test]
fn progress_panel_rows_are_capped_and_padded() {
    let rows = progress_panel_rows("Loading 1s\n\nagent step 1: list_files", 32);
    assert_eq!(rows.len(), 3);
    let plain = rows
        .iter()
        .map(|row| strip_ansi_for_test(row))
        .collect::<Vec<_>>();
    assert_eq!(plain[0].trim_end(), "Loading 1s");
    assert_eq!(plain[1].trim_end(), "");
    assert_eq!(plain[2].trim_end(), "agent step 1: list_files");
    assert!(plain.iter().all(|row| visible_len(row) == 32));
}

#[test]
fn progress_dock_rows_render_above_prompt() {
    let panel = progress_panel_rows("Loading 1s\n\nagent step 1: list_files", 48);
    let rows = compose_rendered_dock_rows("deepseek [model] › ", "", 0, 48, &panel, false);
    let plain = rows
        .lines
        .iter()
        .map(|row| strip_ansi_for_test(row))
        .collect::<Vec<_>>();

    let loading_row = plain
        .iter()
        .position(|row| row.trim_end() == "Loading 1s")
        .unwrap();
    let step_row = plain
        .iter()
        .position(|row| row.trim_end() == "agent step 1: list_files")
        .unwrap();
    let prompt_row = plain
        .iter()
        .position(|row| row.starts_with("deepseek [model] ›"))
        .unwrap();
    let help_row = plain
        .iter()
        .position(|row| row.contains("Enter send"))
        .unwrap();

    assert!(loading_row < prompt_row);
    assert!(step_row < prompt_row);
    assert_eq!(loading_row + 2, step_row);
    assert!(prompt_row < help_row);
}

#[test]
fn sequential_stream_cursor_advances_with_text() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.set_transcript_start_row(Some(0));
    composer.advance_transcript_text("abc");
    assert_eq!(composer.transcript_cursor_row, Some(0));
    assert_eq!(composer.transcript_cursor_column, 3);
    composer.advance_transcript_text("\ndef");
    assert_eq!(composer.transcript_cursor_row, Some(1));
    assert_eq!(composer.transcript_cursor_column, 3);
}

#[test]
fn transcript_cursor_ignores_ansi_sequences() {
    let mut composer = DockedComposer::new("prompt › ".to_string());
    composer.set_transcript_start_row(Some(0));
    composer.advance_transcript_text("\x1b[36;1ma\x1b[0m\x1b[38;2;125;207;255mb\x1b[0m");

    assert_eq!(composer.transcript_cursor_row, Some(0));
    assert_eq!(composer.transcript_cursor_column, 2);
}

#[test]
fn parses_forced_terminal_size() {
    assert_eq!(parse_forced_terminal_size("80x24"), Some((80, 24)));
    assert_eq!(parse_forced_terminal_size("0x0"), Some((1, 1)));
    assert_eq!(parse_forced_terminal_size("80:24"), None);
}

fn strip_ansi_for_test(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if take_ansi_sequence(ch, &mut chars).is_some() {
            continue;
        }
        out.push(ch);
    }
    out
}
