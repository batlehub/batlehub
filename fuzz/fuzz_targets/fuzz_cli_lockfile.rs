#![no_main]

use libfuzzer_sys::fuzz_target;

use batlehub_core::lockfile::{read, Format};

// `batlehub-cli check` reads lockfiles from whatever repository it is run in,
// and a clone of anybody's repository is a lockfile (RFC 0040 T9). The first
// byte picks a format, the rest is the file. Every reader, on any bytes:
//
//   * returns, without panicking — a lockfile is not a way to crash `check`;
//   * keeps no empty name or version, nothing twice, and nothing it did not
//     read out of the file — no pin is longer than the input;
//   * names the format's own registry kind on every pin.
fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let format = Format::ALL[selector as usize % Format::ALL.len()];
    let Ok(pins) = read(format, body) else {
        return;
    };
    // `read` decodes lossily, and each invalid byte becomes a 3-byte U+FFFD,
    // so the bound is the decoded text, not the raw bytes.
    let text_len = String::from_utf8_lossy(body).len();
    let mut seen = std::collections::HashSet::new();
    for pin in &pins {
        assert_eq!(pin.kind, format.kind());
        assert!(!pin.name.is_empty() && !pin.version.is_empty());
        assert!(seen.insert((pin.name.as_str(), pin.version.as_str())), "a pin twice");
        // `.nvmrc` adds the `v` the dist tree spells; `.terraform.lock.hcl`
        // the `providers/` prefix. Nothing else is invented.
        assert!(pin.version.len() <= text_len + 1);
    }
});
