//! The pool's certificates (ADR-0031): the coordinator's own certificate
//! authority signs the coordinator's server certificate and one client
//! certificate per member. Keys are ECDSA P-256 (rcgen with ring).

use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DistinguishedName,
    DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PublicKeyData,
    SanType,
};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    #[error("certificate: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PkiError>;

/// A certificate and its key, both PEM.
#[derive(Debug, Clone)]
pub struct Pair {
    pub cert: String,
    pub key: String,
}

const CA_NAME: &str = "devstack pool CA";

/// A new certificate authority for a pool.
pub fn new_ca() -> Result<Pair> {
    let key = KeyPair::generate()?;
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    p.distinguished_name = dn(CA_NAME);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.not_after = years_from_now(20);
    let cert = p.self_signed(&key)?;
    Ok(Pair {
        cert: cert.pem(),
        key: key.serialize_pem(),
    })
}

fn dn(cn: &str) -> DistinguishedName {
    let mut d = DistinguishedName::new();
    d.push(DnType::CommonName, cn);
    d
}

fn years_from_now(years: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::days(years * 365)
}

fn issuer(ca: &Pair) -> Result<Issuer<'static, KeyPair>> {
    let key = KeyPair::from_pem(&ca.key)?;
    Ok(Issuer::from_ca_cert_pem(&ca.cert, key)?)
}

/// The coordinator's server certificate, for the names and addresses members
/// reach it at (an IP address or a DNS name each).
pub fn server_cert(ca: &Pair, names: &[String]) -> Result<Pair> {
    let key = KeyPair::generate()?;
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    p.distinguished_name = dn("devstack coordinator");
    p.subject_alt_names = names
        .iter()
        .map(|n| match n.parse::<std::net::IpAddr>() {
            Ok(ip) => Ok(SanType::IpAddress(ip)),
            Err(_) => n
                .clone()
                .try_into()
                .map(SanType::DnsName)
                .map_err(PkiError::Rcgen),
        })
        .collect::<Result<_>>()?;
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.not_after = years_from_now(10);
    let cert = p.signed_by(&key, &issuer(ca)?)?;
    Ok(Pair {
        cert: cert.pem(),
        key: key.serialize_pem(),
    })
}

/// A member's key and its certificate signing request (`name`: the machine,
/// for the administrator's eyes only).
pub fn member_request(name: &str) -> Result<(String, String)> {
    let key = KeyPair::generate()?;
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    p.distinguished_name = dn(name);
    let csr = p.serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.pem()?))
}

/// The member id of a request: a short hash of its public key, so the same
/// key always asks as the same member.
pub fn member_id(csr_pem: &str) -> Result<String> {
    let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
    let h = blake3::hash(csr.public_key.der_bytes());
    Ok(h.to_hex()[..16].to_string())
}

/// The name a member's certificate is issued for, and what a requester
/// checks when it connects to that member as a node: its identity is its
/// key, wherever it is reached (ADR-0027).
pub fn member_dns(id: &str) -> String {
    format!("{id}.member.devstack")
}

/// Sign a member's request: a certificate whose common name is the member id,
/// for [`member_dns`], good as a client (every member) and as a server (a
/// member that runs lanes as a node).
pub fn sign_member(ca: &Pair, csr_pem: &str, id: &str) -> Result<String> {
    let mut csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
    csr.params.distinguished_name = dn(id);
    csr.params.subject_alt_names = vec![SanType::DnsName(
        member_dns(id).try_into().map_err(PkiError::Rcgen)?,
    )];
    csr.params.is_ca = IsCa::NoCa;
    csr.params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    csr.params.not_after = years_from_now(5);
    Ok(csr.signed_by(&issuer(ca)?)?.pem())
}

/// SHA-256 of the first certificate in a PEM text, hex: what an
/// administrator compares (`devstack join --fingerprint`).
pub fn fingerprint(pem: &str) -> Result<String> {
    let der = first_der(pem)?;
    Ok(hex(&Sha256::digest(&der)))
}

/// SHA-256 of a DER certificate, hex.
pub fn fingerprint_der(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The member id a certificate was issued to (its common name).
pub fn member_of(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let cn = cert.subject().iter_common_name().next()?;
    cn.as_str().ok().map(str::to_string)
}

/// The DER bytes of the first certificate in a PEM text.
pub fn first_der(pem: &str) -> Result<Vec<u8>> {
    use rustls_pki_types::pem::PemObject;
    rustls_pki_types::CertificateDer::from_pem_slice(pem.as_bytes())
        .map(|c| c.to_vec())
        .map_err(|e| PkiError::Other(format!("no certificate in PEM: {e:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_is_signed_by_the_pool_ca() {
        let ca = new_ca().unwrap();
        let server = server_cert(&ca, &["127.0.0.1".into(), "coordinator.local".into()]).unwrap();
        assert!(server.cert.contains("BEGIN CERTIFICATE"));
        let (_key, csr) = member_request("dev-pc").unwrap();
        let id = member_id(&csr).unwrap();
        assert_eq!(id.len(), 16);
        assert_eq!(
            member_id(&csr).unwrap(),
            id,
            "the same key asks as the same member"
        );
        let cert = sign_member(&ca, &csr, &id).unwrap();
        assert_eq!(
            member_of(&first_der(&cert).unwrap()).as_deref(),
            Some(id.as_str())
        );
        assert_ne!(fingerprint(&cert).unwrap(), fingerprint(&ca.cert).unwrap());
        assert_eq!(fingerprint(&ca.cert).unwrap().len(), 64);
    }
}
