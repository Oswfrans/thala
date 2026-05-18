//! Worker runner command construction.
//!
//! Execution backends decide where a worker runs. This module decides which
//! worker CLI runs there and how runner extensions are prepared.

use std::fmt::Write as _;

use crate::core::error::ThalaError;
use crate::core::workflow::{is_valid_extension_name, WorkerConfig, WorkerRunnerKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRunnerPlan {
    pub runner: WorkerRunnerKind,
    pub install_steps: Vec<RunnerCommand>,
    pub run: RunnerCommand,
}

pub struct WorkerRunner;

impl WorkerRunner {
    pub fn plan(
        config: &WorkerConfig,
        model: &str,
        prompt: PromptSource<'_>,
    ) -> Result<WorkerRunnerPlan, ThalaError> {
        validate_extensions(&config.extensions)?;

        match config.runner {
            WorkerRunnerKind::Pi => Ok(pi_plan(config, model, prompt)),
            WorkerRunnerKind::Opencode => Ok(opencode_plan(model, prompt)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PromptSource<'a> {
    Inline(&'a str),
    File(&'a str),
}

fn pi_plan(config: &WorkerConfig, model: &str, prompt: PromptSource<'_>) -> WorkerRunnerPlan {
    let install_steps = config
        .extensions
        .iter()
        .map(|extension| RunnerCommand {
            program: "pi".into(),
            args: vec!["install".into(), extension_install_source(extension)],
        })
        .collect();

    let mut args = vec![
        "--model".into(),
        model.into(),
        "--no-session".into(),
        "-p".into(),
    ];
    match prompt {
        PromptSource::Inline(prompt) => args.push(prompt.into()),
        PromptSource::File(path) => {
            args.push(format!("@{path}"));
        }
    }

    WorkerRunnerPlan {
        runner: WorkerRunnerKind::Pi,
        install_steps,
        run: RunnerCommand {
            program: "pi".into(),
            args,
        },
    }
}

fn opencode_plan(model: &str, prompt: PromptSource<'_>) -> WorkerRunnerPlan {
    let mut args = vec!["--model".into(), model.into(), "--no-session".into()];
    match prompt {
        PromptSource::Inline(prompt) => {
            args.push("-p".into());
            args.push(prompt.into());
        }
        PromptSource::File(path) => {
            args.push("-p".into());
            args.push(format!("@{path}"));
        }
    }

    WorkerRunnerPlan {
        runner: WorkerRunnerKind::Opencode,
        install_steps: Vec::new(),
        run: RunnerCommand {
            program: "opencode".into(),
            args,
        },
    }
}

pub fn validate_extensions(extensions: &[String]) -> Result<(), ThalaError> {
    for extension in extensions {
        if !is_valid_extension_name(extension) {
            return Err(ThalaError::backend(
                "worker-runner",
                format!("unsafe worker extension name `{extension}`"),
            ));
        }
    }
    Ok(())
}

pub fn extension_env_value(config: &WorkerConfig) -> Result<String, ThalaError> {
    validate_extensions(&config.extensions)?;
    Ok(config.extensions.join(","))
}

pub fn extension_install_source(extension: &str) -> String {
    let package = match extension {
        "autoresearch" => "pi-autoresearch",
        "goal" => "@narumitw/pi-goal",
        other => other,
    };
    format!("npm:{package}")
}

pub fn local_runner_script(config: &WorkerConfig) -> Result<String, ThalaError> {
    validate_extensions(&config.extensions)?;

    let extensions = config
        .extensions
        .iter()
        .fold(String::new(), |mut output, extension| {
            let _ = writeln!(output, "  {:?}", extension_install_source(extension));
            output
        });

    let body = match config.runner {
        WorkerRunnerKind::Pi => format!(
            r#"extensions=(
{extensions})

if ! command -v pi >/dev/null 2>&1; then
  echo "pi binary not found" >&2
  exit 127
fi

for extension in "${{extensions[@]}}"; do
  pi install "${{extension}}"
done

exec pi --model "${{THALA_MODEL}}" --no-session -p "$(cat "${{THALA_PROMPT_PATH}}")"
"#
        ),
        WorkerRunnerKind::Opencode => r#"if ! command -v opencode >/dev/null 2>&1; then
  echo "opencode binary not found" >&2
  exit 127
fi

exec opencode --model "${THALA_MODEL}" --no-session -p "$(cat "${THALA_PROMPT_PATH}")"
"#
        .into(),
    };

    Ok(format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

if [ -z "${{THALA_MODEL:-}}" ] || [ -z "${{THALA_PROMPT_PATH:-}}" ]; then
  echo "THALA_MODEL and THALA_PROMPT_PATH are required" >&2
  exit 2
fi

{body}"#
    ))
}
