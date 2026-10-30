use super::*;
use crate::visual_tests::{fixture, lock, simulator};
use std::path::Path;

fn load_app(profile: &Path, preferences_path: &Path) -> App {
    let mut app = App::blank(profile.into());
    app.preferences_path = Some(preferences_path.into());
    app.load_preferences();
    app
}

fn saved_motion(path: &Path) -> bool {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["reduced_motion"].as_bool().unwrap()
}

#[test]
fn motion_toggle_survives_relaunch_and_profile_changes_without_touching_pairing_state() {
    let _guard = lock();
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("client.json");
    let other_profile = dir.path().join("other.json");
    let saved = dir.path().join("config/preferences.json");
    let pairing = b"pairing metadata stays unchanged";
    let journal = b"pending create request stays unchanged";
    std::fs::write(&profile, pairing).unwrap();
    std::fs::write(dir.path().join("client.json.create-request.json"), journal).unwrap();
    std::fs::write(
        dir.path().join("other.json.preferences.json"),
        r#"{"reduced_motion":false}"#,
    )
    .unwrap();
    let mut app = fixture();
    app.profile = profile.clone();
    app.preferences_path = Some(saved.clone());
    app.load_preferences();
    assert!(!app.reduced_motion);
    app.page = Page::Settings;
    let mut ui = simulator(&app, (800.0, 600.0));
    ui.click("Reduce motion").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(matches!(messages.as_slice(), [Msg::ToggleMotion]));
    let _ = app.update(messages[0].clone());
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
    assert!(saved_motion(&saved));

    let mut relaunched = load_app(&other_profile, &saved);
    assert!(relaunched.reduced_motion);
    let _ = relaunched.update(Msg::ToggleMotion);
    assert!(!load_app(&profile, &saved).reduced_motion);
    assert_eq!(std::fs::read(&profile).unwrap(), pairing);
    assert_eq!(
        std::fs::read(dir.path().join("client.json.create-request.json")).unwrap(),
        journal
    );
    assert!(!dir.path().join("client.json.preferences.json").exists());
}

#[test]
fn legacy_preference_is_imported_once_and_its_source_is_retained() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("custom.json");
    let legacy = dir.path().join("custom.json.preferences.json");
    let saved = dir.path().join("config/preferences.json");
    let original = r#"{"reduced_motion":true}"#;
    std::fs::write(&legacy, original).unwrap();
    let app = load_app(&profile, &saved);
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
    assert!(saved_motion(&saved));
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), original);

    // The app-wide file takes precedence, even if the legacy source is later corrupt.
    std::fs::write(&legacy, "invalid legacy JSON").unwrap();
    let app = load_app(&profile, &saved);
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
}

#[test]
fn first_launch_defaults_are_persisted_and_later_profiles_do_not_reimport() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("config/preferences.json");
    let app = load_app(&dir.path().join("client.json"), &saved);
    assert!(!app.reduced_motion);
    assert!(app.error.is_none());
    assert!(!saved_motion(&saved));
    std::fs::write(
        dir.path().join("other.json.preferences.json"),
        r#"{"reduced_motion":true}"#,
    )
    .unwrap();
    assert!(!load_app(&dir.path().join("other.json"), &saved).reduced_motion);
}

#[test]
fn corrupt_global_or_legacy_preferences_are_reported_without_overwriting_either() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("client.json");
    let legacy = dir.path().join("client.json.preferences.json");
    let saved = dir.path().join("preferences.json");
    std::fs::write(&legacy, r#"{"reduced_motion":true}"#).unwrap();
    std::fs::write(&saved, "invalid global JSON").unwrap();
    let app = load_app(&profile, &saved);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .starts_with("Could not load preferences:")
    );
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        "invalid global JSON"
    );
    assert!(saved_motion(&legacy));

    std::fs::remove_file(&saved).unwrap();
    std::fs::write(&legacy, "invalid legacy JSON").unwrap();
    let app = load_app(&profile, &saved);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .starts_with("Could not load preferences:")
    );
    assert!(!saved.exists());
    assert_eq!(
        std::fs::read_to_string(&legacy).unwrap(),
        "invalid legacy JSON"
    );
}

#[test]
fn failed_preference_save_keeps_the_previous_motion_setting_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("preferences.json");
    std::fs::create_dir(&saved).unwrap();
    let mut app = App::blank(dir.path().join("client.json"));
    app.preferences_path = Some(saved.clone());
    let _ = app.update(Msg::ToggleMotion);
    assert!(!app.reduced_motion);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .starts_with("Could not save preferences:")
    );
    std::fs::remove_dir(&saved).unwrap();
    let _ = app.update(Msg::ToggleMotion);
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
    assert!(saved_motion(&saved));
}

#[cfg(unix)]
#[test]
fn failed_import_write_still_honors_the_legacy_setting_and_retains_it_for_retry() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("client.json");
    let legacy = dir.path().join("client.json.preferences.json");
    let config = dir.path().join("config");
    let saved = config.join("preferences.json");
    std::fs::write(&legacy, r#"{"reduced_motion":true}"#).unwrap();
    std::fs::create_dir(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o500)).unwrap();
    let app = load_app(&profile, &saved);
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(app.reduced_motion);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .starts_with("Could not save preferences:")
    );
    assert!(!saved.exists());
    assert!(saved_motion(&legacy));
    let app = load_app(&profile, &saved);
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
    assert!(saved_motion(&saved));
}

#[test]
fn relative_legacy_profile_preferences_are_imported() {
    // Isolate the working directory without changing it for parallel tests.
    if std::env::var_os("HOMECLOUD_TEST_RELATIVE_PREFERENCES").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .current_dir(dir.path())
            .env("HOMECLOUD_TEST_RELATIVE_PREFERENCES", "1")
            .args([
                "--exact",
                "preferences_tests::relative_legacy_profile_preferences_are_imported",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::fs::write("client.json.preferences.json", r#"{"reduced_motion":true}"#).unwrap();
    let saved = std::env::current_dir()
        .unwrap()
        .join("config/preferences.json");
    let app = load_app(Path::new("client.json"), &saved);
    assert!(app.reduced_motion);
    assert!(app.error.is_none());
    assert!(saved_motion(&saved));
}

#[cfg(target_os = "linux")]
#[test]
fn platform_preferences_respect_xdg_config_home() {
    if std::env::var_os("HOMECLOUD_TEST_CONFIG_ROOT").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("HOMECLOUD_TEST_CONFIG_ROOT", dir.path())
            .args([
                "--exact",
                "preferences_tests::platform_preferences_respect_xdg_config_home",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = PathBuf::from(std::env::var_os("HOMECLOUD_TEST_CONFIG_ROOT").unwrap());
    assert_eq!(
        preferences::default_path().unwrap(),
        root.join("homecloud/preferences.json")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn platform_preferences_use_application_support() {
    assert!(
        preferences::default_path()
            .unwrap()
            .ends_with("Library/Application Support/dev.homecloud/preferences.json")
    );
}
