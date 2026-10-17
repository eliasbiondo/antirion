//! The admin API's OpenAPI document, embedded by the admin crate and read by the console's generator,
//! parses with the strict YAML loader.

use std::path::Path;

use antirion_config::{Format, parse};

#[test]
fn the_admin_api_contract_parses_with_the_strict_loader() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/admin-api.yaml");
    let text = std::fs::read(path).unwrap();
    let doc = parse(&text, Format::Yaml).unwrap();
    assert!(doc["openapi"].as_str().unwrap().starts_with("3.1"));
    assert!(doc["paths"].as_object().unwrap().len() > 100);
}
