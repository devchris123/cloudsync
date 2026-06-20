use std::{sync::Arc, time::Instant};

use jsonwebtoken::Algorithm;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub aud: Audience,
    pub email: String,
    pub exp: usize,
}

#[derive(Deserialize, Clone)]
struct OidcDiscovery {
    jwks_uri: String,
    issuer: String,
}

#[derive(Deserialize, Clone)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "kty")]
enum Jwk {
    #[serde(rename = "RSA")]
    Rsa { kid: String, n: String, e: String },
    EC {
        kid: String,
        x: String,
        y: String,
        crv: String,
    },
}

#[derive(Debug)]
enum ValidationError {
    MissingKid,
    UnknownKid(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for ValidationError {
    fn from(err: anyhow::Error) -> Self {
        ValidationError::Other(err)
    }
}

impl From<jsonwebtoken::errors::Error> for ValidationError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        ValidationError::Other(err.into())
    }
}

#[derive(Clone)]
pub struct OidcValidator {
    pub issuer: String,
    pub discovery_url: String,
    pub audience: String,
    pub client: Client,
    well_known_cache: Arc<RwLock<Option<(OidcDiscovery, std::time::Instant)>>>,
    jwks_cache: Arc<RwLock<Option<(Jwks, std::time::Instant)>>>,
}

