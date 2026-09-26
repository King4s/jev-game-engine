//! Sanitize an exported recording before it is published: validate it through the engine's own
//! loader, replace exactly the identifiers the operator names, validate the result again and only
//! then write it.
//!
//! It never guesses what is sensitive and it never rewrites numbers, so the recorded distances,
//! tolerances and elapsed times survive unchanged and stay checkable. A result the engine would no
//! longer accept is not written at all, and the process exits non-zero saying why.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Usage shown for every argument error.
const USAGE: &str =
    "Usage: recording-sanitize <input.json> <output.json> --redact from=to [--redact from=to ...]";

fn main() -> Result<()> {
    let Invocation {
        input,
        output,
        replacements,
    } = parse_arguments(std::env::args().skip(1))?;

    // The source must be a recording this engine accepts, so a corrupt input is refused before
    // anything is rewritten.
    let source = jev_game_engine::recording::load(&input)
        .with_context(|| format!("input {input} did not validate as a recording"))?;

    let text = std::fs::read_to_string(&input)
        .with_context(|| format!("could not read {input} as text"))?;
    let mut document: Value =
        serde_json::from_str(&text).with_context(|| format!("{input} is not JSON"))?;

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (from, to) in &replacements {
        counts.insert(from.clone(), replace_in_strings(&mut document, from, to));
    }

    // Compact output keeps a published artifact small; the loader accepts either form.
    let sanitized = serde_json::to_string(&document).context("could not serialize the result")?;
    let check = check_path(&output);
    std::fs::write(&check, &sanitized)
        .with_context(|| format!("could not write the validation copy {}", check.display()))?;
    let validated = jev_game_engine::recording::load(&check.to_string_lossy());
    if validated.is_err() {
        let _ = std::fs::remove_file(&check);
        return validated.map(|_| ()).with_context(|| {
            format!("the sanitized recording no longer validates; nothing was written to {output}")
        });
    }
    let validated = validated.expect("checked above");
    std::fs::rename(&check, &output)
        .with_context(|| format!("could not move the validated result to {output}"))?;

    println!(
        "Sanitized {} -> {} ({} events, session {})",
        input,
        output,
        validated.events.len(),
        validated.id
    );
    for (from, to) in &replacements {
        let count = counts.get(from).copied().unwrap_or(0);
        println!("  {from} -> {to}: {count} replacement(s)");
    }
    println!(
        "Validated: the result loads through the engine's own loader. Only string values were \
         rewritten and only through the pairs above, so numbers, distances, tolerances and \
         elapsed times are the recorded ones. What a pair did not name was left alone, so review \
         the output before publishing it; the source {} is unchanged.",
        source.id
    );
    Ok(())
}

/// What the command line asked for: the two paths and every replacement to apply.
struct Invocation {
    input: String,
    output: String,
    replacements: Vec<(String, String)>,
}

/// Reads the input path, the output path and every `--redact from=to` pair.
fn parse_arguments(arguments: impl Iterator<Item = String>) -> Result<Invocation> {
    let mut positional: Vec<String> = Vec::new();
    let mut replacements: Vec<(String, String)> = Vec::new();
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        let pair = if argument == "--redact" {
            arguments
                .next()
                .context("--redact needs a from=to argument after it")?
        } else if let Some(pair) = argument.strip_prefix("--redact=") {
            pair.to_string()
        } else if argument.starts_with("--") {
            bail!("unknown option {argument}\n{USAGE}");
        } else {
            positional.push(argument);
            continue;
        };
        let (from, to) = pair
            .split_once('=')
            .with_context(|| format!("--redact expects from=to, got {pair}\n{USAGE}"))?;
        if from.is_empty() {
            bail!("--redact from must not be empty\n{USAGE}");
        }
        replacements.push((from.to_string(), to.to_string()));
    }

    let (input, output) = match positional.as_slice() {
        [input, output] => (input.clone(), output.clone()),
        _ => bail!("{USAGE}"),
    };
    if replacements.is_empty() {
        bail!(
            "Refusing to run without --redact: this tool replaces exactly the identifiers you \
             name and does not detect them for you.\n{USAGE}"
        );
    }
    Ok(Invocation {
        input,
        output,
        replacements,
    })
}

/// Replaces `from` with `to` in every string value of the document and reports how many
/// occurrences were rewritten. Numbers, booleans and nulls are never touched.
fn replace_in_strings(node: &mut Value, from: &str, to: &str) -> usize {
    match node {
        Value::String(value) => {
            let occurrences = value.matches(from).count();
            if occurrences > 0 {
                *value = value.replace(from, to);
            }
            occurrences
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| replace_in_strings(item, from, to))
            .sum(),
        Value::Object(entries) => entries
            .values_mut()
            .map(|value| replace_in_strings(value, from, to))
            .sum(),
        _ => 0,
    }
}

/// The sibling path the sanitized result is validated at before it replaces the output file.
/// The loader requires the `.json` extension, so the output path keeps it.
fn check_path(output: &str) -> PathBuf {
    let path = Path::new(output);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sanitized".to_string());
    path.with_file_name(format!("{stem}.validated.json"))
}
