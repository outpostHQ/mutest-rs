#[path = "../../build_support/runtime_contract.rs"]
#[allow(
    dead_code,
    reason = "the shared module also defines the build-only enforcement entrypoint"
)]
mod runtime_contract;

#[test]
fn an_override_keeping_the_private_identity_and_full_metadata_is_accepted() {
    assert_eq!(
        runtime_contract::validate("-Zembed-metadata=yes\x1f-Cmetadata=mutest-runtime-private-v1"),
        Ok(())
    );
    assert_eq!(
        runtime_contract::validate(
            "-Dwarnings\x1f-Z\x1fembed-metadata=yes\x1f-C\x1fmetadata=mutest-runtime-private-v1"
        ),
        Ok(())
    );
}

#[test]
fn an_override_dropping_the_private_identity_or_full_metadata_is_refused() {
    assert!(
        runtime_contract::validate("-Zembed-metadata=yes")
            .unwrap_err()
            .contains("metadata=mutest-runtime-private-v1")
    );
    assert!(
        runtime_contract::validate(
            "-Zembed-metadata=yes\x1f-Cmetadata=mutest-runtime-private-v1\x1f-Zembed-metadata=no"
        )
        .unwrap_err()
        .contains("embed-metadata=yes")
    );
    assert!(
        runtime_contract::validate("-Zembed-metadata=yes\x1f-Cmetadata=another-project").is_err()
    );
}
