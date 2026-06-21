use serde_json::Value;

const UI_SCALER_FIELD: &str = "uiScaler";
const CURSOR_SIZE_FIELD: &str = "cursorSize";

pub fn read_pc_ui_scaler() -> Result<Option<f64>, String> {
    read_pc_common_setting_number(UI_SCALER_FIELD)
}

pub fn read_pc_cursor_size() -> Result<Option<f64>, String> {
    read_pc_common_setting_number(CURSOR_SIZE_FIELD)
}

fn read_pc_common_setting_number(field: &str) -> Result<Option<f64>, String> {
    let Some(settings) = platform::read_common_settings()? else {
        return Ok(None);
    };

    let value: Value = serde_json::from_str(&settings)
        .map_err(|error| format!("failed to parse Arknights common settings JSON: {error}"))?;
    Ok(value.get(field).and_then(Value::as_f64))
}

#[cfg(windows)]
mod platform {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{
                RegGetValueW, HKEY_CURRENT_USER, REG_BINARY, REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY,
            },
        },
    };

    const REGISTRY_SUBKEY: &str = "Software\\Hypergryph\\Arknights";
    const REGISTRY_VALUE: &str = "common_setting_h2012961537";

    pub fn read_common_settings() -> Result<Option<String>, String> {
        read_registry_string(REGISTRY_SUBKEY, REGISTRY_VALUE)
    }

    fn read_registry_string(subkey: &str, value_name: &str) -> Result<Option<String>, String> {
        let subkey = wide_null(subkey);
        let value_name = wide_null(value_name);
        let mut value_type = REG_VALUE_TYPE::default();
        let mut byte_len = 0u32;

        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                RRF_RT_ANY,
                Some(&mut value_type),
                None,
                Some(&mut byte_len),
            )
        };
        if status != ERROR_SUCCESS {
            return Ok(None);
        }
        if byte_len == 0 {
            return Ok(None);
        }

        let mut buffer = vec![0u8; byte_len as usize];
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                RRF_RT_ANY,
                Some(&mut value_type),
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut byte_len),
            )
        };
        if status != ERROR_SUCCESS {
            return Ok(None);
        }

        buffer.truncate(byte_len as usize);
        if value_type == REG_SZ {
            return Ok(decode_utf16_registry_string(&buffer));
        }
        if value_type == REG_BINARY {
            return Ok(decode_utf8_registry_blob(&buffer));
        }

        Ok(None)
    }

    fn decode_utf16_registry_string(bytes: &[u8]) -> Option<String> {
        let wide = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        let len = wide.iter().position(|ch| *ch == 0).unwrap_or(wide.len());
        (len > 0).then(|| String::from_utf16_lossy(&wide[..len]))
    }

    fn decode_utf8_registry_blob(bytes: &[u8]) -> Option<String> {
        let len = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        if len == 0 {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes[..len]).into_owned())
    }

    fn wide_null(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn read_common_settings() -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ui_scaler_from_common_settings_json() {
        let value: Value = serde_json::from_str(r#"{"uiScaler":0.5,"cursorSize":0.25}"#).unwrap();
        assert_eq!(
            value.get(UI_SCALER_FIELD).and_then(Value::as_f64),
            Some(0.5)
        );
        assert_eq!(value.get(CURSOR_SIZE_FIELD).and_then(Value::as_f64), Some(0.25));
    }
}
