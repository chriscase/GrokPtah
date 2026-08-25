//! Bounded range and cursor reads, UTF-8 continuity, and classification.

use super::support::Fixture;
use crate::{
    ContentVerdict, DocumentIdentity, Eol, IdentityStability, MAX_CHUNK_BYTES, MAX_CHUNK_LINES,
    MAX_LINE_CHARS, RequestedLimits, SourceRequest, SourceViewError, Utf8Decoder,
    read::EffectiveLimits,
};

fn limits(bytes: Option<u64>, lines: Option<usize>, chars: Option<usize>) -> RequestedLimits {
    RequestedLimits {
        max_bytes: bytes,
        max_lines: lines,
        max_line_chars: chars,
    }
}

// ------------------------------------------------------------- limits

#[test]
fn caller_limits_are_clamped_into_the_supported_window() {
    let huge = EffectiveLimits::clamp(limits(Some(u64::MAX), Some(usize::MAX), Some(usize::MAX)));
    assert_eq!(huge.max_bytes, MAX_CHUNK_BYTES);
    assert_eq!(huge.max_lines, MAX_CHUNK_LINES);
    assert_eq!(huge.max_line_chars, MAX_LINE_CHARS);

    let zero = EffectiveLimits::clamp(limits(Some(0), Some(0), Some(0)));
    let unset = EffectiveLimits::clamp(RequestedLimits::default());
    assert_eq!(zero, unset, "zero means default, never unbounded");
}

#[test]
fn the_effective_limits_are_reported_back() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let document = fixture
        .open_request(
            &SourceRequest::new(&token, "src/main.rs").with_limits(limits(Some(64), Some(2), None)),
        )
        .expect("read");
    assert_eq!(document.limits.max_bytes, 64);
    assert_eq!(document.limits.max_lines, 2);
    assert_eq!(document.limits.max_line_chars, 2_000);
}

#[test]
fn a_file_above_the_document_ceiling_is_refused_without_reading_it() {
    let fixture = Fixture::new();
    let path = fixture.path("huge.bin");
    let file = std::fs::File::create(&path).expect("create");
    file.set_len(crate::MAX_DOCUMENT_BYTES + 1)
        .expect("set_len");
    drop(file);
    let token = fixture.token();

    assert_eq!(
        fixture.open(&token, "huge.bin").unwrap_err(),
        SourceViewError::TooLarge {
            byte_len: crate::MAX_DOCUMENT_BYTES + 1,
            max_bytes: crate::MAX_DOCUMENT_BYTES,
        },
    );
}

// --------------------------------------------------------- range reads

#[test]
fn a_whole_small_file_arrives_in_one_chunk_with_real_line_numbers() {
    let fixture = Fixture::new();
    let token = fixture.token();
    let document = fixture.open(&token, "src/nested/deep.txt").expect("read");

    assert_eq!(document.content.verdict, ContentVerdict::Text);
    assert!(document.content.complete_scan);
    assert_eq!(document.chunk.eol, Eol::Lf);
    assert!(document.chunk.eof);
    assert!(document.chunk.next_cursor.is_none());
    assert_eq!(
        document
            .chunk
            .lines
            .iter()
            .map(|line| (line.number, line.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "alpha"), (2, "beta"), (3, "gamma")],
    );
}

#[test]
fn a_range_read_starts_where_it_is_told() {
    let fixture = Fixture::new();
    fixture.write("range.txt", b"one\ntwo\nthree\n");
    let token = fixture.token();

    let document = fixture
        .open_request(&SourceRequest::new(&token, "range.txt").at_byte(4))
        .expect("read");
    assert_eq!(document.chunk.start_byte, 4);
    assert_eq!(Fixture::chunk_text(&document), "two\nthree");
}

#[test]
fn a_range_past_the_end_is_refused_rather_than_clamped() {
    let fixture = Fixture::new();
    fixture.write("range.txt", b"short\n");
    let token = fixture.token();
    assert_eq!(
        fixture
            .open_request(&SourceRequest::new(&token, "range.txt").at_byte(9_999))
            .unwrap_err(),
        SourceViewError::RangeInvalid,
    );
}

#[test]
fn a_range_exactly_at_the_end_yields_an_empty_final_chunk() {
    let fixture = Fixture::new();
    fixture.write("range.txt", b"short\n");
    let token = fixture.token();
    let document = fixture
        .open_request(&SourceRequest::new(&token, "range.txt").at_byte(6))
        .expect("read");
    assert!(document.chunk.lines.is_empty());
    assert!(document.chunk.eof);
}

