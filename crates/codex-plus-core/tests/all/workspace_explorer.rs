use std::fs;

use codex_plus_core::workspace_explorer::{WorkspaceExplorer, route_with_home};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn workspace_explorer_setting_defaults_off_and_survives_serde_round_trip() {
    use codex_plus_core::settings::BackendSettings;

    let defaults = BackendSettings::default();
    assert!(!defaults.codex_app_workspace_explorer_enabled);
    let mut legacy = serde_json::to_value(&defaults).unwrap();
    assert_eq!(legacy["codexAppWorkspaceExplorerEnabled"], false);
    legacy.as_object_mut().unwrap().remove("codexAppWorkspaceExplorerEnabled");
    let restored: BackendSettings = serde_json::from_value(legacy).unwrap();
    assert!(!restored.codex_app_workspace_explorer_enabled);

    let mut enabled = defaults;
    enabled.codex_app_workspace_explorer_enabled = true;
    let stored = serde_json::to_value(&enabled).unwrap();
    assert_eq!(stored["codexAppWorkspaceExplorerEnabled"], true);
    let restored: BackendSettings = serde_json::from_value(stored).unwrap();
    assert!(restored.codex_app_workspace_explorer_enabled);
    assert_eq!(serde_json::to_value(restored).unwrap()["codexAppWorkspaceExplorerEnabled"], true);
}

#[test]
fn workspace_explorer_setting_survives_partial_updates() {
    use codex_plus_core::settings::{BackendSettings, SettingsStore};

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("settings.json");
    let store = SettingsStore::new(path.clone());
    store.save(&BackendSettings::default()).unwrap();

    for enabled in [true, false] {
        let updated = store
            .update(json!({ "codexAppWorkspaceExplorerEnabled": enabled }))
            .unwrap();
        assert_eq!(updated.codex_app_workspace_explorer_enabled, enabled);
        assert_eq!(store.load().unwrap().codex_app_workspace_explorer_enabled, enabled);
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["codexAppWorkspaceExplorerEnabled"], json!(enabled));

        // 无关设置的部分更新不能覆盖文件树开关。
        let updated = store.update(json!({ "codexAppPasteFix": false })).unwrap();
        assert_eq!(updated.codex_app_workspace_explorer_enabled, enabled);
        assert_eq!(store.load().unwrap().codex_app_workspace_explorer_enabled, enabled);
    }
}

#[test]
fn lists_directories_first_with_nested_paths_pages_and_empty_directories() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("z-dir")).unwrap();
    fs::create_dir(root.path().join("empty")).unwrap();
    fs::write(root.path().join("a.txt"), "hello").unwrap();
    fs::write(root.path().join("z-dir/文档.txt"), "你好").unwrap();
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    let page = explorer.list("", None, Some(2)).unwrap();
    assert_eq!(page["items"][0]["name"], "empty");
    assert_eq!(page["items"][1]["kind"], "directory");
    assert_eq!(page["nextCursor"], 2);
    assert_eq!(page["truncated"], false);
    assert!(page["rootToken"].as_str().is_some_and(|token| !token.is_empty()));
    let last = explorer.list("", Some(2), Some(2)).unwrap();
    assert_eq!(last["items"][0]["path"], "a.txt");
    assert!(last["nextCursor"].is_null());
    assert_eq!(last["truncated"], false);
    assert_eq!(explorer.list("empty", None, None).unwrap()["items"], json!([]));
    assert_eq!(explorer.list("z-dir", None, None).unwrap()["items"][0]["path"], "z-dir/文档.txt");
    assert_eq!(explorer.preview("z-dir/文档.txt").unwrap()["text"], "你好");
    assert!(explorer.list("", Some(4), None).is_err());
    assert!(explorer.list("", None, Some(0)).is_err());
}

