use super::*;

#[tokio::test]
async fn legacy_config_keeps_python_for_remaining_operations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy adapter.py");
    std::fs::write(&path, "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'kind':'error','data':'legacy adapter'}))\n").unwrap();
    let mut legacy = serde_json::to_value(Config::default()).unwrap();
    legacy.as_object_mut().unwrap().remove("host_command");
    legacy["host_script"] = serde_json::json!(path);
    legacy["sudo"] = serde_json::json!(false);
    let config: Config = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        config
            .host_backend()
            .request(HostRequest::ReserveStatus)
            .await
            .unwrap_err()
            .to_string(),
        "legacy adapter"
    );
}

#[tokio::test]
async fn default_native_reads_never_reach_the_configured_primary_even_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("primary-called");
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value.as_object_mut().unwrap().remove("host_script");
    value["host_command"] = serde_json::json!({
        "program":"/bin/sh", "args":["-c", r#"cat >/dev/null; touch "$1"; printf '%s' '{"kind":"error","data":"primary executable"}'"#, "fixture", marker]
    });
    value["sudo"] = serde_json::json!(false);
    let config: Config = serde_json::from_value(value).unwrap();
    let backend = config.host_backend();
    for request in [HostRequest::Inventory, HostRequest::Observe { vms: vec![] }] {
        let _ = backend.request(request).await;
        assert!(
            !marker.exists(),
            "Native reads must never fall back to the primary command"
        );
    }
    assert_eq!(
        backend
            .request(HostRequest::ReserveStatus)
            .await
            .unwrap_err()
            .to_string(),
        "primary executable"
    );
    assert!(marker.exists());
}
