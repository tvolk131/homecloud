#[cfg(not(target_os = "linux"))]
#[test]
fn local_storage_command_fails_before_opening_controller_state() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("must-not-be-created");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_homecloudd"))
        .arg("--state-dir")
        .arg(&state)
        .arg("--config")
        .arg(directory.path().join("not-a-controller-config.json"))
        .arg("collect-storage")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("local root Linux host adapter"));
    assert!(output.stdout.is_empty());
    assert!(!state.exists());
}
