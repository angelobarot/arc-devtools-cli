/// Human-readable byte size, e.g. `1536.0` → `"1.5KB"`.
pub fn human_bytes(n: f64) -> String {
    if n <= 0.0 {
        return "0B".to_string();
    }
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{}B", v.round() as i64)
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(human_bytes(0.0), "0B");
        assert_eq!(human_bytes(-5.0), "0B");
        assert_eq!(human_bytes(512.0), "512B");
        assert_eq!(human_bytes(1023.0), "1023B");
        assert_eq!(human_bytes(1024.0), "1.0KB");
        assert_eq!(human_bytes(1536.0), "1.5KB");
        assert_eq!(human_bytes(1048576.0), "1.0MB");
    }
}
