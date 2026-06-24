//! Shared `#[cfg(test)]` constructors used by the platform discovery
//! submodule tests (`mumu`, `ldplayer`, `process`).

use super::ProcessInfo;

pub(super) fn unique_test_dir(prefix: &str) -> String {
    format!(
        "arknights-ruler-{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

pub(super) fn process(
    name: &str,
    process_id: u32,
    parent_process_id: Option<u32>,
    executable_path: Option<&str>,
) -> ProcessInfo {
    ProcessInfo {
        name: name.to_string(),
        process_id,
        parent_process_id,
        executable_path: executable_path.map(str::to_string),
        command_line: None,
        creation_time: 100,
    }
}
