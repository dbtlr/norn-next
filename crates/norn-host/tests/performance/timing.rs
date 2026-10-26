use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct GetPrerequisites {
    content_complete_ns: Option<u128>,
    structured_ns: BTreeMap<String, u128>,
}

impl GetPrerequisites {
    pub(super) fn content_complete(&mut self, observed_ns: u128) {
        self.content_complete_ns = Some(observed_ns);
    }

    pub(super) fn structured(&mut self, path: &str, observed_ns: u128) {
        self.structured_ns.insert(path.to_owned(), observed_ns);
    }

    pub(super) fn available_ns(&self, path: &str) -> Option<u128> {
        Some(
            self.content_complete_ns?
                .max(*self.structured_ns.get(path)?),
        )
    }
}
