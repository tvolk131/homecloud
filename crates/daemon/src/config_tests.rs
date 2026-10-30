use super::*;

#[tokio::test]
async fn legacy_config_still_invokes_python_with_the_script_path_as_one_argument() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy adapter.py");
    std::fs::write(&path, "import json,sys\njson.load(sys.stdin)\nprint(json.dumps({'kind':'error','data':'legacy adapter'}))\n").unwrap();
    let mut legacy = serde_json::to_value(Config::default()).unwrap();
    legacy.as_object_mut().unwrap().remove("host_command");
    legacy.as_object_mut().unwrap().remove("host_observer");
    legacy["host_script"] = serde_json::json!(path);
    legacy["sudo"] = serde_json::json!(false);
    let config: Config = serde_json::from_value(legacy).unwrap();
    assert!(config.host_command.is_none());
    assert!(config.host_observer.is_none());
    assert_eq!(
        config
            .host_backend()
            .request(HostRequest::Inventory)
            .await
            .unwrap_err()
            .to_string(),
        "legacy adapter"
    );
}

#[tokio::test]
async fn explicit_commands_route_native_reads_and_keep_primary_operations_separate() {
    let command = |message| {
        serde_json::json!({
            "program":"/bin/sh", "args":["-c", "cat >/dev/null; printf '%s' \"$1\"", "fixture",
                serde_json::json!({"kind":"error","data":message}).to_string()]
        })
    };
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value.as_object_mut().unwrap().remove("host_script");
    value["host_command"] = command("primary executable");
    value["host_observer"] = command("native observer");
    value["sudo"] = serde_json::json!(false);
    let config: Config = serde_json::from_value(value).unwrap();
    let backend = config.host_backend();
    assert_eq!(
        backend
            .request(HostRequest::Inventory)
            .await
            .unwrap_err()
            .to_string(),
        "native observer"
    );
    assert_eq!(
        backend
            .request(HostRequest::ReserveStatus)
            .await
            .unwrap_err()
            .to_string(),
        "primary executable"
    );
}

#[test]
fn command_arguments_default_to_empty_for_native_binaries() {
    let mut config = serde_json::to_value(Config::default()).unwrap();
    config["host_observer"] =
        serde_json::json!({"program":"/usr/local/libexec/homecloud-host-observer"});
    let config: Config = serde_json::from_value(config).unwrap();
    assert!(config.host_observer.unwrap().args.is_empty());
}
