//! Real-component conformance via the testkit: the checked `.wasm`
//! matches the typed declarations (capability ids, imports, help/usage).
//!
//! Requires `DEKOPON_PROVIDER_COMPONENT` to point at the built component
//! (`scripts/build-component.sh`). When unset, the test reports a skip
//! instead of failing, so `cargo test` stays green without a build.

use std::path::PathBuf;

use dekopon_provider_pledge::Pledge;
use dekopon_provider_sdk_testkit::conformance;

fn component() -> Option<PathBuf> {
    match std::env::var_os("DEKOPON_PROVIDER_COMPONENT") {
        None => {
            eprintln!(
                "skipping component conformance: DEKOPON_PROVIDER_COMPONENT is unset; run scripts/build-component.sh"
            );
            None
        }
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "DEKOPON_PROVIDER_COMPONENT does not name a file: {}",
                path.display()
            );
            Some(path)
        }
    }
}

#[test]
fn built_component_conforms_to_its_typed_declarations() {
    let Some(component) = component() else {
        return;
    };
    conformance::<Pledge>(&component).expect("JSONL, clock and random only");
}
