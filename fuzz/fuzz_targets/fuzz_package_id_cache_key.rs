#![no_main]

use libfuzzer_sys::fuzz_target;

use batlehub_core::entities::PackageId;

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);

    let Ok(registry): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(name): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(version): arbitrary::Result<String> = u.arbitrary() else {
        return;
    };
    let Ok(artifact): arbitrary::Result<Option<String>> = u.arbitrary() else {
        return;
    };

    let id = match artifact {
        Some(art) => PackageId::new(&registry, name, version).with_artifact(art),
        None => PackageId::new(&registry, name, version),
    };

    let key = id.cache_key();

    // Key must be deterministic and contain the registry component.
    assert!(key.contains(&registry));
    assert_eq!(id.cache_key(), key);
});