#[test]
fn rejects_ambiguous_and_windows_special_paths() {
    let root = TempDir::new().unwrap();
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    for path in ["..", "../secret", "a/../b", "/etc", "C:/Windows", "\\root", "\\\\server\\share", "\\\\?\\C:\\", "\\\\.\\NUL", "file:stream", "CON", "aux.txt", "COM1", "LPT9.log", "COM¹", "LPT².txt", "a.", "a ", "a//b", "a/./b", "a\\/b", "a\0b"] {
        assert!(explorer.list(path, None, None).is_err(), "{path:?}");
        assert!(explorer.preview(path).is_err(), "{path:?}");
    }
    assert!(explorer.list(&"a".repeat(1025), None, None).is_err());
    assert!(explorer.list(&vec!["a"; 129].join("/"), None, None).is_err());
}

#[test]
fn bounds_preview_and_validates_utf8_and_binary() {
    let root = TempDir::new().unwrap();
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    fs::write(root.path().join("empty"), []).unwrap();
    assert_eq!(explorer.preview("empty").unwrap()["text"], "");
    fs::write(root.path().join("exact"), "a".repeat(65536)).unwrap();
    assert_eq!(explorer.preview("exact").unwrap()["truncated"], false);
    for prefix in [65533, 65534, 65535] {
        let text = format!("{}😀tail", "a".repeat(prefix));
        fs::write(root.path().join("long"), text).unwrap();
        let result = explorer.preview("long").unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(result["text"].as_str().unwrap(), "a".repeat(prefix));
    }
    for (name, bytes) in [("binary", vec![0, 1, 2]), ("invalid", vec![0xff]), ("short", vec![0xe4, 0xb8])] {
        fs::write(root.path().join(name), bytes).unwrap();
        assert!(explorer.preview(name).is_err());
    }
    assert!(explorer.preview("").is_err());
    fs::write(root.path().join("gone"), "text").unwrap();
    fs::remove_file(root.path().join("gone")).unwrap();
    assert_eq!(explorer.preview("gone").unwrap_err().code(), "not_found");
}

#[test]
fn caps_page_size_and_refuses_unstable_overlarge_enumeration() {
    let root = TempDir::new().unwrap();
    for index in 0..501 {
        fs::write(root.path().join(format!("{index:05}")), "").unwrap();
    }
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    assert_eq!(explorer.list("", None, None).unwrap()["items"].as_array().unwrap().len(), 200);
    assert_eq!(explorer.list("", None, Some(999)).unwrap()["items"].as_array().unwrap().len(), 500);
    for index in 501..10000 {
        fs::write(root.path().join(format!("{index:05}")), "").unwrap();
    }
    assert_eq!(explorer.list("", None, None).unwrap_err().code(), "too_many_entries");
}

#[test]
fn retained_root_rejects_replacement() {
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    let explorer = WorkspaceExplorer::open(&root).unwrap();
    fs::rename(&root, parent.path().join("moved")).unwrap();
    assert_eq!(explorer.list("", None, None).unwrap_err().code(), "outside_workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("secret"), "replacement").unwrap();
    assert_eq!(explorer.preview("secret").unwrap_err().code(), "outside_workspace");
}

#[cfg(any(unix, windows))]
#[test]
fn lists_links_but_never_traverses_them() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_dir(outside.path(), root.path().join("link")) {
        // Windows 创建符号链接可能需要开发者模式；其他错误不能静默跳过。
        if error.raw_os_error() == Some(1314) { return; }
        panic!("create symlink: {error}");
    }
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    assert_eq!(explorer.list("", None, None).unwrap()["items"][0]["kind"], "link");
    assert!(explorer.list("link", None, None).is_err());
    assert!(explorer.preview("link/secret").is_err());
}

#[cfg(windows)]
#[test]
fn junction_is_listed_as_link_and_cannot_be_traversed() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret"), "outside").unwrap();
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:EXPLORER_TEST_LINK -Target $env:EXPLORER_TEST_TARGET | Out-Null"])
        .env("EXPLORER_TEST_LINK", root.path().join("junction"))
        .env("EXPLORER_TEST_TARGET", outside.path())
        .status().unwrap();
    assert!(status.success());
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    assert_eq!(explorer.list("", None, None).unwrap()["items"][0]["kind"], "link");
    assert!(explorer.list("junction", None, None).is_err());
    assert!(explorer.preview("junction/secret").is_err());
    drop(explorer);
    // 只移除 junction 本身，不递归删除目标。
    fs::remove_dir(root.path().join("junction")).unwrap();
    assert!(outside.path().join("secret").exists());
}

