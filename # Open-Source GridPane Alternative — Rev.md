# Open-Source GridPane Alternative — Revised Architecture Plan

## 1. Project Goal

Build an open-source control panel for managing WordPress sites on Linux servers.

The system should make it possible to:

* Provision WordPress sites
* Manage domains and DNS-related configuration
* Automatically configure Nginx
* Automatically issue/renew SSL certificates
* Support multiple PHP versions
* Isolate sites
* Manage databases
* Configure caching
* Create/restore backups
* Clone sites
* Create staging environments
* Manage WordPress through WP-CLI
* Monitor sites and servers
* Apply resource limits
* Manage multiple servers from one dashboard

The long-term goal is:

```text
                 WP Panel
                    │
       ┌────────────┼────────────┐
       │            │            │
    Server 1     Server 2     Server 3
       │            │            │
    Agent        Agent        Agent
       │            │            │
   ┌───┼───┐    ┌───┼───┐    ┌───┼───┐
   WP1 WP2 WP3   WP4 WP5 WP6   WP7 WP8 WP9
```

The panel is the **control plane**.

Each server runs a lightweight **agent**.

---

# 2. Core Architecture

I recommend four major components.

```text
┌─────────────────────────────────────────────┐
│                 WP Panel                    │
│                                             │
│  Web UI + API + Authentication + Database   │
└──────────────────────┬──────────────────────┘
                       │
                 Secure connection
                       │
        ┌──────────────┴──────────────┐
        │                             │
┌───────▼────────┐             ┌──────▼────────┐
│   WP Agent     │             │   WP Agent    │
│   Server 1     │             │   Server 2    │
└───────┬────────┘             └──────┬─────────┘
        │                             │
        ▼                             ▼
   Docker/Nginx                  Docker/Nginx
        │                             │
    WordPress                    WordPress
```

### Components

### A. Control Panel

Written in:

```text
Rust
├── Axum
├── SQLx
├── Askama
├── HTMX
└── Tailwind
```

Responsible for:

* UI
* API
* authentication
* users
* servers
* sites
* jobs
* configuration
* scheduling
* audit logs

### B. Server Agent

Rust daemon running as a system service.

Responsible for privileged operations:

* Docker
* Nginx
* filesystem
* SSL
* databases
* users
* backups
* WP-CLI

### C. Worker

The panel should have a job/worker system.

For example:

```text
Create Site
     │
     ▼
Job #1024
     │
     ├── Create filesystem
     ├── Create user
     ├── Create database
     ├── Create PHP container
     ├── Configure Nginx
     ├── Configure SSL
     ├── Install WordPress
     └── Health check
```

### D. WordPress Site Containers

Each site gets its own PHP-FPM container.

```text
wp-example-com
├── PHP-FPM
├── resource limits
└── mounted WordPress files
```

---

# 3. Why Agent Architecture?

The original design put the entire control system directly on the server.

Instead:

```text
Internet
   │
   ▼
Panel
   │
   │ authenticated API
   ▼
Agent
   │
   ├── Docker
   ├── Nginx
   ├── MariaDB
   ├── filesystem
   └── SSL
```

This provides several advantages.

### Security

The web-facing panel doesn't need unrestricted root access.

### Multi-server

One panel can eventually manage:

```text
Hetzner Server A
Hetzner Server B
DigitalOcean Server
AWS Server
Vultr Server
```

### Open-source architecture

People can install:

```text
Panel
```

on one machine and:

```text
Agent
```

on their own servers.

---

# 4. Server Architecture

Each managed server looks approximately like this:

```text
Linux Server
│
├── Nginx
│
├── WP Agent
│
├── Docker
│
├── MariaDB
│
├── Restic
│
├── SSL certificates
│
└── /var/www/
     │
     ├── example.com/
     │    ├── public_html/
     │    ├── logs/
     │    └── backups/
     │
     ├── example2.com/
     │    ├── public_html/
     │    ├── logs/
     │    └── backups/
     │
     └── example3.com/
```

---

# 5. WordPress Site Isolation

Every site gets a dedicated Linux UID.

Example:

```text
example.com
UID: 10001

example2.com
UID: 10002

example3.com
UID: 10003
```

Filesystem:

```text
/var/www/example.com
    owner 10001:10001

/var/www/example2.com
    owner 10002:10002
```

