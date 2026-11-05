//! Destructive commands confirm on a terminal and refuse without one
//! (RFC 0040 §4.5). Called before the request is built, so a refusal sends
//! nothing.

use anyhow::Result;

use crate::exit::{CliError, ExitCode};
use crate::render::{ctx, interactive};

const SHOWN: usize = 20;

/// Ask before `verb`-ing `objects`. `note` is the one fact the caller is most
/// likely to be surprised by (a deleted version number is spent forever).
pub fn confirm(verb: &str, objects: &[String], note: Option<&str>) -> Result<()> {
    if ctx().yes {
        return Ok(());
    }
    decide(verb, objects, note, interactive(), || {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
}

fn decide(
    verb: &str,
    objects: &[String],
    note: Option<&str>,
    tty: bool,
    read: impl FnOnce() -> std::io::Result<String>,
) -> Result<()> {
    eprintln!("{}", summary(verb, objects, note));
    if !tty {
        return Err(CliError::new(
            ExitCode::Confirm,
            format!("refusing to {verb} without a terminal; pass --yes"),
        )
        .into());
    }
    eprint!("Proceed? [y/N] ");
    let answer = read()?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::new(ExitCode::Confirm, "aborted").into())
    }
}

fn summary(verb: &str, objects: &[String], note: Option<&str>) -> String {
    let mut out = format!("this will {verb}:");
    for o in objects.iter().take(SHOWN) {
        out.push_str(&format!("\n  {o}"));
    }
    if objects.len() > SHOWN {
        out.push_str(&format!("\n  … and {} more", objects.len() - SHOWN));
    }
    if let Some(note) = note {
        out.push_str(&format!("\n{note}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::Report;

    fn exit_of(r: Result<()>) -> Option<u8> {
        r.err().map(|e| Report::from_error(&e).exit)
    }

    #[test]
    fn without_a_terminal_it_refuses_with_nine_and_never_reads() {
        let r = decide("delete", &["a".into()], None, false, || {
            panic!("must not read stdin without a terminal")
        });
        assert_eq!(exit_of(r), Some(9));
    }

    #[test]
    fn on_a_terminal_only_yes_proceeds() {
        for (answer, want) in [
            ("y\n", None),
            ("YES\n", None),
            ("n\n", Some(9)),
            ("\n", Some(9)),
        ] {
            let r = decide("yank", &["a".into()], None, true, || Ok(answer.into()));
            assert_eq!(exit_of(r), want, "{answer:?}");
        }
    }

    #[test]
    fn the_summary_stops_at_twenty() {
        let objects: Vec<String> = (0..25).map(|i| format!("o{i}")).collect();
        let s = summary("delete", &objects, Some("spent forever"));
        assert!(s.contains("o19") && !s.contains("o20"), "{s}");
        assert!(
            s.contains("… and 5 more") && s.ends_with("spent forever"),
            "{s}"
        );
    }
}
