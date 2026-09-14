//! Must-use plugin that hooks into WordPress to automatically purge the Nginx
//! FastCGI cache when content changes. The plugin is written to
//! `wp-content/mu-plugins/wp-panel-cache.php` during site creation and
//! whenever cache settings change.
//!
//! Requires `ngx_cache_purge` in Nginx. When absent, the purge location is not
//! emitted and WordPress still works — the hooks simply have no effect.

use crate::config::Config;
use crate::store::SiteRecord;
use wp_common::Result;

/// Content of the must-use plugin. Pure function — unit testable, no I/O.
pub fn mu_plugin_php(_domain: &str) -> String {
    format!(
        r#"<?php
/**
 * Plugin Name: WP Panel Cache
 * Description: Automatically purges the Nginx FastCGI cache when content changes.
 * Version: 1.0.0
 * Author: WP Panel
 *
 * This is a must-use plugin managed by wp-panel. Do not edit or remove.
 * It hooks into WordPress save/delete events and purges the Nginx cache
 * via the /wp-panel-purge location (requires ngx_cache_purge module).
 */

if (!defined('ABSPATH')) {{
    exit;
}}

/**
 * Purge a single URL from the Nginx FastCGI cache.
 *
 * @param string $url The full URL to purge.
 */
function wp_panel_purge_url($url) {{
    $path = wp_parse_url($url, PHP_URL_PATH);
    if (empty($path)) {{
        return;
    }}

    $host = wp_parse_url($url, PHP_URL_HOST);
    if (empty($host)) {{
        return;
    }}

    $purge_url = 'http://127.0.0.1/wp-panel-purge' . $path;

    // Use a short timeout so WordPress doesn't slow down.
    $args = array(
        'timeout' => 2,
        'blocking' => false,
        'headers' => array('Host' => $host),
    );

    wp_remote_get($purge_url, $args);
}}

/**
 * Purge a post and its related URLs (archives, feeds, home page).
 *
 * @param int $post_id The post ID.
 */
function wp_panel_purge_post($post_id) {{
    // Skip revisions and auto-drafts.
    if (wp_is_post_revision($post_id) || wp_is_post_autosave($post_id)) {{
        return;
    }}

    $post = get_post($post_id);
    if (!$post || $post->post_status !== 'publish') {{
        return;
    }}

    $home_url = home_url('/');
    wp_panel_purge_url($home_url);

    // Permalink of the post.
    $permalink = get_permalink($post_id);
    if ($permalink) {{
        wp_panel_purge_url($permalink);
    }}

    // Post type archives.
    $post_type = get_post_type($post_id);
    $archive_url = get_post_type_archive_link($post_type);
    if ($archive_url) {{
        wp_panel_purge_url($archive_url);
    }}

    // Category and tag archives.
    $categories = get_the_category($post_id);
    if (!is_wp_error($categories)) {{
        foreach ($categories as $cat) {{
            $cat_url = get_category_link($cat->term_id);
            if ($cat_url) {{
                wp_panel_purge_url($cat_url);
            }}
        }}
    }}

    $tags = get_the_tags($post_id);
    if ($tags) {{
        foreach ($tags as $tag) {{
            $tag_url = get_tag_link($tag->term_id);
            if ($tag_url) {{
                wp_panel_purge_url($tag_url);
            }}
        }}
    }}

    // Feed URLs.
    $feed_url = get_post_comments_feed_link($post_id);
    if ($feed_url) {{
        wp_panel_purge_url($feed_url);
    }}

    // REST and sitemap entries.
    $rest_url = rest_url('wp/v2/posts/' . $post_id);
    if ($rest_url) {{
        wp_panel_purge_url($rest_url);
    }}

    $sitemap_url = home_url('/wp-sitemap.xml');
    wp_panel_purge_url($sitemap_url);
}}

/**
 * Purge all term-related URLs when a term is edited.
 *
 * @param int $term_id The term ID.
 * @param string $taxonomy The taxonomy slug.
 */
function wp_panel_purge_term($term_id, $taxonomy) {{
    $term_url = get_term_link($term_id, $taxonomy);
    if (!is_wp_error($term_url)) {{
        wp_panel_purge_url($term_url);
    }}

    $home_url = home_url('/');
    wp_panel_purge_url($home_url);

    $sitemap_url = home_url('/wp-sitemap.xml');
    wp_panel_purge_url($sitemap_url);
}}

/**
 * Purge the home page on theme switch.
 */
function wp_panel_purge_theme_switch() {{
    $home_url = home_url('/');
    wp_panel_purge_url($home_url);

    $sitemap_url = home_url('/wp-sitemap.xml');
    wp_panel_purge_url($sitemap_url);
}}

// --- Hooks ----------------------------------------------------------------

// Post operations.
add_action('save_post', 'wp_panel_purge_post', 20, 3);
add_action('delete_post', function ($post_id) {{
    wp_panel_purge_post($post_id);
}});

// Comment operations — purge the parent post.
add_action('comment_post', function ($comment_id, $comment_approved, $commentdata) {{
    if (!empty($commentdata['comment_post_ID'])) {{
        wp_panel_purge_post($commentdata['comment_post_ID']);
    }}
}}, 20, 3);

add_action('wp_set_comment_status', function ($comment_id, $status) {{
    $comment = get_comment($comment_id);
    if ($comment && !empty($comment->comment_post_ID)) {{
        wp_panel_purge_post($comment->comment_post_ID);
    }}
}}, 20, 2);

// Term operations.
add_action('edited_term', 'wp_panel_purge_term', 20, 2);
add_action('create_term', 'wp_panel_purge_term', 20, 2);
add_action('delete_term', 'wp_panel_purge_term', 20, 2);

// Theme switch.
add_action('switch_theme', 'wp_panel_purge_theme_switch');
"#
    )
}

/// Write the mu-plugin to the site's `wp-content/mu-plugins/` directory.
pub async fn write_mu_plugin(config: &Config, site: &SiteRecord) -> Result<()> {
    let path = config
        .site_root(&site.domain)
        .join("public_html")
        .join("wp-content")
        .join("mu-plugins")
        .join("wp-panel-cache.php");

    if config.dry_run {
        tracing::info!(
            path = %path.display(),
            "dry-run: would write mu-plugin"
        );
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(wp_common::Error::internal)?;
    }

    tokio::fs::write(&path, mu_plugin_php(&site.domain))
        .await
        .map_err(wp_common::Error::internal)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mu_plugin_contains_required_hooks() {
        let php = mu_plugin_php("example.com");
        assert!(php.contains("add_action('save_post'"));
        assert!(php.contains("add_action('delete_post'"));
        assert!(php.contains("add_action('comment_post'"));
        assert!(php.contains("add_action('wp_set_comment_status'"));
        assert!(php.contains("add_action('edited_term'"));
        assert!(php.contains("add_action('switch_theme'"));
    }

    #[test]
    fn mu_plugin_has_correct_purge_url() {
        let php = mu_plugin_php("example.com");
        assert!(php.contains("http://127.0.0.1/wp-panel-purge"));
    }

    #[test]
    fn mu_plugin_has_plugin_header() {
        let php = mu_plugin_php("example.com");
        assert!(php.contains("Plugin Name: WP Panel Cache"));
        assert!(php.contains("Version: 1.0.0"));
    }
}
