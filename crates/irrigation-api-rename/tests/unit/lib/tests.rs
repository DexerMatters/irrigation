//! Unit tests for `src/lib.rs`.

use super::*;

#[test]
fn renamed_dependency_components_install_and_read() {
    let runtime = Runtime::new();
    runtime.install(Analyze).expect("root");
    runtime
        .edit(|edit| edit.view::<Input>().values().push(3))
        .expect("edit");
    let _ = runtime.read_outputs::<Analyze>().expect("demand");
    assert_eq!(
        runtime
            .snapshot()
            .view::<Output>()
            .field0()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![(3, 6)]
    );
}
