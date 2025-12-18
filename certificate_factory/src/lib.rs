use file_system::{FileReader, FileSystemError, FileWriter};
use log::Logger;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

#[derive(Error, Debug)]
pub enum CertificateFactoryError {
    #[error("Uh-oh")]
    Oops,
    #[error("hams {0}")]
    Something(#[from] rcgen::Error),
    #[error("File System Error {0}")]
    FileSystemError(#[from] FileSystemError),
}

pub trait CertificateFactory {
    fn client_cert(&self) -> Result<String, CertificateFactoryError>;
}

pub struct SelfSignedCertificateFactory {
    root_ca_cert: PathBuf,
    root_ca_pem: PathBuf,
    intermediate_ca_cert: PathBuf,
    intermediate_ca_pem: PathBuf,
    log: Arc<dyn Logger>,
    file_reader: Box<dyn FileReader>,
    file_writer: Box<dyn FileWriter>,
}

impl SelfSignedCertificateFactory {
    pub fn new(
        log: Arc<dyn Logger>,
        file_reader: Box<dyn FileReader>,
        file_writer: Box<dyn FileWriter>,
        ca_path: &Path,
    ) -> Self {
        let mut root_ca_cert = ca_path.to_path_buf();
        let mut root_ca_pem = ca_path.to_path_buf();
        let mut intermediate_ca_cert = ca_path.to_path_buf();
        let mut intermediate_ca_pem = ca_path.to_path_buf();

        root_ca_cert.push("douglas-ca-root.crt");
        root_ca_pem.push("douglas-ca-root.pem");
        intermediate_ca_cert.push("douglas-ca-intermediate.crt");
        intermediate_ca_pem.push("douglas-ca-intermediate.pem");

        Self {
            root_ca_cert,
            root_ca_pem,
            intermediate_ca_cert,
            intermediate_ca_pem,
            log,
            file_reader,
            file_writer,
        }
    }

    fn get_root_certificate_authority(
        &self,
    ) -> Result<Issuer<'_, KeyPair>, CertificateFactoryError> {
        if self.file_reader.exists(&self.root_ca_cert) && self.file_reader.exists(&self.root_ca_pem)
        {
            self.log
                .info("Loading persisted root certificate authority…");
            self.load(&self.root_ca_cert, &self.root_ca_pem)
        } else {
            self.log.info("Creating root certificate authority…");
            self.create_root_certificate_authority()
        }
    }

    fn get_intermediate_ca_issuer(&self) -> Result<Issuer<'_, KeyPair>, CertificateFactoryError> {
        if self.file_reader.exists(&self.intermediate_ca_cert)
            && self.file_reader.exists(&self.intermediate_ca_pem)
        {
            self.log
                .info("Loading persisted intermediate certificate authority…");
            self.load(&self.intermediate_ca_cert, &self.intermediate_ca_pem)
        } else {
            self.log.info("Creating intermedite certificate authority…");
            self.create_intermediate_certificate_authority()
        }
    }

    fn load(
        &self,
        certificate_path: &Path,
        private_key_path: &Path,
    ) -> Result<Issuer<'_, KeyPair>, CertificateFactoryError> {
        let key_pem = self.file_reader.read_all(private_key_path)?;
        let cert_pem = self.file_reader.read_all(certificate_path)?;
        let key_pair = KeyPair::from_pem(&key_pem)?;
        Ok(Issuer::from_ca_cert_pem(&cert_pem, key_pair)?)
    }

    fn create_root_certificate_authority(
        &self,
    ) -> Result<Issuer<'_, KeyPair>, CertificateFactoryError> {
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut certificate_parameters = CertificateParams::new(Vec::new())?;
        let mut root_dn = DistinguishedName::new();
        root_dn.push(DnType::CommonName, "Douglas CA");
        root_dn.push(DnType::OrganizationName, "Douglas");
        root_dn.push(DnType::CountryName, "US");
        certificate_parameters.distinguished_name = root_dn;

        certificate_parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        certificate_parameters.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];

        let certificate = certificate_parameters.self_signed(&key_pair)?;

        self.file_writer
            .write_all(&self.root_ca_pem, &key_pair.serialize_pem())?;
        self.file_writer
            .write_all(&self.root_ca_pem, &certificate.pem())?;

        Ok(Issuer::new(certificate_parameters, key_pair))
    }

    fn create_intermediate_certificate_authority(
        &self,
    ) -> Result<Issuer<'_, KeyPair>, CertificateFactoryError> {
        let root_issuer = self.get_root_certificate_authority()?;
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut certificate_parameters = CertificateParams::new(Vec::new())?;

        let mut inter_dn = DistinguishedName::new();
        inter_dn.push(DnType::CommonName, "Douglas Intermediate CA");
        inter_dn.push(DnType::OrganizationName, "Douglas");

        certificate_parameters.distinguished_name = inter_dn;
        certificate_parameters.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        certificate_parameters.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];

        let certificate = certificate_parameters.signed_by(&key_pair, &root_issuer)?;

        self.file_writer
            .write_all(&self.intermediate_ca_pem, &key_pair.serialize_pem())?;
        self.file_writer
            .write_all(&self.intermediate_ca_cert, &certificate.pem())?;

        Ok(Issuer::new(certificate_parameters, key_pair))
    }
}

impl CertificateFactory for SelfSignedCertificateFactory {
    fn client_cert(&self) -> Result<String, CertificateFactoryError> {
        let inter_issuer = self.get_intermediate_ca_issuer()?;

        let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)?;
        let mut client_params = CertificateParams::new(vec!["douglas.local".to_string()])?;

        let mut client_dn = DistinguishedName::new();
        client_dn.push(DnType::CommonName, "Douglas Client");
        client_params.distinguished_name = client_dn;

        client_params.is_ca = IsCa::NoCa;

        let now = OffsetDateTime::now_utc();
        client_params.not_before = now;
        client_params.not_after = now + Duration::hours(1);

        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        client_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];

        let certificate = client_params.signed_by(&client_key, &inter_issuer)?;

        Ok(certificate.pem())
    }
}
