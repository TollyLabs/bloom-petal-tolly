// The navigation map of every TOLLY route (trading and agentic), embedded
// at build time from docs/GUIDE.md: static, no capability, always readable.
petal::route_file!(
    spec: petal::static_read_spec(),
    read: |_ctx: &petal::Ctx| petal::DispatchResponse::Read(crate::docs::GUIDE_MD.as_bytes().to_vec())
);
