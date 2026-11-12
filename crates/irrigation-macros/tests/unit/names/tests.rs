//! Unit tests for `src/names.rs`.

use super::*;

#[test]
fn pascal_case_handles_snake_case() {
    assert_eq!(pascal_case("analyze_types").to_string(), "AnalyzeTypes");
}
