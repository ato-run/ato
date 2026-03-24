use anyhow::{Context, Result};

pub(super) fn http_get_bytes(url: &str) -> Result<(u16, Vec<u8>)> {
    let url = url.to_string();
    let panic_url = url.clone();
    std::thread::spawn(move || -> Result<(u16, Vec<u8>)> {
        let response = reqwest::blocking::get(&url)
            .with_context(|| format!("Failed to download from: {}", url))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .with_context(|| format!("Failed to read response body from: {}", url))?;
        Ok((status, bytes.to_vec()))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("HTTP worker thread panicked while fetching {}", panic_url))?
}

pub(super) fn http_get_text(url: &str) -> Result<(u16, String)> {
    let url = url.to_string();
    let panic_url = url.clone();
    std::thread::spawn(move || -> Result<(u16, String)> {
        let response = reqwest::blocking::get(&url)
            .with_context(|| format!("Failed to download from: {}", url))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .with_context(|| format!("Failed to read response body from: {}", url))?;
        Ok((status, text))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("HTTP worker thread panicked while fetching {}", panic_url))?
}
