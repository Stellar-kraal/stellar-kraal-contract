#[cfg(test)]
mod tests {
    use soroban_sdk::{symbol_short};

    use crate::{migration, Error};

    // ── Migration Logic Tests (via module) ──────────────────────────────────

    #[test]
    fn test_schema_compatibility_v1_to_v2_valid() {
        assert_eq!(migration::validate_schema_compatibility(1, 2), Ok(()));
    }

    #[test]
    fn test_schema_compatibility_invalid_direction() {
        assert_eq!(
            migration::validate_schema_compatibility(2, 1),
            Err(Error::IncompatibleSchema)
        );
    }

    #[test]
    fn test_schema_compatibility_unsupported_versions() {
        assert_eq!(
            migration::validate_schema_compatibility(1, 3),
            Err(Error::IncompatibleSchema)
        );
    }

    #[test]
    fn test_schema_version_constants() {
        assert_eq!(crate::CURRENT_SCHEMA_VERSION, 2);
        assert_eq!(crate::MIN_SCHEMA_VERSION, 1);
    }
}
