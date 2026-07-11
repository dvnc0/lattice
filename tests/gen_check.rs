//! Smoke tests: generate each fixture → validate with `lattice check`.

use std::path::PathBuf;

use lattice::config::{check_str, Format};
use lattice::gen::{emit, openapi, render};

const SPECS: &[&str] = &[
    "examples/openapi/petstore.yaml",
    "examples/openapi/api_key.yaml",
    "examples/openapi/dispatcher.yaml",
];

#[test]
fn generated_configs_pass_check() {
    for spec in SPECS {
        let path = PathBuf::from(spec);
        let (input, _) =
            openapi::parse(&path).unwrap_or_else(|e| panic!("parse failed for {spec}: {e}"));
        let (config, _) = emit::emit(&input, None);
        let yaml =
            render::to_yaml(&config).unwrap_or_else(|e| panic!("render failed for {spec}: {e}"));

        let report = check_str(&yaml, Format::Yaml)
            .unwrap_or_else(|e| panic!("check_str failed for {spec}: {e}"));

        // Generated configs always use ${ENV_VAR} references for secrets. Filter
        // those out so the test does not require real credentials to be set.
        let structural_errors: Vec<&str> = report
            .errors
            .iter()
            .filter(|e| !e.starts_with("missing environment variable"))
            .map(String::as_str)
            .collect();

        assert!(
            structural_errors.is_empty(),
            "generated config for '{spec}' has structural check errors:\n{}",
            structural_errors.join("\n")
        );
    }
}
