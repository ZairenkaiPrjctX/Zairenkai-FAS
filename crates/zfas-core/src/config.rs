//! Versioned configuration.
//!
//! The user file holds only what the user changed; everything else comes from compiled
//! defaults. Unknown keys are errors (a typo must not silently fall back to a default). Files
//! from older schema versions are migrated step by step; files from newer versions are
//! refused rather than guessed at. Writing is the caller's job and must be atomic (see
//! `zfas-platform::fsutil::write_atomic`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analyzer::AnalyzerConfig;
use crate::cpcs::CpcsConfig;
use crate::policy::{ProfileName, ProfileParams};
use crate::target::TargetConfig;
use crate::thermal::ThermalPolicy;

pub const SCHEMA_VERSION: u32 = 1;

/// One migration: rewrite a table from version `n` to `n + 1`.
pub type Migration = fn(&mut toml::Table) -> Result<(), String>;

/// `MIGRATIONS[i]` migrates version `i + 1` to `i + 2`. Empty while only version 1 exists.
pub const MIGRATIONS: &[Migration] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Observe and report decisions, never write control nodes.
    Observe,
    /// Apply decisions through the control backend.
    Control,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub enabled: bool,
    pub mode: Mode,
    /// Default profile. ADAPTIVE is available but not the default until it is validated on
    /// hardware (see docs/TESTING.md).
    pub profile: ProfileName,
    /// Controller period.
    pub tick_ms: u64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: Mode::Control,
            profile: ProfileName::Balanced,
            tick_ms: 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Control {
    /// Use cpufreq `scaling_min_freq` floors.
    pub cpu: bool,
    /// cpufreq policies never touched, by directory name (e.g. `policy0`).
    pub exclude_policies: Vec<String>,
    /// Check every N ticks that our values are still in place.
    pub verify_every_ticks: u32,
    /// After a control fault, wait this long before trying to control again.
    pub fault_backoff_ms: u64,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            cpu: true,
            exclude_policies: Vec::new(),
            verify_every_ticks: 6,
            fault_backoff_ms: 30_000,
        }
    }
}

/// Raw-sensor thresholds. None are built in: they must come from the user or a validated
/// device profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneThreshold {
    /// Thermal zone `type` (exact match), e.g. `cpu-1-0-usr`.
    pub zone_type: String,
    pub derate_mc: Option<i64>,
    pub suspend_mc: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thermal {
    /// Highest boost level per Android thermal status (index = status code 0..=6).
    pub level_cap_by_status: [u8; 7],
    /// Cap when no thermal status can be read.
    pub unknown_cap: u8,
    pub recovery_ms: u64,
    pub zones: Vec<ZoneThreshold>,
}

impl Default for Thermal {
    fn default() -> Self {
        let p = ThermalPolicy::default();
        Self {
            level_cap_by_status: p.level_cap_by_status,
            unknown_cap: p.unknown_cap,
            recovery_ms: p.recovery_ms,
            zones: Vec::new(),
        }
    }
}

impl Thermal {
    pub fn policy(&self) -> ThermalPolicy {
        ThermalPolicy {
            level_cap_by_status: self.level_cap_by_status,
            unknown_cap: self.unknown_cap,
            recovery_ms: self.recovery_ms,
        }
    }
}

/// Per-profile parameter overrides; unset fields keep the built-in value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileOverride {
    pub max_level: Option<u8>,
    pub unknown_max_level: Option<u8>,
    pub short_level: Option<u8>,
    pub step_up: Option<u8>,
    pub step_down: Option<u8>,
    pub degrade_miss_ratio: Option<f64>,
    pub recover_miss_ratio: Option<f64>,
    pub consecutive_trigger: Option<u32>,
    pub eval_ticks: Option<u32>,
    pub cooldown_ms: Option<u64>,
    pub short_boost_ms: Option<u64>,
    pub max_boost_ms: Option<u64>,
    pub recovery_ms: Option<u64>,
    pub min_confidence: Option<f64>,
}

