//! The demo language: a line per statement, good enough to type into a box.
//!
//! ```text
//! x := 1
//! y := 2
//! print(show(x), y)
//! ```
//!
//! The grammar is deliberately tiny; a parser error names the line it found.

use crate::demo::ast;

/// Parses a program, or explains the first line that does not parse.
pub fn parse(text: &str) -> Result<ast::Document, String> {
    let mut statements = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let number = index + 1;
        statements.push(statement(line).map_err(|message| format!("line {number}: {message}"))?);
    }
    if statements.is_empty() {
        return Err(String::from("the program is empty"));
    }
    Ok(ast::Document::Program { statements })
}

fn statement(line: &str) -> Result<ast::Statement, String> {
    if let Some(rest) = line.strip_prefix("print") {
        let rest = rest.trim_start();
        let inner = rest
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
            .ok_or_else(|| String::from("print needs parentheses"))?;
        let mut values = Vec::new();
        for piece in split_arguments(inner)? {
            values.push(expression(&piece)?);
        }
        if values.is_empty() {
            return Err(String::from("print needs at least one expression"));
        }
        return Ok(ast::Statement::Print { values });
    }
    let (name, value) = line
        .split_once(":=")
        .ok_or_else(|| String::from("expected `name := value` or `print(...)`"))?;
    let name = name.trim();
    if !is_name(name) {
        return Err(format!("`{name}` is not a name"));
    }
    Ok(ast::Statement::Assign {
        name: name.to_owned(),
        value: expression(value.trim())?,
    })
}

fn expression(text: &str) -> Result<ast::Expression, String> {
    if let Ok(number) = text.parse::<i64>() {
        return Ok(ast::Expression::Number(number));
    }
    if let Some(inner) = text
        .find('(')
        .filter(|_| text.ends_with(')'))
        .map(|index| (&text[..index], &text[index + 1..text.len() - 1]))
    {
        let (callee, inner) = inner;
        let callee = callee.trim();
        if !is_name(callee) {
            return Err(format!("`{callee}` is not a name"));
        }
        let mut args = Vec::new();
        for piece in split_arguments(inner)? {
            args.push(expression(&piece)?);
        }
        return Ok(ast::Expression::Call {
            callee: callee.to_owned(),
            args,
        });
    }
    if is_name(text) {
        return Ok(ast::Expression::Name(text.to_owned()));
    }
    Err(format!("`{text}` is not an expression"))
}

/// Splits on top-level commas; nested calls keep their commas.
fn split_arguments(text: &str) -> Result<Vec<String>, String> {
    let mut pieces = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for character in text.chars() {
        match character {
            '(' => {
                depth += 1;
                current.push(character);
            }
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| String::from("unbalanced `)`"))?;
                current.push(character);
            }
            ',' if depth == 0 => {
                pieces.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if depth != 0 {
        return Err(String::from("unbalanced `(`"));
    }
    if !current.trim().is_empty() {
        pieces.push(current.trim().to_owned());
    }
    for piece in &pieces {
        if piece.is_empty() {
            return Err(String::from("empty argument"));
        }
    }
    Ok(pieces)
}

fn is_name(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && characters.all(|character| character.is_alphanumeric() || character == '_')
}

/// Prints a program back out, one statement per line.
#[must_use]
pub fn print(document: &ast::Document) -> String {
    let ast::Document::Program { statements } = document;
    statements
        .iter()
        .map(|statement| match statement {
            ast::Statement::Assign { name, value } => {
                format!("{name} := {}", expression_text(value))
            }
            ast::Statement::Print { values } => {
                let values: Vec<String> = values.iter().map(expression_text).collect();
                format!("print({})", values.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn expression_text(expression: &ast::Expression) -> String {
    match expression {
        ast::Expression::Name(name) => name.clone(),
        ast::Expression::Number(number) => number.to_string(),
        ast::Expression::Call { callee, args } => {
            let args: Vec<String> = args.iter().map(expression_text).collect();
            format!("{callee}({})", args.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_program_round_trips() {
        let source = "x := 1\ny := 2\nprint(show(x), y)";
        let document = parse(source).expect("parse");
        assert_eq!(print(&document), source);
        assert_eq!(document, crate::demo::program());
    }

    #[test]
    fn a_parse_error_names_the_line() {
        let error = parse("x := 1\nnope\n").expect_err("error");
        assert!(error.starts_with("line 2:"), "{error}");
    }

    #[test]
    fn nested_calls_keep_their_arguments() {
        let document = parse("print(show(add(1, 2)), 3)").expect("parse");
        let ast::Document::Program { statements } = &document;
        let ast::Statement::Print { values } = &statements[0] else {
            panic!("expected print");
        };
        assert_eq!(values.len(), 2);
        let ast::Expression::Call { callee, args } = &values[0] else {
            panic!("expected call");
        };
        assert_eq!(callee, "show");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let document = parse("// a program\n\nx := 1 // bound\n").expect("parse");
        let ast::Document::Program { statements } = document;
        assert_eq!(statements.len(), 1);
    }

    #[test]
    fn an_empty_program_is_an_error_not_an_empty_tree() {
        assert!(parse("   \n\n").is_err());
    }
}
