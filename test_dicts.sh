#!/bin/bash
# =============================================================================
# Variant dictionary generator for on-device debugging.
#
# Each variant patches src/mobi.rs, rebuilds, and writes one .mobi, so a device
# result can be traced to a single changed variable. Born in the v0.6
# white-screen investigation (issue #27); kept because it is the cheapest way to
# isolate a failure that only shows up on hardware.
#
# Every patch is verified before it is applied: the whole run aborts if a
# pattern no longer matches the source, instead of quietly producing a baseline
# build and labelling it as a variant. `--check-patches` applies all of them
# without building, which is the quickest way to see that the harness still
# matches the code.
#
# Usage:
#   OPF_ORIG=/path/to/dict.opf ./test_dicts.sh              # every variant
#   OPF_ORIG=/path/to/dict.opf ./test_dicts.sh Test03 Test09  # just those
#   ./test_dicts.sh --check-patches                         # patches only
#
# Environment:
#   OPF_ORIG    dictionary OPF to build (required unless --check-patches)
#   OUTPUT_DIR  where the .mobi files go (default: target/test_dicts)
#
# src/mobi.rs is copied to a temporary directory on entry and restored on exit
# (success, failure or interrupt), so an uncommitted tree survives a run
# unchanged. The release binary is left as the last variant's build; the next
# cargo invocation rebuilds it from the restored source.
# =============================================================================
set -euo pipefail

KINDLING_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC_MOBI="$KINDLING_DIR/src/mobi.rs"
BINARY="$KINDLING_DIR/target/release/kindling-cli"
OUTPUT_DIR="${OUTPUT_DIR:-$KINDLING_DIR/target/test_dicts}"
OPF_ORIG="${OPF_ORIG:-}"
OPF_DIR=""
TEST_OPF=""

die() {
    echo "error: $*" >&2
    exit 1
}

usage() {
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"
}

# =============================================================================
# Source backup and cleanup
# =============================================================================

[[ -f $SRC_MOBI ]] || die "source not found: $SRC_MOBI"

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/kindling-variants.XXXXXX")
SOURCE_BACKUP="$WORK_DIR/mobi.rs"
cp "$SRC_MOBI" "$SOURCE_BACKUP"
CLEANED=0

cleanup() {
    local status=$?
    [[ $CLEANED -eq 0 ]] || return 0
    CLEANED=1
    local changed=0
    if ! cmp -s "$SOURCE_BACKUP" "$SRC_MOBI"; then
        changed=1
        cp "$SOURCE_BACKUP" "$SRC_MOBI"
    fi
    if [[ -n $TEST_OPF ]]; then
        rm -f "$TEST_OPF"
    fi
    rm -rf "$WORK_DIR"
    if [[ $changed -eq 1 && $status -eq 0 ]]; then
        echo ""
        echo "=== src/mobi.rs restored unchanged."
    elif [[ $changed -eq 1 ]]; then
        echo ""
        echo "=== Variant run failed (exit $status); src/mobi.rs restored unchanged." >&2
    fi
    return 0
}
trap cleanup EXIT
trap 'exit 130' INT TERM

restore_source() {
    cp "$SOURCE_BACKUP" "$SRC_MOBI"
}

# =============================================================================
# Patch helpers. Both abort the run when the source no longer matches, so a
# variant can never be built from an unpatched tree.
# =============================================================================

# patch_text <file> <expected-sites> <old> <new> <label>
patch_text() {
    local file=$1 expected=$2 old=$3 new=$4 label=$5
    python3 - "$file" "$expected" "$old" "$new" "$label" <<'PY'
import sys

path, expected, old, new, label = (
    sys.argv[1],
    int(sys.argv[2]),
    sys.argv[3],
    sys.argv[4],
    sys.argv[5],
)
if old == new:
    sys.exit(f"patch {label!r}: its replacement is identical to its pattern")
with open(path, encoding="utf-8") as handle:
    source = handle.read()
found = source.count(old)
if found != expected:
    sys.exit(
        f"patch {label!r} no longer matches {path}: expected {expected} site(s), "
        f"found {found}.\nPattern:\n{old}\nUpdate the patch to the current source."
    )
with open(path, "w", encoding="utf-8") as handle:
    handle.write(source.replace(old, new, expected))
print(f"  patched {label} ({found} site(s))")
PY
}