This prevents one WordPress installation from directly modifying another site's files.

---

# 6. PHP Architecture

Each site gets its own PHP-FPM container.

Example:

```text
example.com
    PHP 8.3
    1 CPU
    1 GB RAM

example2.com
    PHP 8.4
    2 CPU
    2 GB RAM
```

Docker provides:

* CPU limits
* memory limits
* process isolation
* restart policies

---

# 7. PHP Version Switching

Do **not** stop the old container first.

Instead:

```text
Current

Nginx
  │
  ▼
PHP 8.3
```

When upgrading:

```text
              ┌── PHP 8.3
Nginx ────────┤
              └── PHP 8.4
```

Process:

```text
1. Create PHP 8.4 container
2. Mount same site
3. Start PHP-FPM
4. Health check
5. Test PHP
6. Switch Nginx
7. Verify traffic
8. Remove PHP 8.3
```

This allows near-zero-downtime PHP upgrades.

---

# 8. Database Architecture

I would support **two modes**.

### Mode A — Shared database server

```text
MariaDB
│
├── site1_db
├── site2_db
├── site3_db
└── site4_db
```

Each site gets:

```text
database
database user
database password
```

with permissions restricted to its database.

This is efficient for normal hosting.

### Mode B — Dedicated database

For high-value or isolated installations:

```text
Site
├── PHP container
└── MariaDB container
```

The UI could simply provide:

```text
Database isolation

○ Shared database server
○ Dedicated database container
```

---

# 9. Nginx

Nginx remains on the host.

```text
Internet
   │
   ▼
Nginx
   │
   ├── example.com
   ├── example2.com
   └── example3.com
```

Nginx handles:

* HTTP
* HTTPS
* TLS
* redirects
* static files
* PHP routing
* compression
* caching
* rate limiting
* security rules

The agent generates configurations.

Example:

```text
/etc/nginx/sites-enabled/
├── example.com.conf
├── example2.com.conf
└── example3.com.conf
```

---

# 10. SSL

The panel should automatically manage Let's Encrypt.

Workflow:

```text
Add domain
     ↓
DNS points to server
     ↓
Agent verifies domain
     ↓
Request certificate
     ↓
Install certificate
     ↓
Configure Nginx
     ↓
Automatic renewal
```

Eventually support:

* Let's Encrypt
* custom certificates
* wildcard certificates
* Cloudflare DNS challenge

---

# 11. WordPress Cache

Use Nginx FastCGI caching.

```text
Browser
   │
   ▼
Nginx
   │
   ├── Cache HIT → return immediately
   │
   └── Cache MISS
           ↓
        PHP-FPM
```

But caching needs to be **WordPress-aware**.

Automatically bypass cache for:

```text
/wp-admin
/wp-login.php
logged-in users
WooCommerce cart
WooCommerce checkout
POST requests
certain cookies
```

The panel should eventually expose:

```text
Caching

[✓] FastCGI cache

Cache duration: 1 hour

[Clear Cache]
```

---

# 12. Jobs System

This is a core component, not an afterthought.

Database:

```sql
jobs
-------------------------
id
server_id
site_id
type
status
progress
message
started_at
finished_at
error
created_at
```

Possible jobs:

```text
site.create
site.delete
site.clone
site.migrate

php.install
php.switch

wordpress.install
wordpress.update

plugin.update
theme.update

backup.create
backup.restore

ssl.issue
ssl.renew

cache.clear
```

UI:

```text
Backup example.com

██████████████░░░░ 78%

Uploading backup...

[View Logs]
```

---

# 13. Backup Architecture

Use:

```text
Restic
```

for encrypted, deduplicated backups.

Backup contains:

```text
WordPress files
+
Database dump
+
Site configuration
```

Destination:

```text
S3
Cloudflare R2
Backblaze B2
Wasabi
MinIO
```

The user configures:

```text
Backup destination

Provider: S3
Bucket: wordpress-backups
Region: ...
```

---

# 14. Backup Retention

Support policies such as:

```text
Hourly:   24
Daily:     14
Weekly:    8
Monthly:   12
```

Eventually expose:

```text
Backups

Today
  14:00   1.2 GB
  13:00   1.1 GB
  12:00   1.1 GB

Yesterday
  23:00   1.1 GB

[Restore]
[Download]
[Delete]
```

---

# 15. Restore

Restore should be a proper job.

