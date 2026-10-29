#[test]
fn privileged_adapter_lifecycle_and_failure_boundaries() {
    let output = std::process::Command::new("python3")
        .args(["-m", "unittest", "discover", "-s"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"))
        .args(["-p", "test_*.py"])
        .output()
        .expect("Python 3 is required by the host adapter");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
