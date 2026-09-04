//! SDKMAN: `versions/all`, `candidates/default` and the rendered `sdk list`.
//!
//! RFC 0010 §4.4, §6.2. Three listing documents, all `text/plain`:
//!
//! - `candidates/{c}/{plat}/versions/all` — comma-separated identifiers.
//!   Filtered by splitting on `,` and rejoining; no parser.
//! - `candidates/default/{c}` — one identifier, the "newest" pointer. Names
//!   no list to pick a replacement from, so it is repaired in the handler
//!   against the filtered `versions/all`, the way Go's `@latest` and
//!   RubyGems' gem document are ([`repaired_default`]).
//! - `candidates/{c}/{plat}/versions/list` — the table `sdk list <c>`
//!   prints, in one of two layouts. Both are fixed-width and every version
//!   in them sits in a field a filter can find, so both are filtered rather
//!   than left as "a human view" (decision 5).
//!
//! **Nothing is re-rendered.** The vendor table loses whole lines and has
//! one cell promoted; the grid has cells blanked to their own width. An
//! upstream cosmetic change therefore degrades to "a version we failed to
//! remove" — visible, and caught by the fixture tests — never to a table
//! this proxy corrupted.
//!
//! The chokepoint is `candidates/validate/{c}/{v}/{plat}`, not any of these:
//! a blocked version answers `invalid` there and `sdk install` stops on its
//! own *"Stop! … is not a valid java version."* (§7). The listings exist so
//! the console does not advertise what the install then refuses.

use super::{best_latest, BlockedVersions};
use crate::services::sdkman::versions_in_csv;

/// Remove blocked identifiers from a `versions/all` body.
///
/// Order and spelling are preserved; a trailing newline, if upstream sent
/// one, is kept. An emptied list is an empty body.
pub fn strip_versions_csv(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let trailing_newline = body.ends_with('\n');
    let mut removed = Vec::new();
    let kept: Vec<&str> = body
        .trim_end_matches('\n')
        .split(',')
        .filter(|v| {
            let v = v.trim();
            if !v.is_empty() && blocked.contains(v) {
                removed.push(v.to_owned());
                false
            } else {
                true
            }
        })
        .collect();
    if removed.is_empty() {
        return removed;
    }
    let mut out = kept.join(",");
    if trailing_newline && !out.is_empty() {
        out.push('\n');
    }
    *body = out;
    removed
}

/// The version a `candidates/default/{c}` body names.
pub fn default_version(body: &str) -> &str {
    body.trim()
}

/// Repair a `candidates/default` body against the versions that survived
/// filtering.
///
/// `Some(replacement)` when the named default is blocked and another version
/// can stand in for it — the highest allowed one, by the same rule npm's
/// `dist-tags.latest` is repaired with. `None` when the default is not
/// blocked, or when nothing survived: an empty default is worse than a
/// blocked one, because `sdk install java` would then validate an empty
/// string and print a confusing refusal rather than the protocol's own.
pub fn repaired_default(
    default_body: &str,
    filtered_versions_csv: &str,
    blocked: &BlockedVersions,
) -> Option<String> {
    let current = default_version(default_body);
    if current.is_empty() || !blocked.contains(current) {
        return None;
    }
    let survivors = versions_in_csv(filtered_versions_csv);
    best_latest(&survivors)
}

/// Remove blocked versions from the rendered `versions/list` table, in
/// whichever of its two layouts upstream used.
///
/// The layout is picked by looking for a `| Identifier` header. A document
/// with one is the vendor table (Java); anything else is treated as the grid,
/// where a blocked version is one whitespace-delimited token and blanking it
/// in place cannot misalign anything.
pub fn strip_rendered_list(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    if blocked.is_empty() {
        return Vec::new();
    }
    if vendor_table_identifier_column(body).is_some() {
        strip_vendor_table(body, blocked)
    } else {
        strip_grid(body, blocked)
    }
}

/// The index of the `Identifier` cell in the vendor table's header, when the
/// document has one.
fn vendor_table_identifier_column(body: &str) -> Option<usize> {
    body.lines()
        .filter(|l| l.contains('|'))
        .find_map(|l| l.split('|').position(|cell| cell.trim() == "Identifier"))
}

