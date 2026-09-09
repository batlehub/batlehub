//! The `nodejs.org/dist` index, read once for the three places that need it.
//!
//! RFC 0010 §6.4. `index.tab` is the one document the whole `nodedist` kind
//! hangs on: `blocking` removes rows from it, `upstream_detail` builds the
//! console's version table from it, and the registry client reads a release's
//! publish date out of it so the age gate works for Node without a second
//! request. Three readers of one TSV would be three chances to disagree about
//! which column is which — so the column lookup lives here, header-driven, and
//! the readers ask for fields by name.
//!
//! The column count is **not** assumed. Node's `index.tab` has eleven columns
//! and io.js's has nine; both start `version<TAB>date<TAB>files`, and the header
//! row says the rest. A reader that hard-coded "column ten is `lts`" would read
//! io.js's `modules` column as an LTS codename.

use chrono::{DateTime, NaiveDate, Utc};

/// The one package a `nodedist` registry serves.
///
/// One package, many versions, many files per version is what the ecosystem
/// is: the thing an admin blocks is *a Node release*, and a package per
/// platform or per file would make "block Node 22.11.0" eight operations
/// (RFC 0010 §4.3). The name is `node` rather than the registry's own name so a
/// block, a cache key and a statistics row all agree without consulting the
/// config.
pub const NODEDIST_PACKAGE: &str = "node";

/// The package name on an io.js registry — a separate, long-dead ecosystem
/// with its own `index.tab` shape, configured as a second `nodedist` registry
/// pointed at `https://iojs.org/dist` (RFC 0010 §4.1, decision 13).
pub const IOJS_PACKAGE: &str = "iojs";

/// The package name a `nodedist` registry's routes address, from its upstream.
///
/// `node` unless the upstream is the io.js tree. Derived from the URL rather
/// than configured, because there is nothing else to configure: the tree
/// decides what it serves, and a registry pointed at `iojs.org` that called its
/// package `node` would let a block on one tree's `v3.3.1` hide the other's.
pub fn package_name_for_upstream(upstream: Option<&str>) -> &'static str {
    match upstream {
        Some(url) if url.contains("iojs.org") => IOJS_PACKAGE,
        _ => NODEDIST_PACKAGE,
    }
}

/// One data row of `index.tab`, addressed by the header's column names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRow<'a> {
    /// `v22.11.0` — the first column, and the string every download path is
    /// keyed on. nvm strips nothing from it.
    pub version: &'a str,
    /// `2024-10-29` — the release date, the second column.
    pub date: &'a str,
    /// The LTS codename (`Jod`), or `None` when the row says `-` or the tree has
    /// no such column (io.js).
    pub lts: Option<&'a str>,
}

/// `index.tab`, parsed as far as its readers need.
#[derive(Debug, Clone, Copy)]
pub struct IndexTab<'a> {
    text: &'a str,
    version_col: usize,
    date_col: usize,
    lts_col: Option<usize>,
}

impl<'a> IndexTab<'a> {
    /// Read the header. `None` when line 1 is not a header naming `version`
    /// and `date` — a document this proxy does not understand well enough to
    /// read, which the caller treats as "no rows" rather than as a guess.
    pub fn parse(text: &'a str) -> Option<Self> {
        let header = text.lines().next()?;
        let columns: Vec<&str> = header.split('\t').map(str::trim).collect();
        let col = |name: &str| columns.iter().position(|c| *c == name);
        Some(Self {
            text,
            version_col: col("version")?,
            date_col: col("date")?,
            lts_col: col("lts"),
        })
    }

