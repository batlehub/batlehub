//! One definition of how a value becomes a CSV cell.
//!
//! Three exports write CSV — the audit log, the exposure report and the puller
//! list — and all three carry strings the proxy did not choose: a `User-Agent`
//! from any unauthenticated client, a `summary` from a flag source, a package
//! name from whoever published it. Each had grown its own quoting, or none, so
//! they disagreed about what a comma meant and none of them handled a leading
//! `=`. The rules are the same in all three places, so they live in one.

/// Render `s` as a CSV field: RFC 4180 quoting, plus a guard against the
/// spreadsheet reading it as a formula.
///
/// Quoting alone is not enough. A cell whose text begins with `=`, `+`, `-`,
/// `@`, tab or carriage return is a formula to Excel, LibreOffice and Sheets —
/// `=cmd|'/c calc'!A1` is the usual demonstration — and quoting it per RFC 4180
/// does not change that, because the quotes are consumed by the CSV parser
/// before the formula parser ever sees the text. Prefixing an apostrophe is the
/// documented way to say "this is text": the spreadsheet strips it on display
/// and never evaluates what follows.
///
/// `@` is in the set, which means every npm scoped name — `@scope/name`, the
/// commonest shape in the `package_name` column — is guarded too. That is
/// deliberate: narrowing the set to the leads that look dangerous in this
/// codebase's data is how the next payload gets through. The cost is only in
/// the bytes on disk, since a spreadsheet displays and copies the cell without
/// the apostrophe; a script reading the raw file should strip a leading `'`.
pub fn field(s: &str) -> String {
    let leads_a_formula = s.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if leads_a_formula {
        return format!("\"'{}\"", s.replace('"', "\"\""));
    }
    if s.contains([',', '"', '\n', '\r']) {
        return format!("\"{}\"", s.replace('"', "\"\""));
    }
    s.to_owned()
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn plain_values_are_passed_through() {
        for s in ["", "tokio", "1.2.3", "allowed", "com.example:lib"] {
            assert_eq!(field(s), s, "{s:?} needs no quoting");
        }
    }

    /// An npm scoped name leads with `@`, so it is guarded like any other
    /// formula lead. The spreadsheet still shows `@scope/name`.
    #[test]
    fn a_scoped_npm_name_is_guarded_and_still_reads_the_same() {
        assert_eq!(field("@scope/name"), "\"'@scope/name\"");
    }

    #[test]
    fn separators_and_quotes_are_rfc4180_quoted() {
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("two\nlines"), "\"two\nlines\"");
    }

    #[test]
    fn formula_leads_are_neutralised() {
        // The cell still reads the same to a human; it is no longer executable.
        for s in [
            "=cmd|'/c calc'!A1",
            "+1+1",
            "-2+3",
            "@SUM(A1)",
            "\tleading tab",
        ] {
            let out = field(s);
            assert!(
                out.starts_with("\"'"),
                "{s:?} must be quoted and apostrophe-guarded, got {out}"
            );
        }
    }

    #[test]
    fn a_formula_lead_that_also_needs_quoting_gets_both() {
        assert_eq!(field("=a,b"), "\"'=a,b\"");
        assert_eq!(field("=\"x\""), "\"'=\"\"x\"\"\"");
    }

    #[test]
    fn a_hyphen_inside_a_value_is_not_a_lead() {
        assert_eq!(field("some-package"), "some-package");
    }
}
