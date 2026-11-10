//! The macros must work when the facade dependency is renamed.
//!
//! Cargo uses `irr` as the dependency key; generated code must follow it.

use irr::prelude::*;

#[view(source)]
pub struct Input {
    values: List<u32>,
}

#[view(derived)]
pub struct Output(Map<u32, u32>);

#[component(root)]
pub fn analyze(input: &Input) -> Output {
    Output::from(input.values().map(|value| (*value, *value * 2)))
}

#[tree_view(root = Document, view = Syntax)]
pub mod syntax {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Document {
        Module { items: Vec<Expr> },
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum Expr {
        Literal(i64),
    }
}

#[component(callable)]
pub fn first(input: &Input) -> Option<u32> {
    input.values().get(0)
}

#[derive(Debug)]
pub struct Producer;

#[general_component(writes(Output), reads(Input))]
impl Producer {
    fn run(cx: &mut GeneralCx) -> Result<()> {
        let input = Input::VALUES.to_vec_in(cx)?;
        cx.write(Output::FIELD0)
            .entry(1)
            .set(input.first().copied().unwrap_or(0) * 2);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
