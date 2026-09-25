//! Which registered vault a request's address names.

use norn_wire::{ErrorEnvelope, VaultAddress, VaultName};

use crate::refusal::unsupported_attach_mode;

/// The registered name `address` names, or the refusal a request addressed
/// that way is answered with.
///
/// A registered name resolves to itself. Whether the registry holds it is not
/// decided here: the serving set's own lookup decides it, at the hold or the
/// demand the caller takes next, and refuses an unknown name as
/// `host/unknown-vault` from that read.
///
/// **A root is a dormant carrier.** A root addresses its vault with no
/// registration behind it, which asks for a throwaway attach — disposable
/// derivation over a store thrown away with the work — and the attach seam
/// refuses that mode as `host/unsupported-attach-mode`, through the rendering
/// a demand naming the mode is refused with. Layer 6 consumes it: that is
/// where an unregistered working directory first meets a real client. The
/// call graph reaches nothing past this refusal yet because no host
/// establishes an entry over an unregistered root; the store opens a
/// throwaway database today, and the host lifecycle that would serve one is
/// what Layer 6 builds.
pub(crate) fn registered_name(address: &VaultAddress) -> Result<&VaultName, ErrorEnvelope> {
    address
        .registered_name()
        .ok_or_else(|| unsupported_attach_mode(address.attach_mode()))
}

#[cfg(test)]
mod tests {
    use norn_wire::{AttachMode, ErrorDetail, VaultRoot};

    use super::*;

    /// A registered name resolves to itself, whether or not the registry
    /// holds it: the serving set's lookup is what refuses an unknown one.
    #[test]
    fn a_registered_name_resolves_to_itself() {
        let name = VaultName::new("notes").expect("a legal vault name");
        let address = VaultAddress::name(name.clone());
        assert_eq!(registered_name(&address), Ok(&name));
    }

    /// A root asks for a throwaway attach, which the attach seam refuses.
    #[test]
    fn a_root_is_refused_as_an_unsupported_attach() {
        let address =
            VaultAddress::root(VaultRoot::new("/home/person/notes").expect("an absolute root"));
        let envelope = registered_name(&address).expect_err("a root resolved to a name");
        assert_eq!(
            envelope.detail(),
            &ErrorDetail::unsupported_attach_mode(AttachMode::Throwaway)
        );
    }
}