```text
Select backup
      ↓
[Restore]
      ↓
Confirmation
      ↓
Database restore
      ↓
Filesystem restore
      ↓
Permissions
      ↓
PHP/Nginx verification
      ↓
Health check
```

Ideally support:

```text
Restore entire site

Restore database only

Restore files only
```

---

# 16. WordPress Management

The panel should integrate WP-CLI.

For example:

```text
wp core version
wp plugin list
wp plugin update
wp theme list
wp option get
wp cache flush
```

The agent executes WP-CLI **inside the site's environment**.

This gives the panel a huge amount of WordPress functionality without reinventing everything.

---

# 17. Site Dashboard

The main site page should eventually look like:

```text
example.com
────────────────────────────────

● Online

PHP             8.4
WordPress       6.x
Database        MariaDB
SSL             Valid
Cache           Enabled

CPU             12%
RAM             423 MB
Disk            3.2 GB

────────────────────────────────

[Clear Cache] [Backup] [Clone]

Overview
Domains
WordPress
PHP
Database
SSL
Backups
Staging
Logs
Security
Cron
Files
Settings
```

---

# 18. Staging

This should come after the core MVP.

Concept:

```text
Production
example.com
     │
     │ Clone
     ▼
Staging
staging.example.com
```

Then:

```text
Staging
   │
   │ Push
   ▼
Production
```

Eventually support:

```text
Production → Staging
Staging → Production
```

with database search/replace.

---

# 19. Site Cloning

One of the most useful features.

```text
example.com

[Clone]

Target:
newsite.com

PHP:
8.4

Database:
New database

SSL:
Yes
```

The agent:

```text
copy files
    ↓
copy database
    ↓
search/replace URLs
    ↓
configure Nginx
    ↓
configure SSL
    ↓
health check
```

---

# 20. Migration

Eventually allow:

```text
Import Existing WordPress
```

Methods:

```text
SSH
SFTP
rsync
backup archive
```

Migration workflow:

```text
Source Server
      │
      │ files + DB
      ▼
New Server
      │
      ▼
Configure site
      │
      ▼
Test
      │
      ▼
DNS switch
```

---

# 21. Server Dashboard

For every server:

```text
SERVER-01

CPU              32%
RAM              41%
Disk             58%

Sites             47
Containers        49

Nginx             ●
Docker            ●
MariaDB           ●
Agent             ●

PHP versions

8.2                3
8.3               31
8.4               13
```

---

# 22. Security

The panel should have:

### Authentication

* username/password
* session authentication
* optional 2FA
* API tokens

### Agent authentication

Use public/private keys or mutually authenticated TLS.

```text
Panel
  │
  │ authenticated
  ▼
Agent
```

Never allow an unauthenticated agent API.

### Audit log

Record:

```text
User
Action
Site
Server
Timestamp
Result
```

Example:

```text
Suleman
Changed PHP
example.com
8.3 → 8.4
2026-09-11 17:32
Success
```

---

# 23. Panel Database

Initially:

```text
SQLite
```

for the control panel.

Tables:

```text
users
servers
sites
domains
databases
containers
backups
jobs
certificates
settings
audit_logs
```

Later, if the panel becomes a SaaS/multi-user system:

```text
PostgreSQL
```

can become the recommended production database.

---

# 24. Recommended Rust Project Structure

```text
wp-panel/
│
├── crates/
│   │
│   ├── panel/
│   │   ├── api/
│   │   ├── auth/
│   │   ├── database/
│   │   ├── jobs/
│   │   └── web/
│   │
│   ├── agent/
│   │   ├── docker/
│   │   ├── nginx/
│   │   ├── wordpress/
│   │   ├── database/
│   │   ├── ssl/
│   │   ├── backup/
│   │   └── filesystem/
│   │
│   └── common/
│       ├── models/
│       └── protocol/
│
├── templates/
├── migrations/
├── static/
└── Cargo.toml
```

---

# 25. Communication Protocol

Don't have the panel execute arbitrary shell commands on the server.

Instead define operations.

For example:

```json
{
  "operation": "create_site",
  "site_id": 123,
  "domain": "example.com",
  "php_version": "8.4"
}
```

Agent returns:

```json
{
  "success": true,
  "site_id": 123,
  "container_id": "abc123"
}
```

This gives you a clean API between control plane and server.

---

# 26. MVP

