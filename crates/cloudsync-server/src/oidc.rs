use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct OidcValidator {
    pub issuer: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub aud: String,
    pub email: String,
}

impl OidcValidator {
    pub async fn validate(&self, jwt: String) -> anyhow::Result<Claims> {
        // Fetch well known

        // Fetch JWKS
        let jwks = fetch_jwks(&self.issuer).await?;

        // JWT decode
        let header = jsonwebtoken::decode_header(jwt.as_str())?;

        // Check whick key
        let kid = header
            .kid
            .ok_or_else(|| anyhow::anyhow!("missing kid header field"))?;
        let jwk = jwks
            .keys
            .iter()
            .find(|k| k.kid == kid)
            .ok_or_else(|| anyhow::anyhow!("no matching key for kid: {kid}"))?;
        let key = jsonwebtoken::DecodingKey::from_rsa_components(&jwk.n, &jwk.e)?;

        // Verify
        let claims = jsonwebtoken::decode::<Claims>(
            &jwt,
            &key,
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256),
        )?;

        Ok(claims.claims)
    }
}

#[derive(Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
    issuer: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

async fn fetch_jwks(issuer: &str) -> anyhow::Result<Jwks> {
    // Get jwks_uri
    let well_known = ".well-known/openid-configuration";
    let well_known = format!("{issuer}/{well_known}");
    let client: reqwest::Client = reqwest::Client::new();
    let response = client.get(well_known).send().await?;
    let bytes = response.bytes().await?;
    let oidc = serde_json::from_slice::<OidcDiscovery>(&bytes)?;

    if oidc.issuer != issuer {
        anyhow::bail!("issuer does not match");
    }

    // Fetch jwks
    let jwks_resp = client.get(oidc.jwks_uri).send().await?;
    let bytes = jwks_resp.bytes().await?;
    let jwks = serde_json::from_slice::<Jwks>(&bytes)?;

    Ok(jwks)
}