// -------------------------------------------------------- cursor reads

#[test]
fn paging_a_file_visits_every_line_exactly_once_in_order() {
    let fixture = Fixture::new();
    let body: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    fixture.write("many.txt", body.as_bytes());
    let token = fixture.token();

    let lines = fixture.read_all(&token, "many.txt", 64);
    assert_eq!(lines.len(), 200);
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(line.number, index + 1);
        assert_eq!(line.text, format!("line {}", index + 1));
    }
}

#[test]
fn a_chunk_that_ends_mid_line_says_so_and_the_next_one_continues_it() {
    let fixture = Fixture::new();
    fixture.write("split.txt", b"abcdefghij\nsecond\n");
    let token = fixture.token();

    let first = fixture
        .open_request(&SourceRequest::new(&token, "split.txt").with_limits(limits(
            Some(4),
            None,
            None,
        )))
        .expect("read");
    assert!(first.chunk.continues_next, "the chunk stopped mid-line");
    assert!(!first.chunk.continues_previous);
    assert_eq!(first.chunk.lines[0].number, 1);
    assert_eq!(first.chunk.lines[0].text, "abcd");

    let cursor = first.chunk.next_cursor.expect("more");
    assert!(cursor.continues_line);
    assert_eq!(
        cursor.next_line_number, 1,
        "the unfinished line keeps its number"
    );

    let second = fixture
        .open_request(
            &SourceRequest::new(&token, "split.txt")
                .with_limits(limits(Some(4), None, None))
                .resume(cursor),
        )
        .expect("read");
    assert!(second.chunk.continues_previous);
    assert_eq!(second.chunk.lines[0].number, 1);
    assert_eq!(second.chunk.lines[0].text, "efgh");
}

#[test]
fn the_line_assembler_rejoins_a_continued_line() {
    let fixture = Fixture::new();
    fixture.write("split.txt", b"abcdefghij\nsecond\n");
    let token = fixture.token();
    let lines = fixture.read_all(&token, "split.txt", 4);
    assert_eq!(
        lines
            .iter()
            .map(|line| (line.number, line.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "abcdefghij"), (2, "second")],
    );
}

#[test]
fn a_cursor_minted_against_other_content_is_refused() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"aaa\nbbb\nccc\nddd\n");
    fixture.write("b.txt", b"zzz\nyyy\nxxx\nwww\n");
    let token = fixture.token();

    let first = fixture
        .open_request(&SourceRequest::new(&token, "a.txt").with_limits(limits(Some(4), None, None)))
        .expect("read");
    let cursor = first.chunk.next_cursor.expect("more to read");

    assert_eq!(
        fixture
            .open_request(&SourceRequest::new(&token, "b.txt").resume(cursor))
            .unwrap_err(),
        SourceViewError::CursorInvalid,
    );
}

#[test]
fn a_cursor_with_a_corrupt_carry_is_refused() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"aaa\nbbb\n");
    let token = fixture.token();
    let first = fixture
        .open_request(&SourceRequest::new(&token, "a.txt").with_limits(limits(Some(4), None, None)))
        .expect("read");
    let mut cursor = first.chunk.next_cursor.expect("more");
    cursor.carry_hex = "zz".into();
    assert_eq!(
        fixture
            .open_request(&SourceRequest::new(&token, "a.txt").resume(cursor))
            .unwrap_err(),
        SourceViewError::CursorInvalid,
    );
}

#[test]
fn a_cursor_with_a_zero_line_number_is_refused() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"aaa\nbbb\n");
    let token = fixture.token();
    let first = fixture
        .open_request(&SourceRequest::new(&token, "a.txt").with_limits(limits(Some(4), None, None)))
        .expect("read");
    let mut cursor = first.chunk.next_cursor.expect("more");
    cursor.next_line_number = 0;
    assert_eq!(
        fixture
            .open_request(&SourceRequest::new(&token, "a.txt").resume(cursor))
            .unwrap_err(),
        SourceViewError::CursorInvalid,
    );
}

