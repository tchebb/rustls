use msgs::enums::SignatureScheme;
use msgs::handshake::SessionID;
use rand;
use sign;
use key;
use webpki;
use server;
use error::TLSError;

use std::collections;
use std::sync::{Arc, Mutex};

/// Something which never stores sessions.
pub struct NoServerSessionStorage {}

impl server::StoresServerSessions for NoServerSessionStorage {
    fn generate(&self) -> SessionID {
        SessionID::empty()
    }
    fn put(&self, _id: Vec<u8>, _sec: Vec<u8>) -> bool {
        false
    }
    fn get(&self, _id: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// An implementor of `StoresServerSessions` that stores everything
/// in memory.  If enforces a limit on the number of stored sessions
/// to bound memory usage.
pub struct ServerSessionMemoryCache {
    cache: Mutex<collections::HashMap<Vec<u8>, Vec<u8>>>,
    max_entries: usize,
}

impl ServerSessionMemoryCache {
    /// Make a new ServerSessionMemoryCache.  `size` is the maximum
    /// number of stored sessions.
    pub fn new(size: usize) -> Arc<ServerSessionMemoryCache> {
        debug_assert!(size > 0);
        Arc::new(ServerSessionMemoryCache {
            cache: Mutex::new(collections::HashMap::new()),
            max_entries: size,
        })
    }

    fn limit_size(&self) {
        let mut cache = self.cache.lock().unwrap();
        while cache.len() > self.max_entries {
            let k = cache.keys().next().unwrap().clone();
            cache.remove(&k);
        }
    }
}

impl server::StoresServerSessions for ServerSessionMemoryCache {
    fn generate(&self) -> SessionID {
        let mut v = [0u8; 32];
        rand::fill_random(&mut v);
        SessionID::new(&v)
    }

    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> bool {
        self.cache.lock()
            .unwrap()
            .insert(key, value);
        self.limit_size();
        true
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.cache.lock()
            .unwrap()
            .get(key).cloned()
    }
}

/// Something which never produces tickets.
pub struct NeverProducesTickets {}

impl server::ProducesTickets for NeverProducesTickets {
    fn enabled(&self) -> bool {
        false
    }
    fn get_lifetime(&self) -> u32 {
        0
    }
    fn encrypt(&self, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn decrypt(&self, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// Something which never resolves a certificate.
pub struct FailResolveChain {}

impl server::ResolvesServerCert for FailResolveChain {
    fn resolve(&self,
               _server_name: Option<webpki::DNSNameRef>,
               _sigschemes: &[SignatureScheme])
               -> Option<sign::CertifiedKey> {
        None
    }
}

/// Something which always resolves to the same cert chain.
pub struct AlwaysResolvesChain(sign::CertifiedKey);

impl AlwaysResolvesChain {
    pub fn new_rsa(chain: Vec<key::Certificate>,
                   priv_key: &key::PrivateKey) -> AlwaysResolvesChain {
        let key = sign::RSASigningKey::new(priv_key)
            .expect("Invalid RSA private key");
        let key: Arc<Box<sign::SigningKey>> = Arc::new(Box::new(key));
        AlwaysResolvesChain(sign::CertifiedKey::new(chain, key))
    }

    pub fn new_rsa_with_extras(chain: Vec<key::Certificate>,
                               priv_key: &key::PrivateKey,
                               ocsp: Vec<u8>,
                               scts: Vec<u8>) -> AlwaysResolvesChain {
        let mut r = AlwaysResolvesChain::new_rsa(chain, priv_key);
        if !ocsp.is_empty() {
            r.0.ocsp = Some(ocsp);
        }
        if !scts.is_empty() {
            r.0.sct_list = Some(scts);
        }
        r
    }
}

impl server::ResolvesServerCert for AlwaysResolvesChain {
    fn resolve(&self,
               _server_name: Option<webpki::DNSNameRef>,
               _sigschemes: &[SignatureScheme])
               -> Option<sign::CertifiedKey> {
        Some(self.0.clone())
    }
}

/// Something that resolves do different cert chains/keys based
/// on client-supplied server name (via SNI).
pub struct ResolvesServerCertUsingSNI {
    by_name: collections::HashMap<String, sign::CertifiedKey>,
}

impl ResolvesServerCertUsingSNI {
    /// Create a new and empty (ie, knows no certificates) resolver.
    pub fn new() -> ResolvesServerCertUsingSNI {
        ResolvesServerCertUsingSNI { by_name: collections::HashMap::new() }
    }

    /// Add a new `sign::CertifiedKey` to be used for the given SNI `name`.
    ///
    /// This function fails if `name` is not a valid DNS name, or if
    /// it's not valid for the supplied certificate, or if the certificate
    /// chain is syntactically faulty.
    pub fn add(&mut self, name: &str, ck: sign::CertifiedKey) -> Result<(), TLSError> {
        let checked_name = webpki::DNSNameRef::try_from_ascii_str(name)
            .map_err(|_| TLSError::General("Bad DNS name".into()))?;

        ck.cross_check_end_entity_cert(Some(checked_name))?;
        self.by_name.insert(name.into(), ck);
        Ok(())
    }
}

impl server::ResolvesServerCert for ResolvesServerCertUsingSNI {
    fn resolve(&self,
               server_name: Option<webpki::DNSNameRef>,
               _sigschemes: &[SignatureScheme])
               -> Option<sign::CertifiedKey> {
        if let Some(name) = server_name {
            self.by_name.get(name.into())
                .map(|ck| ck.clone())
        } else {
            // This kind of resolver requires SNI
            None
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use StoresServerSessions;

    #[test]
    fn test_noserversessionstorage_yields_no_sessid() {
        let c = NoServerSessionStorage {};
        assert_eq!(c.generate(), SessionID::empty());
        assert_eq!(c.generate().len(), 0);
        assert!(c.generate().is_empty());
    }

    #[test]
    fn test_noserversessionstorage_drops_put() {
        let c = NoServerSessionStorage {};
        assert_eq!(c.put(vec![0x01], vec![0x02]), false);
    }

    #[test]
    fn test_noserversessionstorage_denies_gets() {
        let c = NoServerSessionStorage {};
        c.put(vec![0x01], vec![0x02]);
        assert_eq!(c.get(&[]), None);
        assert_eq!(c.get(&[0x01]), None);
        assert_eq!(c.get(&[0x02]), None);
    }

    #[test]
    fn test_serversessionmemorycache_yields_sessid() {
        let c = ServerSessionMemoryCache::new(4);
        assert_eq!(c.generate().len(), 32);
    }

    #[test]
    fn test_serversessionmemorycache_accepts_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert_eq!(c.put(vec![0x01], vec![0x02]), true);
    }

    #[test]
    fn test_serversessionmemorycache_persists_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert_eq!(c.put(vec![0x01], vec![0x02]), true);
        assert_eq!(c.get(&[0x01]), Some(vec![0x02]));
        assert_eq!(c.get(&[0x01]), Some(vec![0x02]));
    }

    #[test]
    fn test_serversessionmemorycache_overwrites_put() {
        let c = ServerSessionMemoryCache::new(4);
        assert_eq!(c.put(vec![0x01], vec![0x02]), true);
        assert_eq!(c.put(vec![0x01], vec![0x04]), true);
        assert_eq!(c.get(&[0x01]), Some(vec![0x04]));
    }

    #[test]
    fn test_serversessionmemorycache_drops_to_maintain_size_invariant() {
        let c = ServerSessionMemoryCache::new(4);
        assert_eq!(c.put(vec![0x01], vec![0x02]), true);
        assert_eq!(c.put(vec![0x03], vec![0x04]), true);
        assert_eq!(c.put(vec![0x05], vec![0x06]), true);
        assert_eq!(c.put(vec![0x07], vec![0x08]), true);
        assert_eq!(c.put(vec![0x09], vec![0x0a]), true);

        let mut count = 0;
        if c.get(&[0x01]).is_some() { count += 1; }
        if c.get(&[0x03]).is_some() { count += 1; }
        if c.get(&[0x05]).is_some() { count += 1; }
        if c.get(&[0x07]).is_some() { count += 1; }
        if c.get(&[0x09]).is_some() { count += 1; }

        assert_eq!(count, 4);
    }
}