impl OidcValidator {
    pub fn new(issuer: String, discovery_url: String, audience: String) -> Self {
        OidcValidator {
            issuer: issuer.trim_end_matches('/').to_string(),
            discovery_url: discovery_url.trim_end_matches('/').to_string(),
            audience,
            client: reqwest::Client::new(),
            well_known_cache: Arc::new(RwLock::new(None)),
            jwks_cache: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn validate(&self, jwt: String) -> anyhow::Result<Claims> {
        // Fetch well known
        let oidc = self.fetch_well_known().await?;
        if oidc.issuer != self.issuer {
            anyhow::bail!("issuer does not match");
        }

        // Fetch JWKS
        let jwks = self.fetch_jwks(&oidc).await?;

        // Validate JWT
        let claims = self.validate_jwt(jwt.as_str(), &jwks);

        match claims {
            Ok(claims) => Ok(claims),
            Err(ValidationError::UnknownKid(kid)) => {
                tracing::warn!("unknown kid in JWT: {kid}, refreshing JWKS and retrying");
                // Key might have rotated, reftech JWKS and retry once
                self.invalidate_cache().await;
                let jwks = self.fetch_jwks(&oidc).await?;
                self.validate_jwt(jwt.as_str(), &jwks).map_err(|err| {
                    anyhow::anyhow!("validation failed after refreshing JWKS: {:?}", err)
                })
            }
            Err(ValidationError::MissingKid) => anyhow::bail!("missing kid header field"),
            Err(ValidationError::Other(err)) => Err(err),
        }
    }

    async fn fetch_well_known(&self) -> anyhow::Result<OidcDiscovery> {
        {
            let cache = self.well_known_cache.read().await;
            if let Some((oidc, fetched_at)) = &*cache
                && fetched_at.elapsed() < Duration::from_secs(3600)
            {
                return Ok(oidc.clone());
            }
        }
        let well_known = ".well-known/openid-configuration";
        let well_known = format!("{}/{well_known}", self.discovery_url);
        let response = self.client.get(well_known).send().await?;
        let bytes = response.bytes().await?;
        let oidc = serde_json::from_slice::<OidcDiscovery>(&bytes)?;
        let mut cache = self.well_known_cache.write().await;
        cache.replace((oidc.clone(), Instant::now()));
        Ok(oidc)
    }

    async fn invalidate_cache(&self) {
        let mut cache = self.jwks_cache.write().await;
        cache.take();
    }

    async fn fetch_jwks(&self, oidc: &OidcDiscovery) -> anyhow::Result<Jwks> {
        {
            let cache = self.jwks_cache.read().await;
            if let Some((jwks, fetched_at)) = cache.as_ref()
                && fetched_at.elapsed() < Duration::from_secs(300)
            {
                return Ok(jwks.clone());
            }
        }

        let jwks_resp = self.client.get(&oidc.jwks_uri).send().await?;
        let bytes = jwks_resp.bytes().await?;
        let jwks = serde_json::from_slice::<Jwks>(&bytes)?;
        let mut cache = self.jwks_cache.write().await;
        cache.replace((jwks.clone(), Instant::now()));
        Ok(jwks)
    }

    fn validate_jwt(&self, jwt: &str, jwks: &Jwks) -> Result<Claims, ValidationError> {
        // JWT decode
        let header = jsonwebtoken::decode_header(jwt)?;

        // Check which key
        let kid = header.kid.ok_or_else(|| ValidationError::MissingKid)?;
        let jwk = jwks
            .keys
            .iter()
            .find(|k| match k {
                Jwk::Rsa { kid: mykid, .. } => *mykid == kid,
                Jwk::EC { kid: mykid, .. } => *mykid == kid,
            })
            .ok_or_else(|| ValidationError::UnknownKid(kid))?;

        let (alg, key) = match jwk {
            Jwk::Rsa { n, e, .. } => (
                Algorithm::RS256,
                jsonwebtoken::DecodingKey::from_rsa_components(n, e)?,
            ),
            Jwk::EC { x, y, crv, .. } => {
                let alg = match crv.as_str() {
                    "P-256" => Algorithm::ES256,
                    "P-384" => Algorithm::ES384,
                    _ => {
                        return Err(ValidationError::Other(anyhow::anyhow!(
                            "unsupported curve: {crv}"
                        )));
                    }
                };
                (alg, jsonwebtoken::DecodingKey::from_ec_components(x, y)?)
            }
        };

        // Verify
        let mut validation = jsonwebtoken::Validation::new(alg);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        let claims = jsonwebtoken::decode::<Claims>(jwt, &key, &validation)?;
        Ok(claims.claims)
    }
}

#[cfg(test)]
mod test {
    use crate::oidc::{Audience, Claims, Jwk, Jwks, OidcValidator};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::traits::PublicKeyParts;

    #[test]
    fn test_validate_token_rsa() {
        let (claims, oidc, private_key, jwks) = create_test_setup();

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks).unwrap();

        // Assert
        assert_eq!(res.sub, "user123");
        assert_eq!(res.iss, "issuer");
        assert_eq!(res.aud, Audience::Single("audience".to_string()));
        assert_eq!(res.email, "test@example.com");
    }

    #[test]
    fn test_validate_token_ec() {
        let (claims, oidc, _, _) = create_test_setup();
        let (ec_private_key, jwks) = create_test_elliptic_curve();

        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_ec_pem(ec_private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks).unwrap();

        // Assert
        assert_eq!(res.sub, "user123");
        assert_eq!(res.iss, "issuer");
        assert_eq!(res.aud, Audience::Single("audience".to_string()));
        assert_eq!(res.email, "test@example.com");
    }

    #[test]
    fn test_invalid_token() {
        let (_, oidc, _, jwks) = create_test_setup();

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let token = "invalid.token.value".to_string();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_token_invalid_audience() {
        let (mut claims, oidc, private_key, jwks) = create_test_setup();
        claims.aud = Audience::Single("invalid-audience".to_string());

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_token_invalid_issuer() {
        let (mut claims, oidc, private_key, jwks) = create_test_setup();
        claims.iss = "invalid-issuer".into();

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_missing_kid_header() {
        let (claims, oidc, private_key, jwks) = create_test_setup();

        let header = Header::new(Algorithm::RS256);

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_missing_kid_in_jwks() {
        let (claims, oidc, private_key, mut jwks) = create_test_setup();

        // Remove the key from JWKS
        jwks.keys.clear();

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_expired_token() {
        let (mut claims, oidc, private_key, jwks) = create_test_setup();
        claims.exp = 1; // Set expiration in the past

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-kid".to_string());

        let key = EncodingKey::from_rsa_pem(private_key.as_bytes()).unwrap();
        let token = encode(&header, &claims, &key).unwrap();

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    #[test]
    fn test_reject_algorithm() {
        let (claims, oidc, _, jwks) = create_test_setup();

        // Manually build a token with HS256 header but garbage signature
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256","kid":"test-kid"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_string(&claims).unwrap());
        let token = format!("{header}.{payload}.fakesignature");

        // Execute
        let res = oidc.validate_jwt(token.as_str(), &jwks);

        // Assert
        assert!(res.is_err());
    }

    fn create_test_setup() -> (Claims, OidcValidator, String, Jwks) {
        let (private_key, jwks) = create_test_jwks();
        (
            Claims {
                iss: "issuer".into(),
                sub: "user123".into(),
                aud: Audience::Single("audience".to_string()),
                email: "test@example.com".into(),
                exp: 9999999999,
            },
            OidcValidator::new(
                "issuer".to_string(),
                "discovery_url".to_string(),
                "audience".to_string(),
            ),
            private_key,
            jwks,
        )
    }

    fn create_test_jwks() -> (String, Jwks) {
        let private_key = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDZePvklQRubCg/
2mCTRyEPfmkwjdhRKBJ8ykf7dhj1jFkIPhzhIhpnXK4xYcozcdnBV/DPIb1O2lH9
bCe3oRpcsN9eb6GYglOe3c1hX61Wo/nRv/hF8hlvkrnCg6RyQuzR/q8LfLYk2UEw
HcYxU9nGGo8CVCKIA++qF4RY/G7WraWkX8AAf61TrSEC3v2VYpi45UXEpILZmrUN
B7V42yH2ig+OzUUyCzuNr7wdH954HxhTDK45FuwbD1WN+Zg6UCSJH5JPm5t2CrbP
qbcyoaglDGgoiUZO+Xdpet/feMwfh5Zc3tkOjZfcl/hWFGrsDBnylC8dnFYITTo+
u260Fe7fAgMBAAECggEAXQhImey1zJcwUMCW9+pB1mL5lO/ZTj6aShAu4wAOhVzY
6ZHIwPbZ3MXlLvLqkT9vLCr2tWV1mroCwSr3grLEmEqCA+A1fQyjwR6ZscJAYQQc
5wH8r8912ikmlnPCca73qI4PTBa5xOG75V2XX5rDWuAZtaFQdGdaq6UL1RWIRQWR
q3HK/OC1zuQPaq8rumHm9VadOv1har73gYKMNKkpgDRr4ws+ik9pBnQqdUFxiNal
W57+ao1c/HpRIWnqItmBUIfBwl7Gh6s8d6IZa+dD7jeN2Fi5HYP1VEBtyb0T+aPI
MB6oEKvPGBHC2EO9vNnYSpfYN1ljG6k7ewffySKiIQKBgQD9D6DMnoIUxyZtJj/a
dSJeqG7jlk1iuzJWP+bKAklaztX8+4S//3FM3pSXcGIr0iz6tScHhCovNrSjUVCu
hYxtCL9uR8SGJrupHCNz9vmaLk1HlF/NIiq+pM/qnOp/+L7Bi6iu3abiNE7SRNVm
aKVgZFr6F/DRwB9PTqi4RqD8cwKBgQDb/4xg7ooeFgYUMDPXwwaK3QWJNgXth/8W
E6N6pXRRyhwz9qyekWaod4qbjIb+x+lweFjB+fVAlRpAnJ5J1N9v98z/0MGimcQc
+pPgaoLWbuFZoYGqpS6unM218XtctXOHra8PIe1lTf1MG3xloF51ucJnZZGP2cNG
LGrbYZB05QKBgQDrhtQeHZjsRb5Z8DOV21c1yoYKhCVaMuhSpf7jHOWxArjfUCjp
mZGV/cNGf26fYmpCnL/KmxO4Ba5yIoh5JgrgoDerKFickwguCOZmVANToKyEZnAT
uC0YasSok4sduCGyeY1x0xIzjoOd6DrFqbfh0wVpp0aXsbxyT79wYywKSQKBgAyq
WK2X7hGvWOg+oi1wx+aktNXia1LyemgN92JvNhQjW55OPD/gxRU71JoB7B+s6K6V
7x4zwr/WFa3UlnRPshFjJcUwgoVW7uhwMKVB3Ih117luR+XIHrjkxB8OaPi8ZYtR
H3vyixVC+Ssxhebf5bBHYn7LZSbv9YMLuZcptcRVAoGAKoZHmYDPvTs71LLv4I5q
gS5ZBeVNhXa8kITMuOumdxv7UVY2XLx8wvpZOZKT6td5k+dhSlaW9ClPyBXA60V0
lRsvB0LPb4MH0ye3t9mGe/XoaBWKtheejyQ9CUrDVnQQRxfXXnUkXGMwNgw6RPPd
Nc3GLyMxuf/cuSIU05L70Os=
-----END PRIVATE KEY-----";
        let rsa_key = RsaPrivateKey::from_pkcs8_pem(private_key).unwrap();
        let pub_key = rsa_key.to_public_key();
        let n = URL_SAFE_NO_PAD.encode(pub_key.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(pub_key.e().to_bytes_be());
        (
            private_key.into(),
            Jwks {
                keys: vec![Jwk::Rsa {
                    kid: "test-kid".into(),
                    n: n.into(),
                    e: e.into(),
                }],
            },
        )
    }

    use p256::SecretKey;
    use p256::elliptic_curve::sec1::ToEncodedPoint;

    fn create_test_elliptic_curve() -> (String, Jwks) {
        let priv_key = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgVqWno0lr4PbJz2f4
wNMbbCqMmVl0SdG65jMBSGM9lc+hRANCAARA4l5fHhXZKFQfYg/MtSV3gYcjuY1X
+pxhervZB6m/jLfvVDLZO90N7VhNedAhg4J/GU9jojkfDFQ9k2MrFNPb
-----END PRIVATE KEY-----";

        let ec_key = SecretKey::from_pkcs8_pem(priv_key).unwrap();
        let pub_key = ec_key.public_key().to_encoded_point(false);
        let x = URL_SAFE_NO_PAD.encode(pub_key.x().unwrap());
        let y = URL_SAFE_NO_PAD.encode(pub_key.y().unwrap());

        (
            priv_key.into(),
            Jwks {
                keys: vec![Jwk::EC {
                    kid: "test-kid".into(),
                    x,
                    y,
                    crv: "P-256".into(),
                }],
            },
        )
    }
}
