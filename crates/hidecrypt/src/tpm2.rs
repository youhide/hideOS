//! TPM 2.0, as far as sealing the volume key needs: a storage primary key
//! made the same way every time, a sealed data object under it whose policy
//! is a set of PCR values, and unsealing it through a policy session. The
//! commands are marshalled here, by the TPM 2.0 Library specification, Part
//! 3; the caller moves the bytes to and from `/dev/tpmrm0`.
//!
//! The policy is PCR 7: the Secure Boot state and its keys. Firmware that
//! boots the same signed chain with the same keys gives the same value, so
//! updates do not need a new seal; turning Secure Boot off, or enrolling
//! other keys, changes it, and the volume asks for its passphrase.
//!
//! The session is not salted, so the unsealed key crosses the bus to a
//! discrete TPM in the clear. Firmware TPMs, in most laptops since 2017,
//! have no bus to sniff; parameter encryption is the next step for the rest.

use crate::Error;

const ST_NO_SESSIONS: u16 = 0x8001;
const ST_SESSIONS: u16 = 0x8002;

const CC_CREATE_PRIMARY: u32 = 0x131;
const CC_CREATE: u32 = 0x153;
const CC_LOAD: u32 = 0x157;
const CC_UNSEAL: u32 = 0x15E;
const CC_FLUSH_CONTEXT: u32 = 0x165;
const CC_START_AUTH_SESSION: u32 = 0x176;
const CC_POLICY_PCR: u32 = 0x17F;
const CC_POLICY_GET_DIGEST: u32 = 0x189;

const RH_OWNER: u32 = 0x4000_0001;
const RH_NULL: u32 = 0x4000_0007;
const RS_PW: u32 = 0x4000_0009;

const ALG_SHA256: u16 = 0x000B;
const ALG_AES: u16 = 0x0006;
const ALG_CFB: u16 = 0x0043;
const ALG_NULL: u16 = 0x0010;
const ALG_ECC: u16 = 0x0023;
const ALG_KEYEDHASH: u16 = 0x0008;
const ECC_NIST_P256: u16 = 0x0003;

const SE_POLICY: u8 = 0x01;
const SE_TRIAL: u8 = 0x03;

const FIXED_TPM: u32 = 1 << 1;
const FIXED_PARENT: u32 = 1 << 4;
const SENSITIVE_DATA_ORIGIN: u32 = 1 << 5;
const USER_WITH_AUTH: u32 = 1 << 6;
const NO_DA: u32 = 1 << 10;
const RESTRICTED: u32 = 1 << 16;
const DECRYPT: u32 = 1 << 17;

/// The response code a policy that does not match gives, at any layer.
const RC_POLICY_FAIL: u32 = 0x099D;

/// Moves one command to the TPM and its response back.
pub trait Transport {
    fn transact(&mut self, command: &[u8]) -> std::io::Result<Vec<u8>>;
}

/// The sealed volume key, as it is stored: not secret — only the TPM it was
/// sealed by can open it, and only while the PCRs match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub pcrs: u32,
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

const BLOB_MAGIC: &[u8; 8] = b"HIDETPM2";
const BLOB_VERSION: u8 = 1;

impl Sealed {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(BLOB_MAGIC);
        out.push(BLOB_VERSION);
        out.extend_from_slice(&self.pcrs.to_be_bytes());
        put_b(&mut out, &self.private);
        put_b(&mut out, &self.public);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Sealed, Error> {
        let mut r = Reader::new(bytes);
        if r.take(8)? != BLOB_MAGIC.as_slice() || r.u8()? != BLOB_VERSION {
            return Err(Error::Tpm("not a hideOS TPM2 blob".into()));
        }
        let pcrs = r.u32()?;
        let private = r.b()?.to_vec();
        let public = r.b()?.to_vec();
        Ok(Sealed {
            pcrs,
            private,
            public,
        })
    }
}

