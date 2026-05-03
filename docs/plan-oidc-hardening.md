# OIDC Validation Hardening Plan

## 1. Algorithm flexibility

- Support a configurable list of allowed algorithms (e.g. `RS256`, `ES256`)
- Reject tokens with any `alg` not in the whitelist (prevents algorithm confusion attacks)
- Set `Validation.algorithms` to the allowed list

## 2. Claims validation

- **Issuer (`iss`)**: verify it matches the configured issuer URL
- **Audience (`aud`)**: verify it contains our client ID (prevents cross-service token reuse)
- **Expiry (`exp`) / Not Before (`nbf`)**: confirm `jsonwebtoken::Validation` has these enabled (on by default, but be explicit)

## 3. Graceful error handling

- Handle missing `kid` in token header — return error instead of panic
- Handle unknown `kid` (not found in JWKS) — return clear error

## 4. JWKS caching

- Cache the JWKS response in memory (avoid fetching on every request)
- Use a TTL (e.g. 5-10 minutes) to bound staleness
- On cache hit, use cached keys directly

## 5. JWKS refresh on failure

- If validation fails due to unknown `kid`, refetch JWKS once and retry
- This handles key rotation: Keycloak adds a new key, old cache doesn't have it
- Limit refetches (at most once per N seconds) to prevent abuse triggering constant fetches

## 6. HTTP hardening

- Set a timeout on all outgoing requests (discovery + JWKS fetch), e.g. 5 seconds
- Limit response body size for JWKS/discovery (e.g. 512KB) to prevent resource exhaustion
- Reuse a single `reqwest::Client` (connection pooling)

## 7. Discovery caching

- Cache the `.well-known/openid-configuration` response
- Rarely changes — longer TTL than JWKS (e.g. 1 hour)
- Refetch on startup or on JWKS fetch failure (in case `jwks_uri` changed)
