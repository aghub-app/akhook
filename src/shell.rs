use std::path::Path;

use ast_grep_core::{Doc, Node};
use ast_grep_language::{LanguageExt, SupportLang};

use crate::config::ArgvItem;

/// Every simple command in a shell script, as unquoted words: the commands of
/// lists, pipelines, subshells and substitutions, and the scripts passed to
/// `bash -c` and friends. Assignments and redirections are dropped.
pub fn simple_commands(script: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    collect(script, &mut commands, 0);
    commands
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

fn collect(script: &str, commands: &mut Vec<Vec<String>>, depth: usize) {
    let tree = SupportLang::Bash.ast_grep(script);
    for node in tree.root().dfs().filter(|node| node.kind() == "command") {
        let words = words(&node);
        if words.is_empty() {
            continue;
        }
        if depth < 4
            && SHELLS.contains(&basename(&words[0]))
            && let Some(inner) = words
                .iter()
                .position(|w| w == "-c")
                .and_then(|i| words.get(i + 1))
        {
            collect(inner, commands, depth + 1);
        }
        commands.push(words);
    }
}

fn words<D: Doc>(command: &Node<D>) -> Vec<String> {
    let mut words = Vec::new();
    let mut named = false;
    for child in command.children().filter(Node::is_named) {
        let kind = child.kind();
        if kind == "command_name" {
            named = true;
        } else if !named || kind.ends_with("redirect") || kind == "variable_assignment" {
            continue;
        }
        words.push(unquote(&child));
    }
    words
}

fn unquote<D: Doc>(node: &Node<D>) -> String {
    let text = node.text();
    match node.kind().as_ref() {
        "command_name" | "concatenation" => node.children().map(|child| unquote(&child)).collect(),
        "raw_string" => strip(&text, "'", "'").to_owned(),
        "ansi_c_string" => strip(&text, "$'", "'").to_owned(),
        "string" => unescape(strip(&text, "\"", "\"")),
        "word" | "number" => unescape(&text),
        _ => text.into_owned(),
    }
}

fn strip<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    text.strip_prefix(start)
        .and_then(|rest| rest.strip_suffix(end))
        .unwrap_or(text)
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

fn basename(word: &str) -> &str {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
}

/// The first item names the program (compared by basename); the others must
/// appear among its arguments in order, though not necessarily adjacent, so
/// options in between (`gh -R owner/repo pr create`) do not hide a match.
pub fn argv_matches(pattern: &[ArgvItem], words: &[String]) -> bool {
    let Some((program, rest)) = pattern.split_first() else {
        return false;
    };
    let Some((first, args)) = words.split_first() else {
        return false;
    };
    if !program.matches(basename(first)) {
        return false;
    }
    let mut args = args.iter();
    rest.iter().all(|item| args.any(|arg| item.matches(arg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(words: &[&str]) -> Vec<ArgvItem> {
        words.iter().map(|w| ArgvItem::One((*w).into())).collect()
    }

    fn hits(pattern: &[ArgvItem], script: &str) -> bool {
        simple_commands(script)
            .iter()
            .any(|words| argv_matches(pattern, words))
    }

    #[test]
    fn splits_lists_pipelines_and_substitutions() {
        assert_eq!(
            simple_commands(
                "cd 'a b' && FOO=1 gh pr create --title \"x y\" > out | cat; echo $(git push)"
            ),
            vec![
                vec!["cd", "a b"],
                vec!["gh", "pr", "create", "--title", "x y"],
                vec!["cat"],
                vec!["echo", "$(git push)"],
                vec!["git", "push"],
            ]
        );
    }

    #[test]
    fn matches_program_basename_and_ordered_arguments() {
        let pattern = items(&["gh", "pr", "create"]);
        assert!(hits(&pattern, "gh pr create"));
        assert!(hits(&pattern, "/usr/bin/gh -R a/b pr create --fill"));
        assert!(hits(&pattern, "cd repo && g\\h 'pr' \"create\""));
        assert!(hits(&pattern, "bash -c 'gh pr create'"));
        assert!(!hits(&pattern, "gh pr view"));
        assert!(!hits(&pattern, "echo gh pr create"));
        assert!(!hits(&pattern, "gh create pr"));
        let any = vec![
            ArgvItem::One("gh".into()),
            ArgvItem::Any(vec!["pr".into(), "issue".into()]),
            ArgvItem::Any(vec!["close".into(), "comment".into()]),
        ];
        assert!(hits(&any, "gh issue comment 3 -b hi"));
        assert!(!hits(&any, "gh issue view 3"));
    }
}
