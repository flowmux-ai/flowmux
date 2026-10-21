// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

pub(super) fn is_lottie_json(path: &Path) -> bool {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        || !path.metadata().is_ok_and(|meta| meta.is_file())
    {
        return false;
    }
    let Some(value) = read_lottie_data(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
    else {
        return false;
    };
    value["v"].is_string()
        && value["layers"].is_array()
        && ["w", "h", "fr"]
            .iter()
            .all(|key| value[key].as_f64().is_some_and(|n| n > 0.0))
        && value["ip"]
            .as_f64()
            .zip(value["op"].as_f64())
            .is_some_and(|(start, end)| end > start)
}

#[cfg(all(test, feature = "native-smoke"))]
mod smoke;
#[cfg(all(test, feature = "native-smoke"))]
pub(crate) use smoke::check;
