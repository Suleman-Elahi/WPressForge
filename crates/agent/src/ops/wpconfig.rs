//! Reading database credentials out of a site's `wp-config.php`.
//!
//! Clone, staging and migration all need the *source* site's credentials, which
//! only exist in its `wp-config.php`. The file is parsed textually and never
//! executed: it is PHP, and evaluating it would run whatever a site owner put
//! there.
//!
//! An earlier version of `site::clone` passed an empty password to `mysqldump`,
//! so cloning only worked where socket authentication happened to apply.

use crate::config::Config;
use crate::store::SiteRecord;
use wp_common::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbConfig {
    pub name: String,
    pub user: String,
    pub password: String,
    pub host: String,
    pub prefix: String,
}

impl DbConfig {
    /// Host without the `:port` suffix WordPress allows.
    pub fn host_only(&self) -> &str {
        self.host.split(':').next().unwrap_or(&self.host)
    }

    /// Port from `host:port`, if one was given.
    pub fn port(&self) -> Option<&str> {
        self.host.split_once(':').map(|(_, port)| port)
    }
}

/// Reads and parses `<site root>/public_html/wp-config.php`.
///
/// In dry-run the file usually does not exist; the derived values let the rest
/// of a rehearsal proceed without pretending to have real credentials.
pub async fn read_db_config(config: &Config, site: &SiteRecord) -> Result<DbConfig> {
    let path = config
        .site_root(&site.domain)
        .join("public_html/wp-config.php");

    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => parse_db_config(&contents).ok_or_else(|| {
            Error::Invalid(format!(
                "could not find database constants in {}",
                path.display()
            ))
        }),
        Err(error) if config.dry_run => {
            tracing::info!(
                path = %path.display(),
                %error,
                "dry-run: wp-config.php not readable, using derived values"
            );
            Ok(DbConfig {
                name: site.db_name.clone(),
                user: crate::ops::filesystem::system_user(&site.domain),
                password: String::new(),
                host: "127.0.0.1".to_string(),
                prefix: "wp_".to_string(),
            })
        }
        Err(error) => Err(Error::Internal(format!(
            "reading {}: {error}",
            path.display()
        ))),
    }
}

/// Extracts the database constants from `wp-config.php` text.
///
/// Handles single and double quotes, `define()` and `define ()`, extra
/// whitespace, and `$table_prefix` in either quoting style. Commented-out lines
/// are skipped so a leftover example block cannot win over the live values.
pub fn parse_db_config(contents: &str) -> Option<DbConfig> {
    let name = find_define(contents, "DB_NAME")?;
    let user = find_define(contents, "DB_USER")?;
    let password = find_define(contents, "DB_PASSWORD").unwrap_or_default();
    let host = find_define(contents, "DB_HOST").unwrap_or_else(|| "localhost".to_string());
    let prefix = find_table_prefix(contents).unwrap_or_else(|| "wp_".to_string());

    Some(DbConfig {
        name,
        user,
        password,
        host,
        prefix,
    })
}

/// `define('NAME', 'value');` -> `value`, for the first uncommented occurrence.
fn find_define(contents: &str, constant: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*') {
            continue;
        }
        if !trimmed.contains("define") || !trimmed.contains(constant) {
            continue;
        }

        // Take everything after the constant name, then the next quoted string.
        let after = trimmed.split_once(constant)?.1;
        let after = after.trim_start().trim_start_matches([')', '\'', '"']);
        let after = after.trim_start().trim_start_matches(',').trim_start();

        if let Some(value) = first_quoted(after) {
            return Some(value);
        }
    }
    None
}

/// `$table_prefix = 'wp_';` -> `wp_`
fn find_table_prefix(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        if let Some(after) = trimmed.strip_prefix("$table_prefix") {
            let after = after.trim_start().strip_prefix('=')?.trim_start();
            return first_quoted(after);
        }
    }
    None
}

/// Reads the first single- or double-quoted string, honouring `\'` escapes.
fn first_quoted(input: &str) -> Option<String> {
    let mut chars = input.char_indices();
    let (_, quote) = chars.find(|(_, c)| *c == '\'' || *c == '"')?;

    let mut value = String::new();
    let mut escaped = false;

    for (_, c) in chars {
        if escaped {
            value.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            c if c == quote => return Some(value),
            c => value.push(c),
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPICAL: &str = r#"<?php
// ** Database settings ** //
/** The name of the database for WordPress */
define( 'DB_NAME', 'wp_example_com' );

/** Database username */
define( 'DB_USER', 'wp_example_com' );

/** Database password */
define( 'DB_PASSWORD', 'sup3r-s3cret!' );

/** Database hostname */
define( 'DB_HOST', '127.0.0.1:3307' );

$table_prefix = 'wp_abc123_';
"#;

    #[test]
    fn parses_a_typical_wp_config() {
        let parsed = parse_db_config(TYPICAL).expect("parsed");

        assert_eq!(parsed.name, "wp_example_com");
        assert_eq!(parsed.user, "wp_example_com");
        assert_eq!(parsed.password, "sup3r-s3cret!");
        assert_eq!(parsed.host, "127.0.0.1:3307");
        assert_eq!(parsed.host_only(), "127.0.0.1");
        assert_eq!(parsed.port(), Some("3307"));
        assert_eq!(parsed.prefix, "wp_abc123_");
    }

    #[test]
    fn handles_double_quotes_and_tight_spacing() {
        let config = r#"<?php
define("DB_NAME","tight");
define("DB_USER","user2");
define("DB_PASSWORD","pw2");
$table_prefix="x_";
"#;
        let parsed = parse_db_config(config).expect("parsed");

        assert_eq!(parsed.name, "tight");
        assert_eq!(parsed.user, "user2");
        assert_eq!(parsed.password, "pw2");
        assert_eq!(parsed.prefix, "x_");
        // No DB_HOST given: WordPress's own default.
        assert_eq!(parsed.host, "localhost");
    }

    #[test]
    fn keeps_escaped_quotes_in_passwords() {
        let config = r#"<?php
define('DB_NAME', 'db');
define('DB_USER', 'u');
define('DB_PASSWORD', 'it\'s a "quote"');
"#;
        let parsed = parse_db_config(config).expect("parsed");
        assert_eq!(parsed.password, "it's a \"quote\"");
    }

    #[test]
    fn ignores_commented_out_values() {
        let config = r#"<?php
// define('DB_NAME', 'old_database');
# define('DB_USER', 'old_user');
define('DB_NAME', 'real_database');
define('DB_USER', 'real_user');
define('DB_PASSWORD', 'pw');
"#;
        let parsed = parse_db_config(config).expect("parsed");
        assert_eq!(parsed.name, "real_database");
        assert_eq!(parsed.user, "real_user");
    }

    #[test]
    fn returns_none_without_the_required_constants() {
        assert!(parse_db_config("<?php // nothing here").is_none());
        assert!(parse_db_config("<?php define('DB_NAME', 'only_name');").is_none());
    }
}