    /// The data rows, in document order (newest first, as upstream writes it).
    ///
    /// Blank lines and rows too short to carry a version are skipped, never
    /// guessed at.
    pub fn rows(&self) -> impl Iterator<Item = IndexRow<'a>> + '_ {
        self.text.lines().skip(1).filter_map(move |line| {
            if line.trim().is_empty() {
                return None;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            let version = fields.get(self.version_col).copied()?.trim();
            if version.is_empty() {
                return None;
            }
            let date = fields.get(self.date_col).copied().unwrap_or("").trim();
            let lts = self
                .lts_col
                .and_then(|i| fields.get(i).copied())
                .map(str::trim)
                .filter(|l| !l.is_empty() && *l != "-");
            Some(IndexRow { version, date, lts })
        })
    }
}

/// The versions named by `index.tab`, newest first, or nothing for a document
/// with no recognisable header.
pub fn index_tab_versions(text: &str) -> Vec<String> {
    IndexTab::parse(text)
        .map(|tab| tab.rows().map(|r| r.version.to_owned()).collect())
        .unwrap_or_default()
}

/// An `index.tab` date (`2024-10-29`) as the midnight-UTC instant the age gate
/// compares against.
///
/// The tree publishes a day, not a time. Midnight is the *earliest* instant of
/// that day, so a release is never treated as older than it can be: a
/// `min_age_secs` of 24 hours holds a release published at 23:00 for a full day
/// after the date rolls, never less.
pub fn parse_index_date(date: &str) -> Option<DateTime<Utc>> {
    NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three real rows of Node's eleven-column table, as served on 2026-09-03.
    pub(crate) const NODE_INDEX_TAB: &str = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
        v22.11.0\t2024-10-29\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\tJod\t-\n\
        v22.10.0\t2024-10-16\theaders,linux-x64,src\t10.9.0\t12.4.254.21\t1.49.1\t1.3.0.1-motley\t3.0.15+quic\t127\t-\t-\n\
        v20.18.0\t2024-10-03\theaders,linux-x64,src\t10.8.2\t11.3.244.8\t1.46.0\t1.3.0.1-motley\t3.0.13+quic\t115\tIron\ttrue\n";

    /// io.js's nine columns: no `lts`, no `security`.
    pub(crate) const IOJS_INDEX_TAB: &str =
        "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\n\
        v3.3.1\t2015-09-15\theaders,linux-x64,src\t2.14.3\t4.4.63.30\t1.7.4\t1.2.8\t1.0.2d\t45\n\
        v3.3.0\t2015-09-02\theaders,linux-x64,src\t2.13.3\t4.4.63.30\t1.7.3\t1.2.8\t1.0.2d\t45\n";

    #[test]
    fn reads_node_rows_by_header_name() {
        let tab = IndexTab::parse(NODE_INDEX_TAB).unwrap();
        let rows: Vec<IndexRow<'_>> = tab.rows().collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].version, "v22.11.0");
        assert_eq!(rows[0].date, "2024-10-29");
        assert_eq!(rows[0].lts, Some("Jod"));
        assert_eq!(rows[1].lts, None, "`-` is no codename");
        assert_eq!(rows[2].lts, Some("Iron"));
    }

    #[test]
    fn reads_the_nine_column_iojs_table_without_inventing_an_lts_column() {
        let tab = IndexTab::parse(IOJS_INDEX_TAB).unwrap();
        let rows: Vec<IndexRow<'_>> = tab.rows().collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].version, "v3.3.1");
        assert_eq!(rows[0].date, "2015-09-15");
        assert_eq!(
            rows[0].lts, None,
            "column ten would be past the end; a positional reader would have panicked or lied"
        );
    }

    #[test]
    fn a_document_without_a_header_yields_nothing() {
        assert!(IndexTab::parse("").is_none());
        assert!(IndexTab::parse("v22.11.0\t2024-10-29\n").is_none());
        assert!(index_tab_versions("not a table").is_empty());
    }

    #[test]
    fn versions_are_newest_first_as_the_tree_writes_them() {
        assert_eq!(
            index_tab_versions(NODE_INDEX_TAB),
            ["v22.11.0", "v22.10.0", "v20.18.0"]
        );
    }

    #[test]
    fn a_release_day_is_its_earliest_instant() {
        let dt = parse_index_date("2024-10-29").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-10-29T00:00:00+00:00");
        assert!(parse_index_date("-").is_none());
        assert!(parse_index_date("").is_none());
    }

    #[test]
    fn the_package_name_follows_the_tree() {
        assert_eq!(package_name_for_upstream(None), "node");
        assert_eq!(
            package_name_for_upstream(Some("https://nodejs.org/dist")),
            "node"
        );
        assert_eq!(
            package_name_for_upstream(Some("https://iojs.org/dist")),
            "iojs"
        );
    }
}
