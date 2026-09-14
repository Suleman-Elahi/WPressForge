//! Every privileged action goes through here: a single place that logs the
//! command, honours dry-run, and turns failures into protocol errors.

use std::ffi::OsStr;
use std::process::Output;
use std::time::Instant;
use tokio::process::Command;
use wp_common::protocol::StepReport;
use wp_common::{Error, Result};

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub skipped: bool,
}

impl CommandOutput {
    pub fn trimmed_stdout(&self) -> &str {
        self.stdout.trim()
    }
}

/// Runs `program` with `args`. Arguments are passed as a list, never through a
/// shell, so site names and domains cannot inject commands.
pub async fn run<S: AsRef<OsStr> + std::fmt::Debug>(
    dry_run: bool,
    program: &str,
    args: &[S],
) -> Result<CommandOutput> {
    run_with_env(dry_run, program, args, &[]).await
}

/// Runs `program` with `args` and environment variables.
/// Only environment variable KEYS are logged, never values.
pub async fn run_with_env<S: AsRef<OsStr> + std::fmt::Debug>(
    dry_run: bool,
    program: &str,
    args: &[S],
    env: &[(String, String)],
) -> Result<CommandOutput> {
    let printable = format!("{program} {args:?}");
    let env_keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();

    if dry_run {
        tracing::info!(
            command = %printable,
            env_keys = ?env_keys,
            "dry-run: not executed"
        );
        return Ok(CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            skipped: true,
        });
    }

    tracing::debug!(command = %printable, env_keys = ?env_keys, "executing");
    let started = Instant::now();
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output: Output = cmd
        .output()
        .await
        .map_err(|e| Error::Command {
            command: printable.clone(),
            status: -1,
            stderr: e.to_string(),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code().unwrap_or(-1);

    if !output.status.success() {
        return Err(Error::Command {
            command: printable,
            status: exit_code,
            stderr: stderr.trim().to_string(),
        });
    }

    tracing::debug!(command = %printable, elapsed_ms = started.elapsed().as_millis(), "completed");

    Ok(CommandOutput {
        stdout,
        stderr,
        exit_code,
        skipped: false,
    })
}

/// Collects step reports so the panel can render a timeline.
#[derive(Debug, Default)]
pub struct Steps {
    reports: Vec<StepReport>,
}

impl Steps {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `future`, timing it and recording success or failure.
    pub async fn step<F, T>(&mut self, name: &str, future: F) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        let started = Instant::now();
        let result = future.await;
        let duration_ms = started.elapsed().as_millis() as u64;

        match &result {
            Ok(_) => self.reports.push(StepReport {
                name: name.to_string(),
                ok: true,
                duration_ms,
                detail: None,
            }),
            Err(error) => self.reports.push(StepReport {
                name: name.to_string(),
                ok: false,
                duration_ms,
                detail: Some(error.to_string()),
            }),
        }

        result
    }

    pub fn note(&mut self, name: &str, detail: impl Into<String>) {
        self.reports.push(StepReport {
            name: name.to_string(),
            ok: true,
            duration_ms: 0,
            detail: Some(detail.into()),
        });
    }

    pub fn into_reports(self) -> Vec<StepReport> {
        self.reports
    }
}
