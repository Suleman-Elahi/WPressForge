//! Display helpers used by the panel templates. They live here so the agent can
//! reuse them in CLI output and so templates never need format filters.

use chrono::{DateTime, Utc};

/// Human byte sizes: `1.2 GB`, `840 MB`, `12 KB`.
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value, UNITS[unit])
    } else if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

/// Megabytes into the closest sensible unit: `512 MB`, `2 GB`.
pub fn megabytes(value: u64) -> String {
    if value >= 1024 {
        let gb = value as f64 / 1024.0;
        if (gb.fract() * 10.0).round() == 0.0 {
            format!("{gb:.0} GB")
        } else {
            format!("{gb:.1} GB")
        }
    } else {
        format!("{value} MB")
    }
}

pub fn percent(value: f32) -> String {
    format!("{:.0}%", value.clamp(0.0, 100.0))
}

/// Meter colour band for a utilisation percentage.
pub fn utilisation_tone(value: f32) -> &'static str {
    if value >= 90.0 {
        "bad"
    } else if value >= 75.0 {
        "warn"
    } else {
        "ok"
    }
}

/// `just now`, `4m ago`, `3h ago`, `12 Sep`.
pub fn relative(when: DateTime<Utc>) -> String {
    let delta = Utc::now().signed_duration_since(when);
    let secs = delta.num_seconds();

    match secs {
        s if s < 0 => "in the future".to_string(),
        s if s < 45 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 7 * 86_400 => format!("{}d ago", s / 86_400),
        _ => when.format("%-d %b %Y").to_string(),
    }
}

/// Absolute timestamp for tooltips and audit rows.
pub fn timestamp(when: DateTime<Utc>) -> String {
    when.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// `74 days` until a certificate expires, or `expired`.
pub fn until(when: DateTime<Utc>) -> String {
    let days = when.signed_duration_since(Utc::now()).num_days();
    if days < 0 {
        "expired".to_string()
    } else if days == 0 {
        "today".to_string()
    } else if days == 1 {
        "1 day".to_string()
    } else {
        format!("{days} days")
    }
}

pub fn duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}
