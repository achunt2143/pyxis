//! Error model per spec §21: five standard exit classes.

use std::process::ExitCode;

#[derive(Debug, thiserror::Error)]
pub enum PyxisError {
    /// invalid `pyxis.json`
    #[error("E_CONFIG: {0}")]
    Config(String),
    /// builder failure
    #[error("E_BUILD: {0}")]
    Build(String),
    /// invalid `appinfo.json`
    #[error("E_MANIFEST: {0}")]
    Manifest(String),
    /// invalid package metadata or package build failure
    #[error("E_PACKAGE: {0}")]
    Package(String),
    /// copy/install/launch failure on target
    #[error("E_DEVICE: {0}")]
    Device(String),
}

impl PyxisError {
    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(match self {
            PyxisError::Config(_) => 10,
            PyxisError::Build(_) => 11,
            PyxisError::Manifest(_) => 12,
            PyxisError::Package(_) => 13,
            PyxisError::Device(_) => 14,
        })
    }
}

pub type Result<T> = std::result::Result<T, PyxisError>;
