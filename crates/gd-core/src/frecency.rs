use std::time::{SystemTime, UNIX_EPOCH};

const AGING_THRESHOLD: f64 = 1000.0;
const AGING_FACTOR: f64 = 0.9;
const PRUNE_THRESHOLD: f64 = 1.0;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_secs()
}

pub fn decay_factor(elapsed_secs: u64) -> f64 {
    const HOUR: u64 = 3600;
    const DAY: u64 = 86400;
    const WEEK: u64 = 604_800;

    if elapsed_secs < HOUR {
        1.0
    } else if elapsed_secs < DAY {
        0.7
    } else if elapsed_secs < WEEK {
        0.5
    } else {
        0.25
    }
}

pub fn effective_score(raw_score: f64, last_access: u64, now: u64) -> f64 {
    let elapsed = now.saturating_sub(last_access);
    raw_score * decay_factor(elapsed)
}

pub fn should_age(total_score: f64) -> bool {
    total_score > AGING_THRESHOLD
}

pub fn age_score(score: f64) -> Option<f64> {
    let aged = score * AGING_FACTOR;
    if aged < PRUNE_THRESHOLD {
        None
    } else {
        Some(aged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_within_hour() {
        assert_eq!(decay_factor(0), 1.0);
        assert_eq!(decay_factor(3599), 1.0);
    }

    #[test]
    fn decay_within_day() {
        assert_eq!(decay_factor(3600), 0.7);
        assert_eq!(decay_factor(86399), 0.7);
    }

    #[test]
    fn decay_within_week() {
        assert_eq!(decay_factor(86400), 0.5);
        assert_eq!(decay_factor(604799), 0.5);
    }

    #[test]
    fn decay_over_week() {
        assert_eq!(decay_factor(604800), 0.25);
        assert_eq!(decay_factor(999_999_999), 0.25);
    }

    #[test]
    fn effective_score_applies_decay() {
        let now = 1_000_000;
        let last = now - 100; // within hour
        assert_eq!(effective_score(10.0, last, now), 10.0);

        let last = now - 7200; // within day
        assert!((effective_score(10.0, last, now) - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn aging_prunes_low_scores() {
        assert!(should_age(1001.0));
        assert!(!should_age(999.0));
        assert_eq!(age_score(0.5), None);
        assert_eq!(age_score(10.0), Some(9.0));
    }
}
