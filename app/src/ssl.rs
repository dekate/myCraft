//! HTTPS for `<project>.go`. A local CA (`etc\ssl\mycraft-ca.crt` + key) is
//! created once; it signs one server certificate naming localhost and every
//! project folder, re-issued whenever the project list changes. Trusting the
//! CA (current user's Root store) makes browsers accept it.

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const CA_NAME: &str = "myCraft Local CA";

pub fn dir() -> PathBuf {
    crate::mycraft_root().join("etc").join("ssl")
}

pub fn ca_cert_path() -> PathBuf {
    dir().join("mycraft-ca.crt")
}

fn ca_params() -> CertificateParams {
    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, CA_NAME);
    p.distinguished_name.push(DnType::OrganizationName, "myCraft");
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    p
}

/// The CA (created on first use). The certificate object is re-derived from
/// the saved key with the same name, which is all signing needs.
fn ca() -> Result<(rcgen::Certificate, KeyPair), String> {
    let key_path = dir().join("mycraft-ca.key");
    let crt_path = ca_cert_path();
    let key = match std::fs::read_to_string(&key_path) {
        Ok(pem) => KeyPair::from_pem(&pem).map_err(|e| format!("{}: {e}", key_path.display()))?,
        Err(_) => {
            std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
            let key = KeyPair::generate().map_err(|e| e.to_string())?;
            std::fs::write(&key_path, key.serialize_pem()).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&crt_path);
            key
        }
    };
    let cert = ca_params().self_signed(&key).map_err(|e| e.to_string())?;
    if !crt_path.is_file() {
        std::fs::write(&crt_path, cert.pem()).map_err(|e| e.to_string())?;
    }
    Ok((cert, key))
}

pub fn ensure_ca() -> Result<PathBuf, String> {
    ca().map(|_| ca_cert_path())
}

/// Hostnames the server certificate covers: localhost plus `<project><domain>`.
pub fn host_names(www_root: &Path) -> Vec<String> {
    let cfg = crate::settings::current();
    let mut names: Vec<String> = std::fs::read_dir(www_root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .filter(|n| !n.starts_with(['.', '_']) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .flat_map(|n| {
                    let mut list = vec![cfg.project_hostname(&n)];
                    let go_name = format!("{n}.go");
                    if !list.contains(&go_name) {
                        list.push(go_name);
                    }
                    list
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    names.insert(0, "localhost".into());
    names
}

fn build_config(names: &[String]) -> Result<Arc<rustls::ServerConfig>, String> {
    let (ca_cert, ca_key) = ca()?;
    let mut p = CertificateParams::new(names.to_vec()).map_err(|e| e.to_string())?;
    p.subject_alt_names.push(SanType::IpAddress(std::net::IpAddr::from([127, 0, 0, 1])));
    p.distinguished_name.push(DnType::CommonName, "myCraft local sites");
    // Two years from now at most; browsers dislike longer leaf lifetimes.
    let year = 1970 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) / 31_556_952) as i32;
    p.not_before = rcgen::date_time_ymd(year - 1, 1, 1);
    p.not_after = rcgen::date_time_ymd(year + 1, 1, 1);
    let key = KeyPair::generate().map_err(|e| e.to_string())?;
    let cert = p.signed_by(&key, &ca_cert, &ca_key).map_err(|e| e.to_string())?;

    let chain = vec![CertificateDer::from(cert.der().to_vec()), CertificateDer::from(ca_cert.der().to_vec())];
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key_der)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(cfg))
}

static CACHE: Mutex<Option<(Vec<String>, Arc<rustls::ServerConfig>)>> = Mutex::new(None);

/// TLS settings for the current project list (re-issued when it changes).
pub fn server_config(www_root: &Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let names = host_names(www_root);
    let mut cache = CACHE.lock().unwrap();
    if let Some((n, cfg)) = cache.as_ref() {
        if *n == names {
            return Ok(cfg.clone());
        }
    }
    let cfg = build_config(&names)?;
    *cache = Some((names, cfg.clone()));
    Ok(cfg)
}

/// Whether the CA is in the current user's Root store.
pub fn is_trusted() -> bool {
    let mut cmd = std::process::Command::new("certutil");
    cmd.args(["-user", "-store", "Root", CA_NAME]);
    crate::hide_console(&mut cmd).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Add the CA to the current user's Root store. Windows asks the user to
/// confirm, so this blocks until they answer; run it off the UI thread.
pub fn trust() -> Result<(), String> {
    let crt = ensure_ca()?;
    let mut cmd = std::process::Command::new("certutil");
    cmd.args(["-user", "-addstore", "Root"]).arg(&crt);
    let out = crate::hide_console(&mut cmd).output().map_err(|e| format!("could not run certutil: {e}"))?;
    if out.status.success() && is_trusted() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&out.stdout);
        Err(text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("cancelled").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_a_cert_rustls_accepts() {
        let names = vec!["localhost".to_string(), "demo.go".to_string()];
        // Uses a scratch root so the real etc\ssl is untouched.
        let (ca_cert, ca_key) = {
            let key = KeyPair::generate().unwrap();
            (ca_params().self_signed(&key).unwrap(), key)
        };
        let p = CertificateParams::new(names).unwrap();
        let key = KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &ca_cert, &ca_key).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.der().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
    }
}