#[test]
fn the_line_cap_stops_a_chunk_and_the_cursor_resumes_exactly_there() {
    let fixture = Fixture::new();
    let body: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    fixture.write("many.txt", body.as_bytes());
    let token = fixture.token();

    let first = fixture
        .open_request(&SourceRequest::new(&token, "many.txt").with_limits(limits(
            None,
            Some(5),
            None,
        )))
        .expect("read");
    assert_eq!(first.chunk.lines.len(), 5);
    assert_eq!(first.chunk.lines[4].number, 5);

    let cursor = first.chunk.next_cursor.expect("more to read");
    assert_eq!(cursor.next_line_number, 6);
    let second = fixture
        .open_request(
            &SourceRequest::new(&token, "many.txt")
                .with_limits(limits(None, Some(5), None))
                .resume(cursor),
        )
        .expect("read");
    assert_eq!(second.chunk.lines[0].number, 6);
    assert_eq!(second.chunk.lines[0].text, "line 6");
}

#[test]
fn a_line_wider_than_the_chunk_still_makes_progress_and_keeps_its_bytes() {
    let fixture = Fixture::new();
    let long = "x".repeat(5_000);
    fixture.write("wide.txt", format!("{long}\nafter\n").as_bytes());
    let token = fixture.token();

    let lines = fixture.read_all(&token, "wide.txt", 512);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].number, 1);
    assert_eq!(
        lines[0].text.len(),
        5_000,
        "a long line is reassembled whole"
    );
    assert_eq!(lines[1].text, "after");
}

#[test]
fn a_wide_line_is_cut_on_a_character_boundary() {
    let fixture = Fixture::new();
    fixture.write("wide.txt", "é".repeat(200).as_bytes());
    let token = fixture.token();
    let document = fixture
        .open_request(&SourceRequest::new(&token, "wide.txt").with_limits(limits(
            None,
            None,
            Some(16),
        )))
        .expect("read");
    let line = &document.chunk.lines[0];
    assert!(line.truncated);
    assert_eq!(line.text.chars().count(), 16);
    assert!(line.text.chars().all(|c| c == 'é'));
}

// ----------------------------------------------------- utf-8 continuity

#[test]
fn a_multi_byte_character_split_across_chunks_survives() {
    let fixture = Fixture::new();
    // Each line is eight 4-byte emoji, so a 7-byte budget is guaranteed to cut
    // characters as well as lines.
    let body = "🎯🎯🎯🎯🎯🎯🎯🎯\nsecond line\n";
    fixture.write("emoji.txt", body.as_bytes());
    let token = fixture.token();

    let lines = fixture.read_all(&token, "emoji.txt", 7);
    assert_eq!(
        lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        vec!["🎯🎯🎯🎯🎯🎯🎯🎯", "second line"],
    );
}

#[test]
fn no_chunk_boundary_produces_a_replacement_for_valid_input() {
    let fixture = Fixture::new();
    let body = "aé☃🎯 mixed widths é☃🎯\nsecond ☃ line\n";
    fixture.write("mixed.txt", body.as_bytes());
    let token = fixture.token();

    // Every budget from 1 to 24 bytes cuts somewhere different; none may
    // corrupt a character.
    for budget in 1..=24u64 {
        let mut request =
            SourceRequest::new(&token, "mixed.txt").with_limits(limits(Some(budget), None, None));
        let mut assembler = crate::LineAssembler::new();
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 500, "paging must terminate at budget {budget}");
            let document = fixture.open_request(&request).expect("read");
            assert_eq!(
                document.chunk.lossy_replacements, 0,
                "budget {budget} corrupted a character",
            );
            assembler.push_chunk(&document.chunk);
            match document.chunk.next_cursor {
                Some(cursor) => {
                    request = SourceRequest::new(&token, "mixed.txt")
                        .with_limits(limits(Some(budget), None, None))
                        .resume(cursor)
                }
                None => break,
            }
        }
        let lines = assembler.finish();
        assert_eq!(
            lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["aé☃🎯 mixed widths é☃🎯", "second ☃ line"],
            "budget {budget} lost or reordered content",
        );
    }
}

#[test]
fn the_incremental_decoder_carries_every_split_offset() {
    let text = "aé☃🎯z";
    let bytes = text.as_bytes();
    for split in 0..bytes.len() {
        let mut decoder = Utf8Decoder::new();
        let mut out = String::new();
        let first = decoder.decode(&bytes[..split], false);
        out.push_str(&first.text);
        let second = decoder.decode(&bytes[split..], true);
        out.push_str(&second.text);
        assert_eq!(out, text, "splitting at {split} must be lossless");
        assert_eq!(
            first.replacements + second.replacements,
            0,
            "splitting a valid string must never produce replacements",
        );
        assert!(!decoder.has_carry());
    }
}