impl ProfileOverride {
    pub fn apply(&self, mut p: ProfileParams) -> ProfileParams {
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = self.$f { p.$f = v; } )* };
        }
        set!(
            max_level,
            unknown_max_level,
            short_level,
            step_up,
            step_down,
            degrade_miss_ratio,
            recover_miss_ratio,
            consecutive_trigger,
            eval_ticks,
            cooldown_ms,
            short_boost_ms,
            max_boost_ms,
            recovery_ms,
            min_confidence
        );
        p
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppOverride {
    pub enabled: Option<bool>,
    pub profile: Option<ProfileName>,
    /// Candidate target frame rates; empty means the default ladder.
    pub target_fps: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub general: General,
    pub control: Control,
    pub analyzer: AnalyzerConfig,
    pub target: TargetConfig,
    pub thermal: Thermal,
    pub cpcs: CpcsConfig,
    pub profiles: BTreeMap<String, ProfileOverride>,
    /// Per-package overrides, keyed by Android package name.
    pub apps: BTreeMap<String, AppOverride>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            general: General::default(),
            control: Control::default(),
            analyzer: AnalyzerConfig::default(),
            target: TargetConfig::default(),
            thermal: Thermal::default(),
            cpcs: CpcsConfig::default(),
            profiles: BTreeMap::new(),
            apps: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Syntax(String),
    Version(String),
    Migration { from: u32, detail: String },
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Syntax(e) => write!(f, "config syntax: {e}"),
            ConfigError::Version(e) => write!(f, "config version: {e}"),
            ConfigError::Migration { from, detail } => {
                write!(f, "config migration from v{from}: {detail}")
            }
            ConfigError::Invalid(e) => write!(f, "invalid config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Android package names: letters, digits, `_` and `.` (also blocks any shell or path tricks).
pub fn valid_package(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && !name.starts_with('.')
}

/// Settings resolved for one foreground app.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSettings {
    pub enabled: bool,
    pub profile: ProfileName,
    pub params: ProfileParams,
    pub target_fps: Vec<u32>,
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let inv = |m: String| Err(ConfigError::Invalid(m));
        if self.schema_version != SCHEMA_VERSION {
            return inv(format!(
                "schema_version {} after migration",
                self.schema_version
            ));
        }
        if !(100..=5_000).contains(&self.general.tick_ms) {
            return inv("general.tick_ms must be within 100..=5000".into());
        }
        if self.control.verify_every_ticks == 0 {
            return inv("control.verify_every_ticks must be at least 1".into());
        }
        for p in &self.control.exclude_policies {
            if !p.starts_with("policy")
                || !p[6..].chars().all(|c| c.is_ascii_digit())
                || p.len() == 6
            {
                return inv(format!(
                    "control.exclude_policies: '{p}' is not a cpufreq policy name"
                ));
            }
        }
        let a = &self.analyzer;
        if a.window_frames < a.min_samples || a.min_samples < 10 || a.window_frames > 4_096 {
            return inv("analyzer: need 10 <= min_samples <= window_frames <= 4096".into());
        }
        if !(0.0..=1.0).contains(&a.deadline_tolerance)
            || !(a.ewma_alpha > 0.0 && a.ewma_alpha <= 1.0)
        {
            return inv("analyzer: deadline_tolerance in 0..=1 and ewma_alpha in (0, 1]".into());
        }
        if a.max_interval_ns == 0 || a.idle_timeout_ns < a.max_interval_ns {
            return inv("analyzer: idle_timeout_ns must be >= max_interval_ns > 0".into());
        }
        self.target.validate().map_err(ConfigError::Invalid)?;
        self.thermal
            .policy()
            .validate()
            .map_err(ConfigError::Invalid)?;
        for z in &self.thermal.zones {
            if z.zone_type.is_empty() {
                return inv("thermal.zones: zone_type must not be empty".into());
            }
            if let (Some(d), Some(s)) = (z.derate_mc, z.suspend_mc) {
                if d >= s {
                    return inv(format!(
                        "thermal zone {}: derate_mc must be below suspend_mc",
                        z.zone_type
                    ));
                }
            }
        }
        self.cpcs.validate().map_err(ConfigError::Invalid)?;
        for name in self.profiles.keys() {
            let Some(p) = ProfileName::parse(name) else {
                return inv(format!("profiles.{name}: unknown profile"));
            };
            self.profile_params(p)
                .validate()
                .map_err(|e| ConfigError::Invalid(format!("profiles.{name}: {e}")))?;
        }
        for (pkg, o) in &self.apps {
            if !valid_package(pkg) {
                return inv(format!("apps: '{pkg}' is not a valid package name"));
            }
            if o.target_fps.iter().any(|f| !(10..=240).contains(f)) {
                return inv(format!(
                    "apps.{pkg}.target_fps values must be within 10..=240"
                ));
            }
        }
        Ok(())
    }

    pub fn profile_params(&self, p: ProfileName) -> ProfileParams {
        let base = ProfileParams::for_profile(p);
        match self.profiles.get(p.as_str()) {
            Some(o) => o.apply(base),
            None => base,
        }
    }

    pub fn for_app(&self, package: Option<&str>) -> AppSettings {
        let o = package.and_then(|p| self.apps.get(p));
        let profile = o.and_then(|o| o.profile).unwrap_or(self.general.profile);
        AppSettings {
            enabled: self.general.enabled && o.and_then(|o| o.enabled).unwrap_or(true),
            profile,
            params: self.profile_params(profile),
            target_fps: o.map(|o| o.target_fps.clone()).unwrap_or_default(),
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config is always serializable")
    }
}

/// Bring a parsed table to the current schema version. Returns migration notes.
pub fn migrate(
    table: &mut toml::Table,
    migrations: &[Migration],
    current: u32,
) -> Result<Vec<String>, ConfigError> {
    let mut notes = Vec::new();
    let version = match table.get("schema_version") {
        None => {
            notes.push(format!("schema_version missing; treated as {current}"));
            current
        }
        Some(toml::Value::Integer(v)) if *v >= 1 && *v <= u32::MAX as i64 => *v as u32,
        Some(v) => {
            return Err(ConfigError::Version(format!(
                "schema_version must be a positive integer, got {v}"
            )))
        }
    };
    if version > current {
        return Err(ConfigError::Version(format!(
            "file uses schema {version}, this build understands up to {current}; refusing to guess"
        )));
    }
    for from in version..current {
        let step = migrations
            .get(from as usize - 1)
            .ok_or_else(|| ConfigError::Migration {
                from,
                detail: "no migration step".into(),
            })?;
        step(table).map_err(|detail| ConfigError::Migration { from, detail })?;
        notes.push(format!("migrated schema {from} -> {}", from + 1));
    }
    table.insert(
        "schema_version".into(),
        toml::Value::Integer(current as i64),
    );
    Ok(notes)
}

/// Parse, migrate and validate a user config file.
pub fn parse(text: &str) -> Result<(Config, Vec<String>), ConfigError> {
    let mut table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| ConfigError::Syntax(e.to_string()))?;
    let notes = migrate(&mut table, MIGRATIONS, SCHEMA_VERSION)?;
    let cfg: Config = table
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::Invalid(e.to_string()))?;
    cfg.validate()?;
    Ok((cfg, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_roundtrip() {
        let c = Config::default();
        c.validate().unwrap();
        let (back, notes) = parse(&c.to_toml()).unwrap();
        assert_eq!(back, c);
        assert!(notes.is_empty());
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let (c, notes) =
            parse("schema_version = 1\n[general]\nprofile = \"performance\"\n").unwrap();
        assert_eq!(c.general.profile, ProfileName::Performance);
        assert_eq!(c.general.tick_ms, 500);
        assert!(notes.is_empty());
        let (_, notes) = parse("").unwrap();
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        assert!(matches!(
            parse("[general]\nprofil = \"x\"\n"),
            Err(ConfigError::Invalid(_))
        ));
        assert!(parse("[general]\ntick_ms = 10\n").is_err());
        assert!(parse("[apps.\"com.x;rm -rf\"]\nenabled = true\n").is_err());
        assert!(parse("[apps.\"com.game\"]\ntarget_fps = [1000]\n").is_err());
        assert!(parse("[control]\nexclude_policies = [\"../x\"]\n").is_err());
        assert!(parse("[profiles.balanced]\nmax_level = 11\n").is_err());
        assert!(parse("[profiles.turbo]\nmax_level = 5\n").is_err());
        assert!(parse("[thermal]\nlevel_cap_by_status = [10, 6, 3, 2, 0, 0, 0]\n").is_err());
        assert!(matches!(parse("[general\n"), Err(ConfigError::Syntax(_))));
    }

    #[test]
    fn newer_schema_is_refused() {
        assert!(matches!(
            parse("schema_version = 99\n"),
            Err(ConfigError::Version(_))
        ));
        assert!(matches!(
            parse("schema_version = 0\n"),
            Err(ConfigError::Version(_))
        ));
    }

    #[test]
    fn migration_steps_run_in_order() {
        fn v1_to_v2(t: &mut toml::Table) -> Result<(), String> {
            let old = t.remove("old_key").ok_or("old_key missing")?;
            t.insert("new_key".into(), old);
            Ok(())
        }
        let mut t: toml::Table = "schema_version = 1\nold_key = 3\n".parse().unwrap();
        let notes = migrate(&mut t, &[v1_to_v2], 2).unwrap();
        assert_eq!(notes, vec!["migrated schema 1 -> 2".to_string()]);
        assert_eq!(t["new_key"].as_integer(), Some(3));
        assert_eq!(t["schema_version"].as_integer(), Some(2));
        let mut t: toml::Table = "schema_version = 1\n".parse().unwrap();
        assert!(matches!(
            migrate(&mut t, &[v1_to_v2], 2),
            Err(ConfigError::Migration { from: 1, .. })
        ));
    }

    #[test]
    fn app_overrides_resolve() {
        let (c, _) = parse(
            "[apps.\"com.example.game\"]\nprofile = \"fast_response\"\ntarget_fps = [60, 120]\n\
             [apps.\"com.example.reader\"]\nenabled = false\n\
             [profiles.fast_response]\nmax_level = 7\n",
        )
        .unwrap();
        let g = c.for_app(Some("com.example.game"));
        assert_eq!(g.profile, ProfileName::FastResponse);
        assert_eq!(g.params.max_level, 7);
        assert_eq!(g.target_fps, vec![60, 120]);
        assert!(!c.for_app(Some("com.example.reader")).enabled);
        assert_eq!(c.for_app(None).profile, ProfileName::Balanced);
    }
}