/// The vendor-grouped table: a blocked version is one whole line, matched on
/// its `Identifier` cell.
///
/// The `Vendor` cell is written only on the first line of each vendor's
/// block. Dropping that line would orphan the block, so its vendor cell is
/// promoted into the next surviving line of the same block, in the same
/// fixed-width field — the cells are the same width by construction, so the
/// promoted text is the dropped line's cell verbatim. A block none of whose
/// lines survive disappears with its vendor, which is correct.
fn strip_vendor_table(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let Some(id_col) = vendor_table_identifier_column(body) else {
        return Vec::new();
    };
    let trailing_newline = body.ends_with('\n');
    let mut removed = Vec::new();
    let mut out: Vec<String> = Vec::new();
    // The vendor cell of a dropped first-of-block line, waiting for the next
    // surviving line of that block.
    let mut pending_vendor: Option<String> = None;
    let mut in_table = false;

    for line in body.lines() {
        let is_row = line.contains('|');
        if !is_row {
            // The `----` rule under the header separates it from the rows;
            // a `====` rule (or anything else) closes the table. Either way
            // no vendor block continues across it.
            if !line.starts_with('-') {
                in_table = false;
            }
            pending_vendor = None;
            out.push(line.to_owned());
            continue;
        }
        let cells: Vec<&str> = line.split('|').collect();
        if cells.get(id_col).map(|c| c.trim()) == Some("Identifier") {
            in_table = true;
            out.push(line.to_owned());
            continue;
        }
        if !in_table || cells.len() <= id_col {
            out.push(line.to_owned());
            continue;
        }
        let identifier = cells[id_col].trim();
        let vendor_cell = cells[0];
        let starts_block = !vendor_cell.trim().is_empty();

        if !identifier.is_empty() && blocked.contains(identifier) {
            removed.push(identifier.to_owned());
            if starts_block {
                pending_vendor = Some(vendor_cell.to_owned());
            }
            continue;
        }

        if starts_block {
            pending_vendor = None;
            out.push(line.to_owned());
        } else if let Some(vendor) = pending_vendor.take() {
            let mut promoted = cells.clone();
            promoted[0] = &vendor;
            out.push(promoted.join("|"));
        } else {
            out.push(line.to_owned());
        }
    }

    if removed.is_empty() {
        return removed;
    }
    let mut rebuilt = out.join("\n");
    if trailing_newline {
        rebuilt.push('\n');
    }
    *body = rebuilt;
    removed
}

/// The column-major grid: a blocked version is one whitespace-delimited
/// token, blanked in place to its own width.
///
/// The remaining versions do not move up. That keeps the column widths, the
/// `> * +` markers and the legend intact; a gap in a list of versions is a
/// smaller wrong than a re-packed table that no longer aligns.
fn strip_grid(body: &mut String, blocked: &BlockedVersions) -> Vec<String> {
    let mut removed = Vec::new();
    let mut out = String::with_capacity(body.len());
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut rebuilt = String::with_capacity(line.len());
        for (token, is_word) in tokens(line) {
            if is_word && blocked.contains(token) {
                removed.push(token.to_owned());
                rebuilt.extend(std::iter::repeat_n(' ', token.chars().count()));
            } else {
                rebuilt.push_str(token);
            }
        }
        out.push_str(&rebuilt);
    }
    if !removed.is_empty() {
        *body = out;
    }
    removed
}