#[test]
fn the_decoder_replaces_genuinely_invalid_bytes() {
    let mut decoder = Utf8Decoder::new();
    let decoded = decoder.decode(&[b'a', 0xff, b'b'], true);
    assert_eq!(decoded.text, "a\u{FFFD}b");
    assert_eq!(decoded.replacements, 1);
}

#[test]
fn an_incomplete_sequence_at_end_of_stream_becomes_a_replacement() {
    let mut decoder = Utf8Decoder::new();
    let held = decoder.decode(&[b'a', 0xf0, 0x9f], false);
    assert_eq!(held.text, "a");
    assert!(decoder.has_carry());
    let flushed = decoder.decode(&[], true);
    assert_eq!(flushed.text, "\u{FFFD}");
    assert_eq!(flushed.replacements, 1);
}

#[test]
fn a_carry_longer_than_three_bytes_is_rejected() {
    assert!(Utf8Decoder::resume(vec![0xf0, 0x9f, 0x8e, 0xaf]).is_none());
    assert!(Utf8Decoder::resume(vec![0xf0, 0x9f, 0x8e]).is_some());
}

// ------------------------------------------------------ classification

#[test]
fn nul_bearing_content_is_binary_and_no_text_is_returned() {
    let fixture = Fixture::new();
    fixture.write("image.bin", &[0x89, 0x50, 0x00, 0x4e, 0x47]);
    let token = fixture.token();
    let document = fixture.open(&token, "image.bin").expect("read");

    assert_eq!(document.content.verdict, ContentVerdict::Binary);
    assert!(document.content.complete_scan);
    assert!(document.chunk.lines.is_empty());
    assert!(document.chunk.next_cursor.is_none());
}

#[test]
fn invalid_utf8_without_nul_is_lossy_text_and_the_replacements_are_counted() {
    let fixture = Fixture::new();
    fixture.write("latin.txt", &[b'a', 0xff, b'b', b'\n', 0xfe, b'\n']);
    let token = fixture.token();
    let document = fixture.open(&token, "latin.txt").expect("read");

    assert_eq!(document.content.verdict, ContentVerdict::TextLossy);
    assert_eq!(document.chunk.lossy_replacements, 2);
    assert!(document.chunk.lines[0].text.contains('\u{FFFD}'));
}

#[test]
fn a_classification_that_only_saw_a_prefix_says_so() {
    let fixture = Fixture::new();
    let mut body = vec![b'a'; usize::try_from(crate::BINARY_SCAN_BYTES).unwrap() + 4_096];
    body[0] = b'x';
    fixture.write("long.txt", &body);
    let token = fixture.token();
    let document = fixture.open(&token, "long.txt").expect("read");

    assert_eq!(document.content.verdict, ContentVerdict::Text);
    assert_eq!(document.content.scanned_bytes, crate::BINARY_SCAN_BYTES);
    assert!(
        !document.content.complete_scan,
        "a verdict from a prefix must not claim to cover the file",
    );
}

#[test]
fn a_prefix_cut_mid_character_is_not_mistaken_for_invalid_utf8() {
    let fixture = Fixture::new();
    let mut body = "é"
        .repeat(usize::try_from(crate::BINARY_SCAN_BYTES).unwrap())
        .into_bytes();
    body.truncate(usize::try_from(crate::BINARY_SCAN_BYTES).unwrap() + 1);
    fixture.write("wide.txt", &body);
    let token = fixture.token();
    let document = fixture.open(&token, "wide.txt").expect("read");
    assert_eq!(document.content.verdict, ContentVerdict::Text);
    assert!(!document.content.complete_scan);
}

// ---------------------------------------------------------- identities

#[test]
fn document_identity_is_a_content_digest_within_budget() {
    let fixture = Fixture::new();
    fixture.write("a.txt", b"same bytes\n");
    fixture.write("b.txt", b"same bytes\n");
    fixture.write("c.txt", b"other bytes\n");
    let token = fixture.token();

    let digest = |name: &str| match fixture.open(&token, name).expect("read").identity {
        DocumentIdentity::Content { digest } => digest,
        other => panic!("expected a content digest, got {other:?}"),
    };
    assert_eq!(digest("a.txt"), digest("b.txt"));
    assert_ne!(digest("a.txt"), digest("c.txt"));
    assert_eq!(digest("a.txt").len(), 64, "BLAKE3-256 in hex");
}

