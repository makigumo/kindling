//! Reflowable books must carry the OPF spine's page-progression-direction
//! into the EXTH header.
//!
//! kindlegen derives EXTH 525 (primary writing mode) and 527 (page
//! progression direction) from the spine for reflowable books as well as
//! fixed-layout ones. Without 527 the KF8 reader turns pages left-to-right
//! even when the text is vertical, so tapping the left edge goes back.
//!
//! These builds exercise the whole path - OPF parse, `build_book_exth`, and
//! the KF8 record - rather than the EXTH writer alone.

mod common;

use common::*;

use std::fs;
use std::path::Path;
use std::process::Command;

/// Build a one-chapter reflowable book with the given spine direction and
/// return its parsed KF8 metadata.
fn build_with_ppd(label: &str, ppd: Option<&str>) -> ParsedMobi {
    let dir = std::env::temp_dir().join(format!("kindling_ppd_{label}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let spine_attr = ppd
        .map(|p| format!(" page-progression-direction=\"{p}\""))
        .unwrap_or_default();
    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="BookId">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>PPD Fixture</dc:title>
    <dc:language>ja</dc:language>
    <dc:identifier id="BookId">kindling-ppd-{label}</dc:identifier>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine{spine_attr}>
    <itemref idref="ch1"/>
  </spine>
</package>
"#
    );
    fs::write(dir.join("book.opf"), opf).unwrap();
    fs::write(
        dir.join("ch1.xhtml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" class="vrtl">
<head><title>PPD Fixture</title></head>
<body><p>縦書きの本文</p></body>
</html>
"#,
    )
    .unwrap();

    let out = dir.join("out.azw3");
    build_opf(&dir.join("book.opf"), &out);
    parse_mobi_file(&fs::read(&out).unwrap()).expect("built file should parse")
}

fn build_opf(opf: &Path, out: &Path) {
    let run = Command::new(kindling_bin())
        .arg("build")
        .arg(opf)
        .arg("-o")
        .arg(out)
        .arg("--no-validate")
        .output()
        .expect("failed to spawn kindling-cli");
    assert!(
        run.status.success(),
        "build failed for {}: {}\n{}",
        opf.display(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn exth_string(section: &MobiSection, tag: u32) -> Option<String> {
    section
        .exth_first(tag)
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

#[test]
fn rtl_reflowable_book_gets_vertical_writing_and_rtl_progression() {
    let parsed = build_with_ppd("rtl", Some("rtl"));
    let section = parsed.kf8_or_kf7();

    assert_eq!(
        exth_string(section, 525).as_deref(),
        Some("vertical-rl"),
        "an rtl spine must give a reflowable book vertical-rl writing mode"
    );
    assert_eq!(
        exth_string(section, 527).as_deref(),
        Some("rtl"),
        "an rtl spine must give a reflowable book rtl page progression"
    );
}

#[test]
fn ltr_reflowable_book_keeps_horizontal_writing_and_writes_ltr() {
    let parsed = build_with_ppd("ltr", Some("ltr"));
    let section = parsed.kf8_or_kf7();

    assert_eq!(
        exth_string(section, 525).as_deref(),
        Some("horizontal-lr"),
        "an ltr spine must keep horizontal-lr writing mode"
    );
    assert_eq!(
        exth_string(section, 527).as_deref(),
        Some("ltr"),
        "an explicit ltr spine must be recorded in EXTH 527"
    );
}

#[test]
fn absent_ppd_writes_horizontal_writing_without_527() {
    let parsed = build_with_ppd("none", None);
    let section = parsed.kf8_or_kf7();

    assert_eq!(
        exth_string(section, 525).as_deref(),
        Some("horizontal-lr"),
        "a book with no spine direction must default to horizontal-lr"
    );
    assert_eq!(
        exth_string(section, 527),
        None,
        "a book with no spine direction must not invent EXTH 527"
    );
}
