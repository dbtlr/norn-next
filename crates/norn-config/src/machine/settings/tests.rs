use super::*;
#[test]
fn absent_knobs_have_contract_defaults() {
    let (s, w) = parse("", Path::new("config.toml")).unwrap();
    assert!(w.is_empty());
    assert_eq!(s.endpoint, default_endpoint());
    assert_eq!(s.read_connections, 4);
    assert_eq!(s.read_idle_grace, Duration::from_secs(5));
    assert_eq!(s.count_result_max_bytes, None);
    assert_eq!(s.count_results_total_bytes, None);
    assert_eq!(s.background_threads, 1);
    assert_eq!(s.background_turn_budget, Duration::from_millis(100));
    assert_eq!(s.idle_after, Duration::from_secs(3600));
    assert_eq!(s.auto_exit_after, Duration::from_secs(1800));
    assert_eq!(s.ready_wait, Duration::from_secs(30));
    assert_eq!(s.drain_bound, Duration::from_secs(10));
    assert!(!s.request_log);
}
#[test]
fn bad_files_and_ports_refuse_with_path_line_and_fix() {
    for text in [
        "bad = [",
        "endpoint = 1",
        "[endpoint]\nport=0",
        "[endpoint]\nport=65536",
        "[endpoint]\nport=-1",
        "[endpoint]\nport='80'",
    ] {
        let error = parse(text, Path::new("config.toml")).unwrap_err();
        assert_eq!(error.path, Path::new("config.toml"));
        assert!(error.line.is_some(), "{text}");
        assert!(!error.fix.is_empty());
    }
}
#[test]
fn each_bad_knob_warns_once_and_uses_its_default() {
    for (key, bad) in [
        ("read_connections", vec!["1", "65", "'4'"]),
        (
            "read_idle_grace",
            vec!["'601s'", "4", "'oops'", "'18446744073709551615d'"],
        ),
        ("count_result_max_bytes", vec!["0", "-1", "false"]),
        ("count_results_total_bytes", vec!["0", "'1'"]),
        ("background_threads", vec!["0", "9", "false"]),
        ("background_turn_budget", vec!["'9ms'", "'1001ms'", "true"]),
        ("idle_after", vec!["'59s'", "'8d'", "'1.5h'"]),
        ("auto_exit_after", vec!["'59s'", "'25h'", "'1h30m'"]),
        ("ready_wait", vec!["'999ms'", "'301s'", "'-1s'"]),
        ("drain_bound", vec!["'0s'", "'21s'", "' 1s'"]),
        ("request_log", vec!["1", "'true'"]),
    ] {
        for bad in bad {
            let (s, w) = parse(&format!("[serve]\n{key}={bad}"), Path::new("config.toml")).unwrap();
            assert_eq!(s, ServingSettings::default(), "{key}={bad}");
            assert_eq!(w.len(), 1);
            assert_eq!(w[0].key, format!("serve.{key}"));
            assert!(w[0].message.contains("default"));
        }
    }
}
#[test]
fn valid_settings_and_duration_units_are_used() {
    let (s, w) = parse("[endpoint]\nport=12345\n[serve]\nread_connections=64\nread_idle_grace='0ms'\ncount_result_max_bytes=1\ncount_results_total_bytes=2\nbackground_threads=8\nbackground_turn_budget='10ms'\nidle_after='7d'\nauto_exit_after='24h'\nready_wait='5m'\ndrain_bound='20s'\nrequest_log=true", Path::new("config.toml")).unwrap();
    assert!(w.is_empty());
    assert_eq!(s.endpoint.port(), 12345);
    assert_eq!(s.read_connections, 64);
    assert_eq!(s.read_idle_grace, Duration::ZERO);
    assert_eq!(s.count_result_max_bytes, Some(1));
    assert_eq!(s.count_results_total_bytes, Some(2));
    assert_eq!(s.background_threads, 8);
    assert_eq!(s.background_turn_budget, Duration::from_millis(10));
    assert_eq!(s.idle_after, Duration::from_secs(604800));
    assert_eq!(s.auto_exit_after, Duration::from_secs(86400));
    assert_eq!(s.ready_wait, Duration::from_secs(300));
    assert_eq!(s.drain_bound, Duration::from_secs(20));
    assert!(s.request_log);
}
#[test]
fn unknown_tables_and_keys_warn_but_client_is_silent() {
    let (_, w) = parse(
        "[client]\nanything=1\n[future]\nx=1\n[serve]\nfuture=1\n[endpoint]\nfuture=1",
        Path::new("config.toml"),
    )
    .unwrap();
    assert_eq!(w.len(), 3);
    assert!(w.iter().all(|w| !w.key.starts_with("client")));
}
#[test]
fn inconsistent_quota_pair_falls_back_together() {
    let (s, w) = parse(
        "[serve]\ncount_result_max_bytes=2\ncount_results_total_bytes=1",
        Path::new("config.toml"),
    )
    .unwrap();
    assert_eq!(s.count_result_max_bytes, None);
    assert_eq!(s.count_results_total_bytes, None);
    assert_eq!(w.len(), 1);
}
#[test]
fn ephemeral_port_is_used_and_warned_once() {
    let port = crate::file::ephemeral_range().unwrap().0;
    let (s, w) = parse(
        &format!("[endpoint]\nport={port}"),
        Path::new("config.toml"),
    )
    .unwrap();
    assert_eq!(s.endpoint.port(), port);
    assert_eq!(w.len(), 1);
}
#[test]
fn malformed_serve_table_degrades_once() {
    let (s, w) = parse("serve=42", Path::new("config.toml")).unwrap();
    assert_eq!(s, ServingSettings::default());
    assert_eq!(w.len(), 1);
}

