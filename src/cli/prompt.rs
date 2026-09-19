//! Interactive prompts.
//!
//! The only place in fzv that reads stdin, kept apart so the commands stay about
//! what they do rather than how they ask.

use crate::error::{Result, err};
use std::io;

/// Asks a yes/no question (defaulting to no).
pub fn confirm(message: &str) -> Result<bool> {
    print!("{message} [y/N] ");
    io::Write::flush(&mut io::stdout()).map_err(crate::error::Error::from)?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(crate::error::Error::from)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Shows a numbered list and returns the chosen items.
///
/// An empty result means the user cancelled.
pub fn choose(title: &str, items: &[String], multi: bool) -> Result<Vec<String>> {
    if items.is_empty() {
        return Err(err!("no versions available"));
    }
    println!("{title}");
    for (index, item) in items.iter().enumerate() {
        println!("  {:>3}) {item}", index + 1);
    }
    if multi {
        println!("Enter numbers separated by spaces (for example: 1 3 5), or q to cancel.");
    } else {
        println!("Enter a number, or q to cancel.");
    }
    print!("> ");
    io::Write::flush(&mut io::stdout()).map_err(crate::error::Error::from)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(crate::error::Error::from)?;
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("q") || answer.eq_ignore_ascii_case("cancel") {
        return Ok(Vec::new());
    }

    let mut indexes = Vec::new();
    for token in answer.split(|character: char| character == ',' || character.is_whitespace()) {
        if token.is_empty() {
            continue;
        }
        let number: usize = token
            .parse()
            .map_err(|_| err!("invalid selection '{token}'"))?;
        if number == 0 || number > items.len() {
            return Err(err!("selection {number} is outside 1..={}", items.len()));
        }
        if !indexes.contains(&(number - 1)) {
            indexes.push(number - 1);
        }
        if !multi {
            break;
        }
    }
    if indexes.is_empty() {
        return Err(err!("no version selected"));
    }
    indexes.sort_unstable();
    Ok(indexes
        .into_iter()
        .map(|index| items[index].clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_rejects_an_empty_list() {
        assert!(choose("Pick", &[], false).is_err());
    }
}