/// Seals `secret` to the current values of `pcrs`, a bitmap of PCR
/// indices in the SHA-256 bank.
pub fn seal(tpm: &mut impl Transport, secret: &[u8], pcrs: u32) -> Result<Sealed, Error> {
    let policy = trial_policy(tpm, pcrs)?;
    let primary = create_primary(tpm)?;
    let result = (|| {
        let mut cmd = Command::new(ST_SESSIONS, CC_CREATE);
        cmd.handle(primary);
        cmd.password_auth();
        // TPM2B_SENSITIVE_CREATE: no auth value, the secret as data.
        let mut sensitive = Vec::new();
        put_b(&mut sensitive, &[]);
        put_b(&mut sensitive, secret);
        put_b(&mut cmd.params, &sensitive);
        put_b(&mut cmd.params, &sealed_template(&policy));
        put_b(&mut cmd.params, &[]);
        cmd.params.extend_from_slice(&0u32.to_be_bytes());
        let response = run(tpm, cmd.finish())?;
        let mut r = Reader::new(&response);
        let _parameter_size = r.u32()?;
        let private = r.b()?.to_vec();
        let public = r.b()?.to_vec();
        Ok(Sealed {
            pcrs,
            private,
            public,
        })
    })();
    flush(tpm, primary);
    result
}

/// The secret, if the PCRs are what they were when it was sealed.
/// `Ok(None)` when they are not: a policy that does not match is the
/// expected outcome after a change to the boot chain, not an error.
pub fn unseal(tpm: &mut impl Transport, sealed: &Sealed) -> Result<Option<Vec<u8>>, Error> {
    let primary = create_primary(tpm)?;
    let result = (|| {
        let mut cmd = Command::new(ST_SESSIONS, CC_LOAD);
        cmd.handle(primary);
        cmd.password_auth();
        put_b(&mut cmd.params, &sealed.private);
        put_b(&mut cmd.params, &sealed.public);
        let response = run(tpm, cmd.finish())?;
        let object = Reader::new(&response).u32()?;

        let session = start_session(tpm, SE_POLICY)?;
        let unsealed = (|| {
            policy_pcr(tpm, session, sealed.pcrs)?;
            let mut cmd = Command::new(ST_SESSIONS, CC_UNSEAL);
            cmd.handle(object);
            cmd.policy_auth(session);
            match run(tpm, cmd.finish()) {
                Ok(response) => {
                    let mut r = Reader::new(&response);
                    let _parameter_size = r.u32()?;
                    Ok(Some(r.b()?.to_vec()))
                }
                Err(Error::TpmCode(code)) if code & 0xFFF == RC_POLICY_FAIL => Ok(None),
                Err(error) => Err(error),
            }
        })();
        // A failed command leaves the session open; a successful unseal
        // has closed it, and flushing it again is a harmless error.
        flush(tpm, session);
        flush(tpm, object);
        unsealed
    })();
    flush(tpm, primary);
    result
}

/// The policy digest of PolicyPCR over the current values, from a trial
/// session: the TPM computes it, so this code need not hash PCR values.
fn trial_policy(tpm: &mut impl Transport, pcrs: u32) -> Result<Vec<u8>, Error> {
    let session = start_session(tpm, SE_TRIAL)?;
    let result = (|| {
        policy_pcr(tpm, session, pcrs)?;
        let mut cmd = Command::new(ST_NO_SESSIONS, CC_POLICY_GET_DIGEST);
        cmd.handle(session);
        let response = run(tpm, cmd.finish())?;
        Ok(Reader::new(&response).b()?.to_vec())
    })();
    flush(tpm, session);
    result
}

/// The storage primary key in the owner hierarchy. The template fixes it:
/// the same TPM derives the same key from its seed every time, so nothing
/// needs to be persisted in the TPM.
fn create_primary(tpm: &mut impl Transport) -> Result<u32, Error> {
    let mut cmd = Command::new(ST_SESSIONS, CC_CREATE_PRIMARY);
    cmd.handle(RH_OWNER);
    cmd.password_auth();
    // TPM2B_SENSITIVE_CREATE with an empty auth value and no data.
    put_b(&mut cmd.params, &[0, 0, 0, 0]);
    put_b(&mut cmd.params, &primary_template());
    put_b(&mut cmd.params, &[]);
    cmd.params.extend_from_slice(&0u32.to_be_bytes());
    let response = run(tpm, cmd.finish())?;
    Reader::new(&response).u32()
}

