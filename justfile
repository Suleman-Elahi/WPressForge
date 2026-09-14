# Common tasks. `cargo install just` if you do not have it.

# Run the panel with demo data on http://127.0.0.1:8080
dev:
    WP_PANEL_ADMIN_PASSWORD=devpassword \
    WP_PANEL_LOG=debug,sqlx=warn,tower_http=info \
    cargo run -p wp-panel

# Run an agent locally in dry-run mode (logs commands, touches nothing).
agent:
    WP_AGENT_TOKEN=local-development-token-000001 \
    WP_AGENT_BIND=127.0.0.1:8443 \
    WP_AGENT_STATE=data/agent-state.json \
    WP_AGENT_DRY_RUN=true \
    cargo run -p wp-agent

check:
    cargo check --workspace --all-targets

fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Validate generated vhosts against the nginx installed on THIS machine.
# Run on every OS you intend to support.
test-nginx:
    cargo test -p wp-agent -- --ignored --nocapture nginx_accepts

release:
    cargo build --release --workspace

# Build the per-site PHP images the agent expects.
php-images:
    for v in 8.2 8.3 8.4; do \
        docker build --build-arg PHP_VERSION=$v -t wp-panel/php-fpm:$v deploy/docker/php-fpm; \
    done

# Wipe local state (panel database and agent index).
clean-data:
    rm -rf data
