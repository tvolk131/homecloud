use std::process::Command;

#[test]
fn metrics_check_never_opens_controller_state() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("must-not-create");
    let config = directory.path().join("control.json");
    for settings in [
        serde_json::json!({}),
        serde_json::json!({"metrics":{
            "port":8444,
            "server_name":"localhost",
            "client_identity":directory.path().join("missing-client.pem"),
            "server_certificate":directory.path().join("missing-server.crt")
        }}),
    ] {
        let mut value = serde_json::to_value(homecloud_daemon::Config::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .extend(settings.as_object().unwrap().clone());
        std::fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_homecloudd"))
            .arg("--config")
            .arg(&config)
            .arg("--state-dir")
            .arg(&state)
            .arg("check-metrics")
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!state.exists());
        assert!(!result.stderr.is_empty());
    }
}
