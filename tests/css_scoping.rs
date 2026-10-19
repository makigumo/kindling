//! Stylesheet scoping: a sheet linked only by fixed-layout pages is inlined
//! there instead of entering the shared KF8 CSS flow.
//!
//! The flow is global — every spine document links it — so a rule that only
//! makes sense for a fixed-layout cover still applies to every reflowable
//! chapter once the sheet is concatenated into it. The `fixed_layout_css`
//! fixture is the shape that bites: a pre-paginated front page linking a
//! cover sheet that zeroes the root font size, beside two reflowable
//! chapters that never link it and share a normal sheet of their own.
//!
//! The test checks both halves of the contract: the cover rule reaches the
//! front page (as an inline `<style>` at the position of its `<link>`) and
//! does not reach the shared flow, while the shared sheet still becomes
//! `kindle:flow:0001` for every page.

mod common;

use common::*;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fixed_layout_css")
}

fn build_fixture(opf: &str) -> ParsedMobi {
    let tmp = std::env::temp_dir()
        .join("kindling_css_scoping")
        .join(opf.trim_end_matches(".opf"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let out = tmp.join("out.azw3");
    let run = Command::new(kindling_bin())
        .arg("build")
        .arg(fixture_dir().join(opf))
        .arg("-o")
        .arg(&out)
        .arg("--no-validate")
        .output()
        .expect("failed to spawn kindling-cli");
    assert!(
        run.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    parse_mobi_file(&fs::read(&out).unwrap()).unwrap()
}

/// The KF8 text split at its FDST boundaries: `[0]` is the HTML flow,
/// `[1]` the CSS flow.
fn kf8_flows(parsed: &ParsedMobi) -> Vec<Vec<u8>> {
    let kf8 = parsed.kf8_or_kf7();
    let text = extract_text_blob(parsed, kf8);
    let fdst_raw = kf8.header.fdst_index;
    assert_ne!(fdst_raw, 0xFFFFFFFF, "the fixture should have an FDST");
    let fdst_abs = kf8.record_idx + fdst_raw as usize;
    let rec = parsed.palmdb.record(&parsed.raw, fdst_abs);
    let flows = parse_fdst(rec).expect("FDST should parse");
    flows
        .iter()
        .map(|flow| text[flow.start as usize..flow.end as usize].to_vec())
        .collect()
}

#[test]
fn a_cover_only_stylesheet_is_inlined_and_kept_out_of_the_shared_flow() {
    let parsed = build_fixture("book.opf");
    let docs: Vec<String> = reconstruct_parts_from_mobi(&parsed)
        .expect("KF8 documents should reconstruct")
        .iter()
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect();
    assert_eq!(docs.len(), 3, "one document per spine entry");
    let flows = kf8_flows(&parsed);
    assert_eq!(flows.len(), 2, "HTML flow + shared CSS flow");
    let shared_css = String::from_utf8_lossy(&flows[1]);

    // The cover's rule is scoped to the document that linked it. If it were
    // in the flow, every reflowable page would inherit a zero root size.
    assert!(
        !shared_css.contains("font-size: 0;"),
        "cover-only rule leaked into the shared flow:\n{shared_css}"
    );
    assert!(
        shared_css.contains("font-size: 100%"),
        "the shared sheet should still be in the flow:\n{shared_css}"
    );

    let front = &docs[0];
    assert!(
        front.contains("<style type=\"text/css\">"),
        "the cover sheet should be inlined into its page:\n{front}"
    );
    assert!(
        front.contains("font-size: 0;"),
        "the inlined cover sheet lost its rule:\n{front}"
    );
    assert!(
        front.contains("kindle:flow:0001"),
        "the page's shared-sheet link should still point at the flow:\n{front}"
    );

    // Neither reflowable chapter links the cover sheet, so neither may carry
    // its rule — that is the bug this fixture exists for.
    for (i, chapter) in docs.iter().enumerate().skip(1) {
        assert!(
            chapter.contains("kindle:flow:0001"),
            "chapter {i} lost its stylesheet:\n{chapter}"
        );
        assert!(
            !chapter.contains("font-size: 0;"),
            "chapter {i} picked up the cover-only rule:\n{chapter}"
        );
    }
}

/// A bare image cover page is dropped from the output, so there is no
/// document left to inline its sheet into. It still must not reach the flow:
/// the sheet was cover-only in the source and the surviving chapters never
/// linked it.
#[test]
fn a_dropped_bare_cover_does_not_leave_its_sheet_in_the_shared_flow() {
    let parsed = build_fixture("bare.opf");
    let docs: Vec<String> = reconstruct_parts_from_mobi(&parsed)
        .expect("KF8 documents should reconstruct")
        .iter()
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect();
    assert_eq!(docs.len(), 2, "the bare cover page should be dropped");

    let flows = kf8_flows(&parsed);
    assert_eq!(flows.len(), 2, "HTML flow + shared CSS flow");
    let shared_css = String::from_utf8_lossy(&flows[1]);
    assert!(
        !shared_css.contains("font-size: 0;"),
        "a dropped cover's rule leaked into the shared flow:\n{shared_css}"
    );
    assert!(
        shared_css.contains("font-size: 100%"),
        "the shared sheet should still be in the flow:\n{shared_css}"
    );

    for (i, chapter) in docs.iter().enumerate() {
        assert!(
            chapter.contains("kindle:flow:0001"),
            "chapter {i} lost its stylesheet:\n{chapter}"
        );
        assert!(
            !chapter.contains("font-size: 0;"),
            "chapter {i} picked up the dropped cover's rule:\n{chapter}"
        );
    }
}
