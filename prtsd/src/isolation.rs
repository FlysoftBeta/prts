//! Outer instance policy. Jobs provide lifetime/resource control, not tenant security.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobLimits {
    pub active_processes: u32,
    pub process_memory_bytes: usize,
    pub job_memory_bytes: usize,
    /// Hundredths of one percent; None disables CPU throttling.
    pub cpu_rate: Option<u32>,
    /// JOB_OBJECT_UILIMIT_* mask; zero is the qualified baseline.
    pub ui_restrictions: u32,
}
impl Default for JobLimits {
    fn default() -> Self {
        Self {
            active_processes: 64,
            process_memory_bytes: 4 * 1024 * 1024 * 1024,
            job_memory_bytes: 8 * 1024 * 1024 * 1024,
            cpu_rate: None,
            ui_restrictions: 0,
        }
    }
}
impl JobLimits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=4096).contains(&self.active_processes),
            "active_processes must be 1..4096"
        );
        ensure!(
            self.process_memory_bytes >= 16 * 1024 * 1024,
            "process memory must be >=16 MiB"
        );
        ensure!(
            self.job_memory_bytes >= self.process_memory_bytes,
            "job memory must cover process memory"
        );
        ensure!(
            self.cpu_rate.is_none_or(|v| (1..=10000).contains(&v)),
            "cpu_rate must be 1..10000 or null"
        );
        ensure!(
            self.ui_restrictions & !255 == 0,
            "unknown UI restriction bits"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profile: String,
    pub job_limits: JobLimits,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            profile: "job".into(),
            job_limits: JobLimits::default(),
        }
    }
}
impl Config {
    pub fn from_env() -> Result<Self> {
        let mut value: Self = match std::env::var("PRTS_ISOLATION_CONFIG") {
            Ok(path) => {
                serde_json::from_slice(&std::fs::read(path).context("read isolation config")?)?
            }
            Err(std::env::VarError::NotPresent) => Self::default(),
            Err(e) => return Err(e.into()),
        };
        if let Ok(profile) = std::env::var("PRTS_ISOLATION_PROFILE") {
            value.profile = profile;
        }
        if let Ok(limits) = std::env::var("PRTS_JOB_LIMITS") {
            value.job_limits = serde_json::from_str(&limits)?;
        }
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.profile.as_str(),
                "none" | "job" | "appcontainer" | "lpac"
            ),
            "unknown isolation profile"
        );
        self.job_limits.validate()
    }
}
pub fn available_profiles() -> Vec<&'static str> {
    let mut profiles = vec!["none", "job"];
    if std::env::var("PRTS_I1_EXPERIMENTAL").as_deref() == Ok("1")
        && std::env::var("PRTS_INSTALL_CAPABILITY").as_deref() == Ok("1")
    {
        profiles.push("appcontainer");
    }
    profiles
}

#[cfg(windows)]
pub mod state;
#[cfg(windows)]
pub mod win;
