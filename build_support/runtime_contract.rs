//! The flags `.cargo/config.toml` builds mutest-rs with, which a `RUSTFLAGS` override must keep.

pub const NAMESPACE: &str = "mutest-runtime-private-v1";

/// Checks Cargo's `\x1f`-separated flags for the runtime's private crate identity and full metadata.
pub fn validate(flags: &str) -> Result<(), &'static str> {
    // NOTE: rustc accepts both `-Cname=value` and `-C name=value`; join them to compare one spelling.
    let mut args = Vec::new();
    let mut flags = flags.split('\x1f');
    while let Some(flag) = flags.next() {
        match flag {
            "-C" | "-Z" => args.push(format!("{flag}{}", flags.next().unwrap_or_default())),
            _ => args.push(flag.to_owned()),
        }
    }

    if !args.iter().any(|arg| arg.strip_prefix("-Cmetadata=") == Some(NAMESPACE)) {
        return Err("runtime build requires -Cmetadata=mutest-runtime-private-v1; RUSTFLAGS replaces the repository defaults");
    }
    // The last `-Zembed-metadata` takes effect.
    let embed_metadata = args.iter().rev().find_map(|arg| arg.strip_prefix("-Zembed-metadata="));
    if !matches!(embed_metadata, Some("yes" | "true" | "on" | "1")) {
        return Err("runtime build requires -Zembed-metadata=yes so installed runtimes carry full metadata");
    }
    Ok(())
}

pub fn enforce() {
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    if let Err(reason) = validate(&flags) {
        println!("cargo::error={reason}");
        std::process::exit(1);
    }
}
