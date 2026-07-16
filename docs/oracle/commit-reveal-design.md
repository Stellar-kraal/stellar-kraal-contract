# Commit-Reveal Scheme for Satellite Monitoring Data Verification

## Overview

To prevent front-running, manipulation, and to allow for a dispute window before a price is finalized, `carbon_oracle` implements a commit-reveal scheme. This scheme decouples the commitment to a price entry from its final publication. It enables a challenge window during which authorized participants can dispute the measurement.

## State Machine

The scheme introduces a `CommitmentState` for price entries:

1. **Committed**: A commitment hash is submitted to the contract. Wait, in our implementation it transitions immediately to `ChallengeWindow`.
2. **ChallengeWindow**: A period of time (defined by `challenge_window_duration` ledgers) during which the commitment can be challenged.
3. **Disputed**: The commitment has been challenged by an authorized party. The commitment cannot be revealed. The resolution of the dispute is handled out of scope (e.g. by an off-chain token design or governance).
4. **Revealed**: The challenge window has passed without any dispute. The oracle reveals the preimage of the commitment, which is verified, and the price is finalized on-chain.

## Workflow

### 1. Commit Phase

The oracle generates a local `GEEResult` along with a random 32-byte `salt`. The `salt` hides the cleartext parameters. 

A commitment hash is generated:
```
commitment_hash = SHA256(script_hash || input_params_hash || output_value || timestamp_utc || feed_id || salt)
```

The oracle calls `commit_price` with `commitment_hash`. This stores the commitment and begins the challenge window.

### 2. Challenge Phase

For `challenge_window_duration` ledgers, the commitment remains pending. 
Any authenticated challenger can call `challenge_price` if they believe the measurement is invalid (for example, if they've acquired independent satellite data for the same feed).
If challenged, the commitment enters the `Disputed` state and cannot be revealed.

### 3. Reveal Phase

Once the challenge window has expired, the oracle calls `reveal_price` with the full cleartext parameters, the `salt`, and an Ed25519 signature over the standard attestation payload.

The contract:
1. Re-derives the commitment hash and ensures it matches the stored `commitment_hash`.
2. Verifies the Ed25519 signature.
3. If both checks pass, it transitions the state to `Revealed` and persists the `output_value` to the main price feed.

## Python Bridge Integration

The `oracle_bridge.bridge.OracleBridge` provides two methods to interact with this scheme:
- `commit(result)`: Generates a salt, computes the hash, and submits the commitment. Returns the `salt`.
- `reveal(result, salt)`: Signs the attestation payload and submits the reveal.

This architecture ensures integrity, delays data visibility during the computation phase, and builds the foundation for economically incentivized dispute resolution.
