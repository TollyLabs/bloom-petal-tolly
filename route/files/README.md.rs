petal::route_file!(
    spec: petal::static_read_spec(),
    read: |_ctx: &petal::Ctx| petal::DispatchResponse::Read(crate::docs::README_MD.as_bytes().to_vec())
);