/// `line` as alternating runs of non-whitespace and whitespace, each tagged
/// with whether it is a word. Concatenating the runs gives the line back
/// byte-for-byte.
fn tokens(line: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut rest = line;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let is_word = !rest.starts_with(char::is_whitespace);
        let end = rest
            .find(|c: char| c.is_whitespace() == is_word)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some((run, is_word))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::RegistryKind;

    fn blocked(versions: &[&str]) -> BlockedVersions {
        BlockedVersions::new(
            RegistryKind::Sdkman,
            versions.iter().map(|v| (*v).to_owned()).collect(),
        )
    }

    // ── versions/all ─────────────────────────────────────────────────────────

    #[test]
    fn a_blocked_version_is_removed_from_a_csv_at_every_position() {
        for (body, expected) in [
            ("3.9.8,3.9.9,4.0.0-rc-6", "3.9.8,4.0.0-rc-6"),
            ("3.9.9,3.9.8", "3.9.8"),
            ("3.9.8,3.9.9", "3.9.8"),
            ("3.9.9", ""),
        ] {
            let mut b = body.to_owned();
            assert_eq!(strip_versions_csv(&mut b, &blocked(&["3.9.9"])), ["3.9.9"]);
            assert_eq!(b, expected, "from {body:?}");
        }
    }

    #[test]
    fn a_csv_with_a_trailing_newline_keeps_it() {
        let mut b = "3.9.8,3.9.9\n".to_owned();
        strip_versions_csv(&mut b, &blocked(&["3.9.9"]));
        assert_eq!(b, "3.9.8\n");
    }

    #[test]
    fn an_empty_blocked_set_leaves_the_csv_byte_identical() {
        let mut b = "3.9.8,3.9.9".to_owned();
        assert!(strip_versions_csv(&mut b, &blocked(&["9.9.9"])).is_empty());
        assert_eq!(b, "3.9.8,3.9.9");
    }

    // ── candidates/default ───────────────────────────────────────────────────

    #[test]
    fn a_blocked_default_is_repaired_to_the_newest_allowed_version() {
        let repaired = repaired_default("3.9.9\n", "3.9.7,3.9.8,4.0.0-rc-6", &blocked(&["3.9.9"]));
        assert_eq!(repaired.as_deref(), Some("3.9.8"));
    }

    #[test]
    fn an_allowed_default_is_left_alone() {
        assert_eq!(
            repaired_default("3.9.9", "3.9.8,3.9.9", &blocked(&["3.9.8"])),
            None
        );
    }

    #[test]
    fn a_default_with_no_survivors_is_not_emptied() {
        assert_eq!(repaired_default("3.9.9", "", &blocked(&["3.9.9"])), None);
    }

    // ── the rendered table: vendor layout (Java) ─────────────────────────────

    /// `sdk list java` as the API rendered it on 2026-09-04, trimmed to three
    /// vendor blocks with every column width intact. `21.0.12-tem` is marked
    /// installed (`+`) and `17.0.20-tem` in use (`> *`), as the client asks
    /// for them through `?current=&installed=`.
    const JAVA_TABLE: &str = "\
================================================================================
Available Java Versions for Linux 64bit
================================================================================
 Vendor         | Use | Version            | Identifier
--------------------------------------------------------------------------------
 Corretto       |     | 26.0.2             | 26.0.2-amzn
                |     | 25.0.4             | 25.0.4-amzn
                |     | 21.0.12            | 21.0.12-amzn
 GraalVM CE     |     | 25.3.4+1.r25       | 25.3.4+1.r25-graalce
                |     | 21.0.2             | 21.0.2-graalce
 Temurin        |     | 26.0.2+1.1         | 26.0.2+1.1-tem
                |     | 25.0.4             | 25.0.4-tem
                |   + | 21.0.12            | 21.0.12-tem
                | > * | 17.0.20            | 17.0.20-tem
================================================================================
 > in use   * installed   + local only
--------------------------------------------------------------------------------
 $ sdk install java <Identifier>    install a specific version
 $ sdk install java                 install the default: 25.0.4-tem
 $ sdk install java [TAB]           complete an available identifier
================================================================================
";

    #[test]
    fn a_blocked_java_row_is_removed_by_its_identifier() {
        let mut body = JAVA_TABLE.to_owned();
        let removed = strip_rendered_list(&mut body, &blocked(&["25.0.4-amzn"]));
        assert_eq!(removed, ["25.0.4-amzn"]);
        assert!(!body.contains("25.0.4-amzn"));
        // Every other line is byte-identical.
        let expected: String = JAVA_TABLE
            .lines()
            .filter(|l| !l.ends_with("| 25.0.4-amzn"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(body, expected);
    }

    /// The version column alone is not the identity — `25.0.4` appears under
    /// both Corretto and Temurin — so a block on `25.0.4-tem` must leave
    /// `25.0.4-amzn` in place.
    #[test]
    fn the_identifier_column_decides_not_the_version_column() {
        let mut body = JAVA_TABLE.to_owned();
        strip_rendered_list(&mut body, &blocked(&["25.0.4-tem"]));
        assert!(body.contains("| 25.0.4             | 25.0.4-amzn"));
        assert!(!body.contains("| 25.0.4-tem"));
        assert!(
            body.contains("install the default: 25.0.4-tem"),
            "the help text below the table is not a listing row and is left alone"
        );
    }

    #[test]
    fn blocking_the_first_row_of_a_block_promotes_the_vendor_into_the_next() {
        let mut body = JAVA_TABLE.to_owned();
        strip_rendered_list(&mut body, &blocked(&["26.0.2-amzn"]));
        assert!(
            body.contains(" Corretto       |     | 25.0.4             | 25.0.4-amzn"),
            "{body}"
        );
        assert!(!body.contains("26.0.2-amzn"));
        // The width of the vendor field is unchanged: the table still aligns.
        let header_width = " Vendor         |".len();
        for line in body.lines().filter(|l| l.contains('|')) {
            assert_eq!(line.find('|'), Some(header_width - 1), "{line:?}");
        }
    }

    #[test]
    fn blocking_two_leading_rows_promotes_the_vendor_past_both() {
        let mut body = JAVA_TABLE.to_owned();
        strip_rendered_list(&mut body, &blocked(&["26.0.2-amzn", "25.0.4-amzn"]));
        assert!(body.contains(" Corretto       |     | 21.0.12            | 21.0.12-amzn"));
        assert_eq!(body.matches("Corretto").count(), 1);
    }

    #[test]
    fn blocking_every_row_of_a_block_removes_the_vendor_with_it() {
        let mut body = JAVA_TABLE.to_owned();
        strip_rendered_list(
            &mut body,
            &blocked(&["25.3.4+1.r25-graalce", "21.0.2-graalce"]),
        );
        assert!(!body.contains("GraalVM CE"));
        // The neighbouring blocks are untouched.
        assert!(body.contains(" Corretto       |     | 26.0.2             | 26.0.2-amzn"));
        assert!(body.contains(" Temurin        |     | 26.0.2+1.1         | 26.0.2+1.1-tem"));
    }

    #[test]
    fn the_use_markers_travel_with_their_row() {
        let mut body = JAVA_TABLE.to_owned();
        strip_rendered_list(&mut body, &blocked(&["26.0.2+1.1-tem", "25.0.4-tem"]));
        assert!(body.contains(" Temurin        |   + | 21.0.12            | 21.0.12-tem"));
        assert!(body.contains("                | > * | 17.0.20            | 17.0.20-tem"));
    }

    #[test]
    fn an_empty_blocked_set_leaves_the_java_table_byte_identical() {
        let mut body = JAVA_TABLE.to_owned();
        assert!(strip_rendered_list(&mut body, &blocked(&[])).is_empty());
        assert_eq!(body, JAVA_TABLE);
        assert!(strip_rendered_list(&mut body, &blocked(&["9.9.9-nope"])).is_empty());
        assert_eq!(body, JAVA_TABLE);
    }

    // ── the rendered table: grid layout (everything else) ────────────────────

    /// `sdk list maven` with `?current=3.9.9&installed=3.9.9,3.8.4`, as the
    /// API rendered it on 2026-09-04, trimmed to five rows. Each cell is a
    /// five-character marker field and a fifteen-character version field.
    const MAVEN_GRID: &str = "\
================================================================================
Available Maven Versions
================================================================================
     4.0.0-rc-6          3.9.5               3.6.3               3.1.1
     4.0.0-rc-5          3.9.4               3.6.2               3.1.0
     3.9.16              3.9.1               3.5.4
 > * 3.9.9             * 3.8.4               3.2.5
     3.9.8               3.8.3               3.2.3

================================================================================
+ - local version
* - installed
> - currently in use
================================================================================
";

    #[test]
    fn a_blocked_grid_cell_is_blanked_in_place_and_nothing_else_moves() {
        let mut body = MAVEN_GRID.to_owned();
        let removed = strip_rendered_list(&mut body, &blocked(&["3.9.4"]));
        assert_eq!(removed, ["3.9.4"]);
        let expected = MAVEN_GRID.replace(
            "     4.0.0-rc-5          3.9.4               3.6.2               3.1.0",
            "     4.0.0-rc-5                              3.6.2               3.1.0",
        );
        assert_eq!(body, expected);
    }

    /// A version that is a prefix of another (`3.9.1` of `3.9.16`) is matched
    /// as a whole token, never as a substring.
    #[test]
    fn a_grid_block_matches_whole_tokens_only() {
        let mut body = MAVEN_GRID.to_owned();
        let removed = strip_rendered_list(&mut body, &blocked(&["3.9.1"]));
        assert_eq!(removed, ["3.9.1"]);
        assert!(body.contains("3.9.16"));
        assert!(!body.contains("3.9.1 "), "{body}");
    }

    #[test]
    fn the_markers_and_the_legend_survive_a_grid_block() {
        let mut body = MAVEN_GRID.to_owned();
        strip_rendered_list(&mut body, &blocked(&["3.8.4"]));
        assert!(body.contains(" > * 3.9.9             *                     3.2.5"));
        assert!(body.contains("* - installed\n> - currently in use"));
        assert_eq!(
            body.lines().count(),
            MAVEN_GRID.lines().count(),
            "no line is added or removed"
        );
        for (a, b) in body.lines().zip(MAVEN_GRID.lines()) {
            assert_eq!(a.len(), b.len(), "every line keeps its width: {a:?}");
        }
    }

    #[test]
    fn an_empty_blocked_set_leaves_the_grid_byte_identical() {
        let mut body = MAVEN_GRID.to_owned();
        assert!(strip_rendered_list(&mut body, &blocked(&["0.0.0"])).is_empty());
        assert_eq!(body, MAVEN_GRID);
    }

    /// A document in neither layout — an upstream error page, an "offline"
    /// notice — is passed through: over-listing is the safe direction, and a
    /// version string that happens to appear in prose is blanked, not
    /// corrupted.
    #[test]
    fn an_unrecognised_layout_is_passed_through() {
        let mut body = "Something went wrong upstream.\n".to_owned();
        assert!(strip_rendered_list(&mut body, &blocked(&["3.9.9"])).is_empty());
        assert_eq!(body, "Something went wrong upstream.\n");
    }

    #[test]
    fn tokens_round_trip_the_line() {
        for line in [" > * 3.9.9             * 3.8.4  ", "", "   ", "a b", "a  "] {
            let joined: String = tokens(line).map(|(t, _)| t).collect();
            assert_eq!(joined, line);
        }
    }
}