fn start_session(tpm: &mut impl Transport, kind: u8) -> Result<u32, Error> {
    let mut cmd = Command::new(ST_NO_SESSIONS, CC_START_AUTH_SESSION);
    cmd.handle(RH_NULL);
    cmd.handle(RH_NULL);
    // A caller nonce of the hash's size; its value does not matter for an
    // unbound, unsalted session.
    put_b(&mut cmd.params, &[0x5a; 32]);
    put_b(&mut cmd.params, &[]);
    cmd.params.push(kind);
    cmd.params.extend_from_slice(&ALG_NULL.to_be_bytes());
    cmd.params.extend_from_slice(&ALG_SHA256.to_be_bytes());
    let response = run(tpm, cmd.finish())?;
    Reader::new(&response).u32()
}

fn policy_pcr(tpm: &mut impl Transport, session: u32, pcrs: u32) -> Result<(), Error> {
    let mut cmd = Command::new(ST_NO_SESSIONS, CC_POLICY_PCR);
    cmd.handle(session);
    // An empty digest: the TPM uses the PCRs' current values.
    put_b(&mut cmd.params, &[]);
    put_pcr_selection(&mut cmd.params, pcrs);
    run(tpm, cmd.finish()).map(|_| ())
}

fn flush(tpm: &mut impl Transport, handle: u32) {
    let mut cmd = Command::new(ST_NO_SESSIONS, CC_FLUSH_CONTEXT);
    cmd.params.extend_from_slice(&handle.to_be_bytes());
    let _ = run(tpm, cmd.finish());
}

/// The TCG's ECC P-256 storage key template (TCG EK Credential Profile,
/// "SRK template"), with an empty unique field.
pub fn primary_template() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&ALG_ECC.to_be_bytes());
    t.extend_from_slice(&ALG_SHA256.to_be_bytes());
    let attributes = FIXED_TPM
        | FIXED_PARENT
        | SENSITIVE_DATA_ORIGIN
        | USER_WITH_AUTH
        | NO_DA
        | RESTRICTED
        | DECRYPT;
    t.extend_from_slice(&attributes.to_be_bytes());
    put_b(&mut t, &[]);
    // TPMS_ECC_PARMS: AES-128-CFB for children, no scheme, P-256, no KDF.
    t.extend_from_slice(&ALG_AES.to_be_bytes());
    t.extend_from_slice(&128u16.to_be_bytes());
    t.extend_from_slice(&ALG_CFB.to_be_bytes());
    t.extend_from_slice(&ALG_NULL.to_be_bytes());
    t.extend_from_slice(&ECC_NIST_P256.to_be_bytes());
    t.extend_from_slice(&ALG_NULL.to_be_bytes());
    put_b(&mut t, &[0; 32]);
    put_b(&mut t, &[0; 32]);
    t
}

/// A sealed data object: no user auth, only the policy.
fn sealed_template(policy: &[u8]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&ALG_KEYEDHASH.to_be_bytes());
    t.extend_from_slice(&ALG_SHA256.to_be_bytes());
    t.extend_from_slice(&(FIXED_TPM | FIXED_PARENT | NO_DA).to_be_bytes());
    put_b(&mut t, policy);
    t.extend_from_slice(&ALG_NULL.to_be_bytes());
    put_b(&mut t, &[]);
    t
}

/// TPML_PCR_SELECTION with one bank, SHA-256, and PCRs 0 to 23.
pub fn put_pcr_selection(out: &mut Vec<u8>, pcrs: u32) {
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&ALG_SHA256.to_be_bytes());
    out.push(3);
    out.extend_from_slice(&[
        (pcrs & 0xff) as u8,
        ((pcrs >> 8) & 0xff) as u8,
        ((pcrs >> 16) & 0xff) as u8,
    ]);
}

/// A command being marshalled: handles, authorizations, parameters.
struct Command {
    tag: u16,
    code: u32,
    handles: Vec<u8>,
    auth: Vec<u8>,
    params: Vec<u8>,
}

impl Command {
    fn new(tag: u16, code: u32) -> Command {
        Command {
            tag,
            code,
            handles: Vec::new(),
            auth: Vec::new(),
            params: Vec::new(),
        }
    }

