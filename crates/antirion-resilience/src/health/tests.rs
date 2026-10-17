use serde_json::json;

use super::*;
use crate::settings::HealthConfig;

fn settings(v: serde_json::Value) -> HealthSettings {
    HealthConfig::from_doc(&json!({"resilience": {"health": v}}))
        .for_deployment("d")
        .clone()
}

fn at(secs: f64) -> UtcMicros {
    UtcMicros((secs * 1e6) as i64 + 1_000_000_000_000)
}

fn verdict(class: Class, status: Option<u16>) -> Verdict {
    Verdict {
        class,
        status,
        hint: None,
        counts: true,
    }
}

fn run(r: &HealthRegistry, s: &HealthSettings, now: UtcMicros, class: Class) {
    let t = r.begin("d", "p", None, s, now).unwrap();
    r.finish(&t, &verdict(class, None), s, now);
}

#[test]
fn consecutive_failures_trip_and_successes_reset() {
    let s = settings(json!({"breaker": {"consecutive_failures": 3}}));
    let r = HealthRegistry::default();
    for c in [
        Class::ServerError,
        Class::ServerError,
        Class::Success,
        Class::ServerError,
        Class::ServerError,
    ] {
        run(&r, &s, at(0.0), c);
    }
    assert_eq!(r.view("d", "p", &[], &s, at(0.0)).state, "closed");
    run(&r, &s, at(0.0), Class::CallerError);
    run(&r, &s, at(0.0), Class::ServerError);
    let v = r.view("d", "p", &[], &s, at(0.0));
    assert_eq!((v.state, v.trips, v.consecutive), ("open", 1, 3));
    assert!(r.status("d", "p", &[], &s, at(1.0)).is_err());
}

#[test]
fn half_open_admits_one_probe_and_reopens_for_longer() {
    let s = settings(
        json!({"breaker": {"consecutive_failures": 1, "open_duration": "1s", "max_open_duration": "8s"}}),
    );
    let r = HealthRegistry::default();
    run(&r, &s, at(0.0), Class::ServerError);
    assert!(r.status("d", "p", &[], &s, at(0.9)).is_err());
    let probe = r.begin("d", "p", None, &s, at(1.1)).unwrap();
    assert!(r.begin("d", "p", None, &s, at(1.1)).is_none());
    assert!(r.status("d", "p", &[], &s, at(1.1)).is_err());
    r.finish(&probe, &verdict(Class::ServerError, Some(500)), &s, at(1.1));
    let v = r.view("d", "p", &[], &s, at(1.1));
    assert_eq!((v.state, v.trips), ("open", 2));
    assert!(r.status("d", "p", &[], &s, at(3.0)).is_err());
    let probe = r.begin("d", "p", None, &s, at(3.2)).unwrap();
    r.finish(&probe, &verdict(Class::Success, Some(200)), &s, at(3.2));
    assert_eq!(r.view("d", "p", &[], &s, at(3.2)).state, "closed");
}

#[test]
fn error_rate_needs_the_floor() {
    let s = settings(
        json!({"breaker": {"consecutive_failures": "none", "min_requests": 10, "failure_rate": 0.5, "window": "10s"}}),
    );
    let r = HealthRegistry::default();
    for i in 0..9 {
        run(
            &r,
            &s,
            at(0.0),
            if i % 2 == 0 {
                Class::Success
            } else {
                Class::ServerError
            },
        );
    }
    let v = r.view("d", "p", &[], &s, at(0.0));
    assert_eq!((v.state, v.window_requests, v.window_failures), ("closed", 9, 4));
    run(&r, &s, at(0.0), Class::ServerError);
    assert_eq!(r.view("d", "p", &[], &s, at(0.0)).state, "open");
}

#[test]
fn keys_rotate_skip_dead_ones_and_recheck() {
    let s = settings(json!({"dead_key_recheck": "1s"}));
    let r = HealthRegistry::default();
    let keys: Vec<KeyInfo> = ["k1", "k2", "k3"]
        .iter()
        .map(|n| KeyInfo {
            name: (*n).to_owned(),
            weight: 1,
        })
        .collect();
    let pick = |now| {
        r.select_key("d", "p", &keys, &[], None, KeyMode::Untried, &s, now)
            .unwrap()
    };
    assert_eq!(pick(at(0.0)), "k1");
    let t = r.begin("d", "p", Some("k2"), &s, at(0.0)).unwrap();
    r.finish(&t, &verdict(Class::CredentialRejected, Some(401)), &s, at(0.0));
    let seq: Vec<String> = (0..4).map(|_| pick(at(0.1))).collect();
    assert!(!seq.contains(&"k2".to_owned()));
    assert!(r.begin("d", "p", Some("k2"), &s, at(0.5)).is_none());
    let probe = r.begin("d", "p", Some("k2"), &s, at(1.1)).unwrap();
    assert!(r.begin("d", "p", Some("k2"), &s, at(1.1)).is_none());
    r.finish(&probe, &verdict(Class::Success, Some(200)), &s, at(1.1));
    assert!(!r.view("d", "p", &keys, &s, at(1.1)).keys[1].scope_dead);
}