# patch_trailers <swap|remove>
#
# Touches only the trailer bytes written by compress_text and
# split_text_uncompressed. The record table, EXTH and every other push(0x00) in
# the file are deliberately out of scope.
patch_trailers() {
    local mode=$1
    python3 - "$SRC_MOBI" "$mode" <<'PY'
import re
import sys

path, mode = sys.argv[1], sys.argv[2]
WANTED = 6
BOUNDARY = re.compile(r"^(?:fn |#\[cfg\(test\)\]|mod )", re.M)


def body(text, name):
    start = re.search(rf"^fn {name}\b", text, re.M)
    if start is None:
        sys.exit(f"patch trailers: no `fn {name}` in {path}")
    rest = text[start.end():]
    following = BOUNDARY.search(rest)
    end = start.end() + following.start() if following else len(text)
    return text[start.start():end]


with open(path, encoding="utf-8") as handle:
    source = handle.read()

touched = 0
for name in ("compress_text", "split_text_uncompressed"):
    segment = body(source, name)
    if mode == "swap":
        def flip(match):
            global touched
            touched += 1
            value = "0x81" if match.group(1) == "0x00" else "0x00"
            return f"push({value});"

        updated = re.sub(r"push\((0x81|0x00)\);", flip, segment)
    elif mode == "remove":
        pattern = re.compile(r"^[ \t]*(?:compressed|rec)\.push\(0x(?:00|81)\);[^\n]*\n", re.M)
        updated, removed = pattern.subn("", segment)
        touched += removed
    else:
        sys.exit(f"patch trailers: unknown mode {mode!r}")
    source = source.replace(segment, updated, 1)

if touched != WANTED:
    sys.exit(
        f"patch trailers ({mode}): expected {WANTED} trailer bytes in "
        f"compress_text and split_text_uncompressed, touched {touched}. "
        "Update the patch to the current source."
    )
with open(path, "w", encoding="utf-8") as handle:
    handle.write(source)
print(f"  patched trailers ({mode}, {touched} bytes)")
PY
}

# =============================================================================
# Patch functions: one per variable, each a verified edit of the current source
# =============================================================================

patch_none() {
    # No changes: the tree as it stands.
    :
}

patch_swap_trailers() {
    # Write the two text-record trailers in the opposite order (0x81 then
    # 0x00) from the order the code writes today (0x00 multibyte, 0x81 TBS).
    patch_trailers swap
}

patch_remove_trailers() {
    # Write no trailers at all.
    patch_trailers remove
}

