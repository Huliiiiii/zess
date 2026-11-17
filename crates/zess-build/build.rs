fn main() {
    println!("cargo::rerun-if-env-changed=ZESS_RELEASE_VERSION");
    if let Ok(version) = std::env::var("ZESS_RELEASE_VERSION") {
        let base = env!("CARGO_PKG_VERSION");
        let preview = version
            .strip_prefix(base)
            .and_then(|suffix| suffix.strip_prefix("-dev."));
        assert!(
            version == base
                || preview.is_some_and(|suffix| {
                    suffix.split('.').all(|part| {
                        !part.is_empty()
                            && (part.len() == 1 || !part.starts_with('0'))
                            && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                }),
            "ZESS_RELEASE_VERSION must be {base} or a numbered {base}-dev prerelease"
        );
    }
}