#[test]
fn rate_limits_cool_the_pair_with_a_clamp() {
    let s = settings(json!({"max_cooldown": "3s"}));
    let r = HealthRegistry::default();
    let keys = [KeyInfo {
        name: "k".into(),
        weight: 1,
    }];
    let t = r.begin("d", "p", Some("k"), &s, at(0.0)).unwrap();
    r.finish(
        &t,
        &Verdict {
            class: Class::RateLimited,
            status: Some(429),
            hint: Some(at(86_400.0)),
            counts: true,
        },
        &s,
        at(0.0),
    );
    let blocked = r.status("d", "p", &keys, &s, at(2.9)).unwrap_err();
    assert!(blocked.cooling_only);
    assert_eq!(blocked.recover_at, Some(at(3.0)));
    assert!(r.status("d", "p", &keys, &s, at(3.0)).is_ok());
}

#[test]
fn manual_disable_wins_until_enabled() {
    let s = settings(json!({}));
    let r = HealthRegistry::default();
    r.disable("d", None, at(0.0));
    r.reset("d", "p", at(0.0));
    assert!(r.status("d", "p", &[], &s, at(1000.0)).unwrap_err().disabled);
    r.enable("d", at(1000.0));
    assert!(r.status("d", "p", &[], &s, at(1000.0)).is_ok());
    r.disable("d", Some(at(5.0)), at(1000.0));
    assert!(r.status("d", "p", &[], &s, at(4.0)).is_err());
    assert!(r.status("d", "p", &[], &s, at(5.0)).is_ok());
}

#[test]
fn state_changes_are_kept_until_taken() {
    let s = settings(json!({"breaker": {"consecutive_failures": 1, "open_duration": "1s"}}));
    let r = HealthRegistry::default();
    run(&r, &s, at(0.0), Class::Success);
    assert!(r.take_changes().is_empty());
    run(&r, &s, at(0.0), Class::ServerError);
    let probe = r.begin("d", "p", None, &s, at(1.1)).unwrap();
    r.finish(&probe, &verdict(Class::Success, Some(200)), &s, at(1.1));
    r.disable("d", None, at(1.1));
    r.enable("d", at(1.1));
    let seen: Vec<(&str, &str, &str)> = r
        .take_changes()
        .iter()
        .map(|c| (c.from, c.to, c.reason))
        .collect();
    assert_eq!(
        seen,
        [
            ("closed", "open", "failures"),
            ("open", "half_open", "open_duration_elapsed"),
            ("half_open", "closed", "probe"),
            ("closed", "disabled", "manual"),
            ("disabled", "closed", "manual"),
        ]
    );
    assert!(r.take_changes().is_empty());
}

#[test]
fn a_stream_failing_after_its_commit_counts_as_a_failure() {
    let s = settings(json!({"breaker": {"consecutive_failures": 2}}));
    let r = HealthRegistry::default();
    for _ in 0..2 {
        let t = r.begin("d", "p", None, &s, at(0.0)).unwrap();
        let committed = r
            .finish(&t, &verdict(Class::Success, Some(200)), &s, at(0.0))
            .unwrap();
        assert_eq!(committed.deployment(), "d");
        r.failed_after_commit(&committed, &s, at(0.1));
    }
    let v = r.view("d", "p", &[], &s, at(0.2));
    assert_eq!((v.state, v.window_requests, v.window_failures), ("open", 2, 2));

    let r = HealthRegistry::default();
    let t = r.begin("d", "p", None, &s, at(0.0)).unwrap();
    let committed = r
        .finish(&t, &verdict(Class::Success, Some(200)), &s, at(0.0))
        .unwrap();
    run(&r, &s, at(0.0), Class::Success);
    r.failed_after_commit(&committed, &s, at(0.1));
    assert_eq!(
        r.view("d", "p", &[], &s, at(0.2)).consecutive,
        1,
        "a later success stays"
    );
}

#[test]
fn active_checks_close_an_open_breaker_and_count_failures() {
    let s = settings(json!({"breaker": {"consecutive_failures": 1, "success_threshold": 2}}));
    let r = HealthRegistry::default();
    run(&r, &s, at(0.0), Class::ServerError);
    r.checked("d", true, &s, at(0.1));
    assert_eq!(r.view("d", "p", &[], &s, at(0.1)).state, "open");
    r.checked("d", true, &s, at(0.2));
    assert_eq!(r.view("d", "p", &[], &s, at(0.2)).state, "closed");
    r.checked("d", false, &s, at(0.3));
    assert_eq!(r.view("d", "p", &[], &s, at(0.3)).state, "open");
}

#[test]
fn ignoring_the_breaker_takes_no_probe_slot() {
    let s = settings(json!({"breaker": {"consecutive_failures": 1, "open_duration": "1s"}}));
    let r = HealthRegistry::default();
    run(&r, &s, at(0.0), Class::ServerError);
    assert!(r.status("d", "p", &[], &s, at(0.5)).unwrap_err().breaker_only);
    assert!(r.blocked("d", &s, at(0.5), false));
    assert!(!r.blocked("d", &s, at(0.5), true));
    let probe = r.begin("d", "p", None, &s, at(1.1)).unwrap();
    assert!(r.begin("d", "p", None, &s, at(1.1)).is_none());
    assert!(r.begin_ignoring_breaker("d", "p", None, &s, at(1.1)).is_some());
    r.abandon(&probe);
}