# Bash 3.2 (macOS /bin/bash) cannot parse a heredoc inside $(), so the blocks are
# read into variables instead.
CSS_HEAD_OLD=
IFS= read -r -d '' CSS_HEAD_OLD <<'RUST' || true
    if result.contains("<head") {
        let style_block: String = head_re
            .find(&result)
            .map(|head_match| {
                style_re
                    .find_iter(head_match.as_str())
                    .map(|m| m.as_str().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let new_head = if style_block.is_empty() {
            "<head><guide></guide></head>".to_string()
        } else {
            format!("<head>{}<guide></guide></head>", style_block)
        };
        result =
            std::borrow::Cow::Owned(head_re.replace_all(&result, new_head.as_str()).to_string());
    }
RUST

CSS_HEAD_NEW=
IFS= read -r -d '' CSS_HEAD_NEW <<'RUST' || true
    if result.contains("<head") {
        // Variant: the head rewrite keeps no <style> block, so nothing from a
        // dictionary's source CSS reaches the shipped text.
        let _ = style_re;
        result = std::borrow::Cow::Owned(
            head_re
                .replace_all(&result, "<head><guide></guide></head>")
                .to_string(),
        );
    }
RUST

patch_revert_head_css() {
    # Drop the style-block preservation in strip_idx_markup and rewrite every
    # head to a bare <head><guide></guide></head>.
    patch_text "$SRC_MOBI" 1 "$CSS_HEAD_OLD" "$CSS_HEAD_NEW" "head CSS"
}

patch_revert_front_matter() {
    # Build the dictionary body with no front-matter sections in front of it.
    patch_text "$SRC_MOBI" 1 \
        'let fm_body = front_matter_sections.join("<mbp:pagebreak/>");' \
        'let fm_body = String::new(); // variant: no front matter' \
        "front matter"
}

patch_revert_hr_separators() {
    # Close each entry with the page break only, dropping the <hr/> before it.
    patch_text "$SRC_MOBI" 1 \
        'entry_close
                .replace_all(&result, "<hr/><mbp:pagebreak/>")' \
        'entry_close
                .replace_all(&result, "<mbp:pagebreak/>")' \
        "entry separators"
}

patch_revert_bd4() {
    # Everything build bd4e70c ("Fix dictionary CSS, front matter, and entry
    # separators") touched, reverted in the current tree. This used to check the
    # old file out of git; it now composes the three patches above, so the
    # variant still compiles against today's crate.
    patch_revert_head_css
    patch_revert_front_matter
    patch_revert_hr_separators
}

patch_revert_all() {
    # Every variable at once.
    patch_swap_trailers
    patch_revert_bd4
}

PATCH_FUNCTIONS=(
    patch_none
    patch_swap_trailers
    patch_remove_trailers
    patch_revert_head_css
    patch_revert_front_matter
    patch_revert_hr_separators
    patch_revert_bd4
    patch_revert_all
)

# =============================================================================
# Variant selection
# =============================================================================

SELECT=()
RUN_COUNT=0

selected() {
    local id=$1 selector
    if [[ ${#SELECT[@]} -eq 0 ]]; then
        return 0
    fi
    for selector in "${SELECT[@]}"; do
        if [[ $id == "$selector" ]]; then
            return 0
        fi
    done
    return 1
}

# =============================================================================
# Build and run
# =============================================================================

build_binary() {
    local log="$WORK_DIR/build.log"
    echo "  Building..."
    if ! (cd "$KINDLING_DIR" && cargo build --release) >"$log" 2>&1; then
        tail -40 "$log" >&2
        die "cargo build failed; see $log"
    fi
    [[ -x $BINARY ]] || die "build produced no executable at $BINARY"
    tail -2 "$log"
}

# create_test_opf <test_id> <description>
# Writes a copy of OPF_ORIG beside it (so relative hrefs still resolve) with a
# unique title and identifier, and leaves its path in TEST_OPF.
create_test_opf() {
    local test_id=$1 description=$2
    TEST_OPF="$OPF_DIR/${test_id}.opf"
    sed \
        -e "s|<dc:title>.*</dc:title>|<dc:title>${test_id} ${description}</dc:title>|" \
        -e "s|<dc:identifier[^>]*>.*</dc:identifier>|<dc:identifier id=\"BookId\" opf:scheme=\"UUID\">${test_id}-$(date +%s)</dc:identifier>|" \
        "$OPF_ORIG" >"$TEST_OPF"
    [[ -s $TEST_OPF ]] || die "could not write test OPF: $TEST_OPF"
}

# run_binary <output_mobi> [flags...]
run_binary() {
    local output=$1
    shift
    local log="$WORK_DIR/run.log"
    echo "  Running kindling-cli build..."
    if ! "$BINARY" build "$TEST_OPF" -o "$output" "$@" >"$log" 2>&1; then
        tail -20 "$log" >&2
        die "kindling-cli build failed for $output"
    fi
    [[ -s $output ]] || die "kindling-cli wrote no output at $output"
    tail -5 "$log"
}

# run_test <test_id> <description> <patch_function> [flags...]
run_test() {
    local test_id=$1 description=$2 patch_fn=$3
    shift 3
    local flags=("$@")

    if ! selected "$test_id"; then
        return 0
    fi
    RUN_COUNT=$((RUN_COUNT + 1))

    local output_mobi="$OUTPUT_DIR/${test_id}_${description// /_}.mobi"
    local test_start
    test_start=$(date +%s)

    echo ""
    echo "========================================================================"
    echo "=== ${test_id}: ${description}"
    if [[ ${#flags[@]} -gt 0 ]]; then
        echo "=== Flags: ${flags[*]}"
    else
        echo "=== Flags: none"
    fi
    echo "========================================================================"

    restore_source
    echo "  Applying patches: ${patch_fn}"
    "$patch_fn"

    build_binary
    create_test_opf "$test_id" "$description"
    if [[ ${#flags[@]} -gt 0 ]]; then
        run_binary "$output_mobi" "${flags[@]}"
    else
        run_binary "$output_mobi"
    fi
    rm -f "$TEST_OPF"
    TEST_OPF=""

    local bytes elapsed
    bytes=$(wc -c <"$output_mobi" | tr -d ' ')
    elapsed=$(( $(date +%s) - test_start ))
    echo "  Done: ${output_mobi} ($((bytes / 1024 / 1024)) MB, ${elapsed}s)"
}

# =============================================================================
# --check-patches: apply every patch, build nothing
# =============================================================================

check_patches() {
    local patch_fn
    echo "Checking that every patch still matches $SRC_MOBI"
    for patch_fn in "${PATCH_FUNCTIONS[@]}"; do
        restore_source
        "$patch_fn"
        echo "  ok    ${patch_fn}"
    done
    restore_source
    echo "All ${#PATCH_FUNCTIONS[@]} patches matched; source restored."
}

# =============================================================================# Argument parsing
# =============================================================================

CHECK_ONLY=0
for arg in "$@"; do
    case $arg in
        --check-patches)
            CHECK_ONLY=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            die "unknown option: $arg (try --help)"
            ;;
        *)
            SELECT+=("$arg")
            ;;
    esac
done

if [[ $CHECK_ONLY -eq 1 ]]; then
    if [[ ${#SELECT[@]} -gt 0 ]]; then
        die "--check-patches does not take variant names"
    fi
    check_patches
    exit 0
fi

[[ -n $OPF_ORIG ]] || die "set OPF_ORIG to the dictionary OPF to build (try --help)"
[[ -f $OPF_ORIG ]] || die "OPF not found: $OPF_ORIG"
OPF_DIR=$(cd "$(dirname "$OPF_ORIG")" && pwd)
OPF_ORIG="$OPF_DIR/$(basename "$OPF_ORIG")"

ALL_IDS=(
    Test01 Test02 Test03 Test04 Test05 Test06 Test07
    Test08 Test09 Test10 Test11 Test12 Test13 Test14
)
if [[ ${#SELECT[@]} -gt 0 ]]; then
    for selector in "${SELECT[@]}"; do
        known=0
        for id in "${ALL_IDS[@]}"; do
            if [[ $id == "$selector" ]]; then
                known=1
            fi
        done
        [[ $known -eq 1 ]] || die "unknown variant: $selector (known: ${ALL_IDS[*]})"
    done
fi

mkdir -p "$OUTPUT_DIR"

SCRIPT_START=$(date +%s)

echo "=================================================================="
echo "Variant Dictionary Generator"
echo "=================================================================="
echo "Source OPF: $OPF_ORIG"
echo "Output dir: $OUTPUT_DIR"
echo "Started at: $(date)"
echo ""

# --- Group A: text-record trailers ---

run_test "Test01" "Current" \
    patch_none \
    --kindle-limits

run_test "Test02" "Current NoKL" \
    patch_none \
    --no-kindle-limits

run_test "Test03" "SwappedTrailers" \
    patch_swap_trailers \
    --kindle-limits

run_test "Test04" "SwappedTrailers NoKL" \
    patch_swap_trailers \
    --no-kindle-limits

run_test "Test05" "NoTrailers" \
    patch_remove_trailers \
    --kindle-limits

# --- Group B: CSS in the head ---

run_test "Test06" "NoHeadCSS" \
    patch_revert_head_css \
    --kindle-limits

run_test "Test07" "NoHeadCSS NoKL" \
    patch_revert_head_css \
    --no-kindle-limits

# --- Group C: Front matter ---

run_test "Test08" "NoFrontMatter" \
    patch_revert_front_matter \
    --kindle-limits

# --- Group D: Entry separators ---

run_test "Test09" "NoHrSeparators" \
    patch_revert_hr_separators \
    --kindle-limits

# --- Group E: Compound patches ---

run_test "Test10" "RevertBd4" \
    patch_revert_bd4 \
    --kindle-limits

run_test "Test11" "RevertBd4 NoKL" \
    patch_revert_bd4 \
    --no-kindle-limits

run_test "Test12" "RevertAll" \
    patch_revert_all \
    --kindle-limits

# --- Group F: Compression ---

run_test "Test13" "NoCompress" \
    patch_none \
    --no-compress --kindle-limits

run_test "Test14" "NoCompress NoKL" \
    patch_none \
    --no-compress --no-kindle-limits

[[ $RUN_COUNT -gt 0 ]] || die "no variant ran"

# =============================================================================
# Summary
# =============================================================================

TOTAL_ELAPSED=$(( $(date +%s) - SCRIPT_START ))

echo ""
echo "=================================================================="
echo "ALL VARIANTS COMPLETE"
echo "=================================================================="
echo "Total time: ${TOTAL_ELAPSED}s ($((TOTAL_ELAPSED / 60))m $((TOTAL_ELAPSED % 60))s)"
echo "Output directory: $OUTPUT_DIR"
echo ""
echo "Generated files:"
ls -lh "$OUTPUT_DIR"/Test*.mobi 2>/dev/null || echo "  (no files found)"
echo ""
echo "Variant matrix:"
echo "  Test01  Current, --kindle-limits (baseline)"
echo "  Test02  Current, --no-kindle-limits"
echo "  Test03  Trailers written 0x81 then 0x00, --kindle-limits"
echo "  Test04  Trailers written 0x81 then 0x00, --no-kindle-limits"
echo "  Test05  No trailers at all, --kindle-limits"
echo "  Test06  No <style> preserved in the head rewrite, --kindle-limits"
echo "  Test07  No <style> preserved in the head rewrite, --no-kindle-limits"
echo "  Test08  No front matter, --kindle-limits"
echo "  Test09  No <hr/> between entries, --kindle-limits"
echo "  Test10  CSS + front matter + separators reverted (bd4e70c), --kindle-limits"
echo "  Test11  CSS + front matter + separators reverted, --no-kindle-limits"
echo "  Test12  Every variable reverted, --kindle-limits"
echo "  Test13  No compression, --kindle-limits"
echo "  Test14  No compression, --no-kindle-limits"