#[test]
fn durations_reject_overflow_and_non_integer_forms() {
    for value in [
        "",
        "1",
        "-1s",
        "+1s",
        "1.0s",
        "1s1ms",
        "1 s",
        "18446744073709551616ms",
        "18446744073709551615s",
    ] {
        let (_, warnings) = parse(
            &format!("[serve]\nread_idle_grace='{value}'"),
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(warnings.len(), 1, "{value}");
    }
}

#[test]
fn port_refusal_points_to_endpoint_not_another_tables_port() {
    let error = parse(
        "[client]\nport=7\n[endpoint]\nport=0",
        Path::new("config.toml"),
    )
    .unwrap_err();
    assert_eq!(error.line, Some(4));
}

#[test]
fn port_line_survives_other_top_level_shapes() {
    for (text, line) in [
        ("x = 1\n[endpoint]\nport = 0", 3),
        ("[[jobs]]\na=1\n[endpoint]\nport = 0", 4),
    ] {
        assert_eq!(
            parse(text, Path::new("config.toml")).unwrap_err().line,
            Some(line)
        );
    }
}

#[test]
fn top_level_warning_names_the_shape_and_fallback() {
    for (text, key, message) in [
        ("client=1", "client", "expected a table; ignored"),
        ("future=1", "future", "unknown key; ignored"),
        (
            "serve=1",
            "serve",
            "expected a table; using all serve defaults",
        ),
    ] {
        let (settings, warnings) = parse(text, Path::new("config.toml")).unwrap();
        assert_eq!(settings, ServingSettings::default());
        assert_eq!(
            warnings,
            vec![SettingsWarning {
                key: key.into(),
                message: message.into()
            }]
        );
    }
}

#[test]
fn only_the_resolved_port_gets_an_ephemeral_warning() {
    let ephemeral = crate::file::ephemeral_range().unwrap().0;
    let (settings, warnings) = parse_with_port(
        &format!("[endpoint]\nport={ephemeral}"),
        Path::new("config.toml"),
        NonZeroU16::new(12345),
    )
    .unwrap();
    assert_eq!(settings.endpoint.port(), 12345);
    assert!(warnings.is_empty());
    let (settings, warnings) = parse_with_port(
        "[endpoint]\nport=12345",
        Path::new("config.toml"),
        NonZeroU16::new(ephemeral),
    )
    .unwrap();
    assert_eq!(settings.endpoint.port(), ephemeral);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains(&ephemeral.to_string()));
}