    fn handle(&mut self, handle: u32) {
        self.handles.extend_from_slice(&handle.to_be_bytes());
    }

    /// The empty password, for the owner hierarchy and the primary key.
    fn password_auth(&mut self) {
        self.auth.extend_from_slice(&RS_PW.to_be_bytes());
        put_b(&mut self.auth, &[]);
        self.auth.push(0);
        put_b(&mut self.auth, &[]);
    }

    /// A policy session that asserted no auth value: no HMAC.
    fn policy_auth(&mut self, session: u32) {
        self.auth.extend_from_slice(&session.to_be_bytes());
        put_b(&mut self.auth, &[]);
        self.auth.push(0);
        put_b(&mut self.auth, &[]);
    }

    fn finish(self) -> Vec<u8> {
        let mut body = self.handles;
        if self.tag == ST_SESSIONS {
            body.extend_from_slice(&(self.auth.len() as u32).to_be_bytes());
            body.extend_from_slice(&self.auth);
        }
        body.extend_from_slice(&self.params);
        let mut out = Vec::with_capacity(10 + body.len());
        out.extend_from_slice(&self.tag.to_be_bytes());
        out.extend_from_slice(&((10 + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&self.code.to_be_bytes());
        out.extend_from_slice(&body);
        out
    }
}

/// Sends a command and returns the response after its header, or the
/// response code as an error.
fn run(tpm: &mut impl Transport, command: Vec<u8>) -> Result<Vec<u8>, Error> {
    let response = tpm.transact(&command)?;
    let mut r = Reader::new(&response);
    let _tag = r.u16()?;
    let size = r.u32()? as usize;
    let code = r.u32()?;
    if code != 0 {
        return Err(Error::TpmCode(code));
    }
    response
        .get(10..size.min(response.len()))
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::Tpm("short response".into()))
}

fn put_b(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let slice = self
            .bytes
            .get(self.at..self.at + n)
            .ok_or_else(|| Error::Tpm("short response".into()))?;
        self.at += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?.first().copied().unwrap_or_default())
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([
            b.first().copied().unwrap_or_default(),
            b.get(1).copied().unwrap_or_default(),
        ]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        let mut a = [0u8; 4];
        a.copy_from_slice(b);
        Ok(u32::from_be_bytes(a))
    }

    fn b(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u16()? as usize;
        self.take(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blob_round_trips() {
        let sealed = Sealed {
            pcrs: 1 << 7,
            private: vec![1, 2, 3],
            public: vec![4, 5],
        };
        assert_eq!(Sealed::from_bytes(&sealed.to_bytes()).unwrap(), sealed);
        assert!(Sealed::from_bytes(b"HIDETPM2\x02").is_err());
        assert!(Sealed::from_bytes(b"short").is_err());
    }

    #[test]
    fn pcr_7_is_bit_7_of_the_first_byte() {
        let mut out = Vec::new();
        put_pcr_selection(&mut out, 1 << 7);
        assert_eq!(out, [0, 0, 0, 1, 0, 0x0B, 3, 0x80, 0, 0]);
    }

    #[test]
    fn the_primary_template_is_the_tcg_srk() {
        let t = primary_template();
        // type ECC, nameAlg SHA-256, attributes 0x00030472.
        assert_eq!(&t[..8], &[0x00, 0x23, 0x00, 0x0B, 0x00, 0x03, 0x04, 0x72]);
        assert_eq!(t.len(), 8 + 2 + 12 + 2 * (2 + 32));
    }

    #[test]
    fn a_command_carries_its_size_and_authorization() {
        let mut cmd = Command::new(ST_SESSIONS, CC_LOAD);
        cmd.handle(0x8000_0000);
        cmd.password_auth();
        let bytes = cmd.finish();
        assert_eq!(&bytes[..2], &[0x80, 0x02]);
        assert_eq!(
            u32::from_be_bytes(bytes[2..6].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert_eq!(&bytes[6..10], &[0, 0, 0x01, 0x57]);
        // authorizationSize, then TPM_RS_PW with empty nonce and hmac.
        assert_eq!(&bytes[14..18], &[0, 0, 0, 9]);
        assert_eq!(&bytes[18..22], &[0x40, 0, 0, 9]);
    }
}
