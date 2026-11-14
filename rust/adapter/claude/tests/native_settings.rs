use oxap_adapter_claude::native::{
    apply_flag_settings_request, decode_settings, get_settings_request, FlagSettings,
};

#[test]
fn an_apply_flag_settings_request_writes_its_settings_with_their_keys_sorted_as_a_go_map() {
    assert_eq!(
        apply_flag_settings_request(&FlagSettings::default()).unwrap(),
        r#"{"subtype":"apply_flag_settings","settings":{}}"#
    );
    assert_eq!(
        apply_flag_settings_request(&FlagSettings {
            effort_level: Some("high"),
            ..FlagSettings::default()
        })
        .unwrap(),
        r#"{"subtype":"apply_flag_settings","settings":{"effortLevel":"high"}}"#
    );
    assert_eq!(
        apply_flag_settings_request(&FlagSettings {
            auto_compact_enabled: Some(Some(true)),
            auto_compact_window: Some(Some(120000)),
            effort_level: Some("max"),
        })
        .unwrap(),
        r#"{"subtype":"apply_flag_settings","settings":{"autoCompactEnabled":true,"autoCompactWindow":120000,"effortLevel":"max"}}"#
    );
}

#[test]
fn a_flag_setting_cleared_live_is_written_as_null_and_an_unset_one_is_left_out() {
    assert_eq!(
        apply_flag_settings_request(&FlagSettings {
            auto_compact_enabled: Some(None),
            auto_compact_window: Some(None),
            effort_level: None,
        })
        .unwrap(),
        r#"{"subtype":"apply_flag_settings","settings":{"autoCompactEnabled":null,"autoCompactWindow":null}}"#
    );
    assert_eq!(
        apply_flag_settings_request(&FlagSettings {
            auto_compact_enabled: Some(Some(false)),
            auto_compact_window: Some(None),
            effort_level: None,
        })
        .unwrap(),
        r#"{"subtype":"apply_flag_settings","settings":{"autoCompactEnabled":false,"autoCompactWindow":null}}"#
    );
}

#[test]
fn a_get_settings_request_carries_only_its_subtype() {
    assert_eq!(
        get_settings_request().unwrap(),
        r#"{"subtype":"get_settings"}"#
    );
}

#[test]
fn a_settings_answer_reads_the_applied_model_and_effort_and_the_effective_compaction() {
    let settings =
        decode_settings(br#"{"applied":{"model":"resumed","effort":"high"},"effective":{}}"#)
            .unwrap();
    assert_eq!(settings.applied.model, "resumed");
    assert_eq!(settings.applied.effort, "high");
    assert_eq!(settings.effective.auto_compact_enabled, None);
    assert_eq!(settings.effective.auto_compact_window, None);
    let settings = decode_settings(
        br#"{"applied":{"model":null},"effective":{"autoCompactEnabled":false,"autoCompactWindow":5000}}"#,
    )
    .unwrap();
    assert_eq!(settings.applied.model, "");
    assert_eq!(settings.applied.effort, "");
    assert_eq!(settings.effective.auto_compact_enabled, Some(false));
    assert_eq!(settings.effective.auto_compact_window, Some(5000));
}

#[test]
fn a_null_settings_answer_or_member_reads_as_its_zero_value_and_a_mistyped_one_is_refused() {
    for empty in [&b"null"[..], br#"{"applied":null,"effective":null}"#, b"{}"] {
        let settings = decode_settings(empty).unwrap();
        assert_eq!(
            settings.applied.model,
            "",
            "{}",
            String::from_utf8_lossy(empty)
        );
        assert_eq!(settings.effective.auto_compact_window, None);
    }
    for refused in [
        &b"[]"[..],
        br#"{"applied":[]}"#,
        br#"{"effective":{"autoCompactWindow":1.5}}"#,
        br#"{"effective":{"autoCompactEnabled":"yes"}}"#,
    ] {
        assert!(
            decode_settings(refused).is_err(),
            "{}",
            String::from_utf8_lossy(refused)
        );
    }
}
