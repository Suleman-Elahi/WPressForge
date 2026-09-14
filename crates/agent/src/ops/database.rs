//! MariaDB provisioning. Shared mode creates a schema and a user restricted to
//! it; dedicated mode gives the site its own container.

use crate::config::Config;
use crate::exec;
use crate::ops::filesystem::system_user;
use crate::store::SiteRecord;
use rand::Rng;
use wp_common::Result;
use wp_common::models::DatabaseMode;

pub struct Credentials {
    pub name: String,
    pub user: String,
    pub password: String,
    pub host: String,
}

/// 24 characters from an alphabet that survives shell quoting and wp-config.
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

pub async fn create(config: &Config, site: &SiteRecord) -> Result<Credentials> {
    let name = db_name(&site.domain);
    let user = system_user(&site.domain);
    let password = generate_password();

    let host = match site.database_mode {
        DatabaseMode::Shared => "127.0.0.1".to_string(),
        DatabaseMode::Dedicated => {
            start_dedicated(config, site, &name, &user, &password).await?;
            format!("{}-db", site.container_name())
        }
    };

    if site.database_mode == DatabaseMode::Shared {
        // Statements are parameter-free DDL built from sanitised identifiers.
        let sql = format!(
            "CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci; \
             CREATE USER IF NOT EXISTS '{user}'@'localhost' IDENTIFIED BY '{password}'; \
             GRANT ALL PRIVILEGES ON `{name}`.* TO '{user}'@'localhost'; \
             FLUSH PRIVILEGES;"
        );
        exec::run(config.dry_run, "mysql", &["--protocol=socket", "-e", &sql]).await?;
    }

    Ok(Credentials {
        name,
        user,
        password,
        host,
    })
}

async fn start_dedicated(
    config: &Config,
    site: &SiteRecord,
    name: &str,
    user: &str,
    password: &str,
) -> Result<()> {
    let container = format!("{}-db", site.container_name());
    let data = config.site_root(&site.domain).join("mysql");

    exec::run(
        config.dry_run,
        "docker",
        &[
            "run".to_string(),
            "--detach".to_string(),
            "--restart".to_string(),
            "unless-stopped".to_string(),
            "--name".to_string(),
            container,
            "--memory".to_string(),
            "512m".to_string(),
            "--volume".to_string(),
            format!("{}:/var/lib/mysql", data.display()),
            "--env".to_string(),
            format!("MARIADB_DATABASE={name}"),
            "--env".to_string(),
            format!("MARIADB_USER={user}"),
            "--env".to_string(),
            format!("MARIADB_PASSWORD={password}"),
            "--env".to_string(),
            "MARIADB_RANDOM_ROOT_PASSWORD=yes".to_string(),
            "--label".to_string(),
            format!("wp-panel.site={}", site.domain),
            "mariadb:11.4".to_string(),
        ],
    )
    .await
    .map(|_| ())
}

pub async fn drop(config: &Config, site: &SiteRecord) -> Result<()> {
    match site.database_mode {
        DatabaseMode::Shared => {
            let name = db_name(&site.domain);
            let user = system_user(&site.domain);
            let sql = format!(
                "DROP DATABASE IF EXISTS `{name}`; DROP USER IF EXISTS '{user}'@'localhost';"
            );
            exec::run(config.dry_run, "mysql", &["--protocol=socket", "-e", &sql])
                .await
                .map(|_| ())
        }
        DatabaseMode::Dedicated => {
            let container = format!("{}-db", site.container_name());
            crate::ops::docker::remove(config, &container).await
        }
    }
}

/// Writes a gzipped dump into the site's backups directory.
pub async fn dump(config: &Config, site: &SiteRecord) -> Result<String> {
    let target = config
        .site_root(&site.domain)
        .join("backups/database.sql")
        .display()
        .to_string();

    exec::run(
        config.dry_run,
        "sh",
        &[
            "-c".to_string(),
            format!(
                "mysqldump --protocol=socket --single-transaction --quick '{}' > '{}'",
                db_name(&site.domain),
                target
            ),
        ],
    )
    .await?;

    Ok(target)
}

pub async fn import(config: &Config, site: &SiteRecord, path: &str) -> Result<()> {
    exec::run(
        config.dry_run,
        "sh",
        &[
            "-c".to_string(),
            format!(
                "mysql --protocol=socket '{}' < '{}'",
                db_name(&site.domain),
                path
            ),
        ],
    )
    .await
    .map(|_| ())
}

pub fn db_name(domain: &str) -> String {
    system_user(domain)
}