#[test]
fn a_file_beyond_the_digest_budget_reports_a_pinned_identity() {
    let fixture = Fixture::new();
    let path = fixture.path("big.bin");
    let file = std::fs::File::create(&path).expect("create");
    file.set_len(crate::CONTENT_DIGEST_BUDGET + 1).expect("len");
    drop(file);
    let token = fixture.token();

    // A sparse file of NULs classifies as binary, which is the honest answer;
    // the identity is still reported as pinned rather than as content.
    let document = fixture.open(&token, "big.bin").expect("read");
    match document.identity {
        DocumentIdentity::Pinned { stability, digest } => {
            assert_eq!(digest.len(), 64);
            if cfg!(unix) {
                assert_eq!(stability, IdentityStability::Exact);
            }
        }
        other => panic!("expected a pinned identity, got {other:?}"),
    }
}

// ---------------------------------------------------------- line shape

#[test]
fn line_ending_shape_is_reported_per_chunk() {
    let fixture = Fixture::new();
    fixture.write("crlf.txt", b"a\r\nb\r\n");
    fixture.write("mixed.txt", b"a\r\nb\n");
    fixture.write("none.txt", b"single line, no newline");
    let token = fixture.token();

    let eol = |name: &str| fixture.open(&token, name).expect("read").chunk.eol;
    assert_eq!(eol("crlf.txt"), Eol::Crlf);
    assert_eq!(eol("mixed.txt"), Eol::Mixed);
    assert_eq!(eol("none.txt"), Eol::None);
}

#[test]
fn carriage_returns_are_stripped_from_rendered_lines() {
    let fixture = Fixture::new();
    fixture.write("crlf.txt", b"alpha\r\nbeta\r\n");
    let token = fixture.token();
    let document = fixture.open(&token, "crlf.txt").expect("read");
    assert_eq!(Fixture::chunk_text(&document), "alpha\nbeta");
}

#[test]
fn an_empty_file_yields_no_lines_rather_than_one_blank_line() {
    let fixture = Fixture::new();
    fixture.write("empty.txt", b"");
    let token = fixture.token();
    let document = fixture.open(&token, "empty.txt").expect("read");
    assert!(document.chunk.lines.is_empty());
    assert!(document.chunk.eof);
    assert_eq!(document.byte_len, 0);
}

#[test]
fn a_file_without_a_trailing_newline_keeps_its_last_line() {
    let fixture = Fixture::new();
    fixture.write("tail.txt", b"one\ntwo");
    let token = fixture.token();
    let document = fixture.open(&token, "tail.txt").expect("read");
    assert_eq!(Fixture::chunk_text(&document), "one\ntwo");
}

#[test]
fn the_language_hint_comes_from_the_name() {
    assert_eq!(crate::language_for("src/main.rs"), "rust");
    assert_eq!(crate::language_for("src/App.tsx"), "tsx");
    assert_eq!(crate::language_for("Cargo.lock"), "toml");
    assert_eq!(crate::language_for("Dockerfile"), "dockerfile");
    assert_eq!(crate::language_for("notes"), "plain");
}

#[test]
fn a_genuine_blank_line_survives_paging() {
    let fixture = Fixture::new();
    fixture.write("blanks.txt", b"first\n\nthird\n\n\nsixth\n");
    let token = fixture.token();

    for budget in [3u64, 5, 7, 4096] {
        let lines = fixture.read_all(&token, "blanks.txt", budget);
        assert_eq!(
            lines
                .iter()
                .map(|line| (line.number, line.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, "first"),
                (2, ""),
                (3, "third"),
                (4, ""),
                (5, ""),
                (6, "sixth"),
            ],
            "budget {budget} altered the blank lines",
        );
    }
}

#[test]
fn a_file_that_is_only_newlines_yields_only_blank_lines() {
    let fixture = Fixture::new();
    fixture.write("newlines.txt", b"\n\n\n");
    let token = fixture.token();
    let lines = fixture.read_all(&token, "newlines.txt", 4096);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.text.is_empty()));
}
