use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::{
    config::{AstSpec, CheckSpec, RuleEvent, RuleSpec},
    model::FileAction,
};

macro_rules! source {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../presets/omp/", $name, ".md")),
        )
    };
}

// Exactly the 27 builtin-defaults sources in omp v18.2.11's index.ts.
const OMP: &[(&str, &str)] = &[
    source!("go-add-cleanup"),
    source!("go-bench-loop"),
    source!("go-exp-promoted"),
    source!("go-ioutil"),
    source!("go-join-hostport"),
    source!("go-new-expr"),
    source!("go-rand-v2"),
    source!("go-range-int"),
    source!("rs-box-leak"),
    source!("rs-future-prelude"),
    source!("rs-lazylock"),
    source!("rs-match-ergonomics"),
    source!("rs-parking-lot"),
    source!("rs-result-type"),
    source!("ts-bare-catch"),
    source!("ts-import-type"),
    source!("ts-no-any"),
    source!("ts-no-deprecated-leftovers"),
    source!("ts-no-dynamic-import"),
    source!("ts-no-inline-cast-access"),
    source!("ts-no-local-is-record"),
    source!("ts-no-return-type"),
    source!("ts-no-test-timers"),
    source!("ts-no-tiny-functions"),
    source!("ts-promise-with-resolvers"),
    source!("ts-redundant-clear-guard"),
    source!("ts-set-map"),
];

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn values(self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

#[derive(Deserialize)]
struct Frontmatter {
    condition: Option<OneOrMany>,
    #[serde(rename = "astCondition")]
    ast_condition: Option<OneOrMany>,
    scope: String,
}

pub fn omp_rules() -> Result<Vec<RuleSpec>> {
    let scope_pattern = Regex::new(r"tool:(?:edit|write)\(([^)]*)\)")?;
    OMP.iter()
        .map(|(name, markdown)| {
            let source = markdown
                .strip_prefix("---\n")
                .context("missing frontmatter")?;
            let (header, body) = source
                .split_once("\n---\n")
                .context("unterminated frontmatter")?;
            let front: Frontmatter =
                serde_yaml::from_str(header).with_context(|| format!("omp/{name}"))?;
            let paths: BTreeSet<_> = scope_pattern
                .captures_iter(&front.scope)
                .map(|capture| capture[1].to_string())
                .collect();
            if paths.is_empty() {
                bail!("omp/{name}: no file scope");
            }
            let mut checks = Vec::new();
            for regex in front.condition.into_iter().flat_map(OneOrMany::values) {
                checks.push(CheckSpec::Regex { regex });
            }
            let language = if name.starts_with("go-") {
                "go"
            } else if name.starts_with("rs-") {
                "rust"
            } else {
                "tsx"
            };
            for pattern in front.ast_condition.into_iter().flat_map(OneOrMany::values) {
                checks.push(CheckSpec::Ast {
                    ast: AstSpec {
                        language: Some(language.into()),
                        pattern,
                    },
                });
            }
            if checks.is_empty() {
                bail!("omp/{name}: no checks");
            }
            Ok(RuleSpec {
                id: format!("omp/{name}"),
                on: RuleEvent::FileChange,
                paths: paths.into_iter().collect(),
                actions: vec![FileAction::Create, FileAction::Modify],
                checks,
                message: body.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn imports_complete_tagged_preset() {
        let rules = super::omp_rules().unwrap();
        assert_eq!(rules.len(), 27);
        assert_eq!(
            rules
                .iter()
                .filter(|r| r
                    .checks
                    .iter()
                    .any(|c| matches!(c, crate::config::CheckSpec::Ast { .. })))
                .count(),
            5
        );
    }
}
