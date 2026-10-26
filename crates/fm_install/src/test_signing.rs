// Signing for tests only (feature "testing"; never in the app): a throwaway key pair, so tests of
// the feed signature in fm_install, fm_pack and the app sign the packages that they create. The
// output has the Tauri format of `tauri signer sign --app-version`, which `package::verify` reads;
// the fixture test in package.rs proves that format with a real tauri-cli signature.
use base64::Engine;

/// A throwaway signing key.
pub struct TestKey(minisign::KeyPair);

impl Default for TestKey {
    fn default() -> Self {
        Self(minisign::KeyPair::generate_unencrypted_keypair().expect("test key"))
    }
}

impl TestKey {
    /// The public key as the app keeps it: base64 of the minisign public key file.
    pub fn public(&self) -> String {
        b64(&self.0.pk.to_box().expect("public key box").into_string())
    }

    /// A signature of `bytes` for `version`.
    pub fn sign(&self, bytes: &[u8], version: &str) -> String {
        let trusted = format!("timestamp:0\tfile:test.zip\tversion:{version}");
        let sig = minisign::sign(Some(&self.0.pk), &self.0.sk, std::io::Cursor::new(bytes), Some(&trusted), Some("test")).expect("test signature");
        b64(&sig.into_string())
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}