**Do not build everything above initially.**

The first version should only solve:

```text
SERVER
│
├── Register server
│
└── SITE
    ├── Create
    ├── Delete
    ├── Start
    ├── Stop
    ├── Restart
    │
    ├── Domain
    ├── SSL
    ├── PHP
    ├── MariaDB
    ├── WordPress
    │
    ├── Backup
    ├── Restore
    │
    └── Logs
```

The first milestone should be:

> **I can take a completely clean Ubuntu server and create a working HTTPS WordPress site from the panel.**

For example:

```text
Click "Add Site"

Domain:
example.com

PHP:
8.4

RAM:
1 GB

CPU:
1

Database:
Shared

Backup:
Daily

[Create Site]
```

Then the system automatically does:

```text
Create UID
     ↓
Create filesystem
     ↓
Create database
     ↓
Create PHP container
     ↓
Install WordPress
     ↓
Generate Nginx config
     ↓
Issue SSL
     ↓
Enable cache
     ↓
Health check
     ↓
ONLINE
```

**That is the first thing I'd build.**

---

# 27. Development Phases

### Phase 1 — Server Agent

Build:

```text
wp-agent
```

and make these operations work reliably:

```text
create_site
delete_site
start_site
stop_site
restart_site
get_site_status
```

### Phase 2 — WordPress lifecycle

```text
create WordPress
PHP versions
database
Nginx
SSL
WP-CLI
```

### Phase 3 — Panel

```text
Login
Servers
Sites
Site dashboard
Jobs
Logs
```

### Phase 4 — Backup

```text
Restic
S3/R2
scheduled backups
retention
restore
```

### Phase 5 — Performance

```text
FastCGI cache
Redis
OPcache
compression
static asset optimization
```

### Phase 6 — WordPress features

```text
Plugin management
Theme management
WP updates
WP-CLI terminal
Cron
Search/replace
```

### Phase 7 — Hosting features

```text
Staging
Cloning
Migration
Multisite
Multiple domains
Redirects
```

### Phase 8 — Multi-server

```text
Server provisioning
Server monitoring
Centralized management
Server templates
```

---

# 28. What I Would NOT Build Initially

Avoid these in v1:

```text
❌ Kubernetes
❌ Kubernetes-like orchestration
❌ Custom database engine
❌ Custom container runtime
❌ React SPA
❌ Custom PHP runtime
❌ Custom backup format
❌ Custom WordPress update system
❌ Distributed database
❌ Raft
❌ Microservices everywhere
```

Use existing battle-tested components:

```text
Docker       → containers
Nginx        → web server
MariaDB      → database
Let's Encrypt → SSL
Restic       → backups
WP-CLI       → WordPress operations
SQLite       → panel state
Rust         → orchestration
HTMX         → UI
```

That keeps the project manageable.

---

# 29. Final Architecture

The final vision would be:

```text
                         INTERNET
                            │
                            ▼
                    ┌───────────────┐
                    │   WP PANEL    │
                    │               │
                    │ Rust/Axum     │
                    │ HTMX          │
                    │ SQLite/PG     │
                    │ Jobs          │
                    └───────┬───────┘
                            │
              ┌─────────────┼─────────────┐
              │             │             │
              ▼             ▼             ▼
        ┌──────────┐  ┌──────────┐  ┌──────────┐
        │ Server 1 │  │ Server 2 │  │ Server 3 │
        │  Agent   │  │  Agent   │  │  Agent   │
        └────┬─────┘  └────┬─────┘  └────┬─────┘
             │             │             │
       ┌─────┼─────┐ ┌─────┼─────┐ ┌─────┼─────┐
       │     │     │ │     │     │ │     │     │
      WP1   WP2   WP3 WP4  WP5   WP6 WP7  WP8   WP9
       │
    ┌──┴─────────────┐
    │                │
 PHP-FPM          MariaDB
 Docker           Shared/Dedicated
    │
    ▼
 WordPress
```

### The key philosophy

**Don't build GridPane. Build the infrastructure underneath GridPane's feature set, but make it open, modular, and self-hosted.**

Your first target shouldn't be "support 100 features."

It should be:

> **One command/click → production-ready WordPress site.**

Once that lifecycle is rock-solid, you can progressively add staging, migrations, cloning, monitoring, teams, multi-server management, and eventually a full GridPane-level feature set.
