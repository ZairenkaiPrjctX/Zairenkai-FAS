//! GPU busy evidence (read-only). Only the Qualcomm kgsl node is supported; other vendors'
//! nodes are not guessed.

use crate::sysroot::SysRoot;

pub const KGSL_BUSY: &str = "/sys/class/kgsl/kgsl-3d0/gpu_busy_percentage";

/// `"37 %"` -> 0.37
pub fn parse_percentage(s: &str) -> Option<f64> {
    let v: f64 = s.trim().trim_end_matches('%').trim().parse().ok()?;
    (0.0..=100.0).contains(&v).then_some(v / 100.0)
}

pub fn gpu_busy(root: &SysRoot) -> Option<f64> {
    root.read_string(KGSL_BUSY)
        .ok()
        .as_deref()
        .and_then(parse_percentage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages() {
        assert_eq!(parse_percentage("37 %"), Some(0.37));
        assert_eq!(parse_percentage("100"), Some(1.0));
        assert_eq!(parse_percentage("120 %"), None);
        assert_eq!(parse_percentage("n/a"), None);
    }
}
