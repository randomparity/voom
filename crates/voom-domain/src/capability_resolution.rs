//! Routing resolution semantics for capability dispatch.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityResolution {
    /// Exactly one plugin in the system may claim this capability. Kernel
    /// rejects a second registration claiming the same capability variant.
    Exclusive,
    /// Multiple plugins may claim this capability, but at most one per key
    /// (e.g. one Discover plugin per scheme). Kernel rejects a registration
    /// whose key collides with any existing claim.
    Sharded,
    /// Multiple plugins may claim this capability; no uniqueness enforced.
    /// Kernel picks one at dispatch time using priority (existing behavior).
    Competing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_variants_are_not_equal() {
        assert_ne!(
            CapabilityResolution::Exclusive,
            CapabilityResolution::Sharded
        );
        assert_ne!(
            CapabilityResolution::Sharded,
            CapabilityResolution::Competing
        );
        assert_ne!(
            CapabilityResolution::Exclusive,
            CapabilityResolution::Competing
        );
    }
}
