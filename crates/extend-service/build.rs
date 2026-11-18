use std::{env, fs, path::PathBuf};

fn main() {
    let source = "../../web/public/reference/api.yaml";
    println!("cargo:rerun-if-changed={source}");
    let yaml = fs::read_to_string(source).expect("read public API contract");
    let document: serde_json::Value = serde_yaml::from_str(&yaml).expect("parse public OpenAPI contract");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("openapi.json");
    fs::write(output, serde_json::to_vec(&document).unwrap()).expect("write OpenAPI JSON");
}
