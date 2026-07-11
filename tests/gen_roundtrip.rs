//! Integration tests: parse OpenAPI fixture → generate Lattice YAML → compare golden.
//!
//! To refresh goldens after mapping-rule changes:
//!   UPDATE_GOLDEN=1 cargo test --test gen_roundtrip

use std::path::PathBuf;

use lattice::config::Format;
use lattice::gen::{emit, openapi, render};

struct Fixture {
    spec: &'static str,
    golden: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        spec: "examples/openapi/petstore.yaml",
        golden: "examples/openapi/petstore.expected.yaml",
    },
    Fixture {
        spec: "examples/openapi/api_key.yaml",
        golden: "examples/openapi/api_key.expected.yaml",
    },
    Fixture {
        spec: "examples/openapi/dispatcher.yaml",
        golden: "examples/openapi/dispatcher.expected.yaml",
    },
];

fn generate(spec: &str) -> String {
    let path = PathBuf::from(spec);
    let (input, warnings) = openapi::parse(&path).expect("parse failed");
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let (config, warnings) = emit::emit(&input, None);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    render::to_yaml(&config).expect("render failed")
}

#[test]
fn petstore_roundtrip() {
    roundtrip_fixture(&FIXTURES[0]);
}

#[test]
fn api_key_roundtrip() {
    roundtrip_fixture(&FIXTURES[1]);
}

#[test]
fn dispatcher_roundtrip() {
    roundtrip_fixture(&FIXTURES[2]);
}

fn roundtrip_fixture(fixture: &Fixture) {
    let actual = generate(fixture.spec);
    let golden_path = PathBuf::from(fixture.golden);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&golden_path, &actual).expect("failed to write golden");
        return;
    }

    let expected = std::fs::read_to_string(&golden_path).unwrap_or_else(|_| {
        panic!(
            "golden file '{}' not found — run UPDATE_GOLDEN=1 cargo test --test gen_roundtrip to create it",
            fixture.golden
        )
    });

    assert_eq!(
        actual, expected,
        "golden mismatch for '{}' — run UPDATE_GOLDEN=1 cargo test --test gen_roundtrip to refresh",
        fixture.spec
    );
}

// ── Structural assertions on generated output ─────────────────────────────────

#[test]
fn petstore_has_correct_tool_count() {
    let path = PathBuf::from("examples/openapi/petstore.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    // petstore.yaml has 4 operations: listPets, createPet, showPetById, deletePet
    assert_eq!(
        config.tools.len(),
        4,
        "unexpected tool count: {:?}",
        config.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

#[test]
fn petstore_expose_is_tools() {
    let path = PathBuf::from("examples/openapi/petstore.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    assert_eq!(config.server.expose, lattice::config::ExposeMode::Tools);
}

#[test]
fn dispatcher_expose_is_dispatcher() {
    let path = PathBuf::from("examples/openapi/dispatcher.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    assert_eq!(
        config.server.expose,
        lattice::config::ExposeMode::Dispatcher
    );
}

#[test]
fn petstore_base_url_is_set() {
    let path = PathBuf::from("examples/openapi/petstore.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    assert_eq!(
        config.defaults.base_url.as_deref(),
        Some("https://petstore.example.com/v1")
    );
}

#[test]
fn petstore_has_bearer_auth() {
    let path = PathBuf::from("examples/openapi/petstore.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    assert!(
        matches!(
            config.defaults.auth,
            Some(lattice::config::Auth::Bearer { .. })
        ),
        "expected bearer auth"
    );
}

#[test]
fn api_key_has_api_key_auth() {
    let path = PathBuf::from("examples/openapi/api_key.yaml");
    let (input, _) = openapi::parse(&path).unwrap();
    let (config, _) = emit::emit(&input, None);
    assert!(
        matches!(
            config.defaults.auth,
            Some(lattice::config::Auth::ApiKey { .. })
        ),
        "expected api_key auth"
    );
}

// ── Generate → parse (semantic round-trip) ────────────────────────────────────

#[test]
fn generated_yaml_reparses() {
    for fixture in FIXTURES {
        let yaml = generate(fixture.spec);
        let reparsed = lattice::config::parse_config(&yaml, Format::Yaml)
            .unwrap_or_else(|e| panic!("failed to re-parse output for {}: {e}", fixture.spec));
        // Sanity: the re-parsed config has at least the same server name.
        let path = PathBuf::from(fixture.spec);
        let (input, _) = openapi::parse(&path).unwrap();
        let (original_config, _) = emit::emit(&input, None);
        assert_eq!(
            reparsed.server.name, original_config.server.name,
            "server name changed in round-trip for {}",
            fixture.spec
        );
    }
}