#[cfg(unix)]
#[test]
fn file_symlink_is_never_previewed() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("target"), "text").unwrap();
    std::os::unix::fs::symlink(root.path().join("target"), root.path().join("link")).unwrap();
    let explorer = WorkspaceExplorer::open(root.path()).unwrap();
    assert_eq!(explorer.preview("link").unwrap_err().code(), "outside_workspace");
}

#[test]
fn route_token_rejects_cross_request_root_replacement() {
    let home = TempDir::new().unwrap();
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("file"), "original").unwrap();
    let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads (id TEXT, cwd TEXT, rollout_path TEXT);").unwrap();
    db.execute("INSERT INTO threads VALUES ('session', ?1, '')", [root.to_str().unwrap()]).unwrap();
    let first = json!({"sessionId":"session", "path":"", "cursor":0});
    let token = route_with_home(home.path(), &first, false).unwrap()["rootToken"].clone();
    assert_eq!(route_with_home(home.path(), &first, false).unwrap()["rootToken"], token);
    let preview = json!({"sessionId":"session", "path":"file", "rootToken":token});
    let list = json!({"sessionId":"session", "path":"", "rootToken":token});
    assert_eq!(route_with_home(home.path(), &preview, true).unwrap()["text"], "original");
    for (request, preview) in [
        (json!({"sessionId":"session", "path":"file"}), true),
        (json!({"sessionId":"session", "path":"nested"}), false),
        (json!({"sessionId":"session", "path":"", "cursor":1}), false),
    ] {
        assert_eq!(route_with_home(home.path(), &request, preview).unwrap_err().code(), "invalid_request");
    }
    fs::rename(&root, parent.path().join("original")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "replacement").unwrap();
    assert_eq!(route_with_home(home.path(), &preview, true).unwrap_err().code(), "outside_workspace");
    assert_eq!(route_with_home(home.path(), &list, false).unwrap_err().code(), "outside_workspace");
    let refreshed = route_with_home(home.path(), &first, false).unwrap();
    assert_ne!(refreshed["rootToken"], token);
    let preview = json!({"sessionId":"session", "path":"file", "rootToken":refreshed["rootToken"]});
    assert_eq!(route_with_home(home.path(), &preview, true).unwrap()["text"], "replacement");
}

#[test]
fn session_lookup_is_per_request_and_root_input_is_rejected() {
    let home = TempDir::new().unwrap();
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    fs::write(first.path().join("first"), "one").unwrap();
    fs::write(second.path().join("second"), "two").unwrap();
    let db = rusqlite::Connection::open(home.path().join("state_5.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE threads (id TEXT, cwd TEXT, rollout_path TEXT);").unwrap();
    db.execute("INSERT INTO threads VALUES (?1, ?2, '')", rusqlite::params!["session", first.path().to_str().unwrap()]).unwrap();
    let request = json!({"sessionId":"session", "path":""});
    assert_eq!(route_with_home(home.path(), &request, false).unwrap()["items"][0]["name"], "first");
    db.execute("UPDATE threads SET cwd = ?1", [second.path().to_str().unwrap()]).unwrap();
    assert_eq!(route_with_home(home.path(), &request, false).unwrap()["items"][0]["name"], "second");
    for request in [json!({"sessionId":"missing","path":""}), json!({"sessionId":"session","path":"","root":first.path()}), json!({"sessionId":"session","path":"","cursor":-1}), json!({"sessionId":"session","path":"","cursor":1.5}), json!({"sessionId":"session"})] {
        assert!(route_with_home(home.path(), &request, false).is_err());
    }
}
