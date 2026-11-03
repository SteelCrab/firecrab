use super::*;

const SAMPLE: &str = "# Rules\n\n- a\n  more a\n- b\n\nPlain\ntext\n";

#[test]
fn headings_list_items_and_paragraphs_are_blocks() {
    assert_eq!(
        split(SAMPLE),
        [
            Block { start: 0, end: 1 },
            Block { start: 2, end: 4 },
            Block { start: 4, end: 5 },
            Block { start: 6, end: 8 },
        ]
    );
    assert_eq!(text_of(SAMPLE, Block { start: 2, end: 4 }), "- a\n  more a");
}

#[test]
fn fenced_code_is_one_block_even_with_blank_lines() {
    assert_eq!(split("```sh\na\n\nb\n```\n"), [Block { start: 0, end: 5 }]);
    assert_eq!(
        split("```\nopen\n\nnever closed"),
        [Block { start: 0, end: 4 }]
    );
}

#[test]
fn replace_changes_only_the_target_block() {
    assert_eq!(
        replace(SAMPLE, 2, "- B!").unwrap(),
        "# Rules\n\n- a\n  more a\n- B!\n\nPlain\ntext\n"
    );
    assert_eq!(
        replace(SAMPLE, 1, "- A\n  two lines\n").unwrap(),
        "# Rules\n\n- A\n  two lines\n- b\n\nPlain\ntext\n"
    );
}

#[test]
fn replace_keeps_crlf_and_missing_final_newline() {
    assert_eq!(replace("- a\r\n- b", 0, "- A").unwrap(), "- A\r\n- b");
    assert_eq!(replace("- a\r\n- b", 1, "- B").unwrap(), "- a\r\n- B");
}

#[test]
fn replace_rejects_out_of_range_index() {
    assert!(replace(SAMPLE, 4, "x").is_err());
    assert!(replace("", 0, "x").is_err());
}

#[test]
fn append_adds_a_blank_line_separator() {
    assert_eq!(append("# R\n- a\n", "- b"), "# R\n- a\n\n- b\n");
    assert_eq!(append("", "- b\n"), "- b\n");
    assert_eq!(append("x", "- b"), "x\n\n- b\n");
    assert_eq!(append("x\n\n", "- b"), "x\n\n- b\n");
    assert_eq!(append("x\r\n", "- b"), "x\r\n\r\n- b\r\n");
}
