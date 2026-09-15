//! Every privileged action goes through here: a single place that logs the
//! command, honours dry-run, and turns failures into protocol errors.

use std::ffi::OsStr;
use std::process::Output;
use std::time::Instant;
use tokio::process::Command;
use wp_common::protocol::StepReport;
use wp_common::{Error, Result};

#[derive(Debug, Clone, Default)]
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
    // Never log secrets: environment values were already excluded, but argv
    // carries passwords too (`--admin_password=`, `restic --password`,
    // `mysql -p'...'`). Redact before anything reaches the log or an error.
    let printable = redact_command(program, args);
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
    let output: Output = cmd.output().await.map_err(|e| Error::Command {
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

/// Argument names whose *value* is a secret, in `--name=value` or
/// `--name value` form.
const SECRET_FLAGS: [&str; 8] = [
    "--password",
    "--admin_password",
    "--dbpass",
    "--user_pass",
    "--secret",
    "--token",
    "-p",
    "--restic-password",
];

/// Builds the loggable form of a command with secret values masked.
pub fn redact_command<S: AsRef<OsStr>>(program: &str, args: &[S]) -> String {
    let raw: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut mask_next = false;

    for arg in raw {
        if mask_next {
            out.push("\"***\"".to_string());
            mask_next = false;
            continue;
        }

        // `--name=value`
        if let Some((name, _)) = arg.split_once('=')
            && SECRET_FLAGS.contains(&name)
        {
            out.push(format!("{name}=***"));
            continue;
        }

        // `--name value`
        if SECRET_FLAGS.contains(&arg.as_str()) {
            mask_next = true;
            out.push(format!("{arg:?}"));
            continue;
        }

        // Shell fragments passed to `sh -c` can embed `-p'secret'`.
        out.push(format!("{:?}", mask_inline_secrets(&arg)));
    }

    format!("{program} [{}]", out.join(", "))
}

/// Masks quoted secrets inside a single string: `-p'secret'`,
/// `--password='secret'` and `IDENTIFIED BY 'secret'`. Used for the `sh -c`
/// fragments and inline SQL the agent builds.
fn mask_inline_secrets(input: &str) -> String {
    const MARKERS: [&str; 4] = ["-p'", "--password='", "--dbpass='", "IDENTIFIED BY '"];

    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    'outer: while !rest.is_empty() {
        // Find the earliest marker in what is left.
        let next = MARKERS
            .iter()
            .filter_map(|m| rest.find(m).map(|at| (at, *m)))
            .min_by_key(|(at, _)| *at);

        let Some((at, marker)) = next else {
            out.push_str(rest);
            break;
        };

        out.push_str(&rest[..at + marker.len()]);
        rest = &rest[at + marker.len()..];

        match rest.find('\'') {
            Some(close) => {
                out.push_str("***");
                out.push('\'');
                rest = &rest[close + 1..];
            }
            None => {
                // Unterminated quote: drop the remainder rather than risk
                // logging a secret.
                out.push_str("***");
                break 'outer;
            }
        }
    }

    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_flag_value_pairs() {
        let logged = redact_command(
            "restic",
            &["--repo", "s3:bucket", "--password", "s3cret", "init"],
        );
        assert!(
            logged.contains("s3:bucket"),
            "non-secret args stay visible: {logged}"
        );
        assert!(!logged.contains("s3cret"), "secret leaked: {logged}");
    }

    #[test]
    fn masks_inline_assignments() {
        let logged = redact_command(
            "docker",
            &[
                "exec",
                "c",
                "wp",
                "core",
                "install",
                "--admin_password=hunter2",
                "--skip-email",
            ],
        );
        assert!(logged.contains("--admin_password=***"));
        assert!(!logged.contains("hunter2"));
        assert!(logged.contains("--skip-email"));
    }

    #[test]
    fn masks_secrets_inside_shell_fragments() {
        let logged = redact_command(
            "sh",
            &[
                "-c",
                "mysqldump -uwp -p'dbpass' wp | mysql -uwp -p'dbpass' wp2",
            ],
        );
        assert!(
            !logged.contains("dbpass"),
            "shell fragment leaked: {logged}"
        );
        assert!(logged.contains("mysqldump"));
    }

    #[test]
    fn masks_sql_identified_by() {
        let logged = redact_command(
            "mysql",
            &[
                "-e",
                "CREATE USER 'wp'@'localhost' IDENTIFIED BY 'topsecret';",
            ],
        );
        assert!(!logged.contains("topsecret"));
        assert!(logged.contains("CREATE USER"));
    }

    #[test]
    fn leaves_ordinary_commands_readable() {
        let logged = redact_command("nginx", &["-t"]);
        assert_eq!(logged, "nginx [\"-t\"]");
    }
}

/// Quotes a value for safe inclusion in a `sh -c` fragment.
///
/// Wraps in single quotes and escapes embedded single quotes the POSIX way
/// (`'\''`). Used where a pipeline genuinely needs a shell — copying a database
/// with `mysqldump | mysql` — so that a password containing `;` or `$` cannot
/// become a command.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod shell_quote_tests {
    use super::shell_quote;

    #[test]
    fn wraps_plain_values() {
        assert_eq!(shell_quote("simple"), "'simple'");
    }

    #[test]
    fn neutralises_shell_metacharacters() {
        assert_eq!(shell_quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(shell_quote("$(whoami)"), "'$(whoami)'");
        assert_eq!(shell_quote("back`tick`"), "'back`tick`'");
    }

    #[test]
    fn escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    /// The real invariant: a quoted value must survive `sh` unchanged, whatever
    /// it contains. Checking the quoting by eye (or by counting quotes) is how
    /// injection bugs get waved through.
    #[test]
    fn quoted_values_survive_the_shell_verbatim() {
        for value in [
            "'",
            "it's",
            "a;rm -rf /",
            "$(whoami)",
            "back`tick`",
            "double\"quote",
            "new\nline",
            "p@ssw0rd!#%&*",
        ] {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(value)))
                .output()
                .expect("run sh");

            assert!(output.status.success(), "sh rejected {value:?}");
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                value,
                "value was altered by the shell: {value:?}"
            );
        }
    }
}
